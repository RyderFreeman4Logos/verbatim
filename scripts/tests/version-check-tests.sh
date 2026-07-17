#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
checker="$root/scripts/hooks/check-version-bumped.sh"
pre_push_checker="$root/scripts/hooks/check-pre-push-version-bumps.sh"
test_tmp_root="$(mktemp -d)"

cleanup() {
    local status=$?
    rm -rf -- "$test_tmp_root"
    exit "$status"
}
trap cleanup EXIT

hostile_hooks="$test_tmp_root/hostile-hooks"
hostile_global_config="$test_tmp_root/hostile.gitconfig"
mkdir -p "$hostile_hooks"
printf '#!/usr/bin/env bash\nexit 99\n' >"$hostile_hooks/pre-commit"
chmod +x "$hostile_hooks/pre-commit"
printf '[commit]\n\tgpgSign = true\n[core]\n\thooksPath = %s\n[init]\n\ttemplateDir = %s\n' \
    "$hostile_hooks" "$hostile_hooks" >"$hostile_global_config"
export GIT_CONFIG_GLOBAL="$hostile_global_config"

run_without_local_git_env() {
    local -a unset_args=()
    local var

    while IFS= read -r var; do
        [ -n "$var" ] || continue
        unset_args+=("-u" "$var")
    done < <(git rev-parse --local-env-vars)

    env \
        "${unset_args[@]}" \
        GIT_CONFIG_GLOBAL=/dev/null \
        GIT_CONFIG_NOSYSTEM=1 \
        GIT_CONFIG_SYSTEM=/dev/null \
        "$@"
}

write_manifest() {
    local path="$1"
    local version="$2"
    printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "%s"\n' \
        "$version" >"$path"
}

init_repo() {
    local name="$1"
    local repo="$test_tmp_root/$name"

    mkdir -p "$repo"
    run_without_local_git_env git -C "$repo" init -q -b main
    run_without_local_git_env git -C "$repo" config user.email "test@example.invalid"
    run_without_local_git_env git -C "$repo" config user.name "Version Test"
    printf '%s\n' "$repo"
}

commit_manifest() {
    local repo="$1"
    local version="$2"
    local message="$3"

    write_manifest "$repo/Cargo.toml" "$version"
    run_without_local_git_env git -C "$repo" add Cargo.toml
    run_without_local_git_env git -C "$repo" commit -q -m "$message"
}

assert_failure_matching() {
    local name="$1"
    local pattern="$2"
    local output
    shift 2

    if output="$("$@" 2>&1)"; then
        printf 'FAIL: expected failure: %s\n%s\n' "$name" "$output" >&2
        exit 1
    fi
    if ! grep -q -- "$pattern" <<<"$output"; then
        printf 'FAIL: unexpected failure for %s\n%s\n' "$name" "$output" >&2
        exit 1
    fi
}

assert_success() {
    local name="$1"
    local output
    shift

    if ! output="$("$@" 2>&1)"; then
        printf 'FAIL: expected success: %s\n%s\n' "$name" "$output" >&2
        exit 1
    fi
}

assert_failure_status_matching() {
    local name="$1"
    local expected_status="$2"
    local pattern="$3"
    local output
    local status
    shift 3

    if output="$("$@" 2>&1)"; then
        printf 'FAIL: expected failure: %s\n%s\n' "$name" "$output" >&2
        exit 1
    else
        status=$?
    fi
    if [ "$status" -ne "$expected_status" ]; then
        printf 'FAIL: expected status %s for %s, got %s\n%s\n' \
            "$expected_status" "$name" "$status" "$output" >&2
        exit 1
    fi
    if ! grep -Fq -- "$pattern" <<<"$output"; then
        printf 'FAIL: unexpected failure for %s\n%s\n' "$name" "$output" >&2
        exit 1
    fi
}

assert_file_contains() {
    local name="$1"
    local expected="$2"
    local path="$3"

    if ! grep -Fqx -- "$expected" "$path"; then
        printf 'FAIL: %s is missing from %s: %s\n' "$name" "$path" "$expected" >&2
        exit 1
    fi
}

assert_file_content() {
    local name="$1"
    local expected="$2"
    local path="$3"
    local actual

    actual="$(<"$path")"
    if [ "$actual" != "$expected" ]; then
        printf 'FAIL: unexpected content for %s\nexpected:\n%s\nactual:\n%s\n' \
            "$name" "$expected" "$actual" >&2
        exit 1
    fi
}

run_version_case() {
    local name="$1"
    local base_version="$2"
    local candidate_version="$3"
    local expected="$4"
    local pattern="$5"
    local repo

    repo="$(init_repo "$name")"
    commit_manifest "$repo" "$base_version" base
    run_without_local_git_env git -C "$repo" branch base
    write_manifest "$repo/Cargo.toml" "$candidate_version"
    run_without_local_git_env git -C "$repo" add Cargo.toml

    (
        cd "$repo"
        case "$expected" in
            success)
                assert_success \
                    "$name staged" \
                    run_without_local_git_env "$checker" --scope staged --base-ref base
                ;;
            failure)
                assert_failure_matching \
                    "$name staged" "$pattern" \
                    run_without_local_git_env "$checker" --scope staged --base-ref base
                ;;
            *)
                printf 'FAIL: unknown expectation: %s\n' "$expected" >&2
                exit 1
                ;;
        esac
    )

    printf '%s\n' "$name" >"$repo/fixture-marker"
    run_without_local_git_env git -C "$repo" add Cargo.toml fixture-marker
    run_without_local_git_env git -C "$repo" commit -q -m candidate
    (
        cd "$repo"
        case "$expected" in
            success)
                assert_success \
                    "$name head" \
                    run_without_local_git_env "$checker" --scope head --base-ref base
                ;;
            failure)
                assert_failure_matching \
                    "$name head" "$pattern" \
                    run_without_local_git_env "$checker" --scope head --base-ref base
                ;;
        esac
    )
}

run_pre_push_path() {
    local repo="$1"
    local refs="$2"
    local validator_log="$3"
    local full_gate_log="$4"
    local failing_object="${5:-}"

    (
        cd "$repo"
        if [ -n "$refs" ]; then
            printf '%s\n' "$refs"
        fi | run_without_local_git_env env \
            BASE_REF=base \
            PATH="$repo/test-bin:$PATH" \
            VERSION_CHECK_FAIL_OBJECT="$failing_object" \
            VERSION_CHECK_FULL_GATE_LOG="$full_gate_log" \
            VERSION_CHECK_VALIDATOR_LOG="$validator_log" \
            "$repo/scripts/hooks/check-pre-push-version-bumps.sh"
    )
}

run_just_recipe() {
    local repo="$1"
    local recipe="$2"
    local scope="$3"

    (
        cd "$repo"
        BASE_REF=base VERSION_CHECK_TEST_SKIP_PRE_PUSH_PATH=1 PATH="$repo/test-bin:$PATH" \
            just "$recipe" "$scope"
    )
}

assert_invalid_scope_rejected() {
    local repo="$1"
    local recipe="$2"
    local payload_name="$3"
    local payload="$4"
    local sentinel="$5"
    local output
    local status

    rm -f -- "$sentinel"
    if output="$(run_just_recipe "$repo" "$recipe" "$payload" 2>&1)"; then
        status=0
    else
        status=$?
    fi
    if [ -e "$sentinel" ]; then
        printf 'FAIL: scope payload executed a command: %s (%s)\n' \
            "$recipe" "$payload_name" >&2
        exit 1
    fi
    if [ "$status" -ne 2 ]; then
        printf 'FAIL: expected status 2 for invalid scope: %s (%s), got %s\n%s\n' \
            "$recipe" "$payload_name" "$status" "$output" >&2
        exit 1
    fi
    if ! grep -Fq -- '--scope must be one of: staged, head' <<<"$output"; then
        printf 'FAIL: invalid scope was not rejected for %s (%s)\n%s\n' \
            "$recipe" "$payload_name" "$output" >&2
        exit 1
    fi
}

prepare_pre_push_fixture() {
    local repo="$1"

    assert_file_contains \
        'pre-push object validator command' \
        '      run: scripts/hooks/check-pre-push-version-bumps.sh' \
        "$root/lefthook.yml"
    assert_file_contains 'pre-push object validator stdin' '      use_stdin: true' "$root/lefthook.yml"
    mkdir -p "$repo/scripts/hooks" "$repo/scripts/tests" "$repo/test-bin"
    cp "$root/justfile" "$repo/justfile"
    cp "$checker" "$repo/scripts/hooks/check-version-bumped.sh"
    cp "$pre_push_checker" "$repo/scripts/hooks/check-pre-push-version-bumps.sh"
    cp "$root/scripts/tests/version-check-tests.sh" "$repo/scripts/tests/version-check-tests.sh"
    chmod +x \
        "$repo/scripts/hooks/check-pre-push-version-bumps.sh" \
        "$repo/scripts/hooks/check-version-bumped.sh" \
        "$repo/scripts/tests/version-check-tests.sh"
    printf '#!/usr/bin/env bash\nexit 0\n' >"$repo/test-bin/cargo"
    chmod +x "$repo/test-bin/cargo"
}

install_pre_push_recorders() {
    local repo="$1"

    printf '%s\n' \
        '#!/usr/bin/env bash' \
        'set -euo pipefail' \
        ': "${VERSION_CHECK_VALIDATOR_LOG:?}"' \
        'if [ "$#" -ne 4 ] || [ "$1" != "--scope" ] || [ "$2" != "object" ] || [ "$3" != "--object" ]; then' \
        '    printf "unexpected validator invocation: %s\\n" "$*" >&2' \
        '    exit 64' \
        'fi' \
        'printf "validator|%s|%s|%s|%s\\n" "$1" "$2" "$3" "$4" >>"$VERSION_CHECK_VALIDATOR_LOG"' \
        'if [ -n "${VERSION_CHECK_FAIL_OBJECT:-}" ] && [ "$4" = "$VERSION_CHECK_FAIL_OBJECT" ]; then' \
        '    printf "fixture validator rejected %s\\n" "$4" >&2' \
        '    exit 1' \
        'fi' \
        >"$repo/scripts/hooks/check-version-bumped.sh"
    printf '%s\n' \
        '#!/usr/bin/env bash' \
        'set -euo pipefail' \
        ': "${VERSION_CHECK_FULL_GATE_LOG:?}"' \
        'if [ "$#" -ne 2 ] || [ "$1" != "pre-commit" ] || [ "$2" != "head" ]; then' \
        '    printf "unexpected full gate invocation: %s\\n" "$*" >&2' \
        '    exit 64' \
        'fi' \
        'printf "%s %s\\n" "$1" "$2" >>"$VERSION_CHECK_FULL_GATE_LOG"' \
        >"$repo/test-bin/just"
    chmod +x "$repo/scripts/hooks/check-version-bumped.sh" "$repo/test-bin/just"
}

# Every ordering case runs against both the staged index and the committed HEAD snapshot.
run_version_case "numeric-ordering" "0.1.9" "0.1.10" success ""
run_version_case "patch-downgrade" "0.1.10" "0.1.9" failure "lower than base"
run_version_case "minor-downgrade" "0.2.0" "0.1.99" failure "lower than base"
run_version_case "major-downgrade" "1.0.0" "0.99.99" failure "lower than base"
run_version_case "equal-version" "0.1.0" "0.1.0" failure "equal SemVer precedence"
run_version_case "prerelease-ordering" "1.0.0-rc.2" "1.0.0-rc.10" success ""
run_version_case "build-metadata-equality" "1.0.0+build.1" "1.0.0+build.2" failure "equal SemVer precedence"
run_version_case "malformed-base" "1.0" "1.0.1" failure "malformed base version"
run_version_case "malformed-candidate" "1.0.0" "1.0" failure "malformed .*snapshot version"

repo="$(init_repo snapshot-isolation)"
commit_manifest "$repo" "0.1.0" base
run_without_local_git_env git -C "$repo" branch base
write_manifest "$repo/Cargo.toml" "0.1.1"
run_without_local_git_env git -C "$repo" add Cargo.toml
write_manifest "$repo/Cargo.toml" "0.1.0"
(
    cd "$repo"
    assert_success \
        "staged check ignores a reverted worktree manifest" \
        run_without_local_git_env "$checker" --scope staged --base-ref base
    assert_failure_matching \
        "HEAD check fails when its manifest version is unchanged" \
        "equal SemVer precedence" \
        run_without_local_git_env "$checker" --scope head --base-ref base
)
run_without_local_git_env git -C "$repo" commit -q -m bump
printf 'worktree bytes are deliberately not TOML\n' >"$repo/Cargo.toml"
touch "$repo/unrelated-worktree-file"
(
    cd "$repo"
    assert_success \
        "HEAD check ignores dirty worktree manifest and unrelated files" \
        run_without_local_git_env "$checker" --scope head --base-ref base
)

if [ "${VERSION_CHECK_TEST_SKIP_PRE_PUSH_PATH:-}" != "1" ]; then
    repo="$(init_repo pre-push-objects)"
    commit_manifest "$repo" "0.1.0" base
    run_without_local_git_env git -C "$repo" branch base
    commit_manifest "$repo" "0.1.1" bumped
    good_object="$(run_without_local_git_env git -C "$repo" rev-parse HEAD)"
    run_without_local_git_env git -C "$repo" tag -a v0.1.1 -m v0.1.1
    tag_object="$(run_without_local_git_env git -C "$repo" rev-parse v0.1.1)"
    run_without_local_git_env git -C "$repo" switch -q -c unchanged base
    printf 'unchanged pushed object\n' >"$repo/unchanged-marker"
    run_without_local_git_env git -C "$repo" add unchanged-marker
    run_without_local_git_env git -C "$repo" commit -q -m unchanged
    unchanged_object="$(run_without_local_git_env git -C "$repo" rev-parse HEAD)"
    run_without_local_git_env git -C "$repo" switch -q main
    prepare_pre_push_fixture "$repo"

    sentinel="$repo/invalid-scope-sentinel"
    scope_payload="staged; touch $sentinel"
    for recipe in check-version-bumped pre-commit-fast pre-commit; do
        assert_invalid_scope_rejected \
            "$repo" "$recipe" \
            'command separator' \
            "$scope_payload" \
            "$sentinel"
    done

    (
        cd "$repo"
        assert_success \
            'object check validates an annotated tag object rather than ambient HEAD' \
            run_without_local_git_env "$checker" --scope object --object "$tag_object" --base-ref base
        assert_failure_matching \
            'object check rejects a non-HEAD pushed commit with unchanged version' \
            'equal SemVer precedence' \
            run_without_local_git_env "$checker" --scope object --object "$unchanged_object" --base-ref base
    )

    write_manifest "$repo/Cargo.toml" "0.1.0"
    run_without_local_git_env git -C "$repo" add Cargo.toml
    install_pre_push_recorders "$repo"
    validator_log="$repo/validator.log"
    full_gate_log="$repo/full-gate.log"
    : >"$validator_log"
    : >"$full_gate_log"
    zero_object='0000000000000000000000000000000000000000'
    multi_ref_input="refs/heads/main $good_object refs/heads/main $zero_object
refs/tags/v0.1.1 $tag_object refs/tags/v0.1.1 $zero_object"
    assert_success \
        'pre-push validates every pushed non-deletion object before the full HEAD gate' \
        run_pre_push_path "$repo" "$multi_ref_input" "$validator_log" "$full_gate_log"
    assert_file_content \
        'pre-push validator calls for multiple refs' \
        "validator|--scope|object|--object|$good_object
validator|--scope|object|--object|$tag_object" \
        "$validator_log"
    assert_file_content 'pre-push full gate call' 'pre-commit head' "$full_gate_log"

    : >"$validator_log"
    : >"$full_gate_log"
    assert_failure_matching \
        'pre-push rejects a non-HEAD pushed object when its validator fails' \
        'fixture validator rejected' \
        run_pre_push_path \
        "$repo" \
        "refs/heads/unchanged $unchanged_object refs/heads/unchanged $zero_object" \
        "$validator_log" \
        "$full_gate_log" \
        "$unchanged_object"
    assert_file_content \
        'failing pre-push validator call' \
        "validator|--scope|object|--object|$unchanged_object" \
        "$validator_log"
    assert_file_content 'full gate is skipped after a validator failure' '' "$full_gate_log"

    : >"$validator_log"
    : >"$full_gate_log"
    assert_failure_matching \
        'pre-push rejects malformed local object IDs before the full gate' \
        'invalid pre-push local object ID' \
        run_pre_push_path \
        "$repo" \
        "refs/heads/malformed not-an-object refs/heads/malformed $zero_object" \
        "$validator_log" \
        "$full_gate_log"
    assert_file_content 'malformed input does not invoke the validator' '' "$validator_log"
    assert_file_content 'malformed input does not invoke the full gate' '' "$full_gate_log"

    : >"$validator_log"
    : >"$full_gate_log"
    assert_failure_matching \
        'pre-push rejects missing reference input' \
        'missing pre-push reference input' \
        run_pre_push_path "$repo" '' "$validator_log" "$full_gate_log"
    assert_file_content 'missing input does not invoke the validator' '' "$validator_log"
    assert_file_content 'missing input does not invoke the full gate' '' "$full_gate_log"

    : >"$validator_log"
    : >"$full_gate_log"
    assert_success \
        'pre-push skips deletions but still runs the full gate' \
        run_pre_push_path \
        "$repo" \
        "refs/heads/deleted $zero_object refs/heads/deleted $good_object" \
        "$validator_log" \
        "$full_gate_log"
    assert_file_content 'deletion does not invoke the object validator' '' "$validator_log"
    assert_file_content 'deletion full gate call' 'pre-commit head' "$full_gate_log"
fi

printf 'version-check-tests: PASS\n'
