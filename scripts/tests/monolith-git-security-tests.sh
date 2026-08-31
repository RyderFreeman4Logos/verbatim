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
        'cap increased for src/base.rs' \
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

test_ratchet() {
    local mode="$1" repo bin lines=10 cap=10
    repo="$(init_repo "r-$mode")"
    bin="$test_root/r-bin"
    write_fake_tokenizer "$bin"
    write_lines "$repo/src/x.rs" 10 "$(printf '%01200d' 0)"
    commit_base_without_policy "$repo"
    if [ "$mode" != exact ]; then
        lines=801
        cap=900
    fi
    write_lines "$repo/src/x.rs" "$lines" candidate
    write_policy "$repo" "$(policy_row src/x.rs 20 "$cap")"
    run_without_git_env git -C "$repo" add \
        src/x.rs scripts/monolith/baseline.toml
    if [ "$mode" = exact ]; then
        assert_success 'exact cap' run_checker "$repo" "$bin" staged
    else
        assert_failure_matching 'inflated cap' \
            'BLOCK baseline bootstrap: src/x.rs bounds must exactly match trusted-base' \
            run_checker "$repo" "$bin" staged
    fi
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

test_git_rename_to_monolith() {
    local scope="$1"
    local repo bin_dir

    repo="$(init_repo "rename-$scope")"
    bin_dir="$test_root/rename-$scope-bin"
    write_fake_tokenizer "$bin_dir"
    printf 'base\n' >"$repo/src/original.rs"
    write_policy "$repo" 'files = []'
    commit_base "$repo"
    run_without_git_env git -C "$repo" mv src/original.rs src/renamed.rs
    write_lines "$repo/src/renamed.rs" 801 renamed
    run_without_git_env git -C "$repo" add -- src/renamed.rs
    assert_scope_failure "rename/$scope" \
        'BLOCK new monolith: src/renamed.rs' "$repo" "$bin_dir" "$scope"
}

test_literal_pathname_matrix() {
    local scope="$1"
    local repo bin_dir path candidate_object output status
    local -a paths=(
        '0:large.rs'
        ':leading-colon.rs'
        ':(glob)evil*.rs'
        ':(exclude)*.rs'
        'src/[bracket].rs'
        'src/star*.rs'
        'src/question?.rs'
        $'src/line\nbreak.rs'
    )
    local -a pathspecs=()
    repo="$(init_repo "literal-paths-$scope")"
    bin_dir="$test_root/literal-paths-$scope-bin"
    write_fake_tokenizer "$bin_dir"
    write_policy "$repo" 'files = []'
    commit_base "$repo"
    printf 'decoy\n' >"$repo/large.rs"
    pathspecs+=(':(top,literal)large.rs')
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
        '^BLOCK new monolith:' "${#paths[@]}" "$output"
}

test_literal_trusted_base_matrix() {
    local scope="$1"
    local repo bin_dir path candidate_object policy=''
    local -a paths=(
        '0:large.rs'
        ':leading-colon.rs'
        ':(glob)evil*.rs'
        ':(exclude)*.rs'
        'src/[bracket].rs'
        'src/star*.rs'
        'src/question?.rs'
    )
    local -a pathspecs=()

    repo="$(init_repo "literal-trusted-base-$scope")"
    bin_dir="$test_root/literal-trusted-base-$scope-bin"
    write_fake_tokenizer "$bin_dir"
    for path in "${paths[@]}"; do
        write_lines "$repo/$path" 801 trusted
        policy+="$(policy_row "$path" 20 801)"
        policy+=$'\n'
        pathspecs+=(":(top,literal)$path")
    done
    write_policy "$repo" "$policy"
    run_without_git_env git -C "$repo" add scripts/monolith/baseline.toml
    run_without_git_env git -C "$repo" add -- "${pathspecs[@]}"
    run_without_git_env git -C "$repo" commit -q -m base
    run_without_git_env git -C "$repo" branch base
    candidate_object="$(run_without_git_env git -C "$repo" rev-parse HEAD)"
    case "$scope" in
        staged)
            assert_success 'literal trusted-base staged paths remain readable' \
                run_checker "$repo" "$bin_dir" staged
            ;;
        head)
            assert_success 'literal trusted-base HEAD paths remain readable' \
                run_checker "$repo" "$bin_dir" head
            ;;
        object)
            assert_success 'literal trusted-base object paths remain readable' \
                run_checker "$repo" "$bin_dir" object --object "$candidate_object"
            ;;
        *) die "unsupported literal trusted-base scope: $scope" ;;
    esac
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
    mkdir -p "$repo/scripts/hooks"
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
    chmod +x \
        "$repo/scripts/monolith/check.sh" \
        "$repo/scripts/hooks/check-pre-push-version-bumps.sh" \
        "$repo/scripts/hooks/check-version-bumped.sh"
    write_fake_tokenizer "$bin_dir"
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
    run_without_git_env git -C "$repo" switch -q --detach "$good_object"
    assert_failure_matching 'deletion ref requires a full-gate receipt' \
        'missing full-gate receipt' \
        run_pre_push "$repo" "$bin_dir" \
        "refs/heads/deleted $zero refs/heads/deleted $good_object" \
        "$version_log" "$full_gate_log"
    [ "$(<"$version_log")" = '--scope head' ] || die 'deletion ref did not run the head validator'
    [ ! -s "$full_gate_log" ] || die 'deletion ref reran the full head gate'
    : >"$version_log"
    : >"$full_gate_log"
    assert_failure_matching 'malformed pre-push stdin blocks' \
        'malformed pre-push reference input' \
        run_pre_push "$repo" "$bin_dir" 'malformed input' "$version_log" "$full_gate_log"
    [ ! -s "$version_log" ] || die 'malformed stdin invoked an object validator'
    [ ! -s "$full_gate_log" ] || die 'malformed stdin ran the full head gate'
}

prepare_aggregate_fixture() {
    local repo="$1"
    local bin_dir="$2"

    mkdir -p "$repo/scripts/hooks" "$repo/scripts/monolith" "$repo/scripts/tests" "$bin_dir"
    cp "$root/justfile" "$repo/justfile"
    cp "$checker" "$repo/scripts/monolith/check.sh"
    cp "$root/scripts/hooks/check-version-bumped.sh" \
        "$repo/scripts/hooks/check-version-bumped.sh"
    cp "$root/scripts/monolith/tokenizer_runner.py" \
        "$repo/scripts/monolith/tokenizer_runner.py"
    cp "$root/scripts/monolith/tokenizer_contract.py" \
        "$repo/scripts/monolith/tokenizer_contract.py"
    printf '#!/usr/bin/env bash\nexit 0\n' >"$repo/scripts/tests/version-check-tests.sh"
    printf '#!/usr/bin/env bash\nexit 0\n' >"$repo/scripts/tests/monolith-check-tests.sh"
    cat >"$bin_dir/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${REQUIRE_NO_REPLACE_OBJECTS:-0}" = 1 ] \
    && [ "${GIT_NO_REPLACE_OBJECTS:-}" != 1 ]; then
    printf 'aggregate did not propagate GIT_NO_REPLACE_OBJECTS=1\n' >&2
    exit 65
fi
case "$1" in
    fmt)
        if [ "${FMT_EXPAND:-0}" = 1 ]; then
            awk 'BEGIN { for (line = 1; line <= 801; line++) print "formatted" }' \
                >src/format.rs
        fi
        ;;
    clippy|deny) ;;
    *)
        printf 'unexpected cargo invocation: %s\n' "$*" >&2
        exit 64
        ;;
esac
SH
    chmod +x \
        "$repo/scripts/hooks/check-version-bumped.sh" \
        "$repo/scripts/monolith/check.sh" \
        "$repo/scripts/tests/version-check-tests.sh" \
        "$repo/scripts/tests/monolith-check-tests.sh" \
        "$bin_dir/cargo"
    write_fake_tokenizer "$bin_dir"
}

commit_aggregate_base() {
    local repo="$1"

    run_without_git_env git -C "$repo" add Cargo.toml src scripts
    run_without_git_env git -C "$repo" commit -q -m base
    run_without_git_env git -C "$repo" branch base
}

test_aggregate_validates_final_index() {
    local repo safe_repo bin_dir safe_bin_dir final_tree validated_tree output

    repo="$(init_repo aggregate-final-index)"
    bin_dir="$test_root/aggregate-final-index-bin"
    prepare_aggregate_fixture "$repo" "$bin_dir"
    printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "0.1.0"\n' >"$repo/Cargo.toml"
    printf 'base\n' >"$repo/src/format.rs"
    write_policy "$repo" 'files = []'
    commit_aggregate_base "$repo"
    printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "0.1.1"\n' >"$repo/Cargo.toml"
    printf 'candidate\n' >"$repo/src/format.rs"
    run_without_git_env git -C "$repo" add Cargo.toml src/format.rs

    if output="$(FMT_EXPAND=1 run_aggregate "$repo" "$bin_dir" 2>&1)"; then
        printf '%s\n' "$output" >&2
        die 'aggregate accepted rustfmt-expanded final index'
    fi
    grep -Fq 'BLOCK new monolith: src/format.rs' <<<"$output" \
        || die 'aggregate did not validate the rustfmt-expanded final index'

    safe_repo="$(init_repo aggregate-safe-index)"
    safe_bin_dir="$test_root/aggregate-safe-index-bin"
    prepare_aggregate_fixture "$safe_repo" "$safe_bin_dir"
    printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "0.1.0"\n' >"$safe_repo/Cargo.toml"
    printf 'base\n' >"$safe_repo/src/format.rs"
    write_policy "$safe_repo" 'files = []'
    commit_aggregate_base "$safe_repo"
    printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "0.1.1"\n' >"$safe_repo/Cargo.toml"
    printf 'candidate\n' >"$safe_repo/src/format.rs"
    run_without_git_env git -C "$safe_repo" add Cargo.toml src/format.rs
    if output="$(
        REQUIRE_NO_REPLACE_OBJECTS=1 FMT_EXPAND=0 \
            run_aggregate "$safe_repo" "$safe_bin_dir" 2>&1
    )"; then
        :
    else
        printf '%s\n' "$output" >&2
        die 'aggregate rejected a safe final index or lost replacement-object isolation'
    fi
    validated_tree="$(sed -n 's/^Final staged tree receipt: //p' <<<"$output")"
    final_tree="$(run_without_git_env git -C "$safe_repo" write-tree)"
    [ "$validated_tree" = "$final_tree" ] \
        || die 'aggregate receipt did not identify the final staged tree'
    assert_output_count 'both staged validators use the final tree' \
        "^Validated staged tree: $final_tree$" 2 "$output"
}

test_fmt_restages_literal_paths() {
    local repo bin_dir
    local -a staged_paths=()

    repo="$(init_repo fmt-literal-brackets)"
    bin_dir="$test_root/fmt-literal-brackets-bin"
    prepare_aggregate_fixture "$repo" "$bin_dir"
    printf 'staged\n' >"$repo/[a].rs"
    printf 'unstaged\n' >"$repo/a.rs"
    run_without_git_env git -C "$repo" add -- ':(top,literal)[a].rs'
    run_fmt_fixture "$repo" "$bin_dir"
    mapfile -d '' -t staged_paths < <(
        run_without_git_env git -C "$repo" diff --cached --name-only -z
    )
    [ "${#staged_paths[@]}" -eq 1 ] && [ "${staged_paths[0]}" = '[a].rs' ] \
        || die 'fmt re-staged a path matched by bracket pathspec magic'

    repo="$(init_repo fmt-literal-exclude)"
    bin_dir="$test_root/fmt-literal-exclude-bin"
    prepare_aggregate_fixture "$repo" "$bin_dir"
    printf 'staged\n' >"$repo/:(exclude)*.rs"
    printf 'unstaged\n' >"$repo/unvalidated.md"
    run_without_git_env git -C "$repo" add -- ':(top,literal):(exclude)*.rs'
    run_fmt_fixture "$repo" "$bin_dir"
    mapfile -d '' -t staged_paths < <(
        run_without_git_env git -C "$repo" diff --cached --name-only -z
    )
    [ "${#staged_paths[@]}" -eq 1 ] \
        && [ "${staged_paths[0]}" = ':(exclude)*.rs' ] \
        || die 'fmt re-staged an unrelated path through exclude pathspec magic'

    repo="$(init_repo fmt-partial-stage-refusal)"
    bin_dir="$test_root/fmt-partial-stage-refusal-bin"
    prepare_aggregate_fixture "$repo" "$bin_dir"
    printf 'base one\nbase two\n' >"$repo/partial.rs"
    run_without_git_env git -C "$repo" add partial.rs
    run_without_git_env git -C "$repo" commit -q -m base
    printf 'staged one\nbase two\n' >"$repo/partial.rs"
    run_without_git_env git -C "$repo" add partial.rs
    printf 'staged one\nunstaged two\n' >"$repo/partial.rs"
    assert_failure_matching 'fmt preserves partial-stage refusal' \
        'refusing -- these files are partially staged' \
        run_fmt_fixture "$repo" "$bin_dir"
}

test_replacement_refs_are_ignored() {
    local repo bin_dir pre_push_bin bad_object good_object transport_remote gated_remote
    local base_repo base_bin base_object replacement_object
    local source_tree received_object received_tree
    local version_log full_gate_log output

    repo="$(init_repo replacement-candidate)"
    bin_dir="$test_root/replacement-candidate-bin"
    write_fake_tokenizer "$bin_dir"
    write_policy "$repo" 'files = []'
    commit_base "$repo"
    write_lines "$repo/src/evil.rs" 801 evil
    run_without_git_env git -C "$repo" add src/evil.rs
    run_without_git_env git -C "$repo" commit -q -m bad
    bad_object="$(run_without_git_env git -C "$repo" rev-parse HEAD)"
    run_without_git_env git -C "$repo" branch bad "$bad_object"
    run_without_git_env git -C "$repo" switch -q -c good base
    mkdir -p "$repo/src"
    printf 'safe\n' >"$repo/src/evil.rs"
    run_without_git_env git -C "$repo" add src/evil.rs
    run_without_git_env git -C "$repo" commit -q -m good
    good_object="$(run_without_git_env git -C "$repo" rev-parse HEAD)"
    run_without_git_env git -C "$repo" replace "$bad_object" "$good_object"
    assert_failure_matching 'replacement candidate cannot hide a monolith' \
        'BLOCK new monolith: src/evil.rs' \
        run_checker "$repo" "$bin_dir" object --object "$bad_object"

    base_repo="$(init_repo replacement-trusted-base)"
    base_bin="$test_root/replacement-trusted-base-bin"
    write_fake_tokenizer "$base_bin"
    write_lines "$base_repo/src/large.rs" 801 trusted
    write_policy "$base_repo" "$(policy_row src/large.rs 20 801)"
    commit_base "$base_repo"
    base_object="$(run_without_git_env git -C "$base_repo" rev-parse base)"
    printf 'candidate\n' >"$base_repo/src/candidate.rs"
    run_without_git_env git -C "$base_repo" add src/candidate.rs
    run_without_git_env git -C "$base_repo" commit -q -m candidate
    run_without_git_env git -C "$base_repo" branch candidate
    run_without_git_env git -C "$base_repo" switch -q -c replacement-base base
    write_policy "$base_repo" "$(policy_row src/large.rs 20 802)"
    run_without_git_env git -C "$base_repo" add scripts/monolith/baseline.toml
    run_without_git_env git -C "$base_repo" commit -q -m replacement-base
    replacement_object="$(run_without_git_env git -C "$base_repo" rev-parse HEAD)"
    run_without_git_env git -C "$base_repo" replace "$base_object" "$replacement_object"
    run_without_git_env git -C "$base_repo" switch -q candidate
    assert_success 'replacement trusted base cannot change monolith authority' \
        run_checker "$base_repo" "$base_bin" head

    pre_push_bin="$test_root/replacement-pre-push-bin"
    prepare_pre_push_fixture "$repo" "$pre_push_bin"
    transport_remote="$test_root/replacement-transport.git"
    run_without_git_env git init -q --bare "$transport_remote"
    push_fixture "$repo" "$transport_remote" refs/heads/bad refs/heads/probe
    source_tree="$(run_without_git_env env GIT_NO_REPLACE_OBJECTS=1 \
        git -C "$repo" rev-parse "${bad_object}^{tree}")"
    received_object="$(run_without_git_env git --git-dir="$transport_remote" \
        rev-parse refs/heads/probe)"
    received_tree="$(run_without_git_env git --git-dir="$transport_remote" \
        rev-parse "${received_object}^{tree}")"
    [ "$received_object" = "$bad_object" ] \
        || die 'ungated local push did not preserve the original malicious object'
    [ "$received_tree" = "$source_tree" ] \
        || die 'ungated local push did not preserve the original malicious tree'

    gated_remote="$test_root/replacement-gated.git"
    run_without_git_env git init -q --bare "$gated_remote"
    install_push_hook "$repo"
    version_log="$repo/version.log"
    full_gate_log="$repo/full-gate.log"
    : >"$version_log"
    : >"$full_gate_log"
    if output="$(gated_push_fixture "$repo" "$pre_push_bin" "$gated_remote" \
        refs/heads/bad refs/heads/main "$version_log" "$full_gate_log" 2>&1)"; then
        printf '%s\n' "$output" >&2
        die 'replacement-ref pre-push accepted the original malicious tree'
    fi
    grep -Fq 'BLOCK new monolith: src/evil.rs' <<<"$output" \
        || die 'replacement-ref pre-push did not inspect the original malicious tree'
    [ "$(wc -l <"$version_log" | tr -d '[:space:]')" -eq 1 ] \
        || die 'replacement-ref pre-push did not parse the complete object stream'
    [ ! -s "$full_gate_log" ] || die 'replacement-ref pre-push ran the full gate'
    if run_without_git_env git --git-dir="$gated_remote" show-ref --verify --quiet \
        refs/heads/main; then
        die 'replacement-ref pre-push failure occurred after transport'
    fi
    if run_without_git_env git --git-dir="$gated_remote" cat-file -e \
        "${bad_object}^{commit}" 2>/dev/null; then
        die 'replacement-ref pre-push failure transported an object before validation'
    fi
}

test_missing_regular_object_blocks_before_transport() {
    local repo bin_dir pre_push_bin control_remote gated_remote source_ref blob_id
    local object_path tree_id candidate remote_main receiver_candidate
    local version_log full_gate_log output

    repo="$(init_repo missing-regular-object)"
    bin_dir="$test_root/missing-regular-object-bin"
    write_fake_tokenizer "$bin_dir"
    write_lines "$repo/src/remote-owned.rs" 801 remote
    write_policy "$repo" 'files = []'
    commit_base "$repo"
    control_remote="$test_root/missing-object-control.git"
    gated_remote="$test_root/missing-object-gated.git"
    run_without_git_env git init -q --bare "$control_remote"
    run_without_git_env git init -q --bare "$gated_remote"
    blob_id="$(run_without_git_env git -C "$repo" rev-parse HEAD:src/remote-owned.rs)"
    source_ref="refs/heads/$(run_without_git_env git -C "$repo" \
        symbolic-ref --short HEAD)"
    push_fixture "$repo" "$control_remote" "$source_ref" refs/heads/main
    push_fixture "$repo" "$gated_remote" "$source_ref" refs/heads/main
    remote_main="$(run_without_git_env git --git-dir="$gated_remote" \
        rev-parse refs/heads/main)"
    [ "$remote_main" = "$(run_without_git_env git -C "$repo" rev-parse HEAD)" ] \
        || die 'local base push did not transfer the remote-owned base object'
    run_without_git_env git --git-dir="$control_remote" cat-file -e "${blob_id}^{blob}" \
        || die 'control receiver does not own the prerequisite blob'
    run_without_git_env git --git-dir="$gated_remote" cat-file -e "${blob_id}^{blob}" \
        || die 'gated receiver does not own the prerequisite blob'
    run_without_git_env git -C "$repo" update-index --add \
        --cacheinfo "100644,$blob_id,src/ghost.rs"
    tree_id="$(run_without_git_env git -C "$repo" write-tree)"
    candidate="$(printf 'missing regular blob\n' \
        | run_without_git_env git -C "$repo" commit-tree "$tree_id" -p HEAD)"
    run_without_git_env git -C "$repo" update-ref refs/heads/bad "$candidate"
    object_path="$(run_without_git_env git -C "$repo" rev-parse --git-path \
        "objects/${blob_id:0:2}/${blob_id:2}")"
    if [[ "$object_path" != /* ]]; then
        object_path="$repo/$object_path"
    fi
    [ -f "$object_path" ] || die 'fixture expected a loose remote-owned blob'
    rm -f -- "$object_path"

    push_fixture "$repo" "$control_remote" refs/heads/bad refs/heads/bad
    receiver_candidate="$(run_without_git_env git --git-dir="$control_remote" \
        rev-parse refs/heads/bad)"
    [ "$receiver_candidate" = "$candidate" ] \
        || die 'ungated receiver did not accept the candidate with its owned blob'

    pre_push_bin="$test_root/missing-regular-object-pre-push-bin"
    prepare_pre_push_fixture "$repo" "$pre_push_bin"
    install_push_hook "$repo"
    version_log="$repo/version.log"
    full_gate_log="$repo/full-gate.log"
    : >"$version_log"
    : >"$full_gate_log"
    if output="$(gated_push_fixture "$repo" "$pre_push_bin" "$gated_remote" \
        refs/heads/bad refs/heads/bad "$version_log" "$full_gate_log" 2>&1)"; then
        printf '%s\n' "$output" >&2
        die 'pre-push accepted a missing regular blob that the receiver already owns'
    fi
    grep -Fq 'cannot materialize candidate snapshot blob for src/ghost.rs' <<<"$output" \
        || die 'missing regular blob did not produce the exact-object failure'
    [ "$(wc -l <"$version_log" | tr -d '[:space:]')" -eq 1 ] \
        || die 'missing regular blob pre-push did not parse the complete object stream'
    [ ! -s "$full_gate_log" ] || die 'missing regular blob pre-push ran the full gate'
    [ "$(run_without_git_env git --git-dir="$gated_remote" \
        rev-parse refs/heads/main)" = "$remote_main" ] \
        || die 'gated receiver main changed after pre-push rejection'
    if run_without_git_env git --git-dir="$gated_remote" show-ref --verify --quiet \
        refs/heads/bad; then
        die 'missing regular blob was transported before pre-push rejection'
    fi
    if run_without_git_env git --git-dir="$gated_remote" cat-file -e \
        "${candidate}^{commit}" 2>/dev/null; then
        die 'missing regular blob pre-push transported an object before validation'
    fi
}
