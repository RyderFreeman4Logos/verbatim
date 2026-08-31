#!/usr/bin/env bash
set -euo pipefail
export GIT_NO_REPLACE_OBJECTS=1

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 2
}

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "cannot determine the repository root"
cd "$repo_root"

version_checker="scripts/hooks/check-version-bumped.sh"
monolith_checker="scripts/monolith/check.sh"
[ -x "$version_checker" ] || die "missing executable version checker: $version_checker"
[ -x "$monolith_checker" ] || die "missing executable monolith checker: $monolith_checker"

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
    "$version_checker" --scope object --object "$local_object"
    "$monolith_checker" --scope object --object "$local_object"
done

[ "$input_seen" -eq 1 ] || die "missing pre-push reference input"

"$version_checker" --scope head
"$monolith_checker" --scope head

full_gate_receipt="${VERBATIM_FULL_GATE_RECEIPT:-}"
[ -n "$full_gate_receipt" ] || die "missing full-gate receipt"
[ -f "$full_gate_receipt" ] || die "missing full-gate receipt: $full_gate_receipt"

receipt_has_line() {
    local expected="$1"
    local count

    count="$(grep -Fxc -- "$expected" "$full_gate_receipt" || true)"
    [ "$count" = 1 ]
}

head="$(git rev-parse HEAD)" || die "cannot determine HEAD"
tree="$(git rev-parse 'HEAD^{tree}')" || die "cannot determine HEAD tree"
receipt_has_line "PRE_HEAD=$head" || die "full-gate receipt does not match HEAD"
receipt_has_line "PRE_TREE=$tree" || die "full-gate receipt does not match HEAD tree"
receipt_has_line 'PRE_ATTESTATION=PASS' || die "full-gate receipt lacks pre-attestation"
receipt_has_line 'INNER_GATE_EXIT=0' || die "full-gate receipt inner gate did not pass"
receipt_has_line 'POST_ATTESTATION=PASS' || die "full-gate receipt lacks post-attestation"
receipt_has_line 'GATE_EXIT=0' || die "full-gate receipt gate_exit did not pass"
printf 'pre-push: attested full-gate receipt for HEAD %s\n' "$head"
