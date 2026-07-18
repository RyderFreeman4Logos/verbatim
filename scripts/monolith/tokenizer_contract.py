#!/usr/bin/env python3
"""Strict tokenizer wire-contract and monolith policy validation."""

from __future__ import annotations

import argparse
import json
from pathlib import Path, PurePosixPath
import sys
import tomllib
from typing import NoReturn, Sequence

MAX_SIGNED_64 = 9_223_372_036_854_775_807
POLICY_MAX_BYTES = 1_048_576
ESTIMATE_KEYS = frozenset(
    {"model", "tokens", "input_cost", "output_cost", "breakdown"}
)


class ContractError(ValueError):
    """A bounded, user-safe tokenizer protocol violation."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


def _contract_error(code: str, message: str) -> NoReturn:
    raise ContractError(code, message)


def _read_bounded(path: Path, maximum_bytes: int, stream: str) -> bytes:
    try:
        size = path.stat().st_size
    except OSError as error:
        _contract_error(f"{stream}-read", f"cannot stat {stream}: {error}")
    if size > maximum_bytes:
        _contract_error(f"{stream}-oversize", f"{stream} exceeds its byte cap")
    try:
        return path.read_bytes()
    except OSError as error:
        _contract_error(f"{stream}-read", f"cannot read {stream}: {error}")


def _reject_constant(value: str) -> NoReturn:
    _contract_error("json-constant", f"invalid JSON constant {value!r}")


def _reject_float(value: str) -> NoReturn:
    _contract_error("json-float", f"JSON float is not allowed: {value!r}")


def _bounded_integer(value: str) -> int:
    digits = value.removeprefix("-")
    if len(digits) > 19:
        _contract_error("json-integer-domain", "JSON integer exceeds signed 64-bit")
    parsed = int(value)
    if not -MAX_SIGNED_64 - 1 <= parsed <= MAX_SIGNED_64:
        _contract_error("json-integer-domain", "JSON integer exceeds signed 64-bit")
    return parsed


def _reject_duplicate_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            _contract_error("json-duplicate-key", "JSON object contains a duplicate key")
        result[key] = value
    return result


def _strict_json_document(raw: bytes) -> object:
    if b"\0" in raw:
        _contract_error("json-nul", "JSON output contains NUL")
    try:
        text = raw.decode("utf-8", errors="strict")
    except UnicodeDecodeError:
        _contract_error("json-utf8", "JSON output is not strict UTF-8")
    if not text.endswith("\n") or text.endswith("\n\n"):
        _contract_error("json-framing", "JSON output must end with exactly one LF")
    document = text[:-1]
    if not document.startswith("{") or not document.endswith("}"):
        _contract_error("json-framing", "JSON output must contain exactly one object")
    if document != document.rstrip():
        _contract_error("json-framing", "JSON output has trailing whitespace before LF")
    try:
        return json.loads(
            document,
            parse_constant=_reject_constant,
            parse_float=_reject_float,
            parse_int=_bounded_integer,
            object_pairs_hook=_reject_duplicate_keys,
        )
    except ContractError:
        raise
    except (UnicodeError, ValueError, json.JSONDecodeError):
        _contract_error("json-syntax", "JSON output is malformed or has trailing data")


def validate_version_output(
    stdout_path: Path,
    stderr_path: Path,
    expected_version: str,
    maximum_bytes: int,
) -> None:
    stdout = _read_bounded(stdout_path, maximum_bytes, "stdout")
    stderr = _read_bounded(stderr_path, maximum_bytes, "stderr")
    if stderr:
        _contract_error("version-stderr", "successful version check wrote stderr")
    expected = f"tokuin {expected_version}\n".encode("ascii")
    if stdout != expected:
        _contract_error("version-framing", "version stdout is not byte-exact")


def validate_estimate_output(
    stdout_path: Path,
    stderr_path: Path,
    input_path: Path,
    expected_model: str,
    maximum_count: int,
    maximum_bytes: int,
    expected_tokens: int | None = None,
) -> int:
    stdout = _read_bounded(stdout_path, maximum_bytes, "stdout")
    stderr = _read_bounded(stderr_path, maximum_bytes, "stderr")
    if stderr:
        _contract_error("estimate-stderr", "successful estimate wrote stderr")
    value = _strict_json_document(stdout)
    if not isinstance(value, dict) or set(value) != ESTIMATE_KEYS:
        _contract_error("estimate-schema", "estimate JSON has an incompatible schema")
    if value["model"] != expected_model:
        _contract_error("estimate-model", "estimate JSON model does not match the request")
    for nullable in ("input_cost", "output_cost", "breakdown"):
        if value[nullable] is not None:
            _contract_error(
                "estimate-schema", f"estimate JSON {nullable} must be null"
            )
    tokens = value["tokens"]
    if (
        not isinstance(tokens, int)
        or isinstance(tokens, bool)
        or not 0 <= tokens <= min(maximum_count, MAX_SIGNED_64)
    ):
        _contract_error(
            "estimate-token-domain", "estimate token count is outside signed 64-bit"
        )
    try:
        input_bytes = input_path.stat().st_size
    except OSError as error:
        _contract_error("estimate-input", f"cannot stat tokenizer input: {error}")
    if input_bytes == 0 and tokens != 0:
        _contract_error("estimate-token-domain", "empty input must have zero tokens")
    if input_bytes > 0 and (tokens == 0 or tokens > input_bytes):
        _contract_error(
            "estimate-token-domain", "token count is impossible for the input byte size"
        )
    if expected_tokens is not None and tokens != expected_tokens:
        _contract_error(
            "known-answer-mismatch", "tokenizer failed the pinned known-answer count"
        )
    return tokens


def decode_runner_receipt(
    path: Path, schema_version: int, outcomes: set[str]
) -> tuple[str, int | None, str | None, int | None, str | None]:
    keys = {
        "schema_version", "outcome", "status", "interrupted_signal",
        "output_limit_stream", "stdout_bytes", "stdout_captured_bytes",
        "stdout_truncated", "stderr_bytes", "stderr_captured_bytes",
        "stderr_truncated", "value", "protocol_error",
    }
    if path.stat().st_size > 8192:
        raise ValueError("receipt exceeds byte cap")
    data = json.loads(path.read_text(encoding="utf-8"), parse_constant=_reject_constant)
    if not isinstance(data, dict) or set(data) != keys:
        raise ValueError("receipt keys do not match the schema")
    if data["schema_version"] != schema_version or data["outcome"] not in outcomes:
        raise ValueError("unsupported receipt schema or outcome")
    status = data["status"]
    if status is not None and (
        not isinstance(status, int) or isinstance(status, bool) or not 0 <= status <= 255
    ):
        raise ValueError("invalid receipt status")
    stream = data["output_limit_stream"]
    if stream not in (None, "stdout", "stderr"):
        raise ValueError("invalid receipt output-limit stream")
    value = data["value"]
    if value is not None and (
        not isinstance(value, int) or isinstance(value, bool) or not 0 <= value <= MAX_SIGNED_64
    ):
        raise ValueError("invalid receipt value")
    protocol_error = data["protocol_error"]
    if protocol_error is not None and (
        not isinstance(protocol_error, str)
        or not protocol_error
        or any(character in protocol_error for character in "\0\t\r\n")
        or len(protocol_error) > 128
    ):
        raise ValueError("invalid receipt protocol error")
    for prefix in ("stdout", "stderr"):
        total = data[f"{prefix}_bytes"]
        captured = data[f"{prefix}_captured_bytes"]
        truncated = data[f"{prefix}_truncated"]
        if (
            not isinstance(total, int) or isinstance(total, bool) or total < 0
            or not isinstance(captured, int) or isinstance(captured, bool)
            or not 0 <= captured <= total or not isinstance(truncated, bool)
        ):
            raise ValueError(f"invalid receipt {prefix} counters")
    return data["outcome"], status, stream, value, protocol_error


def _policy_fail(label: str, message: str) -> NoReturn:
    print(f"ERROR: {label} baseline policy: {message}", file=sys.stderr)
    raise SystemExit(2)


def parse_policy(arguments: argparse.Namespace) -> None:
    required = {
        "path",
        "kind",
        "baseline_tokens",
        "baseline_lines",
        "issue",
        "rationale",
    }
    expected_tokenizer = {
        "command": arguments.command,
        "version": arguments.version,
        "revision": arguments.revision,
        "model": arguments.model,
        "format": arguments.output_format,
        "timeout_seconds": arguments.timeout,
        "max_output_bytes": arguments.max_output_bytes,
        "known_answer_input": arguments.known_answer_input,
        "known_answer_tokens": arguments.known_answer_tokens,
    }
    try:
        if arguments.policy.stat().st_size > POLICY_MAX_BYTES:
            _policy_fail(arguments.label, "policy exceeds its tracked byte cap")
        data = tomllib.loads(arguments.policy.read_text(encoding="utf-8"))
    except SystemExit:
        raise
    except Exception as error:
        _policy_fail(arguments.label, f"cannot parse TOML: {error}")
    if not isinstance(data, dict):
        _policy_fail(arguments.label, "top level must be a table")
    unknown_top = set(data) - {"files", "tokenizer"}
    if unknown_top:
        _policy_fail(
            arguments.label,
            f"unknown top-level key(s): {', '.join(sorted(unknown_top))}",
        )
    tokenizer = data.get("tokenizer")
    if not isinstance(tokenizer, dict):
        _policy_fail(arguments.label, "tokenizer must be a table")
    unknown = set(tokenizer) - set(expected_tokenizer)
    missing = set(expected_tokenizer) - set(tokenizer)
    if unknown:
        _policy_fail(
            arguments.label,
            f"tokenizer has unknown key(s): {', '.join(sorted(unknown))}",
        )
    if missing:
        _policy_fail(
            arguments.label,
            f"tokenizer is missing key(s): {', '.join(sorted(missing))}",
        )
    for key, expected in expected_tokenizer.items():
        if tokenizer[key] != expected:
            _policy_fail(
                arguments.label,
                f"tokenizer.{key} must be {expected!r}, got {tokenizer[key]!r}",
            )
    entries = data.get("files")
    if not isinstance(entries, list):
        _policy_fail(arguments.label, "files must be an array of tables")
    allowed_kinds = {"source", "test", "doc", "config", "other"}
    seen: set[str] = set()
    for number, entry in enumerate(entries, start=1):
        if not isinstance(entry, dict):
            _policy_fail(arguments.label, f"entry #{number} must be a table")
        unknown = set(entry) - required
        missing = required - set(entry)
        if unknown or missing:
            qualifier = "unknown" if unknown else "missing"
            keys = unknown or missing
            _policy_fail(
                arguments.label,
                f"entry #{number} has {qualifier} key(s): {', '.join(sorted(keys))}",
            )
        path = entry["path"]
        kind = entry["kind"]
        tokens = entry["baseline_tokens"]
        lines = entry["baseline_lines"]
        issue = entry["issue"]
        rationale = entry["rationale"]
        if not isinstance(path, str) or not path:
            _policy_fail(arguments.label, f"entry #{number} has invalid path")
        if any(character in path for character in ("\0", "\n", "\r", "\t")):
            _policy_fail(arguments.label, f"entry #{number} path has control bytes")
        path_object = PurePosixPath(path)
        if (
            path_object.is_absolute()
            or ".." in path_object.parts
            or path.startswith("./")
            or "//" in path
        ):
            _policy_fail(arguments.label, f"non-canonical path: {path!r}")
        if path in seen:
            _policy_fail(arguments.label, f"duplicate path: {path}")
        seen.add(path)
        if kind not in allowed_kinds:
            _policy_fail(arguments.label, f"entry for {path} has invalid kind")
        for field, value in (("baseline_tokens", tokens), ("baseline_lines", lines)):
            if (
                not isinstance(value, int)
                or isinstance(value, bool)
                or not 0 <= value <= arguments.maximum_count
            ):
                _policy_fail(arguments.label, f"entry for {path} has invalid {field}")
        if issue != "368":
            _policy_fail(arguments.label, f"entry for {path} must use issue = \"368\"")
        if not isinstance(rationale, str) or not rationale.strip():
            _policy_fail(arguments.label, f"entry for {path} has missing rationale")
        if any(character in rationale for character in ("\t", "\n", "\r")):
            _policy_fail(arguments.label, f"entry for {path} rationale has control whitespace")
        print(f"{path}\t{kind}\t{tokens}\t{lines}\t{issue}\t{rationale}")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="action", required=True)
    policy = subparsers.add_parser("policy")
    policy.add_argument("--label", required=True)
    policy.add_argument("--policy", type=Path, required=True)
    policy.add_argument("--command", required=True)
    policy.add_argument("--version", required=True)
    policy.add_argument("--revision", required=True)
    policy.add_argument("--model", required=True)
    policy.add_argument("--output-format", required=True)
    policy.add_argument("--timeout", type=int, required=True)
    policy.add_argument("--max-output-bytes", type=int, required=True)
    policy.add_argument("--known-answer-input", required=True)
    policy.add_argument("--known-answer-tokens", type=int, required=True)
    policy.add_argument("--maximum-count", type=int, required=True)
    return parser


def main(argv: Sequence[str]) -> int:
    arguments = build_parser().parse_args(argv[1:])
    if arguments.action == "policy":
        parse_policy(arguments)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
