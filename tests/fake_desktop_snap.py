#!/usr/bin/env python3
"""Fake desktop-snap.exe for the wrapper tests. Behaviour is steered by FAKE_* env vars.

It records every capture call to FAKE_RECORD (JSON lines), writes a dummy PNG to the
staging directory when FAKE_WRITE_PNG=1, and answers --version / doctor / list.
"""
import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

ERROR_CODES = {
    0: None,
    1: "not_found",
    2: "flat_image",
    3: "ambiguous_match",
    4: "window_minimized",
    5: "capture_denied",
    6: "denied_sensitive_window",
    7: "runtime_error",
    8: "policy_denied",
}


def record(entry):
    target = os.environ.get("FAKE_RECORD")
    if target:
        with open(target, "a", encoding="utf-8") as handle:
            handle.write(json.dumps(entry, ensure_ascii=False) + "\n")


def value_after(argv, flag):
    if flag not in argv:
        return None
    index = argv.index(flag)
    return argv[index + 1] if index + 1 < len(argv) else None


def staged(win_path):
    return Path(os.environ["FAKE_STAGING_LINUX_DIR"]) / win_path.split("\\")[-1]


def main():
    argv = sys.argv[1:]
    if os.environ.get("FAKE_IGNORE_TERM") == "1":
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
    if argv[:1] == ["--version"]:
        print(json.dumps({"ok": True, "version": "fake-1", "build_hash": "abc123"}))
        return 0
    if argv[:1] == ["doctor"]:
        wgc = os.environ.get("FAKE_WGC", "1") == "1"
        print(json.dumps({"ok": True, "wgc_supported": wgc, "build_hash": "abc123"}))
        return 0
    if argv[:1] == ["list"]:
        entry = {"argv": argv, "stdin": ""}
        extra = value_after(argv, "--extra-denylist")
        if extra and staged(extra).exists():
            entry["extra_denylist_content"] = staged(extra).read_text(encoding="utf-8")
        record(entry)
        raw_windows = os.environ.get("FAKE_LIST_WINDOWS")
        windows = json.loads(raw_windows) if raw_windows else []
        print(json.dumps({"ok": True, "exit_code": 0, "windows": windows}))
        return 0

    stdin_data = sys.stdin.read()
    entry = {"argv": argv, "stdin": stdin_data}
    win_out = value_after(argv, "--out")
    extra = value_after(argv, "--extra-denylist")
    if extra and staged(extra).exists():
        entry["extra_denylist_content"] = staged(extra).read_text(encoding="utf-8")
    record(entry)

    if os.environ.get("FAKE_CHILD_PIDFILE"):
        child = subprocess.Popen(["sleep", "30"])
        Path(os.environ["FAKE_CHILD_PIDFILE"]).write_text(str(child.pid), encoding="utf-8")
    sleep_s = float(os.environ.get("FAKE_SLEEP", "0"))
    if sleep_s:
        time.sleep(sleep_s)

    code = int(os.environ.get("FAKE_EXIT", "0"))
    if os.environ.get("FAKE_WRITE_PNG") == "1" and win_out:
        staged(win_out).write_bytes(b"PNGDATA")
    if os.environ.get("FAKE_NO_JSON") == "1":
        return code
    receipt = {
        "ok": code == 0,
        "exit_code": code,
        "error_code": ERROR_CODES.get(code, "runtime_error"),
        "message": "fake",
        "backend": "wgc-window",
        "flat": os.environ.get("FAKE_FLAT") == "1",
    }
    print(json.dumps(receipt))
    return code


if __name__ == "__main__":
    sys.exit(main())
