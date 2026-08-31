#!/usr/bin/env bash
set -euo pipefail
export GIT_NO_REPLACE_OBJECTS=1

usage() {
    cat >&2 <<'EOF'
Usage: scripts/hooks/check-version-bumped.sh --scope staged|head|object [--object <object-id>] [--base-ref <ref>]

Scopes:
  staged  compare the Cargo.toml blob in the index with the base ref
  head    compare HEAD:Cargo.toml with the base ref
  object  compare a committed object's Cargo.toml blob with the base ref

Versions:
  Both versions must be valid SemVer 2.0.0. The snapshot must have strictly
  greater SemVer precedence than the base; build metadata does not affect it.
EOF
}

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 2
}

scope=""
base_ref="${BASE_REF:-}"
object_id=""
expected_tree=""

while [ "$#" -gt 0 ]; do
    case "$1" in
        --scope)
            [ "$#" -ge 2 ] || die "--scope requires a value"
            scope="$2"
            shift 2
            ;;
        --base-ref)
            [ "$#" -ge 2 ] || die "--base-ref requires a value"
            base_ref="$2"
            shift 2
            ;;
        --object)
            [ "$#" -ge 2 ] || die "--object requires a value"
            object_id="$2"
            shift 2
            ;;
        --expected-tree)
            [ "$#" -ge 2 ] || die "--expected-tree requires a value"
            expected_tree="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage
            die "unknown argument: $1"
            ;;
    esac
done

case "$scope" in
    staged|head)
        [ -z "$object_id" ] || die "--object is only valid with --scope object"
        ;;
    object)
        [ -n "$object_id" ] || die "--scope object requires --object"
        ;;
    "") usage; die "--scope is required" ;;
    *) usage; die "--scope must be one of: staged, head, object" ;;
esac

if [ "$scope" != "staged" ] && [ -n "$expected_tree" ]; then
    die "--expected-tree is only valid with --scope staged"
fi

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "cannot determine the repository root"
cd "$repo_root"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=../tests/fixture-cleanup.sh
source "$script_dir/../tests/fixture-cleanup.sh"

if [ -z "$base_ref" ]; then
    base_ref="origin/${DEFAULT_BRANCH:-main}"
fi

case "$base_ref" in
    -*|*:*|*$'\n'*|*$'\r'*) die "invalid base ref: $base_ref" ;;
esac

base_commit="$(git rev-parse --verify --quiet "${base_ref}^{commit}")" \
    || die "cannot resolve base ref: $base_ref"
base_tree="$(git rev-parse --verify --quiet "${base_commit}^{tree}")" \
    || die "cannot resolve base tree"
tmp_root="$(mktemp -d)" || die "cannot create version snapshot directory"
cleanup() {
    local status=$?
    if ! cleanup_fixture_root "$tmp_root" "$repo_root"; then
        [ "$status" -ne 0 ] || status=1
    fi
    exit "$status"
}
trap cleanup EXIT

snapshot_blob() {
    local label="$1" tree="$2" output_name="$3"
    local entries_file entry matched_entry metadata entry_path
    local entry_mode entry_type entry_id trailing count=0

    entries_file="$(mktemp "$tmp_root/entries.XXXXXX")" \
        || die "cannot create $label Cargo.toml entry list"
    if ! git ls-tree -z "$tree" -- ':(top,literal)Cargo.toml' >"$entries_file"; then
        die "cannot enumerate $label Cargo.toml tree entry"
    fi
    while IFS= read -r -d '' entry; do
        count=$((count + 1))
        matched_entry="$entry"
    done <"$entries_file"
    [ "$count" -gt 0 ] || die "$label Cargo.toml is missing"
    [ "$count" -eq 1 ] || die "$label Cargo.toml has multiple tree entries"
    case "$matched_entry" in
        *$'\t'*) ;;
        *) die "malformed $label Cargo.toml tree entry" ;;
    esac
    metadata="${matched_entry%%$'\t'*}"
    entry_path="${matched_entry#*$'\t'}"
    [ "$entry_path" = Cargo.toml ] \
        || die "$label Cargo.toml tree entry path mismatch"
    read -r entry_mode entry_type entry_id trailing <<<"$metadata"
    [ -n "$entry_mode" ] && [ -n "$entry_type" ] \
        && [ -n "$entry_id" ] && [ -z "$trailing" ] \
        || die "malformed $label Cargo.toml tree metadata"
    case "$entry_id" in
        ''|*[!0-9A-Fa-f]*) die "invalid $label Cargo.toml blob ID" ;;
    esac
    case "$entry_mode:$entry_type" in
        100644:blob|100755:blob) ;;
        *) die "$label Cargo.toml is not a regular blob" ;;
    esac
    printf -v "$output_name" '%s' "$entry_id"
}

version_from_blob() {
    local label="$1" blob_id="$2" manifest_file version

    manifest_file="$(mktemp "$tmp_root/manifest.XXXXXX")" \
        || die "cannot create $label Cargo.toml snapshot file"
    if ! git cat-file blob "$blob_id" >"$manifest_file"; then
        die "cannot materialize $label Cargo.toml blob: $blob_id"
    fi
    if ! version="$(python3 - "$manifest_file" <<'PY'
import sys
import tomllib

try:
    with open(sys.argv[1], "rb") as manifest:
        data = tomllib.load(manifest)
    version = data["workspace"]["package"]["version"]
except (KeyError, OSError, TypeError, tomllib.TOMLDecodeError) as error:
    print(f"cannot read workspace.package.version: {error}", file=sys.stderr)
    raise SystemExit(2)
if not isinstance(version, str):
    print("workspace.package.version must be a string", file=sys.stderr)
    raise SystemExit(2)
print(version)
PY
    )"; then
        die "cannot read workspace.package.version from $label Cargo.toml blob"
    fi
    printf '%s\n' "$version"
}

semver_precedence() {
    python3 - "$1" "$2" "$3" <<'PY'
import re
import sys

identifier = r"(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
pattern = re.compile(
    rf"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    rf"(?:-({identifier}(?:\.{identifier})*))?"
    rf"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$"
)


def parse(label: str, value: str) -> tuple[tuple[int, int, int], tuple[str, ...] | None]:
    match = pattern.fullmatch(value)
    if match is None:
        print(
            f"ERROR: malformed {label} version: {value!r}; expected SemVer 2.0.0",
            file=sys.stderr,
        )
        raise SystemExit(2)
    return tuple(map(int, match.group(1, 2, 3))), (
        tuple(match.group(4).split(".")) if match.group(4) is not None else None
    )


def compare_prereleases(left: tuple[str, ...] | None, right: tuple[str, ...] | None) -> int:
    if left is None:
        return 0 if right is None else 1
    if right is None:
        return -1
    for left_id, right_id in zip(left, right):
        if left_id == right_id:
            continue
        left_numeric = left_id.isdigit()
        right_numeric = right_id.isdigit()
        if left_numeric and right_numeric:
            return 1 if int(left_id) > int(right_id) else -1
        if left_numeric:
            return -1
        if right_numeric:
            return 1
        return 1 if left_id > right_id else -1
    return (len(left) > len(right)) - (len(left) < len(right))


base_core, base_prerelease = parse("base", sys.argv[1])
current_core, current_prerelease = parse(sys.argv[2], sys.argv[3])
if base_core != current_core:
    print(1 if current_core > base_core else -1)
else:
    print(compare_prereleases(current_prerelease, base_prerelease))
PY
}

case "$scope" in
    staged)
        current_tree="$(git write-tree 2>/dev/null)" \
            || die "cannot capture staged index tree"
        if [ -n "$expected_tree" ] && [ "$current_tree" != "$expected_tree" ]; then
            die "staged index tree does not match aggregate receipt"
        fi
        ;;
    head)
        current_commit="$(git rev-parse --verify --quiet 'HEAD^{commit}')" \
            || die "cannot resolve HEAD as a commit"
        current_tree="$(git rev-parse --verify --quiet "${current_commit}^{tree}")" \
            || die "cannot resolve HEAD tree"
        ;;
    object)
        object_format="$(git rev-parse --show-object-format 2>/dev/null)" \
            || die "cannot determine Git object format"
        case "$object_format" in
            sha1) object_id_length=40 ;;
            sha256) object_id_length=64 ;;
            *) die "unsupported Git object format: $object_format" ;;
        esac
        case "$object_id" in
            *[!0-9A-Fa-f]*) die "invalid object ID" ;;
        esac
        if [ "${#object_id}" -ne "$object_id_length" ]; then
            die "invalid object ID length"
        fi
        current_commit="$(git rev-parse --verify --quiet "${object_id}^{commit}")" \
            || die "cannot resolve object ID as a commit"
        current_tree="$(git rev-parse --verify --quiet "${current_commit}^{tree}")" \
            || die "cannot resolve object snapshot tree"
        ;;
esac

base_blob=""
current_blob=""
snapshot_blob base "$base_tree" base_blob
snapshot_blob "$scope snapshot" "$current_tree" current_blob
base_version="$(version_from_blob base "$base_blob")"
current_version="$(version_from_blob "$scope snapshot" "$current_blob")"

if ! version_precedence="$(semver_precedence "$base_version" "$scope snapshot" "$current_version")"; then
    exit 2
fi

case "$version_precedence" in
    1)
        if [ "$scope" = "staged" ] && [ -n "$expected_tree" ]; then
            printf 'Validated staged tree: %s\n' "$current_tree"
        fi
        printf 'Workspace version bumped in %s snapshot: %s -> %s\n' \
            "$scope" "$base_version" "$current_version"
        ;;
    0)
        printf 'Workspace version must increase in %s snapshot from %s: %s -> %s has equal SemVer precedence\n' \
            "$scope" "$base_ref" "$base_version" "$current_version" >&2
        exit 1
        ;;
    -1)
        printf 'Workspace version must increase in %s snapshot from %s: %s is lower than base %s\n' \
            "$scope" "$base_ref" "$current_version" "$base_version" >&2
        exit 1
        ;;
    *) die "internal error: invalid SemVer precedence result: $version_precedence" ;;
esac
