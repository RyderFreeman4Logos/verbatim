#!/usr/bin/env python3
"""Run the configured tokenizer with the legacy bounded-process contract."""

from __future__ import annotations

import ctypes
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def _group_alive(pgid: int) -> bool:
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _reap_children() -> None:
    while True:
        try:
            pid, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return
        if pid <= 0:
            return


def main(argv: list[str]) -> int:
    timeout = int(argv[1])
    stdout_path = Path(argv[2])
    stderr_path = Path(argv[3])
    command = argv[4:]
    try:
        ctypes.CDLL(None).prctl(36, 1, 0, 0, 0)  # PR_SET_CHILD_SUBREAPER
    except Exception:
        pass

    with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
        try:
            process = subprocess.Popen(
                command,
                stdin=subprocess.DEVNULL,
                stdout=stdout,
                stderr=stderr,
                start_new_session=True,
            )
        except OSError as exc:
            stderr.write(f"cannot execute tokenizer: {exc}\n".encode())
            return 126
        try:
            status = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=0.25)
            except subprocess.TimeoutExpired:
                pass
            if _group_alive(process.pid):
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            try:
                process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            deadline = time.monotonic() + 1
            while True:
                _reap_children()
                if not _group_alive(process.pid) or time.monotonic() >= deadline:
                    break
                time.sleep(0.01)
            return 124
    if status < 0:
        return min(255, 128 - status)
    return min(255, status)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
