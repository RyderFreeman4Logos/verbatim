#!/usr/bin/env bash
# shellcheck shell=bash
# Validate supply-chain provenance schema + fixtures (SUPPLY-001 / #341).
# Does NOT require cargo-cyclonedx.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=fixture-cleanup.sh
source "$root/scripts/tests/fixture-cleanup.sh"
schema_path="${SUPPLY_SCHEMA_PATH:-$root/docs/supply-chain/provenance-manifest.schema.toml}"
profile_path="${SUPPLY_PROFILE_PATH:-$root/docs/supply-chain/examples/runtime-profile.example.toml}"
revocation_path="${SUPPLY_REVOCATION_PATH:-$root/docs/supply-chain/examples/revocation.example.toml}"
strict_mode="${SUPPLY_STRICT:-1}"

die() {
    printf 'supply-chain-provenance-tests: ERROR: %s\n' "$*" >&2
    exit 1
}

pass() {
    printf 'supply-chain-provenance-tests: PASS: %s\n' "$*"
}

[ -f "$schema_path" ] || die "schema missing: $schema_path"
[ -f "$profile_path" ] || die "profile missing: $profile_path"
[ -f "$revocation_path" ] || die "revocation list missing: $revocation_path"

tmp_root="$(mktemp -d)"
cleanup() {
    local status=$?
    if ! cleanup_fixture_root "$tmp_root" "$root"; then
        [ "$status" -ne 0 ] || status=1
    fi
    exit "$status"
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Case 1: schema + example profile + revocation parse and satisfy contract
# ---------------------------------------------------------------------------
python3 - "$schema_path" "$profile_path" "$revocation_path" "$strict_mode" <<'PY'
from __future__ import annotations

import re
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - Python < 3.11
    import tomli as tomllib  # type: ignore

schema_path = Path(sys.argv[1])
profile_path = Path(sys.argv[2])
revocation_path = Path(sys.argv[3])
strict = sys.argv[4] not in {"0", "false", "False", "no", "NO"}


def load(path: Path) -> dict:
    try:
        return tomllib.loads(path.read_text(encoding="utf-8"))
    except Exception as exc:  # noqa: BLE001
        print(f"ERROR: cannot parse {path}: {exc}", file=sys.stderr)
        raise SystemExit(1)


def fail(msg: str) -> None:
    print(f"ERROR: {msg}", file=sys.stderr)
    raise SystemExit(1)


schema = load(schema_path)
profile = load(profile_path)
revocation = load(revocation_path)

if schema.get("schema_version") != 1:
    fail("schema schema_version must be 1")
if schema.get("kind") != "provenance-manifest-schema":
    fail("schema kind must be provenance-manifest-schema")

rp = schema.get("runtime_profile")
if not isinstance(rp, dict):
    fail("schema.runtime_profile must be a table")

required_classes = rp.get("required_classes")
if not isinstance(required_classes, list) or not required_classes:
    fail("schema.runtime_profile.required_classes must be non-empty")
required_classes = [str(c) for c in required_classes]

aliases_raw = rp.get("class_aliases") or {}
if not isinstance(aliases_raw, dict):
    fail("schema.runtime_profile.class_aliases must be a table")

alias_to_class: dict[str, str] = {}
for canonical, alias_list in aliases_raw.items():
    if not isinstance(alias_list, list):
        fail(f"class_aliases.{canonical} must be a list")
    for alias in alias_list:
        alias_to_class[str(alias).lower()] = str(canonical)

digest_cfg = rp.get("digest") or {}
allowed_algs = {str(a).lower() for a in (digest_cfg.get("allowed_algs") or [])}
hex_pattern = re.compile(str(digest_cfg.get("hex_pattern") or r"^[0-9a-f]+$"))
min_hex = {
    str(k).lower(): int(v)
    for k, v in (digest_cfg.get("min_hex_len") or {}).items()
}

req_top = [str(x) for x in (rp.get("required_top_level") or [])]
req_app = [str(x) for x in (rp.get("required_app_fields") or [])]
req_comp = [str(x) for x in (rp.get("required_component_fields") or [])]

for key in req_top:
    if key not in profile:
        fail(f"profile missing required top-level field {key!r}")

if profile.get("schema_version") != 1:
    fail("profile schema_version must be 1")

app = profile.get("app")
if not isinstance(app, dict):
    fail("profile.app must be a table")
for key in req_app:
    val = app.get(key)
    if not isinstance(val, str) or not val.strip():
        fail(f"profile.app.{key} must be a non-empty string")

components = profile.get("components")
if not isinstance(components, list) or not components:
    fail("profile.components must be a non-empty array of tables")

present_classes: set[str] = set()
profile_digests: set[tuple[str, str]] = set()
ids: set[str] = set()

for index, comp in enumerate(components):
    label = f"components[{index}]"
    if not isinstance(comp, dict):
        fail(f"{label}: must be a table")
    for key in req_comp:
        if key not in comp:
            fail(f"{label}: missing field {key!r}")
    cid = comp.get("id")
    if not isinstance(cid, str) or not cid.strip():
        fail(f"{label}: id must be non-empty string")
    if cid in ids:
        fail(f"duplicate component id: {cid}")
    ids.add(cid)
    label = cid

    raw_class = comp.get("class")
    if not isinstance(raw_class, str) or not raw_class.strip():
        fail(f"{label}: class must be non-empty string")
    canonical = alias_to_class.get(raw_class.lower())
    if canonical is None:
        fail(f"{label}: unknown class {raw_class!r} (not in schema aliases)")
    present_classes.add(canonical)

    name = comp.get("name")
    if not isinstance(name, str) or not name.strip():
        fail(f"{label}: name must be non-empty string")

    alg = str(comp.get("digest_alg") or "").lower()
    digest = str(comp.get("digest") or "").lower()
    if alg not in allowed_algs:
        fail(f"{label}: digest_alg {alg!r} not in allowed_algs")
    if not hex_pattern.match(digest):
        fail(f"{label}: digest must match hex pattern")
    min_len = min_hex.get(alg)
    if min_len is not None and len(digest) < min_len:
        fail(f"{label}: digest length {len(digest)} < min {min_len} for {alg}")
    profile_digests.add((alg, digest))

missing = [c for c in required_classes if c not in present_classes]
if missing:
    fail(f"profile missing required component classes: {missing}")

# Revocation list
rl = schema.get("revocation_list")
if not isinstance(rl, dict):
    fail("schema.revocation_list must be a table")
for key in (rl.get("required_top_level") or []):
    if str(key) not in revocation:
        fail(f"revocation missing required top-level field {key!r}")
if revocation.get("schema_version") != 1:
    fail("revocation schema_version must be 1")

revoked_on_re = re.compile(str(rl.get("revoked_on_pattern") or r"^\d{4}-\d{2}-\d{2}$"))
req_entry = [str(x) for x in (rl.get("required_entry_fields") or [])]
revoked = revocation.get("revoked")
if not isinstance(revoked, list) or not revoked:
    fail("revocation.revoked must be a non-empty array")

revoked_digests: set[tuple[str, str]] = set()
for index, entry in enumerate(revoked):
    label = f"revoked[{index}]"
    if not isinstance(entry, dict):
        fail(f"{label}: must be a table")
    for key in req_entry:
        if key not in entry:
            fail(f"{label}: missing field {key!r}")
    alg = str(entry.get("digest_alg") or "").lower()
    digest = str(entry.get("digest") or "").lower()
    if alg not in allowed_algs:
        fail(f"{label}: digest_alg {alg!r} not allowed")
    if not hex_pattern.match(digest):
        fail(f"{label}: digest must be hex")
    reason = entry.get("reason")
    if not isinstance(reason, str) or not reason.strip():
        fail(f"{label}: reason must be non-empty")
    revoked_on = entry.get("revoked_on")
    if not isinstance(revoked_on, str) or not revoked_on_re.match(revoked_on):
        fail(f"{label}: revoked_on must match ISO date")
    revoked_digests.add((alg, digest))

if strict:
    hits = sorted(profile_digests & revoked_digests)
    if hits:
        fail(f"strict mode: profile references revoked digests: {hits}")

print(
    "schema+examples OK: "
    f"{len(components)} components, classes={sorted(present_classes)}, "
    f"revoked={len(revoked_digests)}"
)
PY
pass "schema and example fixtures"

# ---------------------------------------------------------------------------
# Case 2: reject profile missing a required class
# ---------------------------------------------------------------------------
missing_class_profile="$tmp_root/missing-class.toml"
python3 - "$profile_path" "$missing_class_profile" <<'PY'
from pathlib import Path
import sys

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib  # type: ignore

src = Path(sys.argv[1]).read_text(encoding="utf-8")
# Drop all binary-class components by rewriting class binary lines only when
# building a synthetic invalid profile: remove the first binary component block.
# Simpler: load and rewrite without binary components via tomllib+manual emit.
data = tomllib.loads(src)
lines = [
    'schema_version = 1',
    'kind = "runtime-profile"',
    'profile_id = "missing-binary-class"',
    '',
    '[app]',
    f'name = "{data["app"]["name"]}"',
    f'version = "{data["app"]["version"]}"',
    f'git_sha = "{data["app"]["git_sha"]}"',
    '',
]
for comp in data["components"]:
    if str(comp.get("class", "")).lower() == "binary":
        continue
    lines.append("[[components]]")
    for key in ("id", "class", "name", "version", "source", "platform", "image_ref", "role", "path", "digest_alg", "digest"):
        if key in comp:
            val = comp[key]
            if isinstance(val, str):
                lines.append(f'{key} = "{val}"')
            else:
                lines.append(f"{key} = {val}")
    lines.append("")
Path(sys.argv[2]).write_text("\n".join(lines) + "\n", encoding="utf-8")
PY

set +e
out="$(
    SUPPLY_PROFILE_PATH="$missing_class_profile" \
    SUPPLY_REVOCATION_PATH="$revocation_path" \
    SUPPLY_SCHEMA_PATH="$schema_path" \
    SUPPLY_STRICT=1 \
    SUPPLY_SELF_CASE=missing-class \
    python3 - "$schema_path" "$missing_class_profile" "$revocation_path" "1" <<'PY' 2>&1
from __future__ import annotations
import re, sys
from pathlib import Path
try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib  # type: ignore

schema = tomllib.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
profile = tomllib.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
rp = schema["runtime_profile"]
required_classes = [str(c) for c in rp["required_classes"]]
alias_to_class = {}
for canonical, alias_list in (rp.get("class_aliases") or {}).items():
    for alias in alias_list:
        alias_to_class[str(alias).lower()] = str(canonical)
present = set()
for comp in profile.get("components") or []:
    raw = str(comp.get("class", "")).lower()
    if raw in alias_to_class:
        present.add(alias_to_class[raw])
missing = [c for c in required_classes if c not in present]
if missing:
    print(f"ERROR: profile missing required component classes: {missing}", file=sys.stderr)
    raise SystemExit(1)
print("unexpected pass", file=sys.stderr)
raise SystemExit(0)
PY
)"
rc=$?
set -e
if [ "$rc" -eq 0 ]; then
    die "expected missing-class profile to fail, but it passed"
fi
printf '%s\n' "$out" | grep -q 'missing required component classes' \
    || die "missing-class failure message not found: $out"
pass "rejects profile missing required class"

# ---------------------------------------------------------------------------
# Case 3: reject revoked digest in strict mode
# ---------------------------------------------------------------------------
revoked_hit_profile="$tmp_root/revoked-hit.toml"
python3 - "$profile_path" "$revocation_path" "$revoked_hit_profile" <<'PY'
from pathlib import Path
import sys

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib  # type: ignore

profile = tomllib.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
revocation = tomllib.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
rev = revocation["revoked"][0]
# Inject the revoked digest into the first component.
comp0 = profile["components"][0]
lines = [
    'schema_version = 1',
    'kind = "runtime-profile"',
    'profile_id = "revoked-digest-hit"',
    '',
    '[app]',
    f'name = "{profile["app"]["name"]}"',
    f'version = "{profile["app"]["version"]}"',
    f'git_sha = "{profile["app"]["git_sha"]}"',
    '',
]
first = True
for comp in profile["components"]:
    lines.append("[[components]]")
    for key in ("id", "class", "name", "version", "source", "platform", "image_ref", "role", "path", "digest_alg", "digest"):
        if key not in comp:
            continue
        val = comp[key]
        if first and key == "digest_alg":
            val = rev["digest_alg"]
        if first and key == "digest":
            val = rev["digest"]
        if isinstance(val, str):
            lines.append(f'{key} = "{val}"')
        else:
            lines.append(f"{key} = {val}")
    first = False
    lines.append("")
Path(sys.argv[3]).write_text("\n".join(lines) + "\n", encoding="utf-8")
PY

set +e
out="$(
    python3 - "$schema_path" "$revoked_hit_profile" "$revocation_path" "1" <<'PY' 2>&1
from __future__ import annotations
import re, sys
from pathlib import Path
try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib  # type: ignore

schema = tomllib.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
profile = tomllib.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
revocation = tomllib.loads(Path(sys.argv[3]).read_text(encoding="utf-8"))
rp = schema["runtime_profile"]
allowed_algs = {str(a).lower() for a in rp["digest"]["allowed_algs"]}
hex_pattern = re.compile(str(rp["digest"]["hex_pattern"]))
profile_digests = set()
for comp in profile["components"]:
    alg = str(comp["digest_alg"]).lower()
    digest = str(comp["digest"]).lower()
    profile_digests.add((alg, digest))
revoked_digests = set()
for entry in revocation["revoked"]:
    revoked_digests.add((str(entry["digest_alg"]).lower(), str(entry["digest"]).lower()))
hits = sorted(profile_digests & revoked_digests)
if hits:
    print(f"ERROR: strict mode: profile references revoked digests: {hits}", file=sys.stderr)
    raise SystemExit(1)
print("unexpected pass", file=sys.stderr)
raise SystemExit(0)
PY
)"
rc=$?
set -e
if [ "$rc" -eq 0 ]; then
    die "expected revoked-digest profile to fail strict mode, but it passed"
fi
printf '%s\n' "$out" | grep -q 'revoked digests' \
    || die "revoked-digest failure message not found: $out"
pass "rejects revoked digest in strict mode"

printf 'supply-chain-provenance-tests: PASS (all cases)\n'
