# shellcheck shell=bash
# Git snapshot and pre-push fixtures sourced by monolith-check-tests.sh.

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

assert_scope_failure() {
    local name="$1" pattern="$2" repo="$3" bin_dir="$4" scope="$5"
    local candidate_object
    if [ "$scope" = staged ]; then
        assert_failure_matching "$name" "$pattern" run_checker "$repo" "$bin_dir" staged
        return
    fi
    run_without_git_env git -C "$repo" commit -q -m candidate
    candidate_object="$(run_without_git_env git -C "$repo" rev-parse HEAD)"
    if [ "$scope" = head ]; then
        assert_failure_matching "$name" "$pattern" run_checker "$repo" "$bin_dir" head
    elif [ "$scope" = object ]; then
        assert_failure_matching "$name" "$pattern" \
            run_checker "$repo" "$bin_dir" object --object "$candidate_object"
    else
        die "unsupported candidate scope: $scope"
    fi
}

test_bootstrap_attack() {
    local attack="$1" scope="$2"
    local repo bin_dir path lines pattern
    repo="$(init_repo "bootstrap-$attack-$scope")"
    bin_dir="$test_root/bootstrap-$attack-$scope-bin"
    write_fake_tokenizer "$bin_dir"
    if [ "$attack" = candidate-only ]; then
        printf 'base\n' >"$repo/src/inherited.rs"
        commit_base_without_policy "$repo"
        path=src/candidate-only.rs
        lines=801
        pattern='BLOCK baseline bootstrap: candidate-only oversized file src/candidate-only.rs'
    else
        path=src/inherited.rs
        write_lines "$repo/$path" 801 base
        commit_base_without_policy "$repo"
        lines=802
        pattern='BLOCK baseline bootstrap: src/inherited.rs bounds must exactly match trusted-base'
    fi
    write_lines "$repo/$path" "$lines" candidate
    write_policy "$repo" "$(policy_row "$path" 20 "$lines")"
    run_without_git_env git -C "$repo" add "$path" scripts/monolith/baseline.toml
    assert_scope_failure "bootstrap $attack/$scope" \
        "$pattern" "$repo" "$bin_dir" "$scope"
}

test_git_type_change_to_monolith() {
    local scope="$1"
    local repo bin_dir
    repo="$(init_repo "type-change-$scope")"
    bin_dir="$test_root/type-change-$scope-bin"
    write_fake_tokenizer "$bin_dir"
    printf 'target\n' >"$repo/target.txt"
    ln -s target.txt "$repo/:type-change.rs"
    write_policy "$repo" 'files = []'
    run_without_git_env git -C "$repo" add -- \
        target.txt ':(top,literal):type-change.rs' scripts/monolith/baseline.toml
    run_without_git_env git -C "$repo" commit -q -m base
    run_without_git_env git -C "$repo" branch base
    rm -- "$repo/:type-change.rs"
    write_lines "$repo/:type-change.rs" 801 candidate
    run_without_git_env git -C "$repo" add -- ':(top,literal):type-change.rs'
    assert_scope_failure "type change/$scope" \
        'BLOCK new monolith: :type-change.rs' "$repo" "$bin_dir" "$scope"
}

test_literal_pathname_matrix() {
    local scope="$1"
    local repo bin_dir path candidate_object output status
    local -a paths=(':colon.rs' '-option.rs' $'src/line\nbreak.rs' $'src/tab\tbreak.rs')
    local -a pathspecs=()
    repo="$(init_repo "literal-paths-$scope")"
    bin_dir="$test_root/literal-paths-$scope-bin"
    write_fake_tokenizer "$bin_dir"
    write_policy "$repo" 'files = []'
    commit_base "$repo"
    for path in "${paths[@]}"; do
        write_lines "$repo/$path" 801 candidate
        pathspecs+=(":(top,literal)$path")
    done
    run_without_git_env git -C "$repo" add -- "${pathspecs[@]}"
    if [ "$scope" != staged ]; then
        run_without_git_env git -C "$repo" commit -q -m candidate
        candidate_object="$(run_without_git_env git -C "$repo" rev-parse HEAD)"
    fi
    set +e
    case "$scope" in
        staged) output="$(run_checker "$repo" "$bin_dir" staged 2>&1)" ;;
        head) output="$(run_checker "$repo" "$bin_dir" head 2>&1)" ;;
        object)
            output="$(run_checker "$repo" "$bin_dir" object \
                --object "$candidate_object" 2>&1)"
            ;;
        *) die "unsupported literal-path scope: $scope" ;;
    esac
    status=$?
    set -e
    [ "$status" -ne 0 ] || die "literal pathname matrix unexpectedly passed: $scope"
    assert_output_count "literal pathname hard failures/$scope" \
        '^BLOCK new monolith:' 4 "$output"
}

test_annotated_tag_object() {
    local repo bin_dir tag_object
    repo="$(init_repo annotated-tag-object)"
    bin_dir="$test_root/annotated-tag-object-bin"
    write_fake_tokenizer "$bin_dir"
    write_policy "$repo" 'files = []'
    commit_base "$repo"
    write_lines "$repo/src/tagged.rs" 801 candidate
    run_without_git_env git -C "$repo" add -- src/tagged.rs
    run_without_git_env git -C "$repo" commit -q -m candidate
    run_without_git_env git -C "$repo" tag -a candidate-tag -m candidate-tag
    tag_object="$(run_without_git_env git -C "$repo" rev-parse candidate-tag)"
    assert_failure_matching 'annotated tag object is checked literally' \
        'BLOCK new monolith: src/tagged.rs' \
        run_checker "$repo" "$bin_dir" object --object "$tag_object"
}

test_staged_index_mutation_fails_closed() {
    local repo bin_dir marker output status
    repo="$(init_repo staged-index-mutation)"
    bin_dir="$test_root/staged-index-mutation-bin"
    marker="$test_root/staged-index-mutation.marker"
    write_fake_tokenizer "$bin_dir"
    write_policy "$repo" 'files = []'
    commit_base "$repo"
    write_token_fixture "$repo/src/trigger.rs" TOKEN_STABLE
    run_without_git_env git -C "$repo" add -- src/trigger.rs
    set +e
    output="$(
        TOKUIN_FAKE_MODE=index-mutate \
            TOKUIN_FAKE_REPO="$repo" \
            TOKUIN_FAKE_MUTATION_MARKER="$marker" \
            run_checker "$repo" "$bin_dir" staged 2>&1
    )"
    status=$?
    set -e
    [ "$status" -ne 0 ] || die 'staged index mutation unexpectedly passed'
    grep -Fq 'Git index changed while monolith gate was running' <<<"$output" || {
        printf '%s\n' "$output" >&2
        die 'staged index mutation lacked the fail-closed diagnostic'
    }
    run_without_git_env git -C "$repo" diff --cached --name-only -- \
        src/index-late.rs | grep -Fxq 'src/index-late.rs' \
        || die 'mutation fixture did not add the late index path'
}

prepare_pre_push_fixture() {
    local repo="$1"
    local bin_dir="$2"
    mkdir -p "$repo/scripts/hooks" "$repo/test-bin"
    cp "$checker" "$repo/scripts/monolith/check.sh"
    cp "$root/scripts/monolith/tokenizer_runner.py" \
        "$repo/scripts/monolith/tokenizer_runner.py"
    cp "$root/scripts/monolith/tokenizer_contract.py" \
        "$repo/scripts/monolith/tokenizer_contract.py"
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
    [ ! -e "$repo/scripts/monolith/__pycache__" ] \
        || die 'pre-push monolith gate created Python bytecode artifacts'
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
