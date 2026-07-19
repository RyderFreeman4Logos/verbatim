#!/usr/bin/env bash
set -euo pipefail
shopt -s inherit_errexit

root="$(git rev-parse --show-toplevel)"
checker="$root/scripts/hooks/check-version-bumped.sh"
pre_push_checker="$root/scripts/hooks/check-pre-push-version-bumps.sh"
monolith_checker="$root/scripts/monolith/check.sh"
monolith_baseline="$root/scripts/monolith/baseline.toml"
test_tmp_root="$(mktemp -d)"
case_filter="${VERSION_CHECK_TEST_CASE:-}"
registered_case_count=0
executed_case_count=0
declare -A registered_case_names=()
declare -a registered_case_manifest=()
readonly expected_case_manifest_sha256="ba97017570a3545b04a986f8c17315c75ee3a040cf2b38638c7487227732913d"

cleanup() {
    local status=$?
    rm -rf -- "$test_tmp_root"
    exit "$status"
}
trap cleanup EXIT

hostile_hooks="$test_tmp_root/hostile-hooks"
hostile_template="$test_tmp_root/hostile-template"
hostile_global_config="$test_tmp_root/hostile.gitconfig"
hostile_system_config="$test_tmp_root/hostile-system.gitconfig"
mkdir -p "$hostile_hooks" "$hostile_template/hooks"
printf '#!/usr/bin/env bash\nexit 99\n' >"$hostile_hooks/pre-commit"
printf '#!/usr/bin/env bash\nexit 98\n' >"$hostile_template/hooks/pre-commit"
chmod +x "$hostile_hooks/pre-commit"
chmod +x "$hostile_template/hooks/pre-commit"
printf '[commit]\n\tgpgSign = true\n' >"$hostile_global_config"
printf '[core]\n\thooksPath = %s\n[init]\n\ttemplateDir = %s\n' \
    "$hostile_hooks" "$hostile_template" >"$hostile_system_config"
unset GIT_CONFIG_NOSYSTEM
export GIT_CONFIG_GLOBAL="$hostile_global_config"
export GIT_CONFIG_SYSTEM="$hostile_system_config"
export GIT_TEMPLATE_DIR="$hostile_template"

run_without_local_git_env() {
    local -a unset_args=()
    local var

    while IFS= read -r var; do
        [ -n "$var" ] || continue
        unset_args+=("-u" "$var")
    done < <(git rev-parse --local-env-vars)

    env \
        "${unset_args[@]}" \
        -u GIT_TEMPLATE_DIR \
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

    mkdir -p "$repo" || return
    run_without_local_git_env git -C "$repo" init -q -b main || return
    run_without_local_git_env git -C "$repo" config user.email "test@example.invalid" || return
    run_without_local_git_env git -C "$repo" config user.name "Version Test" || return
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

# shellcheck source=scripts/tests/gate-fixture-contract-tests.sh
source "$root/scripts/tests/gate-fixture-contract-tests.sh"
# shellcheck source=scripts/tests/version-pre-push-tests.sh
source "$root/scripts/tests/version-pre-push-tests.sh"

run_version_case() {
    local name="$1"
    shift
    if [ -n "${registered_case_names[$name]+set}" ]; then
        printf 'FAIL: duplicate registered case: %s\n' "$name" >&2
        exit 1
    fi
    registered_case_names["$name"]=1
    registered_case_manifest+=("$name")
    registered_case_count=$((registered_case_count + 1))
    if [ -n "$case_filter" ] && [ "$case_filter" != "$name" ]; then
        return 0
    fi
    executed_case_count=$((executed_case_count + 1))
    printf 'CASE: %s\n' "$name"
    "$@"
}

test_version_ordering_case() {
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

test_version_snapshot_isolation() {
    local repo hooks
    repo="$(init_repo snapshot-isolation)"
    hooks="$(run_without_local_git_env git -C "$repo" \
        rev-parse --path-format=absolute --git-path hooks)"
    case "$hooks" in
        "$repo"/*) ;;
        *) printf 'FAIL: fixture hooks escaped repository: %s\n' "$hooks" >&2; exit 1 ;;
    esac
    [ ! -e "$hooks/pre-commit" ] || {
        printf 'FAIL: hostile init.templateDir populated the fixture\n' >&2
        exit 1
    }
    printf '#!/usr/bin/env bash\nset -euo pipefail\n: > .fixture-local-pre-commit-ran\n' \
        >"$hooks/pre-commit"
    chmod +x "$hooks/pre-commit"
    commit_manifest "$repo" "0.1.0" base
    [ -e "$repo/.fixture-local-pre-commit-ran" ] || {
        printf 'FAIL: fixture-local pre-commit hook did not run\n' >&2
        exit 1
    }
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
}

test_version_snapshot_object_authority() {
    local replacement_repo base_repo missing_repo bad_object good_object base_object benign_object
    local missing_object missing_tree

    replacement_repo="$(init_repo version-replacement-candidate)"
    commit_manifest "$replacement_repo" "0.1.0" base
    run_without_local_git_env git -C "$replacement_repo" branch base
    printf 'unchanged version candidate\n' >"$replacement_repo/marker"
    run_without_local_git_env git -C "$replacement_repo" add marker
    run_without_local_git_env git -C "$replacement_repo" commit -q -m bad
    bad_object="$(run_without_local_git_env git -C "$replacement_repo" rev-parse HEAD)"
    run_without_local_git_env git -C "$replacement_repo" switch -q -c good base
    commit_manifest "$replacement_repo" "0.1.1" good
    good_object="$(run_without_local_git_env git -C "$replacement_repo" rev-parse HEAD)"
    run_without_local_git_env git -C "$replacement_repo" replace "$bad_object" "$good_object"
    (
        cd "$replacement_repo"
        assert_failure_matching \
            'replacement candidate cannot hide an unchanged workspace version' \
            'equal SemVer precedence' \
            run_without_local_git_env "$checker" --scope object --object "$bad_object" --base-ref base
    )

    base_repo="$(init_repo version-replacement-base)"
    commit_manifest "$base_repo" "0.1.0" base
    base_object="$(run_without_local_git_env git -C "$base_repo" rev-parse HEAD)"
    run_without_local_git_env git -C "$base_repo" branch base
    run_without_local_git_env git -C "$base_repo" switch -q -c candidate base
    commit_manifest "$base_repo" "0.1.1" candidate
    run_without_local_git_env git -C "$base_repo" switch -q -c benign-base base
    commit_manifest "$base_repo" "9.0.0" benign-base
    benign_object="$(run_without_local_git_env git -C "$base_repo" rev-parse HEAD)"
    run_without_local_git_env git -C "$base_repo" replace "$base_object" "$benign_object"
    run_without_local_git_env git -C "$base_repo" switch -q candidate
    (
        cd "$base_repo"
        assert_success \
            'replacement trusted base cannot change version authority' \
            run_without_local_git_env "$checker" --scope head --base-ref base
    )

    missing_repo="$(init_repo version-missing-cargo-blob)"
    commit_manifest "$missing_repo" "0.1.0" base
    run_without_local_git_env git -C "$missing_repo" branch base
    missing_object="1111111111111111111111111111111111111111"
    missing_tree="$(printf '100644 blob %s\tCargo.toml\0' "$missing_object" \
        | run_without_local_git_env git -C "$missing_repo" mktree -z --missing)"
    bad_object="$(printf 'missing cargo blob\n' \
        | run_without_local_git_env git -C "$missing_repo" commit-tree "$missing_tree" -p base)"
    (
        cd "$missing_repo"
        assert_failure_matching \
            'missing regular Cargo.toml blob has a named object failure' \
            'cannot materialize object snapshot Cargo.toml blob' \
            run_without_local_git_env "$checker" --scope object --object "$bad_object" --base-ref base
    )
}

# Every ordering case runs against both the staged index and the committed HEAD snapshot.
run_version_case 'V1 numeric ordering' test_version_ordering_case numeric-ordering 0.1.9 0.1.10 success ''
run_version_case 'V2 patch downgrade' test_version_ordering_case patch-downgrade 0.1.10 0.1.9 failure 'lower than base'
run_version_case 'V3 minor downgrade' test_version_ordering_case minor-downgrade 0.2.0 0.1.99 failure 'lower than base'
run_version_case 'V4 major downgrade' test_version_ordering_case major-downgrade 1.0.0 0.99.99 failure 'lower than base'
run_version_case 'V5 equal version' test_version_ordering_case equal-version 0.1.0 0.1.0 failure 'equal SemVer precedence'
run_version_case 'V6 prerelease ordering' test_version_ordering_case prerelease-ordering 1.0.0-rc.2 1.0.0-rc.10 success ''
run_version_case 'V7 build metadata equality' test_version_ordering_case build-metadata-equality 1.0.0+build.1 1.0.0+build.2 failure 'equal SemVer precedence'
run_version_case 'V8 malformed base' test_version_ordering_case malformed-base 1.0 1.0.1 failure 'malformed base version'
run_version_case 'V9 malformed candidate' test_version_ordering_case malformed-candidate 1.0.0 1.0 failure 'malformed .*snapshot version'
run_version_case 'V10 snapshot isolation' test_version_snapshot_isolation
run_version_case 'V11 object authority' test_version_snapshot_object_authority
run_version_case 'V12 pre-push path' run_version_pre_push_tests

[ "$registered_case_count" -eq 12 ] || {
    printf 'FAIL: registered: %s/12\n' "$registered_case_count" >&2
    exit 1
}
actual_case_manifest_sha256="$(
    printf '%s\n' "${registered_case_manifest[@]}" | case_manifest_sha256
)"
if [ "$actual_case_manifest_sha256" != "$expected_case_manifest_sha256" ]; then
    printf 'FAIL: case manifest mismatch: %s\n' "$actual_case_manifest_sha256" >&2
    exit 1
fi
if [ -n "$case_filter" ]; then
    [ "$executed_case_count" -eq 1 ] || {
        printf 'FAIL: selected: %s\n' "$executed_case_count" >&2
        exit 1
    }
else
    [ "$executed_case_count" -eq "$registered_case_count" ] || {
        printf 'FAIL: case count mismatch\n' >&2
        exit 1
    }
fi
printf 'version-check-tests: PASS (%s Tier-4 cases)\n' "$executed_case_count"
