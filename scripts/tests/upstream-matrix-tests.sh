#!/usr/bin/env bash
# shellcheck shell=bash
# Validate docs/architecture/upstream-substitution-matrix.toml (UPSTREAM-001).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
matrix_path="${UPSTREAM_MATRIX_PATH:-$root/docs/architecture/upstream-substitution-matrix.toml}"

die() {
    printf 'upstream-matrix-tests: ERROR: %s\n' "$*" >&2
    exit 1
}

[ -f "$matrix_path" ] || die "matrix missing: $matrix_path"

python3 - "$matrix_path" <<'PY'
from __future__ import annotations

import re
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - Python < 3.11
    import tomli as tomllib  # type: ignore

matrix_path = Path(sys.argv[1])
raw = matrix_path.read_bytes()
try:
    data = tomllib.loads(raw.decode("utf-8"))
except Exception as exc:  # noqa: BLE001 - surface parse errors to the harness
    print(f"ERROR: cannot parse TOML: {exc}", file=sys.stderr)
    raise SystemExit(1)

DISPOSITIONS = {"ADOPT", "WRAP", "UPSTREAM", "KEEP", "DELETE"}
ISO_DATE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
REQUIRED = (
    "id",
    "subsystem",
    "disposition",
    "rationale",
    "owner",
    "reviewed_on",
    "reconsider_when",
)

errors: list[str] = []

if data.get("schema_version") != 1:
    errors.append("schema_version must be 1")

rows = data.get("subsystem")
if not isinstance(rows, list) or not rows:
    errors.append("[[subsystem]] must contain at least one row")
    rows = []

ids: set[str] = set()
for index, row in enumerate(rows):
    label = f"subsystem[{index}]"
    if not isinstance(row, dict):
        errors.append(f"{label}: must be a table")
        continue
    for key in REQUIRED:
        if key not in row:
            errors.append(f"{label}: missing field {key!r}")
    row_id = row.get("id")
    if not isinstance(row_id, str) or not row_id.strip():
        errors.append(f"{label}: id must be a non-empty string")
    else:
        if row_id in ids:
            errors.append(f"duplicate id: {row_id}")
        ids.add(row_id)
        label = row_id

    disposition = row.get("disposition")
    if disposition not in DISPOSITIONS:
        errors.append(f"{label}: disposition must be one of {sorted(DISPOSITIONS)}")

    for key in ("subsystem", "rationale", "owner", "reviewed_on"):
        value = row.get(key)
        if not isinstance(value, str) or not value.strip():
            errors.append(f"{label}: {key} must be a non-empty string")

    reviewed_on = row.get("reviewed_on")
    if isinstance(reviewed_on, str) and reviewed_on.strip() and not ISO_DATE.match(reviewed_on):
        errors.append(f"{label}: reviewed_on must be ISO date YYYY-MM-DD")

    reconsider = row.get("reconsider_when")
    if not isinstance(reconsider, str):
        errors.append(f"{label}: reconsider_when must be a string (empty allowed for non-KEEP)")
    elif disposition == "KEEP" and not reconsider.strip():
        errors.append(f"{label}: KEEP requires non-empty reconsider_when")

    if disposition == "KEEP":
        for key in ("rationale", "owner", "reviewed_on"):
            value = row.get(key)
            if not isinstance(value, str) or not value.strip():
                errors.append(f"{label}: KEEP requires non-empty {key}")

if errors:
    for err in errors:
        print(f"ERROR: {err}", file=sys.stderr)
    raise SystemExit(1)

print(f"upstream-matrix-tests: PASS ({len(rows)} subsystems, {len(ids)} unique ids)")
PY
