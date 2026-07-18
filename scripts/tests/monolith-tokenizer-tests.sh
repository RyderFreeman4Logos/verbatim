# shellcheck shell=bash
# Tokenizer-specific fixtures sourced by monolith-check-tests.sh.

write_fake_tokenizer() {
    local bin_dir="$1"
    mkdir -p "$bin_dir"
    cat >"$bin_dir/tokuin" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "--version" ]; then
    printf 'tokuin %s\n' "${TOKUIN_FAKE_VERSION:-0.3.0}"
    exit 0
fi
case "${TOKUIN_FAKE_MODE:-normal}" in
    failure)
        printf 'fixture failure\n' >&2
        exit 127
        ;;
    malformed)
        printf 'not-json\n'
        exit 0
        ;;
    timeout)
        child_pid_file="${TOKUIN_FAKE_CHILD_PID_FILE:?}"
        (
            trap '' TERM
            printf '%s\n' "$BASHPID" >"$child_pid_file"
            exec sleep 600
        ) &
        wait "$!"
        ;;
    index-mutate)
        repo="${TOKUIN_FAKE_REPO:?}"
        marker="${TOKUIN_FAKE_MUTATION_MARKER:?}"
        if [ ! -e "$marker" ]; then
            awk 'BEGIN { for (line = 1; line <= 801; line++) print "late" }' \
                >"$repo/src/index-late.rs"
            git -C "$repo" add -- src/index-late.rs
            : >"$marker"
        fi
        ;;
    normal) ;;
    *)
        printf 'bad fake mode\n' >&2
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
    chmod +x "$bin_dir/tokuin"
    ln -s tokuin "$bin_dir/csa"
}

write_token_fixture() {
    local path="$1"
    local marker="$2"
    printf '%s\n' "$marker" >"$path"
    awk 'BEGIN { for (byte = 1; byte <= 9000; byte++) printf "x" }' >>"$path"
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
    TOKUIN_FAKE_MODE=failure assert_failure_matching 'tokenizer command failure blocks' \
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
    TOKUIN_FAKE_MODE=malformed assert_failure_matching 'malformed tokenizer output blocks' \
        'tokenizer output was unparsable' \
        run_checker "$malformed_repo" "$malformed_bin" staged
}

prepare_tokenizer_dependency_fixture() {
    local name="$1"
    local repo
    repo="$(init_repo "$name")"
    printf 'base\n' >"$repo/src/tokenizer.rs"
    write_policy "$repo" 'files = []'
    commit_base "$repo"
    write_lines "$repo/src/tokenizer.rs" 801 candidate
    run_without_git_env git -C "$repo" add src/tokenizer.rs
    printf '%s\n' "$repo"
}

test_tokenizer_dependency() {
    local mode="$1" repo bin_dir TOKUIN_FAKE_VERSION
    repo="$(prepare_tokenizer_dependency_fixture "tokenizer-$mode")"
    bin_dir="$test_root/tokenizer-$mode-bin"
    if [ "$mode" = missing ]; then
        mkdir -p "$bin_dir"
        assert_failure_matching 'missing tokenizer' \
            'tokenizer unavailable: required tokuin 0\.3\.0 in PATH' \
            run_checker_clean "$repo" "$bin_dir" staged
        return
    fi
    write_fake_tokenizer "$bin_dir"
    rm -- "$bin_dir/csa"
    if [ "$mode" = wrong-version ]; then
        TOKUIN_FAKE_VERSION=9.9.9
        export TOKUIN_FAKE_VERSION
        assert_failure_matching 'wrong tokenizer version' \
            'tokenizer version mismatch: required tokuin 0\.3\.0, got tokuin 9\.9\.9' \
            run_checker_clean "$repo" "$bin_dir" staged
        return
    fi
    printf 'documented configuration\n' >"$repo/src/tokenizer.rs"
    run_without_git_env git -C "$repo" add src/tokenizer.rs
    assert_success 'documented tokenizer' \
        run_checker_clean "$repo" "$bin_dir" staged
}

test_invalid_tokenizer_timeout() {
    local value="$1"
    local repo bin_dir
    repo="$(prepare_tokenizer_dependency_fixture "tokenizer-timeout-invalid-$value")"
    bin_dir="$test_root/tokenizer-timeout-invalid-$value-bin"
    write_fake_tokenizer "$bin_dir"
    MONOLITH_TOKENIZER_TIMEOUT_SECONDS="$value" assert_failure_matching \
        "invalid timeout '$value'" \
        'invalid MONOLITH_TOKENIZER_TIMEOUT_SECONDS: expected an integer from 1 through 300' \
        run_checker "$repo" "$bin_dir" staged
}

test_tokenizer_timeout_cleans_process_tree() {
    local repo bin_dir child_pid_file output status child_pid
    repo="$(prepare_tokenizer_dependency_fixture tokenizer-timeout-process-tree)"
    bin_dir="$test_root/tokenizer-timeout-process-tree-bin"
    child_pid_file="$test_root/tokenizer-timeout-child.pid"
    write_fake_tokenizer "$bin_dir"
    set +e
    output="$(
        TOKUIN_FAKE_MODE=timeout \
            TOKUIN_FAKE_CHILD_PID_FILE="$child_pid_file" \
            MONOLITH_TOKENIZER_TIMEOUT_SECONDS=1 \
            run_checker_bounded "$repo" "$bin_dir" staged 2>&1
    )"
    status=$?
    set -e
    [ "$status" -ne 0 ] || die 'hanging tokenizer unexpectedly succeeded'
    case "$status" in
        124|137) die 'checker exceeded the independent five-second test bound' ;;
    esac
    grep -Fq 'tokenizer timed out after 1s for src/tokenizer.rs' <<<"$output" || {
        printf '%s\n' "$output" >&2
        die 'timeout failure did not report the timeout-specific diagnostic'
    }
    [ -s "$child_pid_file" ] || die 'hanging tokenizer did not publish its child PID'
    child_pid="$(<"$child_pid_file")"
    case "$child_pid" in
        ''|*[!0-9]*) die "invalid fake tokenizer child PID: $child_pid" ;;
    esac
    if kill -0 "$child_pid" 2>/dev/null; then
        die "fake tokenizer descendant survived timeout cleanup: $child_pid"
    fi
}
