#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
checker="$root/scripts/monolith/check.sh"
pre_push_checker="$root/scripts/hooks/check-pre-push-version-bumps.sh"
test_root="$(mktemp -d)"

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

write_fake_tokenizer() {
    local bin_dir="$1"

    mkdir -p "$bin_dir"
    cat >"$bin_dir/csa" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

case "${CSA_FAKE_MODE:-normal}" in
    failure)
        printf 'fixture tokenizer unavailable\n' >&2
        exit 127
        ;;
    malformed)
        printf 'not-json\n'
        exit 0
        ;;
    normal) ;;
    *)
        printf 'unknown CSA_FAKE_MODE: %s\n' "$CSA_FAKE_MODE" >&2
        exit 64
        ;;
esac

file="${@: -1}"
if grep -q 'TOKEN_8002' "$file"; then
    tokens=8002
elif grep -q 'TOKEN_8001' "$file"; then
    tokens=8001
else
    tokens=20
fi
printf '{"tokens":%s}\n' "$tokens"
SH
    chmod +x "$bin_dir/csa"
}

write_lines() {
    local path="$1"
    local count="$2"
    local text="$3"

    awk -v count="$count" -v text="$text" \
        'BEGIN { for (line = 1; line <= count; line++) print text }' >"$path"
}

write_token_fixture() {
    local path="$1"
    local marker="$2"

    printf '%s\n' "$marker" >"$path"
    awk 'BEGIN { for (byte = 1; byte <= 9000; byte++) printf "x" }' >>"$path"
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

    printf '%s\n' "$content" >"$repo/scripts/monolith/baseline.toml"
}

commit_base() {
    local repo="$1"

    run_without_git_env git -C "$repo" add src scripts/monolith/baseline.toml
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
            CSA_FAKE_MODE="${CSA_FAKE_MODE:-normal}" \
            run_without_git_env "$checker" --scope "$scope" "$@"
    )
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

test_canonical_baseline_rows() {
    python3 - "$root/scripts/monolith/baseline.toml" <<'PY'
import sys
import tomllib

with open(sys.argv[1], "rb") as fh:
    rows = tomllib.load(fh)["files"]
if len(rows) != 24:
    raise SystemExit(f"expected 24 baseline rows, got {len(rows)}")
if any(row.get("issue") != "368" for row in rows):
    raise SystemExit("every canonical baseline row must use issue 368")
if any(not isinstance(row.get("rationale"), str) or not row["rationale"].strip() for row in rows):
    raise SystemExit("every canonical baseline row needs a rationale")
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

test_line_and_token_growth_fail() {
    local line_repo line_bin token_repo token_bin

    line_repo="$(init_repo line-growth)"
    line_bin="$test_root/line-growth-bin"
    write_fake_tokenizer "$line_bin"
    write_lines "$line_repo/src/large.rs" 801 base
    write_policy "$line_repo" "$(policy_row src/large.rs 20 801)"
    commit_base "$line_repo"
    write_lines "$line_repo/src/large.rs" 802 grown
    run_without_git_env git -C "$line_repo" add src/large.rs
    assert_failure_matching 'line growth blocks' 'BLOCK ratchet: src/large.rs' \
        run_checker "$line_repo" "$line_bin" staged

    token_repo="$(init_repo token-growth)"
    token_bin="$test_root/token-growth-bin"
    write_fake_tokenizer "$token_bin"
    write_token_fixture "$token_repo/src/token.rs" TOKEN_8001
    write_policy "$token_repo" "$(policy_row src/token.rs 8001 1)"
    commit_base "$token_repo"
    write_token_fixture "$token_repo/src/token.rs" TOKEN_8002
    run_without_git_env git -C "$token_repo" add src/token.rs
    assert_failure_matching 'token growth blocks' 'BLOCK ratchet: src/token.rs' \
        run_checker "$token_repo" "$token_bin" staged
}

test_new_monolith_and_staged_target_isolation() {
    local repo bin_dir

    repo="$(init_repo staged-target-isolation)"
    bin_dir="$test_root/staged-target-isolation-bin"
    write_fake_tokenizer "$bin_dir"
    printf 'base\n' >"$repo/src/snapshot.rs"
    write_policy "$repo" 'files = []'
    commit_base "$repo"

    write_lines "$repo/src/snapshot.rs" 801 staged
    run_without_git_env git -C "$repo" add src/snapshot.rs
    printf 'worktree-only\n' >"$repo/src/snapshot.rs"
    assert_failure_matching \
        'staged snapshot blocks an over-limit file after worktree shrink' \
        'BLOCK new monolith: src/snapshot.rs' \
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
    assert_failure_matching 'placeholder issue blocks' 'canonical issue' \
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

test_staged_baseline_is_authoritative() {
    local repo bin_dir policy

    repo="$(init_repo staged-baseline-isolation)"
    bin_dir="$test_root/staged-baseline-isolation-bin"
    write_fake_tokenizer "$bin_dir"
    write_lines "$repo/src/base.rs" 801 base
    policy="$(policy_row src/base.rs 20 801)"
    write_policy "$repo" "$policy"
    commit_base "$repo"

    write_policy "$repo" "$(policy_row src/base.rs 20 802)"
    run_without_git_env git -C "$repo" add scripts/monolith/baseline.toml
    write_policy "$repo" "$policy"
    assert_failure_matching 'staged baseline ignores worktree restoration' \
        'cap changed while src/base.rs remains oversized' \
        run_checker "$repo" "$bin_dir" staged
}

test_object_snapshot_and_tokenizer_failures() {
    local object_repo object_bin failure_repo failure_bin malformed_repo malformed_bin object_id

    object_repo="$(init_repo object-isolation)"
    object_bin="$test_root/object-isolation-bin"
    write_fake_tokenizer "$object_bin"
    printf 'base\n' >"$object_repo/src/object.rs"
    write_policy "$object_repo" 'files = []'
    commit_base "$object_repo"
    printf 'committed\n' >"$object_repo/src/object.rs"
    run_without_git_env git -C "$object_repo" add src/object.rs
    run_without_git_env git -C "$object_repo" commit -q -m candidate
    object_id="$(run_without_git_env git -C "$object_repo" rev-parse HEAD)"
    write_lines "$object_repo/src/object.rs" 801 worktree
    assert_success 'object snapshot ignores an oversized worktree' \
        run_checker "$object_repo" "$object_bin" object --object "$object_id"
    assert_failure_matching 'missing object blocks' 'cannot resolve object ID as a commit' \
        run_checker "$object_repo" "$object_bin" object \
        --object 0000000000000000000000000000000000000000

    failure_repo="$(init_repo tokenizer-failure)"
    failure_bin="$test_root/tokenizer-failure-bin"
    write_fake_tokenizer "$failure_bin"
    printf 'base\n' >"$failure_repo/src/failure.rs"
    write_policy "$failure_repo" 'files = []'
    commit_base "$failure_repo"
    write_lines "$failure_repo/src/failure.rs" 801 candidate
    run_without_git_env git -C "$failure_repo" add src/failure.rs
    CSA_FAKE_MODE=failure assert_failure_matching 'tokenizer command failure blocks' \
        'tokenizer failed for src/failure.rs' \
        run_checker "$failure_repo" "$failure_bin" staged

    malformed_repo="$(init_repo tokenizer-malformed)"
    malformed_bin="$test_root/tokenizer-malformed-bin"
    write_fake_tokenizer "$malformed_bin"
    printf 'base\n' >"$malformed_repo/src/malformed.rs"
    write_policy "$malformed_repo" 'files = []'
    commit_base "$malformed_repo"
    write_lines "$malformed_repo/src/malformed.rs" 801 candidate
    run_without_git_env git -C "$malformed_repo" add src/malformed.rs
    CSA_FAKE_MODE=malformed assert_failure_matching 'malformed tokenizer output blocks' \
        'tokenizer output was unparsable' \
        run_checker "$malformed_repo" "$malformed_bin" staged
}

prepare_pre_push_fixture() {
    local repo="$1"
    local bin_dir="$2"

    mkdir -p "$repo/scripts/hooks" "$repo/test-bin"
    cp "$checker" "$repo/scripts/monolith/check.sh"
    cp "$pre_push_checker" "$repo/scripts/hooks/check-pre-push-version-bumps.sh"
    cat >"$repo/scripts/hooks/check-version-bumped.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${VERSION_CHECK_LOG:?}"
SH
    cat >"$repo/test-bin/just" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${FULL_GATE_LOG:?}"
SH
    chmod +x \
        "$repo/scripts/monolith/check.sh" \
        "$repo/scripts/hooks/check-pre-push-version-bumps.sh" \
        "$repo/scripts/hooks/check-version-bumped.sh" \
        "$repo/test-bin/just"
    write_fake_tokenizer "$bin_dir"
}

run_pre_push() {
    local repo="$1"
    local bin_dir="$2"
    local input="$3"
    local version_log="$4"
    local full_gate_log="$5"

    (
        cd "$repo"
        if [ -n "$input" ]; then
            printf '%s\n' "$input"
        fi | PATH="$repo/test-bin:$bin_dir:$PATH" \
            BASE_REF=base \
            VERSION_CHECK_LOG="$version_log" \
            FULL_GATE_LOG="$full_gate_log" \
            run_without_git_env "$repo/scripts/hooks/check-pre-push-version-bumps.sh"
    )
}

test_pre_push_object_parser() {
    local repo bin_dir good_object bad_object zero input output version_log full_gate_log

    repo="$(init_repo pre-push-object-parser)"
    bin_dir="$test_root/pre-push-object-parser-bin"
    printf 'base\n' >"$repo/src/push.rs"
    write_policy "$repo" 'files = []'
    commit_base "$repo"
    printf 'good\n' >"$repo/src/push.rs"
    run_without_git_env git -C "$repo" add src/push.rs
    run_without_git_env git -C "$repo" commit -q -m good
    good_object="$(run_without_git_env git -C "$repo" rev-parse HEAD)"
    run_without_git_env git -C "$repo" branch bad base
    run_without_git_env git -C "$repo" switch -q bad
    write_lines "$repo/src/push.rs" 801 bad
    run_without_git_env git -C "$repo" add src/push.rs
    run_without_git_env git -C "$repo" commit -q -m bad
    bad_object="$(run_without_git_env git -C "$repo" rev-parse HEAD)"
    prepare_pre_push_fixture "$repo" "$bin_dir"
    zero=0000000000000000000000000000000000000000
    version_log="$repo/version.log"
    full_gate_log="$repo/full-gate.log"
    : >"$version_log"
    : >"$full_gate_log"
    input="refs/heads/good $good_object refs/heads/good $zero
refs/heads/bad $bad_object refs/heads/bad $zero"
    if output="$(run_pre_push "$repo" "$bin_dir" "$input" "$version_log" "$full_gate_log" 2>&1)"; then
        die 'expected multi-ref push to reject the monolith object'
    fi
    grep -q 'BLOCK new monolith: src/push.rs' <<<"$output" \
        || die 'multi-ref push did not validate the failing object'
    assert_output_count 'multi-ref object gate execution' 'Scope: object' 2 "$output"
    [ "$(wc -l <"$version_log" | tr -d '[:space:]')" -eq 2 ] \
        || die 'shared parser did not visit both non-deletion refs'
    [ ! -s "$full_gate_log" ] || die 'full gate ran after an object validation failure'

    : >"$version_log"
    : >"$full_gate_log"
    assert_success 'deletion ref is ignored' \
        run_pre_push "$repo" "$bin_dir" \
        "refs/heads/deleted $zero refs/heads/deleted $good_object" \
        "$version_log" "$full_gate_log"
    [ ! -s "$version_log" ] || die 'deletion ref invoked an object validator'
    [ "$(<"$full_gate_log")" = 'pre-commit head' ] \
        || die 'deletion ref did not continue to the full head gate'

    : >"$version_log"
    : >"$full_gate_log"
    assert_failure_matching 'malformed pre-push stdin blocks' \
        'malformed pre-push reference input' \
        run_pre_push "$repo" "$bin_dir" 'malformed input' "$version_log" "$full_gate_log"
    [ ! -s "$version_log" ] || die 'malformed stdin invoked an object validator'
    [ ! -s "$full_gate_log" ] || die 'malformed stdin ran the full head gate'
}

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

printf 'monolith-check-tests: PASS\n'
