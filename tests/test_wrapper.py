"""Tests for bin/desktop-snap (WSL wrapper). The EXE is replaced by tests/fake_desktop_snap.py."""

from __future__ import annotations

import hashlib
import importlib.machinery
import importlib.util
import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent
WRAPPER = REPO / "bin" / "desktop-snap"
FAKE_EXE = REPO / "tests" / "fake_desktop_snap.py"
LIVE_TITLE = "Herdr-Nachtwächter - Live-Status"
LIMITS_TITLE = "Herdr-Nachtwächter - Limits"


class Env:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.staging = root / "staging"
        self.config = root / "config"
        self.state = root / "state"
        self.desktop = root / "desktop"
        self.record = root / "record.jsonl"
        self.out_dir = root / "out"
        for directory in (self.staging, self.config, self.state, self.desktop, self.out_dir):
            directory.mkdir(parents=True, exist_ok=True)
        self.values = {
            "DESKTOP_SNAP_EXE": str(FAKE_EXE),
            "DESKTOP_SNAP_STAGING_DIR": str(self.staging),
            "DESKTOP_SNAP_STAGING_WIN_DIR": r"C:\fake\staging",
            "DESKTOP_SNAP_CONFIG_DIR": str(self.config),
            "DESKTOP_SNAP_STATE_DIR": str(self.state),
            "DESKTOP_SNAP_DESKTOP_TEMP": str(self.desktop),
            # Drive paths (C:\...) map into the temp root, never into the real /mnt/c.
            "DESKTOP_SNAP_MNT_ROOT": str(self.root / "mnt"),
            "FAKE_STAGING_LINUX_DIR": str(self.staging),
            "FAKE_RECORD": str(self.record),
            "FAKE_WRITE_PNG": "1",
        }

    def env(self, extra: dict[str, str] | None = None) -> dict[str, str]:
        base = {k: v for k, v in os.environ.items() if not k.startswith(("FAKE_", "DESKTOP_SNAP_"))}
        base.pop("HERDR_PANE_ID", None)
        base.pop("TMUX_PANE", None)
        base.update(self.values)
        base.update(extra or {})
        return base

    def recorded(self) -> list[dict]:
        if not self.record.exists():
            return []
        return [json.loads(line) for line in self.record.read_text(encoding="utf-8").splitlines() if line]

    def audit_lines(self) -> list[dict]:
        path = self.state / "audit.jsonl"
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line]


@pytest.fixture
def env(tmp_path: Path) -> Env:
    return Env(tmp_path)


def run(env: Env, *args: str, extra_env: dict[str, str] | None = None, timeout: float = 60):
    return subprocess.run(
        [str(WRAPPER), *args],
        env=env.env(extra_env),
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def receipt_of(result: subprocess.CompletedProcess) -> dict:
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    assert lines, f"no JSON on stdout; stderr={result.stderr!r}"
    return json.loads(lines[-1])


def staging_files(env: Env) -> list[str]:
    return sorted(p.name for p in env.staging.iterdir())


def load_wrapper_module():
    loader = importlib.machinery.SourceFileLoader("desktop_snap_wrapper", str(WRAPPER))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    # dataclasses resolves string annotations through sys.modules, so register before executing.
    sys.modules[loader.name] = module
    loader.exec_module(module)
    return module


# ---------------------------------------------------------------- --version & doctor


def test_version_reports_wrapper_and_exe_build_hash(env: Env):
    result = run(env, "--version")
    receipt = receipt_of(result)
    assert result.returncode == 0
    assert receipt["ok"] is True
    assert receipt["version"] == "0.1.0"
    assert receipt["exe_build_hash"] == "abc123"
    expected = hashlib.sha256(WRAPPER.read_bytes()).hexdigest()[:16]
    assert receipt["wrapper_hash"] == expected


def test_doctor_is_green_with_a_wgc_capable_exe(env: Env):
    result = run(env, "doctor")
    receipt = receipt_of(result)
    assert result.returncode == 0
    assert receipt["ok"] is True
    assert all(receipt["checks"].values()), receipt["checks"]
    assert receipt["exe_build_hash"] == "abc123"


def test_doctor_fails_when_the_exe_is_missing(env: Env):
    result = run(env, "doctor", extra_env={"DESKTOP_SNAP_EXE": str(env.root / "missing.exe")})
    receipt = receipt_of(result)
    assert result.returncode == 7
    assert receipt["ok"] is False
    assert receipt["checks"]["exe_present"] is False


# ---------------------------------------------------------------- Capture & Staging


def test_window_success_copies_atomically_with_sha256_and_empties_staging(env: Env):
    out = env.out_dir / "live.png"
    result = run(env, "window", "--title", LIVE_TITLE, "--out", str(out))
    receipt = receipt_of(result)
    assert result.returncode == 0
    assert receipt["ok"] is True
    assert out.read_bytes() == b"PNGDATA"
    assert receipt["sha256"] == hashlib.sha256(b"PNGDATA").hexdigest()
    assert receipt["output_path"] == str(out)
    assert staging_files(env) == []
    assert not any(p.name.endswith(".tmp") for p in env.out_dir.iterdir())


def test_staging_uses_the_windows_path_and_never_a_linux_path(env: Env):
    run(env, "window", "--title", LIVE_TITLE, "--out", str(env.out_dir / "a.png"))
    argv = env.recorded()[0]["argv"]
    win_out = argv[argv.index("--out") + 1]
    assert win_out.startswith(r"C:\fake\staging" + "\\")
    assert win_out.endswith(".png")


def test_failure_exit_removes_staging_and_writes_no_output(env: Env):
    out = env.out_dir / "denied.png"
    result = run(env, "window", "--title", LIVE_TITLE, "--out", str(out), extra_env={"FAKE_EXIT": "5", "FAKE_WRITE_PNG": "1"})
    receipt = receipt_of(result)
    assert result.returncode == 5
    assert receipt["error_code"] == "capture_denied"
    assert not out.exists()
    assert staging_files(env) == []


@pytest.mark.parametrize(
    ("exit_code", "error_code"),
    [
        (1, "not_found"),
        (2, "flat_image"),
        (3, "ambiguous_match"),
        (4, "window_minimized"),
        (5, "capture_denied"),
        (6, "denied_sensitive_window"),
        (8, "policy_denied"),
    ],
)
def test_exit_codes_pass_through_with_the_exe_receipt(env: Env, exit_code: int, error_code: str):
    out = env.out_dir / f"exit{exit_code}.png"
    extra = {"FAKE_EXIT": str(exit_code), "FAKE_WRITE_PNG": "0", "FAKE_FLAT": "1" if exit_code == 2 else "0"}
    result = run(env, "window", "--title", LIVE_TITLE, "--out", str(out), extra_env=extra)
    receipt = receipt_of(result)
    assert result.returncode == exit_code
    assert receipt["exit_code"] == exit_code
    assert receipt["error_code"] == error_code
    assert receipt["ok"] is False
    assert not out.exists(), "no --out file may be written on a failing exit"
    if exit_code == 2:
        assert receipt["flat"] is True


def test_exit_zero_without_staging_png_is_a_runtime_error(env: Env):
    out = env.out_dir / "missing.png"
    result = run(env, "window", "--title", LIVE_TITLE, "--out", str(out), extra_env={"FAKE_WRITE_PNG": "0"})
    receipt = receipt_of(result)
    assert result.returncode == 7
    assert receipt["error_code"] == "runtime_error"
    assert not out.exists()


def test_missing_json_maps_the_exit_code_to_its_error_code(env: Env):
    result = run(env, "window", "--title", LIVE_TITLE, "--out", str(env.out_dir / "x.png"), extra_env={"FAKE_EXIT": "5", "FAKE_NO_JSON": "1"})
    receipt = receipt_of(result)
    assert result.returncode == 5
    assert receipt["error_code"] == "capture_denied"


def test_copy_desktop_temp_writes_an_identical_second_copy(env: Env):
    out = env.out_dir / "limits.png"
    result = run(env, "window", "--title", LIMITS_TITLE, "--out", str(out), "--copy-desktop-temp")
    receipt = receipt_of(result)
    assert result.returncode == 0
    copy = env.desktop / "limits.png"
    assert copy.read_bytes() == b"PNGDATA"
    assert receipt["desktop_temp_path"] == str(copy)


def test_copy_desktop_temp_uses_the_plain_file_name_of_a_windows_out_path(env: Env):
    windows_out = "C:\\Users\\Alice\\foo.png"
    result = run(env, "window", "--title", LIVE_TITLE, "--out", windows_out, "--copy-desktop-temp")
    receipt = receipt_of(result)
    assert result.returncode == 0
    # The real target is the drive path mapped into the temp mount root.
    assert (env.root / "mnt" / "c" / "Users" / "Alice" / "foo.png").read_bytes() == b"PNGDATA"
    assert (env.desktop / "foo.png").read_bytes() == b"PNGDATA"
    assert sorted(p.name for p in env.desktop.iterdir()) == ["foo.png"]
    assert receipt["desktop_temp_path"] == str(env.desktop / "foo.png")


def test_parse_win_profile_accepts_the_two_cmd_lines_and_rejects_noise():
    wrapper = load_wrapper_module()
    profile = wrapper.parse_win_profile("C:\\Users\\Alice\r\nC:\\Users\\Alice\\AppData\\Local\r\n")
    assert profile == {"userprofile": "C:\\Users\\Alice", "localappdata": "C:\\Users\\Alice\\AppData\\Local"}
    with pytest.raises(wrapper.WrapperError):
        wrapper.parse_win_profile("%USERPROFILE%\r\n%LOCALAPPDATA%\r\n")
    with pytest.raises(wrapper.WrapperError):
        wrapper.parse_win_profile("")


def test_default_windows_paths_follow_the_current_user_profile(tmp_path: Path, monkeypatch):
    wrapper = load_wrapper_module()
    for name in ("DESKTOP_SNAP_EXE", "DESKTOP_SNAP_STAGING_DIR", "DESKTOP_SNAP_STAGING_WIN_DIR", "DESKTOP_SNAP_DESKTOP_TEMP"):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv("DESKTOP_SNAP_MNT_ROOT", str(tmp_path / "mnt"))
    monkeypatch.setenv("DESKTOP_SNAP_WIN_USER", "C:\\Users\\Alice")
    monkeypatch.setattr(wrapper, "_WIN_PROFILE", None)
    alice = tmp_path / "mnt" / "c" / "Users" / "Alice"
    assert wrapper.exe_path() == alice / "Apps" / "DesktopSnap" / "desktop-snap.exe"
    assert wrapper.staging_win_dir() == "C:\\Users\\Alice\\AppData\\Local\\Temp\\desktop-snap"
    assert wrapper.staging_dir() == alice / "AppData" / "Local" / "Temp" / "desktop-snap"
    assert wrapper.desktop_temp_dir() == alice / "Desktop" / "temp"


def test_caller_pane_prefers_the_explicit_caller_variable(env: Env):
    run(env, "window", "--title", LIVE_TITLE, "--out", str(env.out_dir / "c.png"),
        extra_env={"DESKTOP_SNAP_CALLER": "caller-7", "HERDR_PANE_ID": "pane-1"})
    run(env, "window", "--title", LIVE_TITLE, "--out", str(env.out_dir / "c2.png"),
        extra_env={"TMUX_PANE": "%3"})
    callers = [entry["caller_pane"] for entry in env.audit_lines()]
    assert callers == ["caller-7", "%3"]


def test_stale_staging_files_are_removed_and_fresh_ones_kept(env: Env):
    old = env.staging / "old.png"
    fresh = env.staging / "fresh.json"
    old.write_bytes(b"old")
    fresh.write_text("{}", encoding="utf-8")
    two_hours_ago = time.time() - 7200
    os.utime(old, (two_hours_ago, two_hours_ago))
    run(env, "window", "--title", LIVE_TITLE, "--out", str(env.out_dir / "a.png"))
    assert not old.exists()
    assert fresh.exists()


# ---------------------------------------------------------------- Timeout & Kill


def test_timeout_returns_exit_7_and_kills_the_hung_exe(env: Env):
    started = time.monotonic()
    result = run(
        env, "window", "--title", LIVE_TITLE, "--out", str(env.out_dir / "t.png"), "--timeout", "1",
        extra_env={"FAKE_SLEEP": "30", "FAKE_IGNORE_TERM": "1"},
    )
    elapsed = time.monotonic() - started
    receipt = receipt_of(result)
    assert result.returncode == 7
    assert receipt["error_code"] == "timeout"
    assert elapsed < 8, f"hard kill took too long: {elapsed:.1f}s"


def _process_alive(pid: int) -> bool:
    stat = Path(f"/proc/{pid}/stat")
    if not stat.exists():
        return False
    return stat.read_text(encoding="utf-8").split(")")[-1].split()[0] != "Z"


def test_timeout_kills_the_whole_process_group(env: Env):
    pidfile = env.root / "child.pid"
    run(
        env, "window", "--title", LIVE_TITLE, "--out", str(env.out_dir / "g.png"), "--timeout", "1",
        extra_env={"FAKE_SLEEP": "30", "FAKE_CHILD_PIDFILE": str(pidfile)},
    )
    child_pid = int(pidfile.read_text(encoding="utf-8"))
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline and _process_alive(child_pid):
        time.sleep(0.05)
    assert not _process_alive(child_pid), "grandchild survived the process-group kill"


def test_exe_gets_no_stdin_from_the_caller(env: Env):
    run(env, "window", "--title", LIVE_TITLE, "--out", str(env.out_dir / "s.png"))
    assert env.recorded()[0]["stdin"] == ""


# ---------------------------------------------------------------- Presets & Denylist


def test_preset_nightwatcher_builds_one_group_with_both_matchers(env: Env):
    result = run(env, "preset", "nightwatcher", "--out", str(env.out_dir / "nw.png"))
    receipt = receipt_of(result)
    assert result.returncode == 0
    assert receipt["ok"] is True
    argv = env.recorded()[0]["argv"]
    assert argv[0] == "group"
    matches = [argv[i + 1] for i, arg in enumerate(argv) if arg == "--match"]
    assert matches == [
        f"process=Herdr-Nachtwaechter.exe;title={LIVE_TITLE}",
        f"process=Herdr-Nachtwaechter.exe;title={LIMITS_TITLE}",
    ]


def test_preset_herdr_selects_active_window_and_skips_minimized_without_titles(env: Env):
    windows = [
        {"hwnd": 2034364, "process": "windowsterminal", "rect": [354, 201, 2564, 1829], "visible": True, "minimized": False, "cloaked": False},
        {"hwnd": 131596654, "process": "windowsterminal", "rect": [-31991, -32000, -31772, -31961], "visible": True, "minimized": True, "cloaked": False},
        {"hwnd": 5575342, "process": "firefox", "rect": [100, 100, 1900, 1000], "visible": True, "minimized": False, "cloaked": False},
    ]
    out = env.out_dir / "herdr.png"
    result = run(env, "preset", "herdr", "--out", str(out), extra_env={"FAKE_LIST_WINDOWS": json.dumps(windows)})
    receipt = receipt_of(result)
    assert result.returncode == 0
    assert receipt["ok"] is True
    assert out.read_bytes() == b"PNGDATA"
    recorded = env.recorded()
    assert len(recorded) == 2
    assert recorded[0]["argv"] == ["list"]  # keine --titles nötig bei reinem Prozess-Filter
    assert recorded[1]["argv"][:5] == ["window", "--hwnd", "2034364", "--process", "WindowsTerminal.exe"]


def test_preset_herdr_fails_closed_when_only_minimized_window_exists(env: Env):
    windows = [
        {"hwnd": 131596654, "process": "windowsterminal", "rect": [-31991, -32000, -31772, -31961], "visible": True, "minimized": True, "cloaked": False},
    ]
    out = env.out_dir / "herdr-min.png"
    result = run(env, "preset", "herdr", "--out", str(out), extra_env={"FAKE_LIST_WINDOWS": json.dumps(windows)})
    receipt = receipt_of(result)
    assert result.returncode == 4
    assert receipt["error_code"] == "window_minimized"
    assert not out.exists()
    assert len(env.recorded()) == 1  # nur list, kein Capture-/Restore-Aufruf
    audit = env.audit_lines()
    assert len(audit) == 1
    assert audit[0]["target"] == "herdr"
    assert audit[0]["target_processes"] == ["WindowsTerminal.exe"]


def test_preset_herdr_fails_closed_when_multiple_active_windows_exist(env: Env):
    windows = [
        {"hwnd": 1001, "process": "windowsterminal", "rect": [0, 0, 1200, 800], "visible": True, "minimized": False, "cloaked": False},
        {"hwnd": 1002, "process": "windowsterminal", "rect": [100, 100, 1300, 900], "visible": True, "minimized": False, "cloaked": False},
    ]
    out = env.out_dir / "herdr-amb.png"
    result = run(env, "preset", "herdr", "--out", str(out), extra_env={"FAKE_LIST_WINDOWS": json.dumps(windows)})
    receipt = receipt_of(result)
    assert result.returncode == 3
    assert receipt["error_code"] == "ambiguous_match"
    assert [c["hwnd"] for c in receipt["candidates"]] == [1001, 1002]
    assert not out.exists()


def test_preset_herdr_fails_closed_not_found_when_no_terminal_visible(env: Env):
    windows = [
        {"hwnd": 5575342, "process": "firefox", "rect": [100, 100, 1900, 1000], "visible": True, "minimized": False, "cloaked": False},
    ]
    out = env.out_dir / "herdr-none.png"
    result = run(env, "preset", "herdr", "--out", str(out), extra_env={"FAKE_LIST_WINDOWS": json.dumps(windows)})
    receipt = receipt_of(result)
    assert result.returncode == 1
    assert receipt["error_code"] == "not_found"
    assert not out.exists()


def test_unknown_preset_is_exit_1_and_never_starts_the_exe(env: Env):
    result = run(env, "preset", "doesnotexist", "--out", str(env.out_dir / "x.png"))
    receipt = receipt_of(result)
    assert result.returncode == 1
    assert receipt["error_code"] == "preset_not_found"
    assert env.recorded() == []


def test_user_presets_win_over_the_repo_default(env: Env):
    (env.config / "presets.json").write_text(
        json.dumps({"presets": {"nightwatcher": [{"title": "Eigenes Fenster"}]}}), encoding="utf-8"
    )
    run(env, "preset", "nightwatcher", "--out", str(env.out_dir / "u.png"))
    argv = env.recorded()[0]["argv"]
    assert argv[argv.index("--match") + 1] == "Eigenes Fenster"


def test_extra_denylist_is_staged_forwarded_and_removed(env: Env):
    payload = {"processes": ["secret-tool.exe"], "titles": ["Confidential"]}
    (env.config / "denylist.json").write_text(json.dumps(payload), encoding="utf-8")
    result = run(env, "window", "--title", LIVE_TITLE, "--out", str(env.out_dir / "d.png"))
    assert result.returncode == 0
    argv = env.recorded()[0]["argv"]
    assert "--extra-denylist" in argv
    assert argv[argv.index("--extra-denylist") + 1].endswith(".json")
    assert json.loads(env.recorded()[0]["extra_denylist_content"]) == payload
    assert staging_files(env) == []


def test_invalid_denylist_json_is_refused_before_the_exe_runs(env: Env):
    (env.config / "denylist.json").write_text("{nicht json", encoding="utf-8")
    result = run(env, "window", "--title", LIVE_TITLE, "--out", str(env.out_dir / "bad.png"))
    assert result.returncode == 7
    assert env.recorded() == []


# ---------------------------------------------------------------- CLI validation


@pytest.mark.parametrize(
    "args",
    [
        ("window", "--title", LIVE_TITLE),
        ("window", "--out", "x.png", "--title", LIVE_TITLE, "--flatten-bg", "red"),
        ("region", "--x", "0", "--y", "0", "--w", "0", "--h", "1", "--out", "x.png", "--process", "a.exe"),
        ("region", "--x", "0", "--y", "0", "--w", "1", "--h", "1"),
    ],
)
def test_invalid_capture_arguments_are_exit_7_without_exe_call(env: Env, args: tuple[str, ...]):
    result = run(env, *args)
    receipt = receipt_of(result)
    assert result.returncode == 7
    assert receipt["ok"] is False
    assert env.recorded() == []


def test_argparse_errors_are_json_with_exit_7(env: Env):
    result = run(env, "region", "--x", "a")
    receipt = receipt_of(result)
    assert result.returncode == 7
    assert receipt["error_code"] == "runtime_error"


def test_region_forwards_the_full_monitor_gate_to_the_exe(env: Env):
    run(env, "region", "--x", "1", "--y", "2", "--w", "3", "--h", "4", "--out", str(env.out_dir / "r.png"),
        "--process", "Herdr-Nachtwaechter.exe", "--allow-full-monitor")
    argv = env.recorded()[0]["argv"]
    assert argv[0] == "region"
    assert "--allow-full-monitor" in argv


def test_list_passes_the_exe_receipt_through(env: Env):
    result = run(env, "list", "--titles")
    assert result.returncode == 0
    assert receipt_of(result)["ok"] is True


def test_list_titles_forwards_the_extra_denylist_and_removes_the_staged_copy(env: Env):
    payload = {"processes": ["secret-tool.exe"], "titles": ["Confidential"]}
    (env.config / "denylist.json").write_text(json.dumps(payload), encoding="utf-8")
    result = run(env, "list", "--titles")
    assert result.returncode == 0
    entry = env.recorded()[0]
    assert entry["argv"][:2] == ["list", "--titles"]
    assert "--extra-denylist" in entry["argv"]
    assert entry["argv"][entry["argv"].index("--extra-denylist") + 1].startswith(r"C:\fake\staging" + "\\")
    assert json.loads(entry["extra_denylist_content"]) == payload
    assert staging_files(env) == []


def test_list_without_denylist_adds_no_extra_argument(env: Env):
    result = run(env, "list")
    assert result.returncode == 0
    assert "--extra-denylist" not in env.recorded()[0]["argv"]
    assert staging_files(env) == []


# ---------------------------------------------------------------- Audit log


def test_audit_entry_has_metadata_and_no_pixel_data(env: Env):
    run(env, "window", "--title", LIVE_TITLE, "--out", str(env.out_dir / "au.png"), extra_env={"HERDR_PANE_ID": "pane-1"})
    entries = env.audit_lines()
    assert len(entries) == 1
    entry = entries[0]
    assert entry["mode"] == "window"
    assert entry["ok"] is True
    assert entry["exit_code"] == 0
    assert entry["caller_pane"] == "pane-1"
    assert entry["sha256"] == hashlib.sha256(b"PNGDATA").hexdigest()
    assert entry["backend"] == "wgc-window"
    assert "PNGDATA" not in json.dumps(entry)
    for key in ("ts", "target", "target_processes", "output_path", "flat", "duration_ms"):
        assert key in entry


def test_audit_records_rejections_too(env: Env):
    run(env, "preset", "doesnotexist", "--out", str(env.out_dir / "no.png"))
    entries = env.audit_lines()
    assert len(entries) == 1
    assert entries[0]["ok"] is False
    assert entries[0]["exit_code"] == 1


def test_audit_ring_buffer_keeps_the_newest_1000_lines(tmp_path: Path):
    wrapper = load_wrapper_module()
    path = tmp_path / "audit.jsonl"
    for index in range(1005):
        wrapper.append_audit(path, {"index": index})
    lines = path.read_text(encoding="utf-8").splitlines()
    assert len(lines) == 1000
    assert json.loads(lines[0])["index"] == 5
    assert json.loads(lines[-1])["index"] == 1004


def test_audit_parallel_writes_under_flock_never_interleave(tmp_path: Path):
    wrapper = load_wrapper_module()
    path = tmp_path / "audit.jsonl"

    def writer(worker: int) -> None:
        for index in range(100):
            wrapper.append_audit(path, {"worker": worker, "index": index, "pad": "x" * 200})

    threads = [threading.Thread(target=writer, args=(worker,)) for worker in range(12)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    lines = path.read_text(encoding="utf-8").splitlines()
    assert len(lines) == 1000
    for line in lines:
        record = json.loads(line)
        assert set(record) == {"worker", "index", "pad"}
