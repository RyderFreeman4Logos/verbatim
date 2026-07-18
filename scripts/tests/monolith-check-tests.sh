#!/usr/bin/env bash
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
checker="$root/scripts/monolith/check.sh"
tokenizer_runner="$root/scripts/monolith/tokenizer_runner.py"
pre_push_checker="$root/scripts/hooks/check-pre-push-version-bumps.sh"
test_root="$(realpath -e "$(mktemp -d)")"
case_filter="${MONOLITH_TEST_CASE:-}"
registered_case_count=0
executed_case_count=0
readonly tokenizer_revision="c68d1f804a4c172846716b7be99e9378e16512b7"
readonly checker_outer_timeout_seconds=15
cleanup() {
    rm -rf -- "$test_root"
}
trap cleanup EXIT
die() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}
run_without_git_env() {
    local -a unset_args=()
    local variable
    while IFS= read -r variable; do
        [ -n "$variable" ] || continue
        unset_args+=("-u" "$variable")
    done < <(git rev-parse --local-env-vars)
    env "${unset_args[@]}" "$@"
}
init_repo() {
    local name="$1"
    local repo="$test_root/$name"
    mkdir -p "$repo"
    run_without_git_env git -C "$repo" init -q
    run_without_git_env git -C "$repo" config user.email test@example.invalid
    run_without_git_env git -C "$repo" config user.name 'Monolith Test'
    mkdir -p "$repo/scripts/monolith" "$repo/src"
    printf '%s\n' "$repo"
}
write_lines() {
    local path="$1"
    local count="$2"
    local text="$3"
    awk -v count="$count" -v text="$text" \
        'BEGIN { for (line = 1; line <= count; line++) print text }' >"$path"
}
policy_row() {
    local path="$1"
    local tokens="$2"
    local lines="$3"
    local issue="${4:-368}"
    local rationale="${5-fixture rationale}"
    cat <<EOF
[[files]]
path = "$path"
kind = "source"
baseline_tokens = $tokens
baseline_lines = $lines
issue = "$issue"
rationale = "$rationale"
EOF
}
write_policy() {
    local repo="$1"
    local content="$2"
    {
        printf '%s\n' "$content"
        cat <<EOF
[tokenizer]
command = "tokuin"
version = "0.3.0"
revision = "$tokenizer_revision"
model = "gpt-4o"
format = "json"
timeout_seconds = 30
max_output_bytes = 1048576
known_answer_input = "Verbatim tokenizer attestation v1"
known_answer_tokens = 7
EOF
    } >"$repo/scripts/monolith/baseline.toml"
}
commit_base() {
    local repo="$1"
    run_without_git_env git -C "$repo" add src scripts/monolith/baseline.toml
    run_without_git_env git -C "$repo" commit -q -m base
    run_without_git_env git -C "$repo" branch base
}
commit_base_without_policy() {
    local repo="$1"
    run_without_git_env git -C "$repo" add src
    run_without_git_env git -C "$repo" commit -q -m base
    run_without_git_env git -C "$repo" branch base
}
run_checker() {
    local repo="$1"
    local bin_dir="$2"
    local scope="$3"
    shift 3
    (
        cd "$repo"
        PATH="$bin_dir:$PATH" \
            BASE_REF=base \
            TOKUIN_FAKE_MODE="${TOKUIN_FAKE_MODE:-normal}" \
            TOKUIN_FAKE_VERSION="${TOKUIN_FAKE_VERSION:-0.3.0}" \
            TOKUIN_FAKE_CHILD_PID_FILE="${TOKUIN_FAKE_CHILD_PID_FILE:-}" \
            TOKUIN_FAKE_OUTPUT="${TOKUIN_FAKE_OUTPUT:-}" \
            TOKUIN_FAKE_OUTPUT_HEX="${TOKUIN_FAKE_OUTPUT_HEX:-}" \
            TOKUIN_FAKE_STDERR_HEX="${TOKUIN_FAKE_STDERR_HEX:-}" \
            TOKUIN_FAKE_TARGET="${TOKUIN_FAKE_TARGET:-}" \
            TOKUIN_FAKE_LIFECYCLE_MODE="${TOKUIN_FAKE_LIFECYCLE_MODE:-}" \
            TOKUIN_FAKE_EXIT_STATUS="${TOKUIN_FAKE_EXIT_STATUS:-}" \
            TOKUIN_FAKE_STREAM="${TOKUIN_FAKE_STREAM:-}" \
            TOKUIN_FAKE_PID_FILE="${TOKUIN_FAKE_PID_FILE:-}" \
            TOKUIN_FAKE_REPO="${TOKUIN_FAKE_REPO:-}" \
            TOKUIN_FAKE_MUTATION_MARKER="${TOKUIN_FAKE_MUTATION_MARKER:-}" \
            MONOLITH_TOKENIZER_TIMEOUT_SECONDS="${MONOLITH_TOKENIZER_TIMEOUT_SECONDS:-}" \
            MONOLITH_TOKENIZER_MAX_OUTPUT_BYTES="${MONOLITH_TOKENIZER_MAX_OUTPUT_BYTES:-}" \
            run_without_git_env "$checker" --scope "$scope" "$@"
    )
}
run_checker_clean() {
    local repo="$1"
    local bin_dir="$2"
    local scope="$3"
    shift 3
    (
        cd "$repo"
        env -i \
            HOME="$test_root/clean-home" \
            LANG=C \
            PATH="$bin_dir:/usr/bin:/bin" \
            BASE_REF=base \
            TOKUIN_FAKE_VERSION="${TOKUIN_FAKE_VERSION:-0.3.0}" \
            "$checker" --scope "$scope" "$@"
    )
}
run_checker_bounded() {
    local repo="$1"
    local bin_dir="$2"
    local scope="$3"
    shift 3
    (
        cd "$repo"
        PATH="$bin_dir:$PATH" \
            BASE_REF=base \
            TOKUIN_FAKE_MODE="${TOKUIN_FAKE_MODE:-normal}" \
            TOKUIN_FAKE_CHILD_PID_FILE="${TOKUIN_FAKE_CHILD_PID_FILE:-}" \
            TOKUIN_FAKE_OUTPUT_HEX="${TOKUIN_FAKE_OUTPUT_HEX:-}" \
            TOKUIN_FAKE_STDERR_HEX="${TOKUIN_FAKE_STDERR_HEX:-}" \
            TOKUIN_FAKE_TARGET="${TOKUIN_FAKE_TARGET:-}" \
            TOKUIN_FAKE_LIFECYCLE_MODE="${TOKUIN_FAKE_LIFECYCLE_MODE:-}" \
            TOKUIN_FAKE_EXIT_STATUS="${TOKUIN_FAKE_EXIT_STATUS:-}" \
            TOKUIN_FAKE_STREAM="${TOKUIN_FAKE_STREAM:-}" \
            TOKUIN_FAKE_PID_FILE="${TOKUIN_FAKE_PID_FILE:-}" \
            MONOLITH_TOKENIZER_TIMEOUT_SECONDS="${MONOLITH_TOKENIZER_TIMEOUT_SECONDS:-}" \
            MONOLITH_TOKENIZER_MAX_OUTPUT_BYTES="${MONOLITH_TOKENIZER_MAX_OUTPUT_BYTES:-}" \
            run_without_git_env timeout --kill-after=1s \
            "${checker_outer_timeout_seconds}s" \
            "$checker" --scope "$scope" "$@"
    )
}
run_registered_case() {
    local name="$1"
    shift
    registered_case_count=$((registered_case_count + 1))
    if [ -n "$case_filter" ] && [ "$case_filter" != "$name" ]; then
        return 0
    fi
    executed_case_count=$((executed_case_count + 1))
    printf 'CASE: %s\n' "$name"
    "$@"
}
assert_success() {
    local name="$1"
    local output
    shift
    if ! output="$("$@" 2>&1)"; then
        printf '%s\n' "$output" >&2
        die "expected success: $name"
    fi
}
assert_failure_matching() {
    local name="$1"
    local pattern="$2"
    local output
    shift 2
    if output="$("$@" 2>&1)"; then
        die "expected failure: $name"
    fi
    grep -Eq -- "$pattern" <<<"$output" || {
        printf '%s\n' "$output" >&2
        die "failure did not match '$pattern': $name"
    }
}
assert_output_count() {
    local name="$1"
    local pattern="$2"
    local expected="$3"
    local output="$4"
    local actual
    actual="$(grep -Ec -- "$pattern" <<<"$output")"
    [ "$actual" -eq "$expected" ] \
        || die "$name expected $expected matches for '$pattern', got $actual"
}
run_aggregate() {
    local repo="$1" bin_dir="$2"
    (
        cd "$repo"
        PATH="$bin_dir:$PATH" BASE_REF=base JUST_NO_DOTENV=true \
            REQUIRE_NO_REPLACE_OBJECTS="${REQUIRE_NO_REPLACE_OBJECTS:-0}" \
            run_without_git_env just pre-commit-fast staged
    )
}
run_fmt_fixture() {
    local repo="$1" bin_dir="$2"
    (cd "$repo" && PATH="$bin_dir:$PATH" run_without_git_env just fmt)
}
run_pre_push() {
    local repo="$1" bin_dir="$2" input="$3" version_log="$4" full_gate_log="$5"
    (
        cd "$repo"
        if [ -n "$input" ]; then
            printf '%s\n' "$input"
        fi | PATH="$repo/test-bin:$bin_dir:$PATH" BASE_REF=base \
            VERSION_CHECK_LOG="$version_log" FULL_GATE_LOG="$full_gate_log" \
            run_without_git_env "$repo/scripts/hooks/check-pre-push-version-bumps.sh"
    )
}
push_fixture() {
    local repo="$1" source_ref="$3" destination_ref="$4" remote
    remote="$(realpath -e -- "$2")" || die 'cannot canonicalize bare fixture'
    case "$remote" in
        "$test_root"/*) ;;
        *) die 'fixture push escaped its temporary root' ;;
    esac
    [ "$(run_without_git_env git --git-dir="$remote" \
        rev-parse --is-bare-repository)" = true ] || die 'push target is not bare'
    run_without_git_env env -u GIT_NO_REPLACE_OBJECTS \
        GIT_ALLOW_PROTOCOL=file GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
        git -C "$repo" push --quiet "$remote" "$source_ref:$destination_ref"
}
install_push_hook() {
    local repo="$1" hooks
    hooks="$(run_without_git_env git -C "$repo" \
        rev-parse --path-format=absolute --git-path hooks)"
    case "$hooks" in
        "$test_root"/*) ;;
        *) die 'fixture hooks escaped their temporary root' ;;
    esac
    cp "$repo/scripts/hooks/check-pre-push-version-bumps.sh" "$hooks/pre-push"
    chmod +x "$hooks/pre-push"
}
gated_push_fixture() {
    local repo="$1" bin_dir="$2" remote="$3" source_ref="$4" destination_ref="$5"
    local version_log="$6" full_gate_log="$7"
    (
        export PATH="$repo/test-bin:$bin_dir:$PATH" BASE_REF=base
        export VERSION_CHECK_LOG="$version_log" FULL_GATE_LOG="$full_gate_log"
        push_fixture "$repo" "$remote" "$source_ref" "$destination_ref"
    )
}
# shellcheck source=scripts/tests/monolith-tokenizer-tests.sh
source "$root/scripts/tests/monolith-tokenizer-tests.sh"
# shellcheck source=scripts/tests/monolith-git-security-tests.sh
source "$root/scripts/tests/monolith-git-security-tests.sh"
test_canonical_baseline_rows() {
    python3 - "$root/scripts/monolith/baseline.toml" "$root/docs/mvp.md" \
        "$tokenizer_revision" <<'PY'
import sys
import tomllib
with open(sys.argv[1], "rb") as fh:
    data = tomllib.load(fh)
rows = data["files"]
if len(rows) != 21:
    raise SystemExit(f"expected 21 baseline rows, got {len(rows)}")
if any(row.get("issue") != "368" for row in rows):
    raise SystemExit("every canonical baseline row must use issue 368")
if any(not isinstance(row.get("rationale"), str) or not row["rationale"].strip() for row in rows):
    raise SystemExit("every canonical baseline row needs a rationale")
rev = sys.argv[3]
expected = {"command": "tokuin", "version": "0.3.0", "revision": rev,
            "model": "gpt-4o", "format": "json", "timeout_seconds": 30,
            "max_output_bytes": 1048576,
            "known_answer_input": "Verbatim tokenizer attestation v1",
            "known_answer_tokens": 7}
if data.get("tokenizer") != expected:
    raise SystemExit("canonical tokenizer metadata mismatch")
with open(sys.argv[2], encoding="utf-8") as fh:
    docs = fh.read()
required = (f"--rev {rev} --locked tokuin", "tokuin 0.3.0", "30-second",
            "MONOLITH_TOKENIZER_TIMEOUT_SECONDS", "known answer")
if missing := [text for text in required if text not in docs]:
    raise SystemExit(f"docs missing: {missing!r}")
PY
}
test_equal_and_decreased_bounds_pass() {
    local repo bin_dir policy
    repo="$(init_repo equal-and-decreased)"
    bin_dir="$test_root/equal-and-decreased-bin"
    write_fake_tokenizer "$bin_dir"
    write_lines "$repo/src/large.rs" 801 base
    policy="$(policy_row src/large.rs 20 801)"
    write_policy "$repo" "$policy"
    commit_base "$repo"
    write_lines "$repo/src/large.rs" 801 equal
    run_without_git_env git -C "$repo" add src/large.rs
    assert_success 'equal baseline bounds pass' run_checker "$repo" "$bin_dir" staged
    write_lines "$repo/src/large.rs" 800 decreased
    run_without_git_env git -C "$repo" add src/large.rs
    assert_success 'decreased source bounds pass' run_checker "$repo" "$bin_dir" staged
}
test_baseline_row_removal_after_refactor_passes() {
    local repo bin_dir
    repo="$(init_repo baseline-row-removal)"
    bin_dir="$test_root/baseline-row-removal-bin"
    write_fake_tokenizer "$bin_dir"
    write_lines "$repo/src/refactored.rs" 801 base
    write_policy "$repo" "$(policy_row src/refactored.rs 20 801)"
    commit_base "$repo"
    write_lines "$repo/src/refactored.rs" 800 refactored
    write_policy "$repo" 'files = []'
    run_without_git_env git -C "$repo" add src/refactored.rs scripts/monolith/baseline.toml
    assert_success 'baseline row removal after shrinking below the global limit passes' \
        run_checker "$repo" "$bin_dir" staged
}
test_generated_lockfile_is_not_a_monolith() {
    local repo bin_dir
    repo="$(init_repo generated-lockfile)"
    bin_dir="$test_root/generated-lockfile-bin"
    write_fake_tokenizer "$bin_dir"
    write_policy "$repo" 'files = []'
    commit_base "$repo"
    write_lines "$repo/Cargo.lock" 801 generated
    run_without_git_env git -C "$repo" add Cargo.lock
    assert_success 'generated Cargo.lock is outside the monolith source policy' \
        run_checker "$repo" "$bin_dir" staged
}
test_policy_mutation_fails_closed() {
    local repo bin_dir policy
    repo="$(init_repo policy-mutation)"
    bin_dir="$test_root/policy-mutation-bin"
    write_fake_tokenizer "$bin_dir"
    write_lines "$repo/src/base.rs" 801 base
    policy="$(policy_row src/base.rs 20 801)"
    write_policy "$repo" "$policy"
    commit_base "$repo"
    write_policy "$repo" "$(policy_row src/base.rs 20 802)"
    run_without_git_env git -C "$repo" add scripts/monolith/baseline.toml
    assert_failure_matching 'cap raise blocks' 'cap changed while src/base.rs remains oversized' \
        run_checker "$repo" "$bin_dir" staged
    write_policy "$repo" "$(policy_row src/base.rs 20 801)
$(policy_row src/new.rs 20 801)"
    write_lines "$repo/src/new.rs" 801 new
    run_without_git_env git -C "$repo" add src/new.rs scripts/monolith/baseline.toml
    assert_failure_matching 'new exemption blocks' 'new exemption row for src/new.rs' \
        run_checker "$repo" "$bin_dir" staged
}
test_issue_rationale_duplicate_and_invalid_rows_fail() {
    local repo bin_dir valid
    repo="$(init_repo invalid-policy)"
    bin_dir="$test_root/invalid-policy-bin"
    write_fake_tokenizer "$bin_dir"
    write_lines "$repo/src/base.rs" 801 base
    valid="$(policy_row src/base.rs 20 801)"
    write_policy "$repo" "$valid"
    commit_base "$repo"
    write_policy "$repo" "$(policy_row src/base.rs 20 801 0)"
    run_without_git_env git -C "$repo" add scripts/monolith/baseline.toml
    assert_failure_matching 'placeholder issue blocks' 'must use issue' \
        run_checker "$repo" "$bin_dir" staged
    write_policy "$repo" '[[files]]
path = "src/base.rs"
kind = "source"
baseline_tokens = 20
baseline_lines = 801
rationale = "fixture rationale"'
    run_without_git_env git -C "$repo" add scripts/monolith/baseline.toml
    assert_failure_matching 'missing issue blocks' 'missing key\(s\): issue' \
        run_checker "$repo" "$bin_dir" staged
    write_policy "$repo" "$(policy_row src/base.rs 20 801 368 '')"
    run_without_git_env git -C "$repo" add scripts/monolith/baseline.toml
    assert_failure_matching 'missing rationale blocks' 'missing rationale' \
        run_checker "$repo" "$bin_dir" staged
    write_policy "$repo" "$valid
$valid"
    run_without_git_env git -C "$repo" add scripts/monolith/baseline.toml
    assert_failure_matching 'duplicate baseline row blocks' 'duplicate path' \
        run_checker "$repo" "$bin_dir" staged
    write_policy "$repo" '[[files]]
path = "src/base.rs"
kind = "source"
baseline_tokens = "20"
baseline_lines = 801
issue = "368"
rationale = "fixture rationale"'
    run_without_git_env git -C "$repo" add scripts/monolith/baseline.toml
    assert_failure_matching 'malformed bound blocks' 'invalid baseline_tokens' \
        run_checker "$repo" "$bin_dir" staged
    write_policy "$repo" '[[files]]
path = "src/base.rs"
kind = "source"
baseline_tokens = 20
baseline_lines = 801
issue = "368"
rationale = "fixture rationale"
unexpected = "blocked"'
    run_without_git_env git -C "$repo" add scripts/monolith/baseline.toml
    assert_failure_matching 'unknown key blocks' 'unknown key' \
        run_checker "$repo" "$bin_dir" staged
    write_policy "$repo" "$(policy_row src/missing.rs 20 801)"
    run_without_git_env git -C "$repo" add scripts/monolith/baseline.toml
    assert_failure_matching 'missing baseline path blocks' 'entry path is missing or not text' \
        run_checker "$repo" "$bin_dir" staged
    write_policy "$repo" "$(policy_row src/base.rs 20 801 368 'changed rationale')"
    run_without_git_env git -C "$repo" add scripts/monolith/baseline.toml
    assert_failure_matching 'required rationale mutation blocks' 'required issue/rationale changed' \
        run_checker "$repo" "$bin_dir" staged
}

if [ -z "$case_filter" ]; then
    test_canonical_baseline_rows
    test_equal_and_decreased_bounds_pass
    test_baseline_row_removal_after_refactor_passes
    test_line_and_token_growth_fail
    test_new_monolith_and_staged_target_isolation
    test_generated_lockfile_is_not_a_monolith
    test_policy_mutation_fails_closed
    test_issue_rationale_duplicate_and_invalid_rows_fail
    test_staged_baseline_is_authoritative
    test_object_snapshot_and_tokenizer_failures
    test_pre_push_object_parser
fi
run_registered_case 'F1 candidate-only staged' test_bootstrap_attack candidate-only staged
run_registered_case 'F1 candidate-only head' test_bootstrap_attack candidate-only head
run_registered_case 'F1 candidate-only object' test_bootstrap_attack candidate-only object
run_registered_case 'F1 inherited-growth staged' test_bootstrap_attack inherited-growth staged
run_registered_case 'F1 inherited-growth head' test_bootstrap_attack inherited-growth head
run_registered_case 'F1 inherited-growth object' test_bootstrap_attack inherited-growth object
run_registered_case 'F2 missing' test_tokenizer_dependency missing
run_registered_case 'F2 wrong version' test_tokenizer_dependency wrong-version
run_registered_case 'F2 clean success' test_tokenizer_dependency documented-success
run_registered_case 'F3 type-change staged' test_git_type_change_to_monolith staged
run_registered_case 'F3 type-change head' test_git_type_change_to_monolith head
run_registered_case 'F3 type-change object' test_git_type_change_to_monolith object
run_registered_case 'F4 invalid zero' test_invalid_tokenizer_timeout 0
run_registered_case 'F4 invalid text' test_invalid_tokenizer_timeout invalid
run_registered_case 'F4 invalid excessive' test_invalid_tokenizer_timeout 301
run_registered_case 'F4 timeout cleanup' test_tokenizer_timeout_cleans_process_tree
run_registered_case 'R2-A literal paths staged' test_literal_pathname_matrix staged
run_registered_case 'R2-A literal paths head' test_literal_pathname_matrix head
run_registered_case 'R2-A literal paths object' test_literal_pathname_matrix object
run_registered_case 'R2-B literal trusted base staged' test_literal_trusted_base_matrix staged
run_registered_case 'R2-B literal trusted base head' test_literal_trusted_base_matrix head
run_registered_case 'R2-B literal trusted base object' test_literal_trusted_base_matrix object
run_registered_case 'R2-B rename staged' test_git_rename_to_monolith staged
run_registered_case 'R2-B rename head' test_git_rename_to_monolith head
run_registered_case 'R2-B rename object' test_git_rename_to_monolith object
run_registered_case 'R2-A annotated tag object' test_annotated_tag_object
run_registered_case 'R2-B staged index mutation' test_staged_index_mutation_fails_closed
run_registered_case 'R2-A aggregate final index' test_aggregate_validates_final_index
run_registered_case 'R2-A literal restage matrix' test_fmt_restages_literal_paths
run_registered_case 'R2-B replacement refs bare push' test_replacement_refs_are_ignored
run_registered_case 'R2-B missing regular object pre-push' test_missing_regular_object_blocks_before_transport
run_registered_case 'R2-C process lifecycle' test_runner_process_lifecycle_matrix
run_registered_case 'R2-C runner output caps' test_runner_output_caps
run_registered_case 'R2-D numeric domain' test_tokenizer_numeric_domain
run_registered_case 'R2-D natural exit 124' test_natural_exit_124_is_not_timeout
run_registered_case 'R2-D checker output cap' test_checker_output_cap
[ "$registered_case_count" -eq 36 ] || die "registered: $registered_case_count/36"
if [ -n "$case_filter" ]; then
    [ "$executed_case_count" -eq 1 ] || die "selected: $executed_case_count"
else
    [ "$executed_case_count" -eq "$registered_case_count" ] || die 'case count mismatch'
fi
printf 'monolith-check-tests: PASS (%s Tier-4 cases)\n' "$executed_case_count"
