#!/usr/bin/env bash
set -euo pipefail
shopt -s inherit_errexit
root="$(git rev-parse --show-toplevel)"
checker="$root/scripts/monolith/check.sh"
tokenizer_runner="$root/scripts/monolith/tokenizer_runner.py"
pre_push_checker="$root/scripts/hooks/check-pre-push-version-bumps.sh"
test_root="$(realpath -e "$(mktemp -d)")"
case_filter="${MONOLITH_TEST_CASE:-}"
registered_case_count=0
executed_case_count=0
declare -A registered_case_names=()
declare -a registered_case_manifest=()
readonly tokenizer_revision="c68d1f804a4c172846716b7be99e9378e16512b7"
readonly checker_outer_timeout_seconds=15
readonly expected_case_manifest_sha256="c42c3a9185d98945b19af7b04f082a5d3a55b76771a8ea1e010666df36c8a622"
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
    env "${unset_args[@]}" \
        -u GIT_TEMPLATE_DIR \
        GIT_CONFIG_GLOBAL=/dev/null \
        GIT_CONFIG_SYSTEM=/dev/null \
        GIT_CONFIG_NOSYSTEM=1 \
        "$@"
}
init_repo() {
    local name="$1"
    local repo="$test_root/$name"
    mkdir -p "$repo" || return
    run_without_git_env git -C "$repo" init -q || return
    run_without_git_env git -C "$repo" config user.email test@example.invalid || return
    run_without_git_env git -C "$repo" config user.name 'Monolith Test' || return
    mkdir -p "$repo/scripts/monolith" "$repo/src" || return
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
            GIT_CONFIG_GLOBAL=/dev/null \
            GIT_CONFIG_SYSTEM=/dev/null \
            GIT_CONFIG_NOSYSTEM=1 \
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
    if [ -n "${registered_case_names[$name]+set}" ]; then
        die "duplicate registered case: $name"
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
    local repo="$1" bin_dir="$2" input="$3" version_log="$4" full_gate_log="$5" compat_bin
    compat_bin="$repo/pre-push-compat-bin"
    mkdir -p "$compat_bin"
    cat >"$compat_bin/just" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "$#" -eq 2 ] && [ "$1" = pre-push-gate ] && [ "$2" = head ]; then
    exec "${MONOLITH_LEGACY_JUST:?}" pre-commit head
fi
exec "${MONOLITH_LEGACY_JUST:?}" "$@"
SH
    chmod +x "$compat_bin/just"
    (
        cd "$repo"
        if [ -n "$input" ]; then
            printf '%s\n' "$input"
        fi | PATH="$compat_bin:$repo/test-bin:$bin_dir:$PATH" BASE_REF=base \
            MONOLITH_LEGACY_JUST="$repo/test-bin/just" \
            VERSION_CHECK_LOG="$version_log" FULL_GATE_LOG="$full_gate_log" \
            run_without_git_env "$repo/scripts/hooks/check-pre-push-version-bumps.sh"
    )
}
push_fixture() {
    local repo="$1" source_ref="$3" destination_ref="$4" remote fixture_path
    local fixture_base_ref fixture_version_log fixture_full_gate_log
    local -a fixture_environment=()
    fixture_path="${5:-}"
    fixture_base_ref="${6:-}"
    fixture_version_log="${7:-}"
    fixture_full_gate_log="${8:-}"
    remote="$(realpath -e -- "$2")" || die 'cannot canonicalize bare fixture'
    case "$remote" in
        "$test_root"/*) ;;
        *) die 'fixture push escaped its temporary root' ;;
    esac
    [ "$(run_without_git_env git --git-dir="$remote" \
        rev-parse --is-bare-repository)" = true ] || die 'push target is not bare'
    if [ -n "$fixture_path" ]; then
        fixture_environment=(
            "PATH=$fixture_path"
            "BASE_REF=${fixture_base_ref:?}"
            "VERSION_CHECK_LOG=${fixture_version_log:?}"
            "FULL_GATE_LOG=${fixture_full_gate_log:?}"
        )
    fi
    run_without_git_env env -u GIT_NO_REPLACE_OBJECTS \
        "${fixture_environment[@]}" GIT_ALLOW_PROTOCOL=file \
        GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
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
    push_fixture "$repo" "$remote" "$source_ref" "$destination_ref" \
        "$repo/test-bin:$bin_dir:$PATH" base "$version_log" "$full_gate_log"
}
# shellcheck source=scripts/tests/gate-fixture-contract-tests.sh
source "$root/scripts/tests/gate-fixture-contract-tests.sh"
# shellcheck source=scripts/tests/monolith-tokenizer-tests.sh
source "$root/scripts/tests/monolith-tokenizer-tests.sh"
# shellcheck source=scripts/tests/monolith-git-security-tests.sh
source "$root/scripts/tests/monolith-git-security-tests.sh"

aggregate_fixture_definition="$(declare -f prepare_aggregate_fixture)"
[ -n "$aggregate_fixture_definition" ] || die 'aggregate fixture helper is unavailable'
eval "${aggregate_fixture_definition/prepare_aggregate_fixture/prepare_aggregate_fixture_without_gate_fixture}"
prepare_aggregate_fixture() {
    local repo="$1"

    prepare_aggregate_fixture_without_gate_fixture "$@"
    printf '#!/usr/bin/env bash\nexit 0\n' >"$repo/scripts/tests/gate-fixture-contract-tests.sh"
    chmod +x "$repo/scripts/tests/gate-fixture-contract-tests.sh"
}

test_hostile_global_system_git_config_is_ignored() {
    local hooks template global_config system_config repo local_hooks bin_dir output fixture_status
    hooks="$test_root/hostile-hooks"
    template="$test_root/hostile-template"
    global_config="$test_root/hostile-global.gitconfig"
    system_config="$test_root/hostile-system.gitconfig"
    mkdir -p "$hooks" "$template/hooks"
    printf '#!/usr/bin/env bash\nexit 99\n' >"$hooks/pre-commit"
    printf '#!/usr/bin/env bash\nexit 98\n' >"$template/hooks/pre-commit"
    chmod +x "$hooks/pre-commit"
    chmod +x "$template/hooks/pre-commit"
    printf '[commit]\n\tgpgSign = true\n' >"$global_config"
    printf '[core]\n\thooksPath = %s\n[init]\n\ttemplateDir = %s\n' \
        "$hooks" "$template" >"$system_config"
    bin_dir="$test_root/hostile-git-bin"
    write_fake_tokenizer "$bin_dir"
    set +e
    output="$(
        (
            unset GIT_CONFIG_NOSYSTEM
            export GIT_CONFIG_GLOBAL="$global_config" GIT_CONFIG_SYSTEM="$system_config"
            export GIT_TEMPLATE_DIR="$template"
            repo="$(init_repo hostile-global-system)" || exit
            local_hooks="$(run_without_git_env git -C "$repo" \
                rev-parse --path-format=absolute --git-path hooks)" || exit
            case "$local_hooks" in
                "$repo"/*) ;;
                *) printf 'fixture hooks escaped repository: %s\n' "$local_hooks" >&2; exit 56 ;;
            esac
            [ ! -e "$local_hooks/pre-commit" ] || {
                printf 'hostile init.templateDir populated the fixture\n' >&2
                exit 57
            }
            printf '#!/usr/bin/env bash\nset -euo pipefail\n: > .fixture-local-pre-commit-ran\n' \
                >"$local_hooks/pre-commit" || exit
            chmod +x "$local_hooks/pre-commit" || exit
            write_policy "$repo" 'files = []' || exit
            printf 'base\n' >"$repo/src/fixture.rs" || exit
            commit_base "$repo" || exit
            [ -e "$repo/.fixture-local-pre-commit-ran" ] || {
                printf 'fixture-local pre-commit hook did not run\n' >&2
                exit 58
            }
            printf 'candidate\n' >"$repo/src/fixture.rs" || exit
            run_without_git_env git -C "$repo" add src/fixture.rs || exit
            run_checker "$repo" "$bin_dir" staged || exit
        ) 2>&1
    )"
    fixture_status=$?
    set -e
    if [ "$fixture_status" -ne 0 ]; then
        printf '%s\n' "$output" >&2
        die 'hostile global/system Git configuration influenced a fixture'
    fi
}
test_constructor_failure_propagates() {
    local bin_dir real_git output status suite selector
    bin_dir="$test_root/constructor-failure-bin"
    real_git="$(command -v git)"
    mkdir -p "$bin_dir"
    cat >"$bin_dir/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
args=("$@")
for ((index = 0; index < $# - 1; index++)); do
    if [ "${args[$index]}" = config ] && [ "${args[$((index + 1))]}" = user.email ]; then
        printf 'DISTINCTIVE-CONSTRUCTOR-FAILURE\n' >&2
        exit 55
    fi
done
exec "${REAL_GIT:?}" "$@"
SH
    chmod +x "$bin_dir/git"
    for suite in monolith version; do
        case "$suite" in
            monolith)
                selector='F2 clean success'
                set +e
                output="$(REAL_GIT="$real_git" PATH="$bin_dir:$PATH" \
                    MONOLITH_TEST_CASE="$selector" \
                    bash "$root/scripts/tests/monolith-check-tests.sh" 2>&1)"
                status=$?
                set -e
                ;;
            version)
                selector='V1 numeric ordering'
                set +e
                output="$(REAL_GIT="$real_git" PATH="$bin_dir:$PATH" \
                    VERSION_CHECK_TEST_CASE="$selector" \
                    bash "$root/scripts/tests/version-check-tests.sh" 2>&1)"
                status=$?
                set -e
                ;;
        esac
        [ "$status" -eq 55 ] || {
            printf '%s\n' "$output" >&2
            die "$suite constructor failure was not propagated immediately (status $status)"
        }
        grep -Fq 'DISTINCTIVE-CONSTRUCTOR-FAILURE' <<<"$output" \
            || die "$suite constructor did not preserve the distinctive failure"
        if grep -Fq 'check-tests: PASS' <<<"$output"; then
            printf '%s\n' "$output" >&2
            die "$suite suite printed PASS after a constructor failure"
        fi
    done
}
test_strict_tokenizer_argv_and_call_log() {
    local bin_dir file repo log output
    local -a calls
    bin_dir="$test_root/strict-tokenizer-bin"
    file="$test_root/strict-tokenizer-input"
    printf 'ordinary fixture input\n' >"$file"
    write_fake_tokenizer "$bin_dir"
    if "$bin_dir/tokuin" --version extra >/dev/null 2>&1; then
        die 'tokenizer fake accepted --version with extra argv'
    fi
    if "$bin_dir/tokuin" estimate --format json --model gpt-4o "$file" >/dev/null 2>&1; then
        die 'tokenizer fake accepted estimate argv in the wrong order'
    fi
    repo="$(prepare_tokenizer_dependency_fixture strict-tokenizer-call-log)"
    log="$test_root/strict-tokenizer-call.log"
    : >"$log"
    set +e
    output="$(TOKUIN_FAKE_CALL_LOG="$log" TOKUIN_FAKE_MAX_CALLS=3 \
        run_checker "$repo" "$bin_dir" staged 2>&1)"
    fixture_status=$?
    set -e
    [ "$fixture_status" -ne 0 ] || die 'strict tokenizer fixture unexpectedly passed its oversized source'
    [ "$(wc -l <"$log" | tr -d '[:space:]')" -eq 3 ] \
        || die 'strict tokenizer fake did not record the exact production call count'
    mapfile -t calls <"$log"
    [ "${calls[0]}" = '--version ' ] || die 'tokenizer version call order mismatch'
    [[ "${calls[1]}" == 'estimate --model gpt-4o --format json '* ]] \
        || die 'tokenizer known-answer call order mismatch'
    [[ "${calls[2]}" == 'estimate --model gpt-4o --format json '* ]] \
        || die 'tokenizer estimate call order mismatch'
    if TOKUIN_FAKE_CALL_LOG="$log" TOKUIN_FAKE_MAX_CALLS=3 \
        "$bin_dir/tokuin" --version >/dev/null 2>&1; then
        die 'strict tokenizer fake accepted an excess call'
    fi
}
test_canonical_baseline_rows() {
    python3 - "$root/scripts/monolith/baseline.toml" "$root/docs/mvp.md" \
        "$tokenizer_revision" <<'PY'
import sys
import tomllib
with open(sys.argv[1], "rb") as fh:
    data = tomllib.load(fh)
rows = data["files"]
if len(rows) != 24:
    raise SystemExit(f"expected 24 baseline rows, got {len(rows)}")
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
test_baseline_row_removal_fails_closed() {
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
    assert_failure_matching 'immutable row removal blocks' 'required row removed for src/refactored.rs' \
        run_checker "$repo" "$bin_dir" staged
}
test_recalibration_cannot_remove_row() {
    local repo bin_dir
    repo="$(init_repo recalibration-removal)"
    bin_dir="$test_root/recalibration-removal-bin"
    write_fake_tokenizer "$bin_dir"
    write_lines "$repo/src/calibrated.rs" 801 base
    write_policy "$repo" "$(policy_row src/calibrated.rs 20 801)"
    commit_base "$repo"
    write_policy "$repo" 'files = []'
    run_without_git_env git -C "$repo" add scripts/monolith/baseline.toml
    assert_failure_matching 'recalibration row removal blocks' 'required row removed for src/calibrated.rs' \
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
    assert_failure_matching 'cap raise blocks' 'cap increased for src/base.rs' \
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

register_modes() {
    local prefix="$1" test_fn="$2" mode
    shift 2
    for mode in staged head object; do
        run_registered_case "$prefix $mode" "$test_fn" "$@" "$mode"
    done
}

run_registered_case 'R0 canonical baseline rows' test_canonical_baseline_rows
run_registered_case 'R0 equal and decreased bounds' test_equal_and_decreased_bounds_pass
run_registered_case 'R0 baseline row removal' test_baseline_row_removal_fails_closed
run_registered_case 'R0 recalibration cannot remove row' test_recalibration_cannot_remove_row
run_registered_case 'R0 line and token growth' test_line_and_token_growth_fail
run_registered_case 'R0 new monolith isolation' test_new_monolith_and_staged_target_isolation
run_registered_case 'R0 generated lockfile exclusion' test_generated_lockfile_is_not_a_monolith
run_registered_case 'R0 policy mutation' test_policy_mutation_fails_closed
run_registered_case 'R0 invalid policy rows' test_issue_rationale_duplicate_and_invalid_rows_fail
run_registered_case 'R0 staged baseline authority' test_staged_baseline_is_authoritative
run_registered_case 'R0 object snapshot tokenizer failures' test_object_snapshot_and_tokenizer_failures
run_registered_case 'R0 pre-push object parser' test_pre_push_object_parser
run_registered_case 'F1 candidate-only staged' test_bootstrap_attack candidate-only staged
run_registered_case 'F1 candidate-only head' test_bootstrap_attack candidate-only head
run_registered_case 'F1 candidate-only object' test_bootstrap_attack candidate-only object
register_modes 'F1 inherited-growth' test_bootstrap_attack inherited-growth
run_registered_case 'R4-F1 below-threshold exact cap' test_ratchet exact
run_registered_case 'R4-F1 below-threshold cap inflation' test_ratchet inflated
run_registered_case 'F2 missing' test_tokenizer_dependency missing
run_registered_case 'F2 wrong version' test_tokenizer_dependency wrong-version
run_registered_case 'F2 clean success' test_tokenizer_dependency documented-success
register_modes 'F3 type-change' test_git_type_change_to_monolith
run_registered_case 'F4 invalid zero' test_invalid_tokenizer_timeout 0
run_registered_case 'F4 invalid text' test_invalid_tokenizer_timeout invalid
run_registered_case 'F4 invalid excessive' test_invalid_tokenizer_timeout 301
run_registered_case 'F4 timeout cleanup' test_tokenizer_timeout_cleans_process_tree
register_modes 'R2-A literal paths' test_literal_pathname_matrix
register_modes 'R2-B literal trusted base' test_literal_trusted_base_matrix
register_modes 'R2-B rename' test_git_rename_to_monolith
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
run_registered_case 'R2-E hostile global-system Git config' test_hostile_global_system_git_config_is_ignored
run_registered_case 'R2-E constructor failure propagation' test_constructor_failure_propagates
run_registered_case 'R2-E strict tokenizer argv and call log' test_strict_tokenizer_argv_and_call_log
[ "$registered_case_count" -eq 53 ] || die "registered: $registered_case_count/53"
actual_case_manifest_sha256="$(
    printf '%s\n' "${registered_case_manifest[@]}" | case_manifest_sha256
)"
[ "$actual_case_manifest_sha256" = "$expected_case_manifest_sha256" ] \
    || die "case manifest mismatch: $actual_case_manifest_sha256"
if [ -n "$case_filter" ]; then
    [ "$executed_case_count" -eq 1 ] || die "selected: $executed_case_count"
else
    [ "$executed_case_count" -eq "$registered_case_count" ] || die 'case count mismatch'
fi
printf 'monolith-check-tests: PASS (%s Tier-4 cases)\n' "$executed_case_count"
