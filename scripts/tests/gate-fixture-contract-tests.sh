#!/usr/bin/env bash
# shellcheck shell=bash
# Shared hermetic fixture helpers and executable gate-boundary contracts.

case_manifest_sha256() {
    python3 -c 'import hashlib, sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())'
}

write_strict_tokuin_fake() {
    local bin_dir="$1"

    mkdir -p "$bin_dir"
    cat >"$bin_dir/tokuin" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

args=("$@")
phase=""
input=""
if [ "$#" -eq 1 ] && [ "$1" = "--version" ]; then
    phase="version"
elif [ "$#" -eq 6 ] \
    && [ "$1" = "estimate" ] \
    && [ "$2" = "--model" ] \
    && [ "$3" = "gpt-4o" ] \
    && [ "$4" = "--format" ] \
    && [ "$5" = "json" ] \
    && [ -f "$6" ]; then
    phase="estimate"
    input="$6"
else
    printf 'strict tokuin fake: unsupported argv\n' >&2
    exit 64
fi

if [ -n "${TOKUIN_FAKE_MAX_CALLS:-}" ] && [ -z "${TOKUIN_FAKE_CALL_LOG:-}" ]; then
    printf 'strict tokuin fake: maximum call count requires a call log\n' >&2
    exit 64
fi
if [ -n "${TOKUIN_FAKE_CALL_LOG:-}" ]; then
    printf '%q ' "${args[@]}" >>"$TOKUIN_FAKE_CALL_LOG"
    printf '\n' >>"$TOKUIN_FAKE_CALL_LOG"
    if [ -n "${TOKUIN_FAKE_MAX_CALLS:-}" ]; then
        calls="$(wc -l <"$TOKUIN_FAKE_CALL_LOG" | tr -d '[:space:]')"
        case "$TOKUIN_FAKE_MAX_CALLS" in
            ''|*[!0-9]*)
                printf 'strict tokuin fake: invalid maximum call count\n' >&2
                exit 64
                ;;
        esac
        if [ "$calls" -gt "$TOKUIN_FAKE_MAX_CALLS" ]; then
            printf 'strict tokuin fake: call count exceeded\n' >&2
            exit 65
        fi
    fi
fi

if [ "$phase" = version ]; then
    printf 'tokuin %s\n' "${TOKUIN_FAKE_VERSION:-0.3.0}"
    exit 0
fi

if [ "$(wc -c <"$input" | tr -d '[:space:]')" = 33 ] \
    && grep -Fxq 'Verbatim tokenizer attestation v1' "$input"; then
    printf '{"model":"gpt-4o","tokens":7,"input_cost":null,"output_cost":null,"breakdown":null}\n'
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
    natural-124)
        printf 'natural exit 124\n' >&2
        exit 124
        ;;
    payload)
        printf '%s' "${TOKUIN_FAKE_OUTPUT:?}"
        exit 0
        ;;
    output-bomb)
        python3 - <<'PY'
import sys
sys.stdout.write("o" * 4096)
sys.stderr.write("e" * 4096)
PY
        exit 0
        ;;
    normal) ;;
    *)
        printf 'bad fake mode\n' >&2
        exit 64
        ;;
esac

if grep -q 'TOKEN_8002' "$input"; then
    tokens=8002
elif grep -q 'TOKEN_8001' "$input"; then
    tokens=8001
else
    input_bytes="$(wc -c <"$input" | tr -d '[:space:]')"
    if [ "$input_bytes" -lt 20 ]; then
        tokens="$input_bytes"
    else
        tokens=20
    fi
fi
printf '{"model":"gpt-4o","tokens":%s,"input_cost":null,"output_cost":null,"breakdown":null}\n' "$tokens"
SH
    chmod +x "$bin_dir/tokuin"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    set -euo pipefail

    root="$(git rev-parse --show-toplevel)"
    test_root="$(mktemp -d)"
    case_filter="${GATE_FIXTURE_TEST_CASE:-}"
    registered_case_count=0
    executed_case_count=0
    declare -A registered_case_names=()
    declare -a registered_case_manifest=()
    readonly expected_case_manifest_sha256="d354cc95daedeee243ebad139bd70526795d4c1247f6cff28ff179fa7a687685"

    cleanup() {
        rm -rf -- "$test_root"
    }
    trap cleanup EXIT

    die() {
        printf 'FAIL: %s\n' "$*" >&2
        exit 1
    }

    assert_success() {
        local name="$1" output
        shift
        if ! output="$("$@" 2>&1)"; then
            printf '%s\n' "$output" >&2
            die "expected success: $name"
        fi
        printf '%s' "$output"
    }

    assert_failure() {
        local name="$1" output
        shift
        if output="$("$@" 2>&1)"; then
            printf '%s\n' "$output" >&2
            die "expected failure: $name"
        fi
    }

    run_gate_case() {
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
        printf 'CASE-PASS: %s\n' "$name"
    }

    run_isolated_git() {
        env -u GIT_TEMPLATE_DIR \
            GIT_CONFIG_GLOBAL=/dev/null \
            GIT_CONFIG_SYSTEM=/dev/null \
            GIT_CONFIG_NOSYSTEM=1 \
            "$@"
    }

    test_host_tokuin_independence() {
        local hostile_bin output
        hostile_bin="$test_root/host-tokuin-bin"
        mkdir -p "$hostile_bin"
        ln -s /bin/false "$hostile_bin/tokuin"
        output="$(PATH="$hostile_bin:$PATH" JUST_NO_DOTENV=true \
            env -u MONOLITH_TEST_CASE just monolith-check-test 2>&1)" || {
            printf '%s\n' "$output" >&2
            die 'monolith suite depended on host tokuin'
        }
        grep -Fq 'monolith-check-tests: PASS (51 Tier-4 cases)' <<<"$output" \
            || { printf '%s\n' "$output" >&2; die 'monolith suite did not produce its complete manifest receipt'; }
        output="$(PATH="$hostile_bin:$PATH" JUST_NO_DOTENV=true \
            env -u VERSION_CHECK_TEST_CASE -u VERSION_CHECK_TEST_SKIP_PRE_PUSH_PATH \
            just version-check-test 2>&1)" || {
            printf '%s\n' "$output" >&2
            die 'version suite depended on host tokuin'
        }
        grep -Fq 'version-check-tests: PASS (12 Tier-4 cases)' <<<"$output" \
            || die 'version suite did not produce its complete manifest receipt'
        printf 'HOST-TOKUIN-INDEPENDENCE: monolith=51 version=12\n'
    }

    receipt_lines() {
        sed -n -e '/^CASE: /p' -e '/^.*-tests: PASS /p'
    }

    test_canonical_entrypoint_sanitization() {
        local clean_monolith hostile_monolith clean_version hostile_version
        clean_monolith="$(JUST_NO_DOTENV=true env -u MONOLITH_TEST_CASE just monolith-check-test 2>&1)" \
            || {
                printf '%s\n' "$clean_monolith" >&2
                die 'clean canonical monolith suite failed'
            }
        hostile_monolith="$(JUST_NO_DOTENV=true MONOLITH_TEST_CASE='F2 clean success' \
            just monolith-check-test 2>&1)" || {
                printf '%s\n' "$hostile_monolith" >&2
                die 'hostile canonical monolith suite failed'
            }
        clean_version="$(JUST_NO_DOTENV=true env -u VERSION_CHECK_TEST_CASE \
            -u VERSION_CHECK_TEST_SKIP_PRE_PUSH_PATH just version-check-test 2>&1)" || {
                printf '%s\n' "$clean_version" >&2
                die 'clean canonical version suite failed'
            }
        hostile_version="$(JUST_NO_DOTENV=true VERSION_CHECK_TEST_CASE='V1 numeric ordering' \
            VERSION_CHECK_TEST_SKIP_PRE_PUSH_PATH=1 just version-check-test 2>&1)" || {
                printf '%s\n' "$hostile_version" >&2
                die 'hostile canonical version suite failed'
            }
        [ "$(receipt_lines <<<"$clean_monolith")" = "$(receipt_lines <<<"$hostile_monolith")" ] \
            || die 'canonical monolith receipt changed under external selector'
        [ "$(receipt_lines <<<"$clean_version")" = "$(receipt_lines <<<"$hostile_version")" ] \
            || die 'canonical version receipt changed under external skip variable'
    }

    test_canonical_pre_commit_fast_executes_gate_fixture_contracts() {
        local just_binary canonical_bin log output case_count case_unique_count
        local case_pass_count case_pass_unique_count

        just_binary="$(command -v just)"
        [ -x "$just_binary" ] || die "just binary is not executable: $just_binary"
        canonical_bin="$test_root/canonical-pre-commit-bin"
        log="$test_root/canonical-pre-commit.log"
        mkdir -p "$canonical_bin"
        cat >"$canonical_bin/just" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

: "${GATE_FIXTURE_JUST_BINARY:?}"
: "${GATE_FIXTURE_LOG:?}"
: "${GATE_FIXTURE_ROOT:?}"
printf 'just|%s\n' "$*" >>"$GATE_FIXTURE_LOG"
case "$*" in
    fmt|"check-version-bumped head"|"check-monolith head"|clippy|deny) exit 0 ;;
    version-check-test) printf 'version-check-tests: PASS (12 Tier-4 cases)\n' ;;
    monolith-check-test) printf 'monolith-check-tests: PASS (51 Tier-4 cases)\n' ;;
    gate-fixture-contract-test)
        cd "$GATE_FIXTURE_ROOT"
        exec "$GATE_FIXTURE_JUST_BINARY" gate-fixture-contract-test
        ;;
    *) printf 'unexpected canonical inner just invocation: %s\n' "$*" >&2; exit 64 ;;
esac
SH
        chmod +x "$canonical_bin/just"

        output="$(
            cd "$root"
            PATH="$canonical_bin:$PATH" JUST_NO_DOTENV=true \
                GATE_FIXTURE_JUST_BINARY="$just_binary" \
                GATE_FIXTURE_LOG="$log" GATE_FIXTURE_ROOT="$root" \
                GATE_FIXTURE_ORACLE=poisoned \
                GATE_FIXTURE_TEST_CASE='R2-E host Tokuin independence' \
                "$just_binary" pre-commit-fast head 2>&1
        )" || {
            printf '%s\n' "$output" >&2
            die 'canonical pre-commit-fast gate-fixture execution failed'
        }

        [ "$(grep -Fc 'just|gate-fixture-contract-test' "$log")" -eq 1 ] \
            || die 'canonical gate-fixture helper recipe was not dispatched exactly once'
        case_count="$(sed -n '/^CASE: R2-[EFG] /p' <<<"$output" | wc -l | tr -d '[:space:]')"
        case_unique_count="$(sed -n '/^CASE: R2-[EFG] /p' <<<"$output" | sort -u | wc -l | tr -d '[:space:]')"
        case_pass_count="$(sed -n '/^CASE-PASS: R2-[EFG] /p' <<<"$output" | wc -l | tr -d '[:space:]')"
        case_pass_unique_count="$(sed -n '/^CASE-PASS: R2-[EFG] /p' <<<"$output" | sort -u | wc -l | tr -d '[:space:]')"
        [ "$case_count" -eq 6 ] && [ "$case_unique_count" -eq 6 ] \
            || die 'canonical gate-fixture receipt did not execute six unique cases'
        [ "$case_pass_count" -eq 6 ] && [ "$case_pass_unique_count" -eq 6 ] \
            || die 'canonical gate-fixture receipt lacks six positive case completions'
        grep -Fq 'gate-fixture-contract-tests: PASS (6 Tier-4 cases)' <<<"$output" \
            || die 'canonical gate-fixture recipe did not produce its complete receipt'
    }

    write_harness_mutation() {
        local source="$1" target="$2" needle="$3" replacement="$4"
        python3 - "$source" "$target" "$needle" "$replacement" <<'PY'
import sys
from pathlib import Path

source = Path(sys.argv[1])
target = Path(sys.argv[2])
needle = sys.argv[3]
replacement = sys.argv[4]
text = source.read_text(encoding="utf-8")
if text.count(needle) != 1:
    raise SystemExit(
        f"mutation needle count mismatch for {needle!r}: {text.count(needle)}"
    )
target.write_text(text.replace(needle, replacement, 1), encoding="utf-8")
PY
    }

    assert_harness_mutation_rejected() {
        local source="$1" target="$2" selector="$3" needle="$4" replacement="$5" label="$6"
        local output status
        write_harness_mutation "$source" "$target" "$needle" "$replacement"
        set +e
        case "$(basename "$source")" in
            monolith-check-tests.sh)
                output="$(env -u VERSION_CHECK_TEST_CASE \
                    MONOLITH_TEST_CASE="$selector" bash "$target" 2>&1)"
                status=$?
                ;;
            version-check-tests.sh)
                output="$(env -u MONOLITH_TEST_CASE \
                    VERSION_CHECK_TEST_CASE="$selector" bash "$target" 2>&1)"
                status=$?
                ;;
            *) status=64; output='unsupported harness mutation source' ;;
        esac
        set -e
        [ "$status" -ne 0 ] || die "$label mutation passed"
        grep -Eq 'registered:|selected:|manifest mismatch' <<<"$output" || {
            printf '%s\n' "$output" >&2
            die "$label mutation did not trip the manifest ratchet"
        }
        if grep -Fq 'check-tests: PASS' <<<"$output"; then
            printf '%s\n' "$output" >&2
            die "$label mutation printed a full PASS receipt"
        fi
    }

    test_direct_and_registered_mutation_matrix() {
        local monolith_source version_source direct registered
        monolith_source="$root/scripts/tests/monolith-check-tests.sh"
        version_source="$root/scripts/tests/version-check-tests.sh"
        direct="run_registered_case 'R0 canonical baseline rows' test_canonical_baseline_rows"
        registered="run_registered_case 'F1 candidate-only head' test_bootstrap_attack candidate-only head"
        assert_harness_mutation_rejected "$monolith_source" \
            "$test_root/monolith-direct-deleted.sh" \
            'R0 canonical baseline rows' "$direct" '' 'direct-case deletion'
        assert_harness_mutation_rejected "$monolith_source" \
            "$test_root/monolith-direct-bypassed.sh" \
            'R0 canonical baseline rows' "$direct" 'test_canonical_baseline_rows' \
            'direct-case registry bypass'
        assert_harness_mutation_rejected "$monolith_source" \
            "$test_root/monolith-registered-deleted.sh" \
            'F1 candidate-only head' "$registered" '' 'registered-case deletion'
        assert_harness_mutation_rejected "$monolith_source" \
            "$test_root/monolith-registered-bypassed.sh" \
            'F1 candidate-only head' "$registered" \
            'test_bootstrap_attack candidate-only head' 'registered-case registry bypass'
        assert_harness_mutation_rejected "$monolith_source" \
            "$test_root/monolith-case-renamed.sh" \
            'R0 canonical baseline rows RENAMED' "$direct" \
            "run_registered_case 'R0 canonical baseline rows RENAMED' test_canonical_baseline_rows" \
            'monolith case rename'
        assert_harness_mutation_rejected "$version_source" \
            "$test_root/version-case-renamed.sh" \
            'V1 numeric ordering RENAMED' \
            "run_version_case 'V1 numeric ordering' test_version_ordering_case" \
            "run_version_case 'V1 numeric ordering RENAMED' test_version_ordering_case" \
            'version case rename'
        printf 'REGISTRY-MUTATION: mutations=6 blocked=6\n'
    }

    prepare_lefthook_fixture() {
        local repo="$1" lefthook_version
        lefthook_version="$(lefthook version)"
        [ "$lefthook_version" = 2.1.10 ] \
            || die "expected Lefthook 2.1.10, got $lefthook_version"
        mkdir -p "$repo/scripts/hooks" "$repo/scripts/monolith" "$repo/test-bin"
        run_isolated_git git -C "$repo" init -q
        cp "$root/lefthook.yml" "$repo/lefthook.yml"
        cp "$root/scripts/hooks/check-pre-push-version-bumps.sh" \
            "$repo/scripts/hooks/check-pre-push-version-bumps.sh"
        chmod +x "$repo/scripts/hooks/check-pre-push-version-bumps.sh"
        cat >"$repo/scripts/hooks/check-version-bumped.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'version|%s\n' "$*" >>"$GATE_FIXTURE_LOG"
SH
        cat >"$repo/scripts/monolith/check.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'monolith|%s\n' "$*" >>"$GATE_FIXTURE_LOG"
SH
        cat >"$repo/test-bin/just" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'just|%s\n' "$*" >>"$GATE_FIXTURE_LOG"
if [ "$#" -eq 2 ] && [ "$1" = pre-push-gate ] && [ "$2" = head ]; then
    printf 'pre-push-gate: PARTIAL PASS (head)\n'
    exit 0
fi
if [ "$#" -eq 1 ] && [ "$1" = pre-commit-fast ]; then
    printf 'pre-commit-fast: fixture PASS\n'
    exit 0
fi
printf 'unexpected just invocation: %s\n' "$*" >&2
exit 64
SH
        chmod +x "$repo/scripts/hooks/check-version-bumped.sh" \
            "$repo/scripts/monolith/check.sh" "$repo/test-bin/just"
    }

    run_lefthook_pre_push() {
        local repo="$1" refs="$2"
        printf '%s\n' "$refs" | (
            cd "$repo"
            run_isolated_git env GATE_FIXTURE_LOG="${GATE_FIXTURE_LOG:?}" \
                PATH="$repo/test-bin:$PATH" LEFTHOOK_NO_AUTO_INSTALL=1 \
                lefthook run pre-push --command version-bump --force --no-auto-install
        )
    }

    test_lefthook_pre_push_boundary() {
        local repo log object refs output expected
        repo="$test_root/lefthook-pre-push"
        log="$repo/boundary.log"
        prepare_lefthook_fixture "$repo"
        : >"$log"
        object='1111111111111111111111111111111111111111'
        refs="refs/heads/main $object refs/heads/main $object
refs/tags/v0.1.1 $object refs/tags/v0.1.1 $object"
        output="$(GATE_FIXTURE_LOG="$log" run_lefthook_pre_push "$repo" "$refs" 2>&1)" || {
            printf '%s\n' "$output" >&2
            die 'real lefthook pre-push boundary failed'
        }
        expected="version|--scope object --object $object
monolith|--scope object --object $object
version|--scope object --object $object
monolith|--scope object --object $object
just|pre-push-gate head"
        [ "$(<"$log")" = "$expected" ] || die 'pre-push boundary did not preserve complete stdin'
        grep -Fq 'pre-push-gate: PARTIAL PASS (head)' <<<"$output" \
            || die 'pre-push boundary did not emit its partial receipt'
    }

    write_lefthook_mutation() {
        local target="$1" mutation="$2"
        python3 - "$root/lefthook.yml" "$target" "$mutation" <<'PY'
import sys
from pathlib import Path

source = Path(sys.argv[1])
target = Path(sys.argv[2])
mutation = sys.argv[3]
text = source.read_text(encoding="utf-8")
if mutation == "malformed":
    changed = "pre-push:\n  commands:\n    version-bump: [\n"
elif mutation == "wrong-hook":
    needle = "pre-push:\n"
    if text.count(needle) != 1:
        raise SystemExit("pre-push hook count mismatch")
    changed = text.replace(needle, "post-push:\n", 1)
elif mutation == "disabled":
    needle = "      use_stdin: true\n"
    if text.count(needle) != 1:
        raise SystemExit("use_stdin count mismatch")
    changed = text.replace(needle, needle + "      skip: true\n", 1)
elif mutation == "removed":
    needle = (
        "    version-bump:\n"
        "      run: scripts/hooks/check-pre-push-version-bumps.sh\n"
        "      use_stdin: true\n"
    )
    if text.count(needle) != 1:
        raise SystemExit("version-bump registration count mismatch")
    changed = text.replace(needle, "", 1)
else:
    raise SystemExit(f"unknown Lefthook mutation: {mutation}")
target.write_text(changed, encoding="utf-8")
PY
    }

    test_lefthook_mutation_matrix() {
        local mutation repo log object refs output lefthook_status detected=0
        object='1111111111111111111111111111111111111111'
        refs="refs/heads/main $object refs/heads/main $object"
        for mutation in malformed wrong-hook disabled removed; do
            repo="$test_root/lefthook-mutation-$mutation"
            log="$repo/boundary.log"
            prepare_lefthook_fixture "$repo"
            write_lefthook_mutation "$repo/lefthook.yml" "$mutation"
            : >"$log"
            set +e
            output="$(GATE_FIXTURE_LOG="$log" run_lefthook_pre_push "$repo" "$refs" 2>&1)"
            lefthook_status=$?
            set -e
            if [ -s "$log" ]; then
                printf '%s\n' "$output" >&2
                die "lefthook mutation executed the protected boundary: $mutation"
            fi
            printf 'LEFTHOOK-MUTATION: %s lefthook-status=%s oracle-status=1 protected-log=empty\n' \
                "$mutation" "$lefthook_status"
            detected=$((detected + 1))
        done
        [ "$detected" -eq 4 ] || die 'lefthook mutation count mismatch'
    }

    test_lefthook_pre_commit_boundary() {
        local repo log output
        repo="$test_root/lefthook-pre-commit"
        log="$repo/boundary.log"
        prepare_lefthook_fixture "$repo"
        : >"$log"
        output="$(
            (
                cd "$repo"
                run_isolated_git env GATE_FIXTURE_LOG="$log" \
                    PATH="$repo/test-bin:$PATH" LEFTHOOK_NO_AUTO_INSTALL=1 \
                    lefthook run pre-commit --command quality-gates --force --no-auto-install
            ) 2>&1
        )" || {
            printf '%s\n' "$output" >&2
            die 'real lefthook pre-commit boundary failed'
        }
        [ "$(<"$log")" = 'just|pre-commit-fast' ] \
            || die 'pre-commit boundary did not dispatch the production aggregate'
        grep -Fq 'pre-commit-fast: fixture PASS' <<<"$output" \
            || die 'pre-commit boundary did not produce its bounded receipt'
    }

    case "${GATE_FIXTURE_ORACLE:-}" in
        canonical-wiring)
            test_canonical_pre_commit_fast_executes_gate_fixture_contracts
            printf 'gate-fixture-contract-wiring-test: PASS\n'
            exit 0
            ;;
        '') ;;
        *) die 'unsupported gate-fixture oracle' ;;
    esac

    run_gate_case 'R2-E host Tokuin independence' test_host_tokuin_independence
    run_gate_case 'R2-F canonical entrypoint sanitization' test_canonical_entrypoint_sanitization
    run_gate_case 'R2-F direct and registered mutation matrix' test_direct_and_registered_mutation_matrix
    run_gate_case 'R2-G lefthook pre-push boundary' test_lefthook_pre_push_boundary
    run_gate_case 'R2-G lefthook mutation matrix' test_lefthook_mutation_matrix
    run_gate_case 'R2-G lefthook pre-commit boundary' test_lefthook_pre_commit_boundary

    [ "$registered_case_count" -eq 6 ] || die "registered: $registered_case_count/6"
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
    printf 'gate-fixture-contract-tests: PASS (%s Tier-4 cases)\n' "$executed_case_count"
fi
