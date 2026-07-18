#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
checker="$root/scripts/hooks/check-version-bumped.sh"
pre_push_checker="$root/scripts/hooks/check-pre-push-version-bumps.sh"
monolith_checker="$root/scripts/monolith/check.sh"
monolith_baseline="$root/scripts/monolith/baseline.toml"
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

assert_success_output() {
    local name="$1"
    local output
    shift

    if ! output="$("$@" 2>&1)"; then
        printf 'FAIL: expected success: %s\n%s\n' "$name" "$output" >&2
        exit 1
    fi
    printf '%s' "$output"
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

assert_output_count() {
    local name="$1"
    local pattern="$2"
    local expected="$3"
    local output="$4"
    local actual

    actual="$(grep -Ec -- "$pattern" <<<"$output")"
    if [ "$actual" -ne "$expected" ]; then
        printf 'FAIL: %s expected %s matches for %s, got %s\n%s\n' \
            "$name" "$expected" "$pattern" "$actual" "$output" >&2
        exit 1
    fi
}

# shellcheck source=scripts/tests/version-pre-push-tests.sh
source "$root/scripts/tests/version-pre-push-tests.sh"

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
    run_version_pre_push_tests
fi

printf 'version-check-tests: PASS\n'
