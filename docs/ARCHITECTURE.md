# desktop-snap architecture

This document describes how `desktop-snap` captures Windows windows from WSL2 and why it is built that way.

## 1. Problem and evidence

Hardware-accelerated OpenGL windows, Vulkan windows, DirectX windows and windows with transparent DWM backgrounds are not reproduced by the classic GDI capture paths. `BitBlt`, `PIL.ImageGrab`, `PrintWindow` and the `ffmpeg ddagrab` capture produce white or empty surfaces for such windows. Windows Graphics Capture (WGC) returns the actual rendered frame.

In a live test on Windows 11 24H2 (build 26100), a WGC prototype captured an `eframe`/`glow` window that uses `with_transparent(true)` on NVIDIA OpenGL, while the four GDI-era paths did not. The prototype was built for `x86_64-pc-windows-gnu` with the `windows` crate 0.62. The WSL interop round trip took 470 to 480 ms, so the service target is one second, not a shorter latency goal.

## 2. Components

- **`desktop-snap.exe`** (Rust, `x86_64-pc-windows-gnu`): window enumeration, policy, WGC capture, alpha conversion, flat-image check, PNG output. It embeds a build hash for `--version`.
- **`bin/desktop-snap`** (Python 3 standard library): the WSL-side wrapper. It resolves Windows paths, stages files, enforces the timeout, writes the audit log and passes receipts through.
- **Presets** in `~/.config/desktop-snap/presets.json`, so new target apps need no rebuild.
- **Denylist extension** in `~/.config/desktop-snap/denylist.json`, which can only add entries.

## 3. Capture backends

Only these backends may produce a successful result:

1. **`wgc-window`** is used for `window`, and for `region` restricted to a target window. It creates a capture item with `IGraphicsCaptureItemInterop::CreateForWindow`, uses a free-threaded frame pool, and reads the frame through a D3D11 staging texture. `IsIconic` is checked first, and minimised windows fail with exit code `4` instead of waiting. Cloaked windows and invisible windows are excluded during selection. A frame that does not arrive within two seconds fails with exit code `4` (`no_frame`).
2. **`wgc-composite`** is used for `group` and presets. Each member window is captured separately with `wgc-window`, so an overlapping window does not corrupt the result. Positions come from `DWMWA_EXTENDED_FRAME_BOUNDS` rather than `GetWindowRect`, which avoids the shadow offsets of docked windows.
3. **`wgc-monitor`** is used only for `monitor` and for a full-monitor `region`, and only with `--allow-full-monitor`.

WGC is the only capture technology. When WGC refuses access, for example because a window is excluded with `WDA_EXCLUDEFROMCAPTURE`, the tool fails with exit code `5`. It never switches to a GDI, `BitBlt`, `PrintWindow` or DXGI path.

DPI handling uses a `PerMonitorV2` application manifest, so coordinates match the physical pixels of mixed-DPI setups.

## 4. Alpha and flat-image check

WGC delivers premultiplied BGRA. The tool converts it to unpremultiplied RGBA. Where alpha is zero, the colour channels are set to zero, which avoids a division by zero. The output is a transparent PNG by default.

The quality check counts distinct colours and the share of the most common colour, using only pixels with alpha greater than zero. A nearly uniform image (`top_color_share >= 0.92` and `distinct_colors <= 50`) is rejected with exit code `2`, and no output file is written. The check guards against silent blank captures; it does not replace the WGC-only rule.

## 5. Security and privacy

1. **Enforcement in the EXE.** The built-in denylist, the containment check, the full-monitor gate and the exit codes `5`, `6` and `8` live in `desktop-snap.exe`. A direct call to the EXE is therefore subject to the same rules as a wrapper call.
2. **Built-in denylist.** It covers password managers, Windows security prompts, remote-access tools, and titles of browser incognito windows and online-banking pages. It is compiled into the EXE. Users can extend it with `--extra-denylist` but cannot turn entries off. A denied match exits with code `6` and writes no pixels.
3. **Fail-closed on unknown process names.** A window whose process name cannot be read, for example an elevated process, is treated as denied.
4. **Metadata minimisation.** `list` returns handle, process name, rectangle, visibility, minimised and cloaked state, and no titles. Titles appear only with `--titles`, and denied windows keep their titles redacted.
5. **No focus changes.** The tool never calls `SetForegroundWindow`, `ShowWindow` or any input-injection API. It does not move the mouse.
6. **Unambiguous selection.** Selection by title, process or HWND must match exactly one window. Zero matches give exit code `1`; several matches give exit code `3` with the candidate list. The tool never picks the first match silently.
7. **Full-screen gate.** `monitor` always requires `--allow-full-monitor`. `region` requires it too, unless the region is bound to a target window (`--process` or `--hwnd`) and lies completely inside that window's frame bounds. A violated gate gives exit code `8`.

## 6. WSL interop

- The wrapper starts the EXE with stdin closed and in its own process group, with a timeout (default eight seconds). On timeout it sends `SIGTERM` to the group and `SIGKILL` after at most two seconds. The receipt then has error code `timeout` and exit code `7`.
- The EXE writes PNG files to local Windows temp, never to a `\\wsl.localhost` path, because the 9p bridge can hang. The wrapper copies the file to the requested target with an atomic replace (temporary file, then `os.replace`) and computes its SHA-256.
- Staging files are removed in a `finally` block, so a failed run leaves nothing behind. Files older than one hour are removed at the start of each run.
- Windows paths are derived from the current user profile at runtime. Nothing is hardcoded.

## 7. Audit log

Every capture attempt through the wrapper appends one JSON line to `~/.local/state/desktop-snap/audit.jsonl`, under an exclusive `flock`. The file keeps the newest 1000 lines. Each line contains metadata only: timestamp, caller label, mode, target, target processes, backend, result, exit code, output path, SHA-256, flat flag and duration. Pixel data is never written.

## 8. CLI contract and exit codes

Each command prints exactly one JSON receipt on stdout. Its fields are `ok`, `exit_code`, `error_code` and `message`, plus command-specific fields such as `backend`, `flat`, `width`, `height`, `distinct_colors`, `top_color_share`, `candidates` and `duration_ms`.

| Exit | `error_code` | Meaning |
| :--- | :--- | :--- |
| `0` | `null` | Capture succeeded |
| `1` | `not_found` | No match, or unknown preset |
| `2` | `flat_image` | Flat image, no file written |
| `3` | `ambiguous_match` | More than one match |
| `4` | `window_minimized` / `no_frame` | Minimised window, or no frame within two seconds |
| `5` | `capture_denied` | WGC refused access; no fallback |
| `6` | `denied_sensitive_window` | Denylist match; no pixels written |
| `7` | `timeout` / `runtime_error` | Timeout, missing EXE, invalid arguments, internal error |
| `8` | `policy_denied` | Policy refused the request |

## 9. Verification

- **Rust:** unit tests for argument parsing, denylist matching, region containment, exit-code mapping, the audit line format, the flat-image metric and the unpremultiply step.
- **Python:** tests for staging lifecycle and cleanup, atomic copy, exit-code pass-through, timeout with hard kill, presets, denylist forwarding, the audit ring buffer and concurrent writes. They use a fake EXE, so they run without Windows.
- **Acceptance on Windows:** `doctor` must report WGC as supported. A live `preset` capture must match the on-screen window without offset. A capture behind another window must still show the target. Negative tests cover the full-monitor gate, the denylist, ambiguous titles and the absence of titles in `list`.
