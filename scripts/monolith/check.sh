#!/usr/bin/env bash
set -euo pipefail
readonly baseline_path="scripts/monolith/baseline.toml"
readonly line_threshold=800
readonly token_threshold=8000
readonly tokenizer_command="tokuin"
readonly tokenizer_version="0.3.0"
readonly tokenizer_revision="c68d1f804a4c172846716b7be99e9378e16512b7"
readonly tokenizer_model="gpt-4o"
readonly tokenizer_format="json"
readonly tokenizer_timeout_default=30
readonly tokenizer_timeout_max=300
readonly tokenizer_output_default=1048576
readonly tokenizer_output_max=1048576
readonly token_count_max=9223372036854775807
readonly tokenizer_known_answer_input="Verbatim tokenizer attestation v1"
readonly tokenizer_known_answer_tokens=7
usage() {
    cat >&2 <<'EOF'
Usage: scripts/monolith/check.sh --scope staged|head|object [--object <object-id>] [--base-ref <ref>] [--report-all]
Scopes:
  staged  evaluate the Git index against the trusted base tree
  head    evaluate the committed HEAD tree against the trusted base tree
  object  evaluate a committed object tree against the trusted base tree
The candidate baseline is always read from its selected Git snapshot. Its
policy is compared to the immutable trusted base before source bytes are
checked. The first baseline is accepted only when it exactly describes every
over-limit text file inherited unchanged from that trusted base.
Requires tokuin 0.3.0. MONOLITH_TOKENIZER_TIMEOUT_SECONDS may override the
30-second per-process timeout with an integer from 1 through 300. Tokenizer
timeouts, failures, and malformed output fail closed.
EOF
}
die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 2
}
scope=""
base_ref="${BASE_REF:-}"
object_id=""
report_all=false
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
        --report-all)
            report_all=true
            shift
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
    staged|head|object) ;;
    "") usage; die "--scope is required" ;;
    *) usage; die "--scope must be one of: staged, head, object" ;;
esac
if [ "$scope" = "object" ] && [ -z "$object_id" ]; then
    die "--scope object requires --object <object-id>"
fi
if [ "$scope" != "object" ] && [ -n "$object_id" ]; then
    die "--object is only valid with --scope object"
fi
repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "cannot determine the repository root"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
tokenizer_runner="$script_dir/tokenizer_runner.py"
tokenizer_contract="$script_dir/tokenizer_contract.py"
[ -r "$tokenizer_contract" ] || die "tokenizer contract helper is unavailable"
cd "$repo_root"
if [ -z "$base_ref" ]; then
    default_branch="${DEFAULT_BRANCH:-main}"
    if git rev-parse --verify --quiet "origin/${default_branch}^{commit}" >/dev/null; then
        base_ref="origin/${default_branch}"
    elif git rev-parse --verify --quiet "${default_branch}^{commit}" >/dev/null; then
        base_ref="$default_branch"
    else
        die "cannot resolve trusted base: origin/${default_branch} or ${default_branch}"
    fi
fi
case "$base_ref" in
    -*|*:*|*$'\n'*|*$'\r'*) die "invalid base ref: $base_ref" ;;
esac
base_commit="$(git rev-parse --verify --quiet "${base_ref}^{commit}")" \
    || die "cannot resolve trusted base ref: $base_ref"
candidate_commit=""
candidate_tree=""
if [ "$scope" = "staged" ]; then
    candidate_tree="$(git write-tree 2>/dev/null)" \
        || die "cannot capture staged index tree"
elif [ "$scope" = "head" ]; then
    candidate_commit="$(git rev-parse --verify --quiet 'HEAD^{commit}')" \
        || die "cannot resolve HEAD as a commit"
elif [ "$scope" = "object" ]; then
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
    [ "${#object_id}" -eq "$object_id_length" ] || die "invalid object ID length"
    candidate_commit="$(git rev-parse --verify --quiet "${object_id}^{commit}")" \
        || die "cannot resolve object ID as a commit"
fi
if [ -z "$candidate_tree" ]; then
    candidate_tree="$(git rev-parse --verify --quiet "${candidate_commit}^{tree}")" \
        || die "cannot resolve candidate tree"
fi
tmp_root="$(mktemp -d)"
cleanup() {
    rm -rf -- "$tmp_root"
}
trap cleanup EXIT
tokenizer_timeout_seconds="${MONOLITH_TOKENIZER_TIMEOUT_SECONDS:-$tokenizer_timeout_default}"
case "$tokenizer_timeout_seconds" in
    ''|*[!0-9]*)
        die "invalid MONOLITH_TOKENIZER_TIMEOUT_SECONDS: expected an integer from 1 through $tokenizer_timeout_max"
        ;;
esac
if [ "${#tokenizer_timeout_seconds}" -gt 3 ] \
    || [ "$tokenizer_timeout_seconds" -lt 1 ] \
    || [ "$tokenizer_timeout_seconds" -gt "$tokenizer_timeout_max" ]; then
    die "invalid MONOLITH_TOKENIZER_TIMEOUT_SECONDS: expected an integer from 1 through $tokenizer_timeout_max"
fi
tokenizer_output_max_bytes="${MONOLITH_TOKENIZER_MAX_OUTPUT_BYTES:-$tokenizer_output_default}"
case "$tokenizer_output_max_bytes" in
    ''|*[!0-9]*)
        die "invalid MONOLITH_TOKENIZER_MAX_OUTPUT_BYTES: expected an integer from 1 through $tokenizer_output_max"
        ;;
esac
if [ "${#tokenizer_output_max_bytes}" -gt 7 ] \
    || [ "$tokenizer_output_max_bytes" -lt 1 ] \
    || [ "$tokenizer_output_max_bytes" -gt "$tokenizer_output_max" ]; then
    die "invalid MONOLITH_TOKENIZER_MAX_OUTPUT_BYTES: expected an integer from 1 through $tokenizer_output_max"
fi
if ! tokenizer_bin="$(command -v "$tokenizer_command" 2>/dev/null)"; then
    die "tokenizer unavailable: required tokuin $tokenizer_version in PATH"
fi
tokenizer_bin="$(realpath -- "$tokenizer_bin" 2>/dev/null)" \
    || die "cannot resolve tokenizer executable identity"
[ -x "$tokenizer_bin" ] || die "resolved tokenizer is not executable: $tokenizer_bin"
run_bounded() {
    local action="$1"
    local stdout_file="$2"
    local stderr_file="$3"
    local receipt_file receipt_fields
    shift 3
    receipt_file="$(mktemp "$tmp_root/tokenizer-receipt.XXXXXX.json")" \
        || die "cannot create tokenizer receipt file"
    bounded_outcome=runner_error
    bounded_status=2
    bounded_output_stream=""
    bounded_value=""
    bounded_protocol_error=""
    if ! python3 -B "$tokenizer_runner" "$action" \
        --timeout-seconds "$tokenizer_timeout_seconds" \
        --max-output-bytes "$tokenizer_output_max_bytes" \
        --stdout "$stdout_file" \
        --stderr "$stderr_file" \
        --receipt "$receipt_file" "$@"; then
        return 2
    fi
    if ! receipt_fields="$(python3 -B "$tokenizer_runner" decode \
        --receipt "$receipt_file")"; then
        return 2
    fi
    IFS=$'\t' read -r bounded_outcome bounded_status bounded_output_stream \
        bounded_value bounded_protocol_error <<<"$receipt_fields"
    [ "$bounded_status" != - ] || bounded_status=""
    [ "$bounded_output_stream" != - ] || bounded_output_stream=""
    [ "$bounded_value" != - ] || bounded_value=""
    [ "$bounded_protocol_error" != - ] || bounded_protocol_error=""
    case "$bounded_outcome" in
        exited) return "${bounded_status:-2}" ;;
        interrupted) return "${bounded_status:-130}" ;;
        timed_out|output_limit|orphaned_descendants|spawn_failed|protocol_failed|cleanup_failed)
            return 125
            ;;
        *)
            bounded_outcome=runner_error
            bounded_status=2
            return 2
            ;;
    esac
}
report_bounded_failure() {
    local context="$1" stderr_file="$2"
    case "$bounded_outcome" in
        timed_out) die "tokenizer $context timed out after ${tokenizer_timeout_seconds}s" ;;
        output_limit)
            die "tokenizer $context output exceeded ${tokenizer_output_max_bytes}-byte limit on $bounded_output_stream"
            ;;
        interrupted)
            printf 'ERROR: tokenizer %s interrupted\n' "$context" >&2
            exit "${bounded_status:-130}"
            ;;
        orphaned_descendants) die "tokenizer $context left descendant processes" ;;
        spawn_failed) die "tokenizer $context could not be spawned" ;;
        protocol_failed)
            die "tokenizer $context protocol failure: ${bounded_protocol_error:-unknown}"
            ;;
        cleanup_failed) die "tokenizer $context process cleanup failed" ;;
        runner_error) die "tokenizer $context runner failed" ;;
    esac
    [ ! -s "$stderr_file" ] || sed 's/^/  /' "$stderr_file" >&2
}
version_stdout="$tmp_root/tokenizer-version.stdout"
version_stderr="$tmp_root/tokenizer-version.stderr"
if run_bounded version "$version_stdout" "$version_stderr" \
    --executable "$tokenizer_bin" --expected-version "$tokenizer_version"; then
    :
else
    tokenizer_status=$?
    report_bounded_failure version "$version_stderr"
    die "tokenizer version check failed with status $tokenizer_status"
fi
known_answer_file="$tmp_root/tokenizer-known-answer.txt"
printf '%s' "$tokenizer_known_answer_input" >"$known_answer_file"
known_answer_stdout="$tmp_root/tokenizer-known-answer.stdout"
known_answer_stderr="$tmp_root/tokenizer-known-answer.stderr"
if run_bounded estimate "$known_answer_stdout" "$known_answer_stderr" \
    --executable "$tokenizer_bin" --model "$tokenizer_model" \
    --input "$known_answer_file" --maximum-count "$token_count_max" \
    --expected-tokens "$tokenizer_known_answer_tokens"; then
    :
else
    tokenizer_status=$?
    report_bounded_failure known-answer "$known_answer_stderr"
    die "tokenizer known-answer attestation failed with status $tokenizer_status"
fi
printf 'Tokenizer attestation passed: tokuin %s model=%s tokens=%s\n' \
    "$tokenizer_version" "$tokenizer_model" "$bounded_value"
snapshot_entry() {
    local snapshot="$1"
    local path="$2"
    local mode_name="$3"
    local object_name="$4"
    local entries_file entry="" matched_entry="" metadata entry_path
    local entry_mode entry_type entry_id trailing treeish count=0
    entries_file="$(mktemp "$tmp_root/entries.XXXXXX")" \
        || die "cannot create snapshot entry list for $path"
    case "$snapshot" in
        candidate) treeish="$candidate_tree" ;;
        base) treeish="$base_commit" ;;
        *) die "internal error: unknown snapshot $snapshot" ;;
    esac
    git ls-tree -z "$treeish" -- ":(top,literal)$path" >"$entries_file"
    while IFS= read -r -d '' entry; do
        count=$((count + 1))
        matched_entry="$entry"
    done <"$entries_file"
    [ "$count" -gt 0 ] || return 1
    [ "$count" -eq 1 ] || die "snapshot has multiple entries for $path"
    entry="$matched_entry"
    case "$entry" in
        *$'\t'*) ;;
        *) die "malformed Git tree entry for $path" ;;
    esac
    metadata="${entry%%$'\t'*}"
    entry_path="${entry#*$'\t'}"
    [ "$entry_path" = "$path" ] \
        || die "Git tree entry path mismatch for $path"
    read -r entry_mode entry_type entry_id trailing <<<"$metadata"
    [ -n "$entry_mode" ] && [ -n "$entry_type" ] \
        && [ -n "$entry_id" ] && [ -z "$trailing" ] \
        || die "malformed Git tree metadata for $path"
    case "$entry_id" in
        *[!0-9A-Fa-f]*|'') die "invalid Git object ID for $path" ;;
    esac
    case "$entry_mode:$entry_type" in
        100644:blob|100755:blob|120000:blob|160000:commit) ;;
        *) die "unsupported Git mode/type for $path: $entry_mode/$entry_type" ;;
    esac
    printf -v "$mode_name" '%s' "$entry_mode"
    printf -v "$object_name" '%s' "$entry_id"
}
materialize_snapshot() {
    local snapshot="$1"
    local path="$2"
    local output_name="$3"
    local snapshot_file mode blob_id
    snapshot_entry "$snapshot" "$path" mode blob_id || return 1
    case "$mode" in
        100644|100755) ;;
        *) return 1 ;;
    esac
    snapshot_file="$(mktemp "$tmp_root/blob.XXXXXX")" \
        || die "cannot create snapshot file for $path"
    if ! git cat-file blob "$blob_id" >"$snapshot_file"; then
        die "cannot materialize $snapshot snapshot blob for $path ($blob_id)"
    fi
    printf -v "$output_name" '%s' "$snapshot_file"
}
parse_policy() {
    local label="$1"
    local file="$2"
    python3 "$tokenizer_contract" policy \
        --label "$label" --policy "$file" \
        --command "$tokenizer_command" --version "$tokenizer_version" \
        --revision "$tokenizer_revision" --model "$tokenizer_model" \
        --output-format "$tokenizer_format" \
        --timeout "$tokenizer_timeout_default" \
        --max-output-bytes "$tokenizer_output_default" \
        --known-answer-input "$tokenizer_known_answer_input" \
        --known-answer-tokens "$tokenizer_known_answer_tokens" \
        --maximum-count "$token_count_max"
}
classify_kind() {
    local path="$1"
    case "$path" in
        *_tests.rs|*_test.rs|*_tests_*.rs|tests/*.rs|*/tests/*.rs|*/benches/*.rs)
            printf 'test\n'
            ;;
        *.md|*.markdown|*.mdx|*.txt|*.rst)
            printf 'doc\n'
            ;;
        *.toml|*.yml|*.yaml|*.json|*.jsonc|*.ini|*.cfg|*.ron)
            printf 'config\n'
            ;;
        *.rs|*.sh|*.bash|*.zsh|*.py|*.ts|*.tsx|*.js|*.jsx|*.go|*.proto|*.c|*.h|*.cpp|*.hpp|*.sql|*.nix|Dockerfile|Makefile|justfile)
            printf 'source\n'
            ;;
        *)
            printf 'other\n'
            ;;
    esac
}
is_generated_artifact() {
    local path="$1"
    case "$path" in
        Cargo.lock) return 0 ;;
        *) return 1 ;;
    esac
}
validate_count_domain() {
    local value="$1" label="$2"
    # Equal-width ASCII decimal strings are compared lexically to avoid signed
    # shell-arithmetic overflow at 9223372036854775808.
    # shellcheck disable=SC2071
    if [ "${#value}" -gt 19 ] \
        || { [ "${#value}" -eq 19 ] \
            && (LC_ALL=C; [[ "$value" > "$token_count_max" ]]); }; then
        die "$label exceeds the signed 64-bit domain: $value"
    fi
}
line_count() {
    local file="$1"
    local path="$2"
    local lines
    if ! lines="$(awk 'END { print NR + 0 }' "$file" 2>/dev/null)"; then
        die "failed to count lines for $path"
    fi
    lines="$(printf '%s' "$lines" | tr -d '[:space:]')"
    case "$lines" in
        ''|*[!0-9]*) die "unparsable line count for $path: $lines" ;;
    esac
    validate_count_domain "$lines" "line count for $path"
    printf '%s\n' "$lines"
}
byte_count() {
    local file="$1"
    local path="$2"
    local bytes
    if ! bytes="$(wc -c <"$file" 2>/dev/null)"; then
        die "failed to count bytes for $path"
    fi
    bytes="$(printf '%s' "$bytes" | tr -d '[:space:]')"
    case "$bytes" in
        ''|*[!0-9]*) die "unparsable byte count for $path: $bytes" ;;
    esac
    validate_count_domain "$bytes" "byte count for $path"
    printf '%s\n' "$bytes"
}
token_count() {
    local file="$1"
    local path="$2"
    local stdout_file stderr_file tokenizer_status
    stdout_file="$(mktemp "$tmp_root/tokenizer.stdout.XXXXXX")" \
        || die "cannot create tokenizer stdout file for $path"
    stderr_file="$(mktemp "$tmp_root/tokenizer.stderr.XXXXXX")" \
        || die "cannot create tokenizer stderr file for $path"
    if run_bounded estimate "$stdout_file" "$stderr_file" \
        --executable "$tokenizer_bin" --model "$tokenizer_model" \
        --input "$file" --maximum-count "$token_count_max"; then
        printf '%s\n' "$bounded_value"
        return 0
    else
        tokenizer_status=$?
    fi
    case "$bounded_outcome" in
        timed_out)
            die "tokenizer timed out after ${tokenizer_timeout_seconds}s for $path"
            ;;
        output_limit)
            die "tokenizer output exceeded ${tokenizer_output_max_bytes}-byte limit for $path ($bounded_output_stream)"
            ;;
        interrupted)
            printf 'ERROR: tokenizer interrupted for %s\n' "$path" >&2
            exit "${bounded_status:-130}"
            ;;
        orphaned_descendants) die "tokenizer left descendant processes for $path" ;;
        spawn_failed) die "tokenizer could not be spawned for $path" ;;
        protocol_failed)
            die "tokenizer protocol failure for $path: ${bounded_protocol_error:-unknown}"
            ;;
        cleanup_failed) die "tokenizer process cleanup failed for $path" ;;
        runner_error) die "tokenizer runner failed for $path" ;;
    esac
    printf 'ERROR: tokenizer failed for %s using %s estimate --model %s --format %s (status %s)\n' \
        "$path" "$tokenizer_bin" "$tokenizer_model" "$tokenizer_format" \
        "$tokenizer_status" >&2
    [ ! -s "$stderr_file" ] || sed 's/^/  /' "$stderr_file" >&2
    exit 2
}
declare -A candidate_kind=()
declare -A candidate_tokens=()
declare -A candidate_lines=()
declare -A candidate_issue=()
declare -A candidate_rationale=()
declare -A base_tokens=()
declare -A base_lines=()
declare -A base_issue=()
declare -A base_rationale=()
declare -A actual_text=()
declare -A actual_kind=()
declare -A actual_tokens=()
declare -A actual_lines=()
declare -A trusted_text=()
declare -A trusted_kind=()
declare -A trusted_tokens=()
declare -A trusted_lines=()
candidate_manifest=""
materialize_snapshot candidate "$baseline_path" candidate_manifest \
    || die "candidate snapshot is missing required baseline: $baseline_path"
candidate_tsv="$tmp_root/candidate-baseline.tsv"
parse_policy candidate "$candidate_manifest" >"$candidate_tsv"
while IFS=$'\t' read -r path kind tokens lines issue rationale; do
    [ -n "$path" ] || continue
    candidate_kind["$path"]="$kind"
    candidate_tokens["$path"]="$tokens"
    candidate_lines["$path"]="$lines"
    candidate_issue["$path"]="$issue"
    candidate_rationale["$path"]="$rationale"
done <"$candidate_tsv"
base_manifest=""
bootstrap=false
if materialize_snapshot base "$baseline_path" base_manifest; then
    base_tsv="$tmp_root/base-baseline.tsv"
    parse_policy trusted-base "$base_manifest" >"$base_tsv"
    while IFS=$'\t' read -r path kind tokens lines issue rationale; do
        [ -n "$path" ] || continue
        base_tokens["$path"]="$tokens"
        base_lines["$path"]="$lines"
        base_issue["$path"]="$issue"
        base_rationale["$path"]="$rationale"
    done <"$base_tsv"
else
    bootstrap=true
fi
measure_candidate() {
    local path="$1"
    local file lines bytes tokens kind
    if [ -n "${actual_text[$path]+set}" ]; then
        return 0
    fi
    if ! materialize_snapshot candidate "$path" file; then
        actual_text["$path"]="0"
        return 0
    fi
    if ! grep -Iq '' "$file" 2>/dev/null; then
        actual_text["$path"]="0"
        return 0
    fi
    actual_text["$path"]="1"
    lines="$(line_count "$file" "$path")"
    bytes="$(byte_count "$file" "$path")"
    tokens=0
    if [ -n "${candidate_tokens[$path]+set}" ] \
        || [ "$bytes" -gt "$token_threshold" ] \
        || [ "$lines" -gt "$line_threshold" ]; then
        tokens="$(token_count "$file" "$path")"
    fi
    kind="$(classify_kind "$path")"
    actual_kind["$path"]="$kind"
    actual_lines["$path"]="$lines"
    actual_tokens["$path"]="$tokens"
}
measure_trusted_base() {
    local path="$1"
    local file lines bytes tokens kind
    if [ -n "${trusted_text[$path]+set}" ]; then
        return 0
    fi
    if ! materialize_snapshot base "$path" file; then
        trusted_text["$path"]="0"
        return 0
    fi
    if ! grep -Iq '' "$file" 2>/dev/null; then
        trusted_text["$path"]="0"
        return 0
    fi
    trusted_text["$path"]="1"
    lines="$(line_count "$file" "$path")"
    bytes="$(byte_count "$file" "$path")"
    tokens=0
    if [ "$bytes" -gt "$token_threshold" ] \
        || [ "$lines" -gt "$line_threshold" ]; then
        tokens="$(token_count "$file" "$path")"
    fi
    kind="$(classify_kind "$path")"
    trusted_kind["$path"]="$kind"
    trusted_lines["$path"]="$lines"
    trusted_tokens["$path"]="$tokens"
}
is_over_limit() {
    local path="$1"
    [ "${actual_text[$path]:-0}" = "1" ] \
        && { [ "${actual_lines[$path]}" -gt "$line_threshold" ] \
            || [ "${actual_tokens[$path]}" -gt "$token_threshold" ]; }
}
is_trusted_over_limit() {
    local path="$1"
    [ "${trusted_text[$path]:-0}" = "1" ] \
        && { [ "${trusted_lines[$path]}" -gt "$line_threshold" ] \
            || [ "${trusted_tokens[$path]}" -gt "$token_threshold" ]; }
}
candidate_paths_file="$tmp_root/candidate-paths.zlist"
git diff --name-only -z --diff-filter=ACMRT \
    "$base_commit" "$candidate_tree" -- >"$candidate_paths_file" \
    || die "cannot enumerate candidate snapshot paths"
declare -A paths_to_check=()
while IFS= read -r -d '' path; do
    if is_generated_artifact "$path"; then
        continue
    fi
    paths_to_check["$path"]=1
done <"$candidate_paths_file"
for path in "${!candidate_tokens[@]}"; do
    paths_to_check["$path"]=1
done
declare -a policy_failures=()
for path in "${!candidate_tokens[@]}"; do
    measure_candidate "$path"
    if [ "${actual_text[$path]:-0}" != "1" ]; then
        policy_failures+=("BLOCK baseline policy: entry path is missing or not text: $path")
        continue
    fi
    if [ "${candidate_kind[$path]}" != "${actual_kind[$path]}" ]; then
        policy_failures+=("BLOCK baseline policy: $path kind ${candidate_kind[$path]} does not match ${actual_kind[$path]}")
    fi
done
if [ "$bootstrap" = true ]; then
    declare -A expected_bootstrap_paths=()
    base_paths_file="$tmp_root/base-paths.zlist"
    git ls-tree -r -z --name-only "$base_commit" >"$base_paths_file"
    while IFS= read -r -d '' path; do
        if is_generated_artifact "$path"; then
            continue
        fi
        measure_trusted_base "$path"
        if is_trusted_over_limit "$path"; then
            expected_bootstrap_paths["$path"]=1
            if [ -z "${candidate_tokens[$path]+set}" ]; then
                policy_failures+=("BLOCK baseline bootstrap: missing required row for $path")
                continue
            fi
            if [ "${candidate_kind[$path]}" != "${trusted_kind[$path]}" ]; then
                policy_failures+=("BLOCK baseline bootstrap: $path kind must match trusted-base ${trusted_kind[$path]}")
            fi
            if [ "${candidate_tokens[$path]}" -ne "${trusted_tokens[$path]}" ] \
                || [ "${candidate_lines[$path]}" -ne "${trusted_lines[$path]}" ]; then
                policy_failures+=("BLOCK baseline bootstrap: $path bounds must exactly match trusted-base ${trusted_tokens[$path]} tokens/${trusted_lines[$path]} lines")
            fi
        fi
    done <"$base_paths_file"
    for path in "${!candidate_tokens[@]}"; do
        if [ -z "${expected_bootstrap_paths[$path]+set}" ]; then
            measure_trusted_base "$path"
            measure_candidate "$path"
            if is_over_limit "$path" \
                && [ "${trusted_text[$path]:-0}" != "1" ]; then
                policy_failures+=("BLOCK baseline bootstrap: candidate-only oversized file $path")
            else
                policy_failures+=("BLOCK baseline bootstrap: new exemption for non-oversized trusted-base path $path")
            fi
        fi
    done
else
    for path in "${!candidate_tokens[@]}"; do
        if [ -z "${base_tokens[$path]+set}" ]; then
            policy_failures+=("BLOCK baseline policy: new exemption row for $path")
        fi
    done
    for path in "${!base_tokens[@]}"; do
        measure_candidate "$path"
        over_limit=false
        if is_over_limit "$path"; then
            over_limit=true
        fi
        if [ -z "${candidate_tokens[$path]+set}" ]; then
            if [ "$over_limit" = true ]; then
                policy_failures+=("BLOCK baseline policy: required row removed while $path remains oversized")
            fi
            continue
        fi
        if [ "${candidate_issue[$path]}" != "${base_issue[$path]}" ] \
            || [ "${candidate_rationale[$path]}" != "${base_rationale[$path]}" ]; then
            policy_failures+=("BLOCK baseline policy: required issue/rationale changed for $path")
        fi
        if [ "$over_limit" = true ]; then
            if [ "${candidate_tokens[$path]}" -ne "${base_tokens[$path]}" ] \
                || [ "${candidate_lines[$path]}" -ne "${base_lines[$path]}" ]; then
                policy_failures+=("BLOCK baseline policy: cap changed while $path remains oversized")
            fi
        elif [ "${candidate_tokens[$path]}" -gt "${base_tokens[$path]}" ] \
            || [ "${candidate_lines[$path]}" -gt "${base_lines[$path]}" ]; then
            policy_failures+=("BLOCK baseline policy: cap increased for $path")
        fi
    done
fi
declare -a failures=()
declare -a warnings=()
checked=0
for path in "${!paths_to_check[@]}"; do
    measure_candidate "$path"
    [ "${actual_text[$path]:-0}" = "1" ] || continue
    checked=$((checked + 1))
    if [ -n "${candidate_tokens[$path]+set}" ]; then
        if [ "${actual_tokens[$path]}" -gt "${candidate_tokens[$path]}" ] \
            || [ "${actual_lines[$path]}" -gt "${candidate_lines[$path]}" ]; then
            failures+=("BLOCK ratchet: $path (${actual_kind[$path]}) grew to ${actual_tokens[$path]} tokens/${actual_lines[$path]} lines; baseline cap ${candidate_tokens[$path]} tokens/${candidate_lines[$path]} lines; issue #${candidate_issue[$path]}")
        elif [ "$report_all" = true ]; then
            warnings+=("WARNING baseline debt: $path (${actual_kind[$path]}) is within cap at ${actual_tokens[$path]} tokens/${actual_lines[$path]} lines; issue #${candidate_issue[$path]}")
        fi
    elif is_over_limit "$path"; then
        failures+=("BLOCK new monolith: $path (${actual_kind[$path]}) exceeds ${line_threshold} lines/${token_threshold} tokens at ${actual_tokens[$path]} tokens/${actual_lines[$path]} lines")
    fi
done
if [ "$scope" = "staged" ]; then
    final_index_tree="$(git write-tree 2>/dev/null)" \
        || die "cannot capture final staged index tree"
    [ "$final_index_tree" = "$candidate_tree" ] \
        || die "Git index changed while monolith gate was running"
fi
printf '=== Monolith No-Growth Gate ===\n'
printf 'Scope: %s\n' "$scope"
printf 'Trusted base: %s\n' "$base_ref"
printf 'Thresholds: %s lines, %s tokens (model: %s)\n' \
    "$line_threshold" "$token_threshold" "$tokenizer_model"
printf 'Candidate baseline rows: %s\n' "${#candidate_tokens[@]}"
printf 'Checked text files: %s\n' "$checked"
if [ "${#policy_failures[@]}" -gt 0 ]; then
    printf '\nPolicy failures:\n'
    printf '%s\n' "${policy_failures[@]}"
fi
if [ "${#failures[@]}" -gt 0 ]; then
    printf '\nHard failures:\n'
    printf '%s\n' "${failures[@]}"
fi
if [ "${#warnings[@]}" -gt 0 ]; then
    printf '\nWarnings:\n'
    printf '%s\n' "${warnings[@]}"
fi
printf '\nSummary: %s policy failure(s), %s hard failure(s), %s warning(s)\n' \
    "${#policy_failures[@]}" "${#failures[@]}" "${#warnings[@]}"
if [ "${#policy_failures[@]}" -gt 0 ] || [ "${#failures[@]}" -gt 0 ]; then
    exit 1
fi
