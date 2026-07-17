#!/usr/bin/env bash
set -euo pipefail

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

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "cannot determine the repository root"
cd "$repo_root"

if [ -z "$base_ref" ]; then
    base_ref="origin/${DEFAULT_BRANCH:-main}"
fi

case "$base_ref" in
    -*|*:*|*$'\n'*|*$'\r'*) die "invalid base ref: $base_ref" ;;
esac

if ! git rev-parse --verify --quiet "${base_ref}^{commit}" >/dev/null; then
    die "cannot resolve base ref: $base_ref"
fi

version_from_blob() {
    local label="$1"
    local object_spec="$2"
    local manifest
    local version

    if ! manifest="$(git cat-file blob "$object_spec" 2>/dev/null)"; then
        die "cannot read $label Cargo.toml blob: $object_spec"
    fi
    if ! version="$(
        printf '%s' "$manifest" \
            | python3 -c 'import sys, tomllib; print(tomllib.loads(sys.stdin.read())["workspace"]["package"]["version"])'
    )"; then
        die "cannot read workspace.package.version from $label Cargo.toml blob: $object_spec"
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
    staged) current_spec=":Cargo.toml" ;;
    head) current_spec="HEAD:Cargo.toml" ;;
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
        if ! object_commit="$(git rev-parse --verify --quiet "${object_id}^{commit}")"; then
            die "cannot resolve object ID as a commit"
        fi
        current_spec="${object_commit}:Cargo.toml"
        ;;
esac

base_version="$(version_from_blob base "${base_ref}:Cargo.toml")"
current_version="$(version_from_blob "$scope snapshot" "$current_spec")"

if ! version_precedence="$(semver_precedence "$base_version" "$scope snapshot" "$current_version")"; then
    exit 2
fi

case "$version_precedence" in
    1)
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
