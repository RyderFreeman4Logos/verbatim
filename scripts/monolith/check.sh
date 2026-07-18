#!/usr/bin/env bash
set -euo pipefail
readonly baseline_path="scripts/monolith/baseline.toml"
readonly line_threshold=800
readonly token_threshold=8000
readonly tokenizer_command="tokuin"
readonly tokenizer_version="0.3.0"
readonly tokenizer_revision="c68d1f804a4c172846716b7be99e9378e16512b7"
readonly tokenizer_model="gpt-4o"
readonly tokenizer_timeout_default=30
readonly tokenizer_timeout_max=300
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
if [ "$scope" = "head" ]; then
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
if [ "$tokenizer_timeout_seconds" -lt 1 ] \
    || [ "$tokenizer_timeout_seconds" -gt "$tokenizer_timeout_max" ]; then
    die "invalid MONOLITH_TOKENIZER_TIMEOUT_SECONDS: expected an integer from 1 through $tokenizer_timeout_max"
fi
if ! tokenizer_bin="$(command -v "$tokenizer_command" 2>/dev/null)"; then
    die "tokenizer unavailable: required tokuin $tokenizer_version in PATH"
fi
run_bounded() {
    local stdout_file="$1"
    local stderr_file="$2"
    shift 2
    python3 "$tokenizer_runner" \
        "$tokenizer_timeout_seconds" "$stdout_file" "$stderr_file" "$@"
}
version_stdout="$tmp_root/tokenizer-version.stdout"
version_stderr="$tmp_root/tokenizer-version.stderr"
if run_bounded "$version_stdout" "$version_stderr" "$tokenizer_bin" --version; then
    tokenizer_version_output="$(<"$version_stdout")"
else
    tokenizer_status=$?
    if [ "$tokenizer_status" -eq 124 ]; then
        die "tokenizer version check timed out after ${tokenizer_timeout_seconds}s"
    fi
    [ ! -s "$version_stderr" ] || sed 's/^/  /' "$version_stderr" >&2
    die "tokenizer version check failed with status $tokenizer_status"
fi
if [ "$tokenizer_version_output" != "tokuin $tokenizer_version" ]; then
    die "tokenizer version mismatch: required tokuin $tokenizer_version, got $tokenizer_version_output"
fi
snapshot_mode() {
    local snapshot="$1"
    local path="$2"
    local output_name="$3"
    local entries_file entry entry_mode="" count=0
    entries_file="$(mktemp "$tmp_root/modes.XXXXXX")" \
        || die "cannot create mode list for $path"
    case "$snapshot" in
        candidate)
            case "$scope" in
                staged) git ls-files --stage -z -- "$path" >"$entries_file" ;;
                head|object) git ls-tree -z "$candidate_commit" -- "$path" >"$entries_file" ;;
            esac
            ;;
        base) git ls-tree -z "$base_commit" -- "$path" >"$entries_file" ;;
        *) die "internal error: unknown snapshot $snapshot" ;;
    esac
    while IFS= read -r -d '' entry; do
        count=$((count + 1))
        entry_mode="${entry%% *}"
    done <"$entries_file"
    [ "$count" -gt 0 ] || return 1
    [ "$count" -eq 1 ] || die "snapshot has multiple index entries for $path"
    case "$entry_mode" in
        100644|100755|120000|160000) ;;
        *) die "unsupported Git mode for $path: $entry_mode" ;;
    esac
    printf -v "$output_name" '%s' "$entry_mode"
}
materialize_snapshot() {
    local snapshot="$1"
    local path="$2"
    local output_name="$3"
    local object_spec object_type snapshot_file mode
    snapshot_mode "$snapshot" "$path" mode || return 1
    case "$mode" in
        100644|100755) ;;
        *) return 1 ;;
    esac
    case "$snapshot" in
        candidate)
            case "$scope" in
                staged) object_spec=":$path" ;;
                head|object) object_spec="${candidate_commit}:$path" ;;
            esac
            ;;
        base) object_spec="${base_commit}:$path" ;;
        *) die "internal error: unknown snapshot $snapshot" ;;
    esac
    if ! object_type="$(git cat-file -t "$object_spec" 2>/dev/null)"; then
        return 1
    fi
    [ "$object_type" = "blob" ] || return 1
    snapshot_file="$(mktemp "$tmp_root/blob.XXXXXX")" \
        || die "cannot create snapshot file for $path"
    if ! git cat-file blob "$object_spec" >"$snapshot_file"; then
        die "cannot materialize $snapshot snapshot blob for $path ($object_spec)"
    fi
    printf -v "$output_name" '%s' "$snapshot_file"
}
parse_policy() {
    local label="$1"
    local file="$2"
    python3 - "$label" "$file" \
        "$tokenizer_command" "$tokenizer_version" "$tokenizer_revision" \
        "$tokenizer_model" "$tokenizer_timeout_default" <<'PY'
import sys
import tomllib
from pathlib import PurePosixPath
label, policy_path, command, version, revision, model, timeout = sys.argv[1:]
required = {"path", "kind", "baseline_tokens", "baseline_lines", "issue", "rationale"}
allowed_kinds = {"source", "test", "doc", "config", "other"}
expected_tokenizer = {
    "command": command,
    "version": version,
    "revision": revision,
    "model": model,
    "timeout_seconds": int(timeout),
}

def fail(message: str) -> None:
    print(f"ERROR: {label} baseline policy: {message}", file=sys.stderr)
    raise SystemExit(2)

try:
    with open(policy_path, "rb") as fh:
        data = tomllib.load(fh)
except Exception as exc:
    fail(f"cannot parse TOML: {exc}")
if not isinstance(data, dict):
    fail("top level must be a table")
unknown_top_level = set(data) - {"files", "tokenizer"}
if unknown_top_level:
    fail(f"unknown top-level key(s): {', '.join(sorted(unknown_top_level))}")
tokenizer = data.get("tokenizer")
if not isinstance(tokenizer, dict):
    fail("tokenizer must be a table")
unknown_tokenizer = set(tokenizer) - set(expected_tokenizer)
missing_tokenizer = set(expected_tokenizer) - set(tokenizer)
if unknown_tokenizer:
    fail(f"tokenizer has unknown key(s): {', '.join(sorted(unknown_tokenizer))}")
if missing_tokenizer:
    fail(f"tokenizer is missing key(s): {', '.join(sorted(missing_tokenizer))}")
for key, expected in expected_tokenizer.items():
    if tokenizer[key] != expected:
        fail(f"tokenizer.{key} must be {expected!r}, got {tokenizer[key]!r}")
entries = data.get("files")
if not isinstance(entries, list):
    fail("files must be an array of tables")
seen: set[str] = set()
for number, entry in enumerate(entries, start=1):
    if not isinstance(entry, dict):
        fail(f"entry #{number} must be a table")
    unknown = set(entry) - required
    missing = required - set(entry)
    if unknown:
        fail(f"entry #{number} has unknown key(s): {', '.join(sorted(unknown))}")
    if missing:
        fail(f"entry #{number} is missing key(s): {', '.join(sorted(missing))}")
    path = entry["path"]
    kind = entry["kind"]
    tokens = entry["baseline_tokens"]
    lines = entry["baseline_lines"]
    issue = entry["issue"]
    rationale = entry["rationale"]
    if not isinstance(path, str) or not path:
        fail(f"entry #{number} has missing or invalid path")
    if any(character in path for character in ("\0", "\n", "\r", "\t")):
        fail(f"entry #{number} path contains control whitespace")
    path_object = PurePosixPath(path)
    if path_object.is_absolute() or ".." in path_object.parts or path.startswith("./") or "//" in path:
        fail(f"entry #{number} has non-canonical path: {path!r}")
    if path in seen:
        fail(f"duplicate path: {path}")
    seen.add(path)
    if kind not in allowed_kinds:
        fail(f"entry for {path} has invalid kind: {kind!r}")
    if not isinstance(tokens, int) or isinstance(tokens, bool) or tokens < 0:
        fail(f"entry for {path} has invalid baseline_tokens")
    if not isinstance(lines, int) or isinstance(lines, bool) or lines < 0:
        fail(f"entry for {path} has invalid baseline_lines")
    if issue != "368":
        fail(f"entry for {path} must use canonical issue = \"368\"")
    if not isinstance(rationale, str) or not rationale.strip():
        fail(f"entry for {path} has missing rationale")
    if "\t" in rationale or "\n" in rationale or "\r" in rationale:
        fail(f"entry for {path} rationale contains unsupported control whitespace")
    print(f"{path}\t{kind}\t{tokens}\t{lines}\t{issue}\t{rationale}")
PY
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
    printf '%s\n' "$bytes"
}
token_count() {
    local file="$1"
    local path="$2"
    local output stdout_file stderr_file tokenizer_status
    stdout_file="$(mktemp "$tmp_root/tokenizer.stdout.XXXXXX")" \
        || die "cannot create tokenizer stdout file for $path"
    stderr_file="$(mktemp "$tmp_root/tokenizer.stderr.XXXXXX")" \
        || die "cannot create tokenizer stderr file for $path"
    if run_bounded "$stdout_file" "$stderr_file" \
        "$tokenizer_bin" estimate --model "$tokenizer_model" --format json "$file"; then
        output="$(<"$stdout_file")"
    else
        tokenizer_status=$?
        if [ "$tokenizer_status" -eq 124 ]; then
            printf 'ERROR: tokenizer timed out after %ss for %s\n' \
                "$tokenizer_timeout_seconds" "$path" >&2
            exit 2
        fi
        printf 'ERROR: tokenizer failed for %s using %s estimate --model %s --format json (status %s)\n' \
            "$path" "$tokenizer_bin" "$tokenizer_model" "$tokenizer_status" >&2
        if [ -s "$stderr_file" ]; then
            sed 's/^/  /' "$stderr_file" >&2
        fi
        exit 2
    fi
    if ! TOKEN_OUTPUT="$output" python3 - <<'PY'
import json
import os
import sys
try:
    value = json.loads(os.environ["TOKEN_OUTPUT"])
except Exception as exc:
    print(f"unparsable tokenizer JSON: {exc}", file=sys.stderr)
    raise SystemExit(2)
tokens = value.get("tokens", value.get("total")) if isinstance(value, dict) else None
if not isinstance(tokens, int) or isinstance(tokens, bool) or tokens < 0:
    print("tokenizer JSON did not contain a non-negative integer tokens/total field", file=sys.stderr)
    raise SystemExit(2)
print(tokens)
PY
    then
        printf 'ERROR: tokenizer output was unparsable for %s\n' "$path" >&2
        printf '%s\n' "$output" >&2
        exit 2
    fi
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
case "$scope" in
    staged)
        git diff --cached --name-only -z --diff-filter=ACMRT "$base_commit" >"$candidate_paths_file"
        ;;
    head|object)
        git diff --name-only -z --diff-filter=ACMRT "$base_commit" "$candidate_commit" >"$candidate_paths_file"
        ;;
esac
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
