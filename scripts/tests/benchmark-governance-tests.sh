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
# Shared validator (Python). Exit 0 = OK, 1 = validation failure, 2 = infra.
# Usage: python3 - "$schema" "$manifest" [expect_fail] [expected_diagnostic]
# ---------------------------------------------------------------------------
validate_py() {
    python3 - "$@" <<'PY'
from __future__ import annotations

import math
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - Python < 3.11
    import tomli as tomllib  # type: ignore

schema_path = Path(sys.argv[1])
manifest_path = Path(sys.argv[2])
expect_fail = len(sys.argv) > 3 and sys.argv[3] in {"1", "true", "fail", "FAIL"}
expected_diag = sys.argv[4] if len(sys.argv) > 4 else None
last_error: str | None = None


def load(path: Path) -> dict:
    try:
        return tomllib.loads(path.read_text(encoding="utf-8"))
    except Exception as exc:  # noqa: BLE001 - parse boundary
        print(f"ERROR: cannot parse {path}: {exc}", file=sys.stderr)
        # Infra/parse failure must not satisfy expect_fail.
        raise SystemExit(2) from exc


def fail(msg: str) -> None:
    global last_error
    last_error = msg
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
required_roles = [
    str(x) for x in (pm.get("required_split_roles") or ["train", "validation", "test"])
]
req_split = [str(x) for x in (pm.get("required_split_fields") or [])]
req_hidden = [str(x) for x in (pm.get("required_hidden_final_fields") or [])]
req_dcp = [str(x) for x in (pm.get("required_dataset_change_policy_fields") or [])]
allowed_baselines = {str(x).lower() for x in (pm.get("allowed_contamination_baselines") or [])}
allowed_methods = {str(x).lower() for x in (pm.get("allowed_confidence_methods") or [])}
expected_kind = pm.get("kind")
if not isinstance(expected_kind, str) or not expected_kind.strip():
    fail("schema.promotion_manifest.kind must be a non-empty string")
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

    kind = doc.get("kind")
    if kind != expected_kind:
        fail(
            f"{label}: kind must be {expected_kind!r}, got {kind!r}"
        )

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
    es_f = float(es)
    if not math.isfinite(es_f):
        fail(f"{label}: effect_size_min must be finite")
    if es_f < 0:
        fail(f"{label}: effect_size_min must be >= 0")

    rt = doc.get("regression_tolerance")
    if not isinstance(rt, (int, float)) or isinstance(rt, bool):
        fail(f"{label}: regression_tolerance must be a number")
    rt_f = float(rt)
    if not math.isfinite(rt_f):
        fail(f"{label}: regression_tolerance must be finite")
    if rt_f < 0:
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

    # Gold rules: forbidden keys must be booleans; True requires independent_validation.
    gold = doc.get("gold")
    if isinstance(gold, dict):
        independent = gold.get("independent_validation") is True
        for key in forbidden_gold:
            if key not in gold:
                continue
            val = gold.get(key)
            if not isinstance(val, bool):
                fail(f"{label}: gold.{key} must be a boolean")
            if val is True and not independent:
                fail(
                    f"{label}: gold.{key}=true requires independent_validation=true"
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

    for role in required_roles:
        if role not in role_groups:
            fail(f"{label}: missing required split role {role!r}")

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

    # Cross-role group isolation for ALL declared splits plus hidden_final.
    # Tuning/calibration/custom roles must not share group_ids with any other role.
    seen: dict[str, str] = {}
    for role in sorted(role_groups):
        groups = role_groups[role]
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
    if expect_fail and exc.code == 1 and last_error is not None:
        if expected_diag is not None and expected_diag not in last_error:
            print(
                f"ERROR: expected diagnostic containing {expected_diag!r}, "
                f"got {last_error!r}",
                file=sys.stderr,
            )
            raise SystemExit(2)
        ok(f"{manifest_path.name}: expected validation failure")
        raise SystemExit(0)
    raise

if expect_fail:
    fail(f"{manifest_path.name}: expected validation failure but manifest passed")
raise SystemExit(0)
PY
}

# ---------------------------------------------------------------------------
# Helpers for synthetic negative fixtures derived from the valid example
# ---------------------------------------------------------------------------
mutate_toml() {
    # mutate_toml <out_path> <python-body-using-doc-dict>
    local out_path="$1"
    local body="$2"
    python3 - "$valid_path" "$out_path" <<PY
from pathlib import Path
import sys
try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib  # type: ignore

src = Path(sys.argv[1])
dst = Path(sys.argv[2])
doc = tomllib.loads(src.read_text(encoding="utf-8"))
$body
# Re-emit a minimal TOML subset via a small encoder for our known shapes.
def enc(obj, indent=0):
    sp = "  " * indent
    if isinstance(obj, dict):
        # top-level mixed: emit scalars first then tables
        lines = []
        tables = []
        for k, v in obj.items():
            if isinstance(v, dict):
                tables.append((k, v))
            elif isinstance(v, list) and v and isinstance(v[0], dict):
                tables.append((k, v))
            else:
                lines.append(f"{sp}{k} = {enc_value(v)}")
        for k, v in tables:
            if isinstance(v, list):
                for item in v:
                    lines.append(f"{sp}[[{k}]]")
                    lines.append(enc(item, indent + 1) if False else "")
            else:
                # nested table path
                lines.append(f"\\n[{k}]" if indent == 0 else f"{sp}[{k}]")
                for sk, sv in v.items():
                    if isinstance(sv, dict):
                        lines.append(f"\\n[{k}.{sk}]")
                        for ssk, ssv in sv.items():
                            lines.append(f"{ssk} = {enc_value(ssv)}")
                    else:
                        lines.append(f"{sk} = {enc_value(sv)}")
        return "\\n".join(lines)
    return enc_value(obj)

def enc_value(v):
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int) and not isinstance(v, bool):
        return str(v)
    if isinstance(v, float):
        if v != v:  # NaN
            return "nan"
        if v == float("inf"):
            return "inf"
        if v == float("-inf"):
            return "-inf"
        return repr(v)
    if isinstance(v, str):
        return '"' + v.replace('\\\\', '\\\\\\\\').replace('"', '\\\\"') + '"'
    if isinstance(v, list):
        return "[" + ", ".join(enc_value(x) for x in v) + "]"
    raise TypeError(type(v))

# Structured emit for our promotion manifests (known keys).
lines = []
order = [
    "schema_version", "kind", "benchmark_id", "spec",
    "confidence_method", "effect_size_min", "regression_tolerance",
    "contamination_baselines",
]
for key in order:
    if key in doc:
        lines.append(f"{key} = {enc_value(doc[key])}")
# tables
for key in ("splits",):
    if key in doc and isinstance(doc[key], dict):
        for role, table in doc[key].items():
            lines.append(f"\\n[{key}.{role}]")
            for sk, sv in table.items():
                lines.append(f"{sk} = {enc_value(sv)}")
for key in ("hidden_final_set", "temporal_holdout", "dataset_change_policy", "gold"):
    if key in doc and isinstance(doc[key], dict):
        lines.append(f"\\n[{key}]")
        for sk, sv in doc[key].items():
            lines.append(f"{sk} = {enc_value(sv)}")
dst.write_text("\\n".join(lines) + "\\n", encoding="utf-8")
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
if ! validate_py "$schema_path" "$leakage_path" fail "leaks across splits"; then
    die "group leakage fixture did not produce the expected validation failure"
fi
pass "group leakage across splits rejected"

# ---------------------------------------------------------------------------
# Case 3: missing confidence_method MUST fail (single-field mutation)
# ---------------------------------------------------------------------------
missing_cm="$tmp_root/missing-confidence-method.toml"
mutate_toml "$missing_cm" 'doc.pop("confidence_method", None)'
if ! validate_py "$schema_path" "$missing_cm" fail "missing required top-level field 'confidence_method'"; then
    die "missing confidence_method did not fail with expected diagnostic"
fi
pass "missing confidence_method rejected"

# ---------------------------------------------------------------------------
# Case 4: missing effect_size_min MUST fail (single-field mutation)
# ---------------------------------------------------------------------------
missing_es="$tmp_root/missing-effect-size.toml"
mutate_toml "$missing_es" 'doc.pop("effect_size_min", None)'
if ! validate_py "$schema_path" "$missing_es" fail "missing required top-level field 'effect_size_min'"; then
    die "missing effect_size_min did not fail with expected diagnostic"
fi
pass "missing effect_size_min rejected"

# ---------------------------------------------------------------------------
# Case 5: gold_from_filename=true without independent_validation MUST fail
# ---------------------------------------------------------------------------
bad_gold="$tmp_root/bad-gold-bool.toml"
mutate_toml "$bad_gold" '
doc["gold"] = {
    "source": "filename",
    "independent_validation": False,
    "gold_from_filename": True,
}
'
if ! validate_py "$schema_path" "$bad_gold" fail "gold.gold_from_filename=true requires independent_validation=true"; then
    die "gold_from_filename boolean without independent_validation did not fail"
fi
pass "gold_from_filename=true without independent_validation rejected"

# ---------------------------------------------------------------------------
# Case 6: gold_from_filename string value MUST fail (type enforcement)
# ---------------------------------------------------------------------------
bad_gold_str="$tmp_root/bad-gold-string.toml"
mutate_toml "$bad_gold_str" '
doc["gold"] = {
    "source": "filename",
    "independent_validation": False,
    "gold_from_filename": "filename-label",
}
'
if ! validate_py "$schema_path" "$bad_gold_str" fail "gold.gold_from_filename must be a boolean"; then
    die "gold_from_filename string value did not fail type check"
fi
pass "gold_from_filename string value rejected"

# ---------------------------------------------------------------------------
# Case 7: non-finite effect_size_min (nan) MUST fail
# ---------------------------------------------------------------------------
nan_es="$tmp_root/nan-effect-size.toml"
mutate_toml "$nan_es" 'doc["effect_size_min"] = float("nan")'
if ! validate_py "$schema_path" "$nan_es" fail "effect_size_min must be finite"; then
    die "effect_size_min=nan did not fail"
fi
pass "effect_size_min=nan rejected"

# ---------------------------------------------------------------------------
# Case 8: non-finite regression_tolerance (inf) MUST fail
# ---------------------------------------------------------------------------
inf_rt="$tmp_root/inf-regression.toml"
mutate_toml "$inf_rt" 'doc["regression_tolerance"] = float("inf")'
if ! validate_py "$schema_path" "$inf_rt" fail "regression_tolerance must be finite"; then
    die "regression_tolerance=inf did not fail"
fi
pass "regression_tolerance=inf rejected"

# ---------------------------------------------------------------------------
# Case 9: missing kind MUST fail
# ---------------------------------------------------------------------------
missing_kind="$tmp_root/missing-kind.toml"
mutate_toml "$missing_kind" 'doc.pop("kind", None)'
if ! validate_py "$schema_path" "$missing_kind" fail "missing required top-level field 'kind'"; then
    die "missing kind did not fail"
fi
pass "missing kind rejected"

# ---------------------------------------------------------------------------
# Case 10: wrong kind MUST fail
# ---------------------------------------------------------------------------
wrong_kind="$tmp_root/wrong-kind.toml"
mutate_toml "$wrong_kind" 'doc["kind"] = "unrelated-document"'
if ! validate_py "$schema_path" "$wrong_kind" fail "kind must be 'benchmark-promotion-manifest'"; then
    die "wrong kind did not fail"
fi
pass "wrong kind rejected"

# ---------------------------------------------------------------------------
# Case 11: tuning split overlapping hidden_final MUST fail
# ---------------------------------------------------------------------------
tuning_leak="$tmp_root/tuning-leak.toml"
mutate_toml "$tuning_leak" '
doc["splits"]["tuning"] = {"group_ids": ["thread-f"]}
'
if ! validate_py "$schema_path" "$tuning_leak" fail "leaks across splits"; then
    die "tuning/hidden_final overlap did not fail"
fi
pass "tuning/hidden_final group leakage rejected"

# ---------------------------------------------------------------------------
# Case 12: calibration split overlapping hidden_final MUST fail
# ---------------------------------------------------------------------------
cal_leak="$tmp_root/calibration-leak.toml"
mutate_toml "$cal_leak" '
doc["splits"]["calibration"] = {"group_ids": ["thread-f"]}
'
if ! validate_py "$schema_path" "$cal_leak" fail "leaks across splits"; then
    die "calibration/hidden_final overlap did not fail"
fi
pass "calibration/hidden_final group leakage rejected"

# ---------------------------------------------------------------------------
# Case 13: missing standard split roles MUST fail
# ---------------------------------------------------------------------------
no_standard="$tmp_root/no-standard-roles.toml"
mutate_toml "$no_standard" '
doc["splits"] = {
    "tuning": {"group_ids": ["thread-a"]},
    "dev": {"group_ids": ["thread-b"]},
    "holdout": {"group_ids": ["thread-c"]},
}
'
if ! validate_py "$schema_path" "$no_standard" fail "missing required split role"; then
    die "missing standard split roles did not fail"
fi
pass "missing standard split roles rejected"

printf 'benchmark-governance-tests: all cases passed\n'
