#!/usr/bin/env bash
# shellcheck shell=bash
# Validate benchmark governance schema + fixtures (EVAL-015 / #340).
# Offline only — no network.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
schema_path="${BENCH_SCHEMA_PATH:-$root/docs/evals/benchmark-manifest.schema.toml}"
valid_path="${BENCH_VALID_PATH:-$root/docs/evals/examples/minimal-benchmark.example.toml}"
leakage_path="${BENCH_LEAKAGE_PATH:-$root/docs/evals/examples/group-leakage.fixture.toml}"

die() {
    printf 'benchmark-governance-tests: ERROR: %s\n' "$*" >&2
    exit 1
}

pass() {
    printf 'benchmark-governance-tests: PASS: %s\n' "$*"
}

[ -f "$schema_path" ] || die "schema missing: $schema_path"
[ -f "$valid_path" ] || die "valid example missing: $valid_path"
[ -f "$leakage_path" ] || die "leakage fixture missing: $leakage_path"

tmp_root="$(mktemp -d)"
cleanup() {
    rm -rf -- "$tmp_root"
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Shared validator (Python). Exit 0 = OK, 1 = validation failure.
# Usage: python3 - "$schema" "$manifest" [expect_fail]
# ---------------------------------------------------------------------------
validate_py() {
    python3 - "$@" <<'PY'
from __future__ import annotations

import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - Python < 3.11
    import tomli as tomllib  # type: ignore

schema_path = Path(sys.argv[1])
manifest_path = Path(sys.argv[2])
expect_fail = len(sys.argv) > 3 and sys.argv[3] in {"1", "true", "fail", "FAIL"}


def load(path: Path) -> dict:
    try:
        return tomllib.loads(path.read_text(encoding="utf-8"))
    except Exception as exc:  # noqa: BLE001
        print(f"ERROR: cannot parse {path}: {exc}", file=sys.stderr)
        raise SystemExit(1)


def fail(msg: str) -> None:
    print(f"ERROR: {msg}", file=sys.stderr)
    raise SystemExit(1)


def ok(msg: str) -> None:
    print(f"OK: {msg}")


schema = load(schema_path)
manifest = load(manifest_path)

if schema.get("schema_version") != 1:
    fail("schema schema_version must be 1")
if schema.get("kind") != "benchmark-manifest-schema":
    fail("schema kind must be benchmark-manifest-schema")

pm = schema.get("promotion_manifest")
if not isinstance(pm, dict):
    fail("schema.promotion_manifest must be a table")

req_top = [str(x) for x in (pm.get("required_top_level") or [])]
isolated_roles = [str(x) for x in (pm.get("isolated_split_roles") or [])]
req_split = [str(x) for x in (pm.get("required_split_fields") or [])]
req_hidden = [str(x) for x in (pm.get("required_hidden_final_fields") or [])]
req_dcp = [str(x) for x in (pm.get("required_dataset_change_policy_fields") or [])]
allowed_baselines = {str(x).lower() for x in (pm.get("allowed_contamination_baselines") or [])}
allowed_methods = {str(x).lower() for x in (pm.get("allowed_confidence_methods") or [])}
gold_cfg = pm.get("gold") or {}
forbidden_gold = {
    str(x) for x in (gold_cfg.get("forbidden_without_independent_validation") or [])
}


def validate_manifest(doc: dict, label: str) -> None:
    for key in req_top:
        if key not in doc:
            fail(f"{label}: missing required top-level field {key!r}")

    if doc.get("schema_version") != 1:
        fail(f"{label}: schema_version must be 1")

    bid = doc.get("benchmark_id")
    if not isinstance(bid, str) or not bid.strip():
        fail(f"{label}: benchmark_id must be a non-empty string")

    method = doc.get("confidence_method")
    if not isinstance(method, str) or not method.strip():
        fail(f"{label}: confidence_method must be a non-empty string")
    if allowed_methods and method.lower() not in allowed_methods:
        fail(f"{label}: confidence_method {method!r} not in allowed_confidence_methods")

    es = doc.get("effect_size_min")
    if not isinstance(es, (int, float)) or isinstance(es, bool):
        fail(f"{label}: effect_size_min must be a number")
    if float(es) < 0:
        fail(f"{label}: effect_size_min must be >= 0")

    rt = doc.get("regression_tolerance")
    if not isinstance(rt, (int, float)) or isinstance(rt, bool):
        fail(f"{label}: regression_tolerance must be a number")
    if float(rt) < 0:
        fail(f"{label}: regression_tolerance must be >= 0")

    baselines = doc.get("contamination_baselines")
    if not isinstance(baselines, list) or not baselines:
        fail(f"{label}: contamination_baselines must be a non-empty array")
    for b in baselines:
        if not isinstance(b, str) or not b.strip():
            fail(f"{label}: contamination_baselines entries must be non-empty strings")
        if allowed_baselines and b.lower() not in allowed_baselines:
            fail(f"{label}: unknown contamination baseline {b!r}")

    dcp = doc.get("dataset_change_policy")
    if not isinstance(dcp, dict):
        fail(f"{label}: dataset_change_policy must be a table")
    for key in req_dcp:
        if key not in dcp:
            fail(f"{label}: dataset_change_policy missing {key!r}")
    if dcp.get("review_required") is not True:
        fail(f"{label}: dataset_change_policy.review_required must be true")

    # Optional temporal_holdout when present must be a table with cutoff string.
    th = doc.get("temporal_holdout")
    if th is not None:
        if not isinstance(th, dict):
            fail(f"{label}: temporal_holdout must be a table when present")
        cutoff = th.get("cutoff")
        if cutoff is not None and (not isinstance(cutoff, str) or not cutoff.strip()):
            fail(f"{label}: temporal_holdout.cutoff must be a non-empty string when set")

    # Gold rules
    gold = doc.get("gold")
    if isinstance(gold, dict):
        independent = gold.get("independent_validation") is True
        for key in forbidden_gold:
            val = gold.get(key)
            # true, or a non-empty truthy source string implying use of that gold
            if val is True and not independent:
                fail(
                    f"{label}: {key}=true requires independent_validation=true"
                )

    # Splits + group isolation
    splits = doc.get("splits")
    if not isinstance(splits, dict) or not splits:
        fail(f"{label}: splits must be a non-empty table")

    role_groups: dict[str, set[str]] = {}

    def collect_groups(role: str, table: dict) -> set[str]:
        for key in req_split:
            if key not in table:
                fail(f"{label}: splits.{role} missing field {key!r}")
        gids = table.get("group_ids")
        if not isinstance(gids, list) or not gids:
            fail(f"{label}: splits.{role}.group_ids must be a non-empty array")
        out: set[str] = set()
        for g in gids:
            if not isinstance(g, str) or not g.strip():
                fail(f"{label}: splits.{role}.group_ids entries must be non-empty strings")
            if g in out:
                fail(f"{label}: splits.{role} duplicate group_id {g!r}")
            out.add(g)
        return out

    for role, table in splits.items():
        if not isinstance(table, dict):
            fail(f"{label}: splits.{role} must be a table")
        role_groups[str(role)] = collect_groups(str(role), table)

    hidden = doc.get("hidden_final_set")
    if not isinstance(hidden, dict):
        fail(f"{label}: hidden_final_set must be a table")
    for key in req_hidden:
        if key not in hidden:
            fail(f"{label}: hidden_final_set missing {key!r}")
    if hidden.get("separate_from_tuning") is not True:
        fail(f"{label}: hidden_final_set.separate_from_tuning must be true")
    hg = hidden.get("group_ids")
    if not isinstance(hg, list) or not hg:
        fail(f"{label}: hidden_final_set.group_ids must be a non-empty array")
    hidden_groups: set[str] = set()
    for g in hg:
        if not isinstance(g, str) or not g.strip():
            fail(f"{label}: hidden_final_set.group_ids entries must be non-empty strings")
        if g in hidden_groups:
            fail(f"{label}: hidden_final_set duplicate group_id {g!r}")
        hidden_groups.add(g)
    role_groups["hidden_final"] = hidden_groups

    # Cross-role group isolation for declared isolated roles that are present.
    seen: dict[str, str] = {}
    for role in isolated_roles:
        groups = role_groups.get(role)
        if groups is None:
            continue
        for g in groups:
            prev = seen.get(g)
            if prev is not None and prev != role:
                fail(
                    f"{label}: group_id {g!r} leaks across splits "
                    f"({prev!r} and {role!r})"
                )
            seen[g] = role

    ok(f"{label}: manifest valid")


try:
    validate_manifest(manifest, manifest_path.name)
except SystemExit as exc:
    if expect_fail and exc.code == 1:
        ok(f"{manifest_path.name}: expected validation failure")
        raise SystemExit(0)
    raise

if expect_fail:
    fail(f"{manifest_path.name}: expected validation failure but manifest passed")
raise SystemExit(0)
PY
}

# ---------------------------------------------------------------------------
# Case 1: valid minimal example MUST pass
# ---------------------------------------------------------------------------
if ! validate_py "$schema_path" "$valid_path"; then
    die "valid minimal example failed validation"
fi
pass "valid minimal example"

# ---------------------------------------------------------------------------
# Case 2: group leakage fixture MUST fail
# ---------------------------------------------------------------------------
if ! validate_py "$schema_path" "$leakage_path" fail; then
    die "group leakage fixture did not produce the expected validation failure"
fi
pass "group leakage across splits rejected"

# ---------------------------------------------------------------------------
# Case 3: missing confidence_method or effect_size_min MUST fail
# ---------------------------------------------------------------------------
missing_stats="$tmp_root/missing-stats.toml"
python3 - "$valid_path" "$missing_stats" <<'PY'
from pathlib import Path
import sys

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib  # type: ignore

src = Path(sys.argv[1]).read_text(encoding="utf-8")
# Drop confidence_method and effect_size_min lines for a synthetic bad manifest.
out_lines = []
for line in src.splitlines():
    stripped = line.strip()
    if stripped.startswith("confidence_method"):
        continue
    if stripped.startswith("effect_size_min"):
        continue
    out_lines.append(line)
Path(sys.argv[2]).write_text("\n".join(out_lines) + "\n", encoding="utf-8")
PY

if ! validate_py "$schema_path" "$missing_stats" fail; then
    die "missing statistical fields did not fail validation"
fi
pass "missing confidence_method/effect_size_min rejected"

# ---------------------------------------------------------------------------
# Case 4: gold_from_filename without independent_validation MUST fail
# ---------------------------------------------------------------------------
bad_gold="$tmp_root/bad-gold.toml"
python3 - "$valid_path" "$bad_gold" <<'PY'
from pathlib import Path
import sys

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib  # type: ignore

text = Path(sys.argv[1]).read_text(encoding="utf-8")
# Rewrite gold block to forbid-filename-as-gold without independent validation.
lines = []
in_gold = False
for line in text.splitlines():
    if line.startswith("[gold]"):
        in_gold = True
        lines.append(line)
        lines.append('source = "filename"')
        lines.append("independent_validation = false")
        lines.append("gold_from_filename = true")
        continue
    if in_gold:
        if line.startswith("["):
            in_gold = False
            lines.append(line)
        # skip original gold keys
        continue
    lines.append(line)
Path(sys.argv[2]).write_text("\n".join(lines) + "\n", encoding="utf-8")
PY

if ! validate_py "$schema_path" "$bad_gold" fail; then
    die "gold_from_filename without independent_validation did not fail"
fi
pass "gold_from_filename without independent_validation rejected"

printf 'benchmark-governance-tests: all cases passed\n'
