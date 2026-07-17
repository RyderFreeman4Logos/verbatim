#!/usr/bin/env bash
set -euo pipefail

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 2
}

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "cannot determine the repository root"
cd "$repo_root"

checker="scripts/hooks/check-version-bumped.sh"
[ -x "$checker" ] || die "missing executable version checker: $checker"

object_format="$(git rev-parse --show-object-format 2>/dev/null)" \
    || die "cannot determine Git object format"
case "$object_format" in
    sha1) object_id_length=40 ;;
    sha256) object_id_length=64 ;;
    *) die "unsupported Git object format: $object_format" ;;
esac

is_zero_object_id() {
    local object_id="$1"

    [ "${#object_id}" -eq "$object_id_length" ] && [[ "$object_id" =~ ^0+$ ]]
}

is_object_id() {
    local object_id="$1"

    [ "${#object_id}" -eq "$object_id_length" ] && [[ "$object_id" =~ ^[0-9A-Fa-f]+$ ]]
}

input_seen=0
line_number=0
while IFS= read -r line || [ -n "$line" ]; do
    line_number=$((line_number + 1))
    input_seen=1
    local_ref=""
    local_object=""
    remote_ref=""
    remote_object=""
    extra=""
    read -r local_ref local_object remote_ref remote_object extra <<<"$line"
    if [ -z "$local_ref" ] || [ -z "$local_object" ] || [ -z "$remote_ref" ] || [ -z "$remote_object" ] \
        || [ -n "$extra" ]; then
        die "malformed pre-push reference input at line $line_number"
    fi
    if is_zero_object_id "$local_object"; then
        continue
    fi
    if ! is_object_id "$local_object"; then
        die "invalid pre-push local object ID at line $line_number"
    fi
    "$checker" --scope object --object "$local_object"
done

[ "$input_seen" -eq 1 ] || die "missing pre-push reference input"

VERSION_CHECK_TEST_SKIP_PRE_PUSH_PATH=1 just pre-commit head
