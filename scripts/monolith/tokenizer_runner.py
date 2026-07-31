#!/usr/bin/env python3
"""Bounded, receipt-driven process runner for the monolith tokenizer."""

from __future__ import annotations

import argparse
import ctypes
from dataclasses import asdict, dataclass
from enum import Enum
import json
import os
from pathlib import Path
import selectors
import shutil
import signal
import sys
import tempfile
import time
from typing import Any, BinaryIO, NoReturn, Sequence

from tokenizer_contract import (
    ContractError,
    decode_runner_receipt,
    validate_estimate_output,
    validate_version_output,
)

PR_SET_CHILD_SUBREAPER = 36
READ_CHUNK_BYTES = 65_536
MAX_ENVIRONMENT_BYTES = 1_048_576
RECEIPT_SCHEMA_VERSION = 2
_INTERRUPTED_SIGNAL: int | None = None
_LIBC = ctypes.CDLL(None, use_errno=True)

class Outcome(str, Enum):
    EXITED = "exited"
    TIMED_OUT = "timed_out"
    OUTPUT_LIMIT = "output_limit"
    INTERRUPTED = "interrupted"
    ORPHANED_DESCENDANTS = "orphaned_descendants"
    SPAWN_FAILED = "spawn_failed"
    PROTOCOL_FAILED = "protocol_failed"
    CLEANUP_FAILED = "cleanup_failed"

@dataclass(frozen=True)
class ProcessIdentity:
    pid: int
    start_time: int

@dataclass
class OwnedChild:
    identity: ProcessIdentity
    pidfd: int
    exec_error_fd: int
    observed_status: int | None = None
    reaped: bool = False

@dataclass
class StreamCapture:
    name: str
    path: Path
    limit: int
    handle: BinaryIO
    total_bytes: int = 0
    captured_bytes: int = 0
    truncated: bool = False

    def consume(self, payload: bytes) -> bool:
        self.total_bytes += len(payload)
        remaining = self.limit - self.captured_bytes
        if remaining > 0:
            captured = payload[:remaining]
            self.handle.write(captured)
            self.captured_bytes += len(captured)
        if len(payload) > remaining:
            self.truncated = True
        return self.truncated

@dataclass(frozen=True)
class Receipt:
    schema_version: int
    outcome: str
    status: int | None
    interrupted_signal: int | None
    output_limit_stream: str | None
    stdout_bytes: int
    stdout_captured_bytes: int
    stdout_truncated: bool
    stderr_bytes: int
    stderr_captured_bytes: int
    stderr_truncated: bool
    value: int | None
    protocol_error: str | None

def fail(message: str) -> NoReturn:
    print(f"tokenizer runner: {message}", file=sys.stderr)
    raise SystemExit(2)

def enable_subreaper() -> None:
    if not sys.platform.startswith("linux") or not Path("/proc/self/stat").exists():
        raise RuntimeError("Linux /proc is required for process-tree ownership")
    if _LIBC.prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0:
        error_number = ctypes.get_errno()
        raise OSError(error_number, os.strerror(error_number))

def pidfd_open(pid: int) -> int:
    descriptor = _LIBC.pidfd_open(pid, 0)
    if descriptor < 0:
        error_number = ctypes.get_errno()
        raise OSError(error_number, os.strerror(error_number))
    return descriptor

def pidfd_signal(descriptor: int, signum: int) -> None:
    if _LIBC.pidfd_send_signal(descriptor, signum, None, 0) != 0:
        error_number = ctypes.get_errno()
        if error_number != 3:
            raise OSError(error_number, os.strerror(error_number))

def signal_handler(signum: int, _frame: object) -> None:
    global _INTERRUPTED_SIGNAL
    if _INTERRUPTED_SIGNAL is None:
        _INTERRUPTED_SIGNAL = signum

def install_signal_handlers() -> None:
    for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, signal_handler)

def read_process_table() -> dict[int, tuple[int, int]]:
    table: dict[int, tuple[int, int]] = {}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            text = (entry / "stat").read_text(encoding="ascii")
            close_parenthesis = text.rfind(")")
            if close_parenthesis < 0:
                continue
            fields = text[close_parenthesis + 2 :].split()
            parent_pid = int(fields[1])
            start_time = int(fields[19])
            table[int(entry.name)] = (parent_pid, start_time)
        except (
            FileNotFoundError,
            PermissionError,
            ProcessLookupError,
            IndexError,
            ValueError,
        ):
            continue
    return table

def descendants_of(owner_pid: int) -> list[ProcessIdentity]:
    table = read_process_table()
    descendants: list[ProcessIdentity] = []
    frontier = [owner_pid]
    seen = {owner_pid}
    while frontier:
        parent = frontier.pop()
        for pid, (parent_pid, start_time) in table.items():
            if parent_pid != parent or pid in seen:
                continue
            seen.add(pid)
            frontier.append(pid)
            descendants.append(ProcessIdentity(pid, start_time))
    return descendants

def current_identity(pid: int) -> ProcessIdentity | None:
    row = read_process_table().get(pid)
    if row is None:
        return None
    return ProcessIdentity(pid, row[1])

def identity_is_alive(identity: ProcessIdentity) -> bool:
    return current_identity(identity.pid) == identity

def signal_identity(identity: ProcessIdentity, signum: int) -> None:
    if not identity_is_alive(identity):
        return
    try:
        descriptor = pidfd_open(identity.pid)
    except ProcessLookupError:
        return
    try:
        if identity_is_alive(identity):
            pidfd_signal(descriptor, signum)
    except ProcessLookupError:
        pass
    finally:
        os.close(descriptor)

def signal_process_group(root: ProcessIdentity, signum: int) -> None:
    if not identity_is_alive(root):
        return
    try:
        os.killpg(root.pid, signum)
    except ProcessLookupError:
        return

def normalize_wait_status(result: os.waitid_result) -> int:
    if result.si_code == os.CLD_EXITED:
        return result.si_status
    return min(255, 128 + result.si_status)

def resolve_executable(command: Sequence[str]) -> list[str]:
    executable = command[0]
    if "/" not in executable:
        resolved = shutil.which(executable)
        if resolved is None:
            raise FileNotFoundError(f"executable not found: {executable}")
        executable = resolved
    elif not os.path.isabs(executable):
        executable = os.path.abspath(executable)
    return [executable, *command[1:]]

def spawn_child(command: Sequence[str], deadline: float) -> tuple[OwnedChild, int, int]:
    if time.monotonic() >= deadline:
        raise TimeoutError("tokenizer spawn budget exhausted")
    argv = resolve_executable(command)
    environment = os.environ.copy()
    environment_bytes = sum(
        len(os.fsencode(key)) + len(os.fsencode(value)) + 2
        for key, value in environment.items()
    )
    if environment_bytes > MAX_ENVIRONMENT_BYTES:
        raise RuntimeError("tokenizer environment exceeds its byte cap")
    stdout_read, stdout_write = os.pipe2(os.O_CLOEXEC)
    stderr_read, stderr_write = os.pipe2(os.O_CLOEXEC)
    error_read, error_write = os.pipe2(os.O_CLOEXEC)
    devnull = os.open(os.devnull, os.O_RDONLY | os.O_CLOEXEC)
    descriptors = {
        stdout_read,
        stdout_write,
        stderr_read,
        stderr_write,
        error_read,
        error_write,
        devnull,
    }
    if time.monotonic() >= deadline:
        for descriptor in descriptors:
            os.close(descriptor)
        raise TimeoutError("tokenizer spawn budget exhausted")
    try:
        pid = os.fork()
    except BaseException:
        for descriptor in descriptors:
            os.close(descriptor)
        raise
    if pid == 0:
        try:
            os.setsid()
            os.dup2(devnull, 0)
            os.dup2(stdout_write, 1)
            os.dup2(stderr_write, 2)
            for descriptor in descriptors - {error_write}:
                if descriptor > 2:
                    os.close(descriptor)
            os.execve(argv[0], argv, environment)
        except BaseException:
            try:
                os.write(error_write, b"1")
            except BaseException:
                pass
            os._exit(127)
    for descriptor in (stdout_write, stderr_write, error_write, devnull):
        os.close(descriptor)
    os.set_blocking(stdout_read, False)
    os.set_blocking(stderr_read, False)
    os.set_blocking(error_read, False)
    try:
        pidfd = pidfd_open(pid)
    except BaseException:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.waitpid(pid, 0)
        for descriptor in (stdout_read, stderr_read, error_read):
            os.close(descriptor)
        raise
    identity = current_identity(pid)
    while identity is None and time.monotonic() < deadline:
        time.sleep(0.001)
        identity = current_identity(pid)
    if identity is None:
        pidfd_signal(pidfd, signal.SIGKILL)
        os.waitpid(pid, 0)
        os.close(pidfd)
        for descriptor in (stdout_read, stderr_read, error_read):
            os.close(descriptor)
        raise RuntimeError("cannot establish tokenizer process identity")
    return OwnedChild(identity, pidfd, error_read), stdout_read, stderr_read

def read_exec_failure(child: OwnedChild) -> bool | None:
    if child.exec_error_fd < 0:
        return None
    try:
        payload = os.read(child.exec_error_fd, 16)
    except BlockingIOError:
        return None
    if payload:
        os.close(child.exec_error_fd)
        child.exec_error_fd = -1
        return True
    os.close(child.exec_error_fd)
    child.exec_error_fd = -1
    return False

def observe_child(child: OwnedChild) -> int | None:
    if child.observed_status is not None:
        return child.observed_status
    try:
        result = os.waitid(
            os.P_PID,
            child.identity.pid,
            os.WEXITED | os.WNOHANG | os.WNOWAIT,
        )
    except ChildProcessError:
        return child.observed_status
    if result is not None:
        child.observed_status = normalize_wait_status(result)
    return child.observed_status

def owned_descendants(root: ProcessIdentity) -> list[ProcessIdentity]:
    return [
        identity
        for identity in descendants_of(os.getpid())
        if identity.pid != root.pid
    ]

def reap_adopted_children(root: ProcessIdentity) -> None:
    for identity in owned_descendants(root):
        try:
            os.waitpid(identity.pid, os.WNOHANG)
        except ChildProcessError:
            continue

def reap_root(child: OwnedChild) -> bool:
    if child.reaped:
        return True
    try:
        reaped_pid, _status = os.waitpid(child.identity.pid, os.WNOHANG)
    except ChildProcessError:
        child.reaped = True
        return True
    if reaped_pid == child.identity.pid:
        child.reaped = True
    return child.reaped

def drain_ready(
    selector: selectors.BaseSelector,
    captures: dict[int, StreamCapture],
    timeout: float,
) -> str | None:
    exceeded_stream: str | None = None
    for key, _events in selector.select(timeout):
        descriptor = key.fd
        try:
            payload = os.read(descriptor, READ_CHUNK_BYTES)
        except BlockingIOError:
            continue
        if not payload:
            selector.unregister(descriptor)
            os.close(descriptor)
            continue
        capture = captures[descriptor]
        if capture.consume(payload) and exceeded_stream is None:
            exceeded_stream = capture.name
    return exceeded_stream

def signal_owned_processes(child: OwnedChild, signum: int) -> None:
    signal_process_group(child.identity, signum)
    try:
        pidfd_signal(child.pidfd, signum)
    except ProcessLookupError:
        pass
    for identity in owned_descendants(child.identity):
        signal_identity(identity, signum)

def owned_processes_settled(
    child: OwnedChild,
    selector: selectors.BaseSelector,
    captures: dict[int, StreamCapture],
    deadline: float,
) -> bool:
    while time.monotonic() < deadline:
        drain_ready(selector, captures, 0.02)
        observe_child(child)
        reap_adopted_children(child.identity)
        if child.observed_status is not None and not owned_descendants(child.identity):
            drain_ready(selector, captures, 0)
            if not selector.get_map():
                return True
        time.sleep(0.005)
    drain_ready(selector, captures, 0)
    observe_child(child)
    reap_adopted_children(child.identity)
    return (
        child.observed_status is not None
        and not owned_descendants(child.identity)
        and not selector.get_map()
    )

def terminate_owned_processes(
    child: OwnedChild,
    selector: selectors.BaseSelector,
    captures: dict[int, StreamCapture],
    term_deadline: float,
    absolute_deadline: float,
) -> bool:
    signal_owned_processes(child, signal.SIGTERM)
    settled = owned_processes_settled(child, selector, captures, term_deadline)
    while not settled and time.monotonic() < absolute_deadline:
        signal_owned_processes(child, signal.SIGKILL)
        settled = owned_processes_settled(
            child,
            selector,
            captures,
            min(absolute_deadline, time.monotonic() + 0.1),
        )
    # The root stays unreaped until this final group fence, so its PGID cannot
    # be reused while killpg is still possible.
    signal_process_group(child.identity, signal.SIGKILL)
    drain_ready(selector, captures, 0)
    observe_child(child)
    reap_adopted_children(child.identity)
    if settled and child.observed_status is not None:
        settled = reap_root(child)
    try:
        os.close(child.pidfd)
    except OSError:
        pass
    if child.exec_error_fd >= 0:
        os.close(child.exec_error_fd)
        child.exec_error_fd = -1
    return settled and child.reaped and not owned_descendants(child.identity)

def close_streams(
    selector: selectors.BaseSelector,
    captures: dict[int, StreamCapture],
) -> None:
    drain_ready(selector, captures, 0)
    for key in list(selector.get_map().values()):
        descriptor = key.fd
        try:
            selector.unregister(descriptor)
        except KeyError:
            pass
        try:
            os.close(descriptor)
        except OSError:
            pass
    selector.close()
    for capture in captures.values():
        capture.handle.flush()
        capture.handle.close()

def atomic_write_receipt(path: Path, receipt: Receipt) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(
        asdict(receipt),
        allow_nan=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8") + b"\n"
    descriptor, temporary_name = tempfile.mkstemp(
        dir=path.parent,
        prefix=f".{path.name}.",
        suffix=".tmp",
    )
    temporary_path = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as receipt_file:
            receipt_file.write(payload)
            receipt_file.flush()
            os.fsync(receipt_file.fileno())
        os.replace(temporary_path, path)
    finally:
        try:
            temporary_path.unlink()
        except FileNotFoundError:
            pass

def make_receipt(
    outcome: Outcome,
    status: int | None,
    interrupted_signal: int | None,
    output_limit_stream: str | None,
    stdout: StreamCapture,
    stderr: StreamCapture,
    value: int | None = None,
    protocol_error: str | None = None,
) -> Receipt:
    return Receipt(
        schema_version=RECEIPT_SCHEMA_VERSION,
        outcome=outcome.value,
        status=status,
        interrupted_signal=interrupted_signal,
        output_limit_stream=output_limit_stream,
        stdout_bytes=stdout.total_bytes,
        stdout_captured_bytes=stdout.captured_bytes,
        stdout_truncated=stdout.truncated,
        stderr_bytes=stderr.total_bytes,
        stderr_captured_bytes=stderr.captured_bytes,
        stderr_truncated=stderr.truncated,
        value=value,
        protocol_error=protocol_error,
    )

def run_command(
    command: Sequence[str],
    timeout_seconds: int,
    max_output_bytes: int,
    stdout_path: Path,
    stderr_path: Path,
    receipt_path: Path,
    *,
    protocol: str | None = None,
    expected_version: str | None = None,
    input_path: Path | None = None,
    expected_model: str | None = None,
    maximum_count: int | None = None,
    expected_tokens: int | None = None,
) -> None:
    global _INTERRUPTED_SIGNAL
    started = time.monotonic()
    absolute_deadline = started + timeout_seconds
    cleanup_reserve = min(1.0, max(0.5, timeout_seconds * 0.5))
    work_deadline = absolute_deadline - cleanup_reserve
    term_deadline = work_deadline + cleanup_reserve / 2
    _INTERRUPTED_SIGNAL = None
    enable_subreaper()
    install_signal_handlers()
    stdout_path.parent.mkdir(parents=True, exist_ok=True)
    stderr_path.parent.mkdir(parents=True, exist_ok=True)
    stdout_capture = StreamCapture(
        "stdout", stdout_path, max_output_bytes, stdout_path.open("wb")
    )
    stderr_capture = StreamCapture(
        "stderr", stderr_path, max_output_bytes, stderr_path.open("wb")
    )
    selector = selectors.DefaultSelector()
    captures: dict[int, StreamCapture] = {}
    child: OwnedChild | None = None
    try:
        try:
            child, stdout_fd, stderr_fd = spawn_child(command, work_deadline)
        except (OSError, RuntimeError) as error:
            close_streams(selector, captures)
            outcome = (
                Outcome.TIMED_OUT
                if time.monotonic() >= work_deadline
                else Outcome.SPAWN_FAILED
            )
            atomic_write_receipt(
                receipt_path,
                make_receipt(
                    outcome,
                    None,
                    None,
                    None,
                    stdout_capture,
                    stderr_capture,
                    protocol_error=type(error).__name__,
                ),
            )
            return
        for descriptor, capture in (
            (stdout_fd, stdout_capture),
            (stderr_fd, stderr_capture),
        ):
            selector.register(descriptor, selectors.EVENT_READ)
            captures[descriptor] = capture
        outcome: Outcome | None = None
        status: int | None = None
        output_limit_stream: str | None = None
        interrupted_signal: int | None = None
        while outcome is None:
            exceeded = drain_ready(selector, captures, 0.02)
            spawn_failed = read_exec_failure(child)
            observed_status = observe_child(child)
            if exceeded is not None:
                outcome = Outcome.OUTPUT_LIMIT
                output_limit_stream = exceeded
                break
            if _INTERRUPTED_SIGNAL is not None:
                interrupted_signal = _INTERRUPTED_SIGNAL
                outcome = Outcome.INTERRUPTED
                status = 128 + interrupted_signal
                break
            if spawn_failed:
                outcome = Outcome.SPAWN_FAILED
                status = None
                break
            if observed_status is not None:
                status = observed_status
                descendants = owned_descendants(child.identity)
                if descendants:
                    outcome = Outcome.ORPHANED_DESCENDANTS
                    break
                if not selector.get_map():
                    outcome = Outcome.EXITED
                    break
            if time.monotonic() >= work_deadline:
                observed_status = observe_child(child)
                if observed_status is None:
                    outcome = Outcome.TIMED_OUT
                else:
                    status = observed_status
                    outcome = (
                        Outcome.ORPHANED_DESCENDANTS
                        if owned_descendants(child.identity)
                        else Outcome.EXITED
                    )
                break
        if outcome is Outcome.EXITED and owned_descendants(child.identity):
            outcome = Outcome.ORPHANED_DESCENDANTS
        if outcome is Outcome.INTERRUPTED:
            term_deadline, absolute_deadline = min(term_deadline, (now := time.monotonic()) + 0.25), min(absolute_deadline, now + 0.5)
        cleanup_succeeded = terminate_owned_processes(
            child,
            selector,
            captures,
            term_deadline,
            absolute_deadline,
        )
        if not cleanup_succeeded:
            keep = outcome in (
                Outcome.TIMED_OUT, Outcome.INTERRUPTED, Outcome.OUTPUT_LIMIT,
            )
            if keep:
                hard = time.monotonic() + 0.5
                while time.monotonic() < hard and owned_descendants(child.identity):
                    signal_process_group(child.identity, signal.SIGKILL)
                    for identity in owned_descendants(child.identity):
                        signal_identity(identity, signal.SIGKILL)
                    observe_child(child)
                    reap_adopted_children(child.identity)
                    if child.observed_status is not None:
                        reap_root(child)
                    time.sleep(0.01)
            if not keep or owned_descendants(child.identity):
                outcome = Outcome.CLEANUP_FAILED
                status = None
        close_streams(selector, captures)
        value: int | None = None
        protocol_error: str | None = None
        if outcome is Outcome.EXITED and status == 0 and protocol is not None:
            try:
                if protocol == "version":
                    if expected_version is None:
                        raise RuntimeError("version protocol lacks an expected version")
                    validate_version_output(
                        stdout_path,
                        stderr_path,
                        expected_version,
                        max_output_bytes,
                    )
                elif protocol == "estimate":
                    if input_path is None or expected_model is None or maximum_count is None:
                        raise RuntimeError("estimate protocol lacks required parameters")
                    value = validate_estimate_output(
                        stdout_path,
                        stderr_path,
                        input_path,
                        expected_model,
                        maximum_count,
                        max_output_bytes,
                        expected_tokens,
                    )
                else:
                    raise RuntimeError(f"unknown tokenizer protocol: {protocol}")
            except ContractError as error:
                outcome = Outcome.PROTOCOL_FAILED
                protocol_error = error.code
                value = None
        receipt = make_receipt(
            outcome,
            status,
            interrupted_signal,
            output_limit_stream,
            stdout_capture,
            stderr_capture,
            value,
            protocol_error,
        )
        atomic_write_receipt(receipt_path, receipt)
    except BaseException:
        if child is not None and not child.reaped:
            terminate_owned_processes(
                child,
                selector,
                captures,
                min(absolute_deadline, time.monotonic() + 0.05),
                absolute_deadline,
            )
        close_streams(selector, captures)
        raise

def decode_receipt(path: Path) -> None:
    outcome, status, stream, value, protocol_error = decode_runner_receipt(
        path, RECEIPT_SCHEMA_VERSION, {member.value for member in Outcome}
    )
    print(
        f"{outcome}\t{status if status is not None else '-'}\t{stream or '-'}"
        f"\t{value if value is not None else '-'}\t{protocol_error or '-'}"
    )

def add_run_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--timeout-seconds", type=int, required=True)
    parser.add_argument("--max-output-bytes", type=int, required=True)
    parser.add_argument("--stdout", type=Path, required=True)
    parser.add_argument("--stderr", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)

def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="action", required=True)
    run_parser = subparsers.add_parser("run")
    add_run_arguments(run_parser)
    run_parser.add_argument("command", nargs=argparse.REMAINDER)
    version_parser = subparsers.add_parser("version")
    add_run_arguments(version_parser)
    version_parser.add_argument("--executable", required=True)
    version_parser.add_argument("--expected-version", required=True)
    estimate_parser = subparsers.add_parser("estimate")
    add_run_arguments(estimate_parser)
    estimate_parser.add_argument("--executable", required=True)
    estimate_parser.add_argument("--model", required=True)
    estimate_parser.add_argument("--input", dest="input_path", type=Path, required=True)
    estimate_parser.add_argument("--maximum-count", type=int, required=True)
    estimate_parser.add_argument("--expected-tokens", type=int)
    decode_parser = subparsers.add_parser("decode")
    decode_parser.add_argument("--receipt", type=Path, required=True)
    return parser

def main(argv: Sequence[str]) -> int:
    parser = build_parser()
    arguments = parser.parse_args(argv[1:])
    if arguments.action == "decode":
        try:
            decode_receipt(arguments.receipt)
        except (OSError, ValueError, json.JSONDecodeError) as error:
            fail(f"invalid receipt: {error}")
        return 0
    if not 1 <= arguments.timeout_seconds <= 300:
        parser.error("--timeout-seconds must be from 1 through 300")
    if not 1 <= arguments.max_output_bytes <= 1_048_576:
        parser.error("--max-output-bytes must be from 1 through 1048576")
    keyword_arguments: dict[str, Any] = {}
    if arguments.action == "run":
        command = list(arguments.command)
        if command and command[0] == "--":
            command.pop(0)
        if not command:
            parser.error("run requires a command after --")
    elif arguments.action == "version":
        command = [arguments.executable, "--version"]
        keyword_arguments = {
            "protocol": "version",
            "expected_version": arguments.expected_version,
        }
    else:
        if not 0 <= arguments.maximum_count <= 2**63 - 1:
            parser.error("--maximum-count must be inside signed 64-bit")
        if arguments.expected_tokens is not None and not 0 <= arguments.expected_tokens <= 2**63 - 1:
            parser.error("--expected-tokens must be inside signed 64-bit")
        command = [
            arguments.executable,
            "estimate",
            "--model",
            arguments.model,
            "--format",
            "json",
            str(arguments.input_path),
        ]
        keyword_arguments = {
            "protocol": "estimate",
            "input_path": arguments.input_path,
            "expected_model": arguments.model,
            "maximum_count": arguments.maximum_count,
            "expected_tokens": arguments.expected_tokens,
        }
    try:
        run_command(
            command,
            arguments.timeout_seconds,
            arguments.max_output_bytes,
            arguments.stdout,
            arguments.stderr,
            arguments.receipt,
            **keyword_arguments,
        )
    except (OSError, RuntimeError) as error:
        fail(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
