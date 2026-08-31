# shellcheck shell=bash
# Git object and pre-push fixtures sourced by version-check-tests.sh.

run_pre_push_path() {
    local repo="$1"
    local refs="$2"
    local validator_log="$3"
    local full_gate_log="$4"
    local failing_object="${5:-}"
    local full_gate_receipt="${6:-}"

    (
        cd "$repo" || exit
        if [ -n "$refs" ]; then
            printf '%s\n' "$refs"
        fi | run_without_local_git_env env \
            BASE_REF=base \
            PATH="$repo/test-bin:$PATH" \
            VERBATIM_FULL_GATE_RECEIPT="$full_gate_receipt" \
            VERSION_CHECK_FAIL_OBJECT="$failing_object" \
            VERSION_CHECK_FULL_GATE_LOG="$full_gate_log" \
            VERSION_CHECK_VALIDATOR_LOG="$validator_log" \
            "$repo/scripts/hooks/check-pre-push-version-bumps.sh"
    )
}

write_full_gate_receipt() {
    local repo="$1"
    local path="$2"
    local head="${3:-$(run_without_local_git_env git -C "$repo" rev-parse HEAD)}"
    local tree="${4:-$(run_without_local_git_env git -C "$repo" rev-parse "$head^{tree}")}"
    local gate_exit="${5:-0}"

    printf 'GATE_COMMAND=just pre-commit head\nPRE_HEAD=%s\nPRE_TREE=%s\nPRE_ATTESTATION=PASS\nINNER_GATE_EXIT=0\nPOST_ATTESTATION=PASS\nGATE_EXIT=%s\n' \
        "$head" "$tree" "$gate_exit" >"$path"
}

run_just_recipe() {
    local repo="$1"
    local recipe="$2"
    local scope="$3"

    (
        cd "$repo" || exit
        BASE_REF=base PATH="$repo/test-bin:$PATH" \
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
    # The assertion must match variables in the fixture verbatim.
    # shellcheck disable=SC2016
    local expected_monolith_object_call='    "$monolith_checker" --scope object --object "$local_object"'

    assert_file_contains \
        'pre-push object validator command' \
        '      run: scripts/hooks/check-pre-push-version-bumps.sh' \
        "$root/lefthook.yml"
    assert_file_contains 'pre-push object validator stdin' '      use_stdin: true' "$root/lefthook.yml"
    assert_file_contains \
        'pre-push monolith object validator command' \
        "$expected_monolith_object_call" \
        "$pre_push_checker"
    mkdir -p "$repo/scripts/hooks" "$repo/scripts/monolith" "$repo/scripts/tests" "$repo/test-bin"
    cp "$root/justfile" "$repo/justfile"
    cp "$checker" "$repo/scripts/hooks/check-version-bumped.sh"
    cp "$pre_push_checker" "$repo/scripts/hooks/check-pre-push-version-bumps.sh"
    cp "$monolith_checker" "$repo/scripts/monolith/check.sh"
    cp "$root/scripts/monolith/tokenizer_runner.py" \
        "$repo/scripts/monolith/tokenizer_runner.py"
    cp "$root/scripts/monolith/tokenizer_contract.py" \
        "$repo/scripts/monolith/tokenizer_contract.py"
    cp "$monolith_baseline" "$repo/scripts/monolith/baseline.toml"
    cp "$root/scripts/tests/version-check-tests.sh" "$repo/scripts/tests/version-check-tests.sh"
    python3 - "$monolith_baseline" "$repo" <<'PY'
import sys
import tomllib
from pathlib import Path

baseline_path = Path(sys.argv[1])
repo = Path(sys.argv[2])
with baseline_path.open("rb") as baseline_file:
    rows = tomllib.load(baseline_file)["files"]
for row in rows:
    fixture_path = repo / row["path"]
    fixture_path.parent.mkdir(parents=True, exist_ok=True)
    fixture_path.write_text("fixture source\n", encoding="utf-8")
PY
    chmod +x \
        "$repo/scripts/hooks/check-pre-push-version-bumps.sh" \
        "$repo/scripts/hooks/check-version-bumped.sh" \
        "$repo/scripts/monolith/check.sh" \
        "$repo/scripts/tests/version-check-tests.sh"
    printf '#!/usr/bin/env bash\nexit 0\n' >"$repo/test-bin/cargo"
    chmod +x "$repo/test-bin/cargo"
    write_strict_tokuin_fake "$repo/test-bin"
}

commit_pre_push_fixture_artifacts() {
    local repo="$1"

    run_without_local_git_env git -C "$repo" add crates justfile scripts
    run_without_local_git_env git -C "$repo" commit -q -m 'install unified hook fixture artifacts'
}

install_pre_push_recorders() {
    local repo="$1"

    # The generated fixture expands these variables when it runs.
    # shellcheck disable=SC2016
    printf '%s\n' \
        '#!/usr/bin/env bash' \
        'set -euo pipefail' \
        ': "${VERSION_CHECK_VALIDATOR_LOG:?}"' \
        'if [ "$#" -eq 2 ] && [ "$1" = "--scope" ] && [ "$2" = "head" ]; then' \
        '    printf "validator|%s|%s\\n" "$1" "$2" >>"$VERSION_CHECK_VALIDATOR_LOG"' \
        '    exit 0' \
        'fi' \
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
    # The generated fixture expands these variables when it runs.
    # shellcheck disable=SC2016
    printf '%s\n' \
        '#!/usr/bin/env bash' \
        'set -euo pipefail' \
        ': "${VERSION_CHECK_FULL_GATE_LOG:?}"' \
        'if [ "$#" -ne 2 ] || [ "$1" != "pre-push-gate" ] || [ "$2" != "head" ]; then' \
        '    printf "unexpected full gate invocation: %s\\n" "$*" >&2' \
        '    exit 64' \
        'fi' \
        'printf "%s %s\\n" "$1" "$2" >>"$VERSION_CHECK_FULL_GATE_LOG"' \
        >"$repo/test-bin/just"
    chmod +x "$repo/scripts/hooks/check-version-bumped.sh" "$repo/test-bin/just"
}

run_version_pre_push_tests() {
    repo="$(init_repo pre-push-objects)"
    prepare_pre_push_fixture "$repo"
    commit_pre_push_fixture_artifacts "$repo"
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

    sentinel="$repo/invalid-scope-sentinel"
    scope_payload="staged; touch $sentinel"
    for recipe in check-version-bumped pre-commit-fast pre-commit pre-push-gate; do
        assert_invalid_scope_rejected \
            "$repo" "$recipe" \
            'command separator' \
            "$scope_payload" \
            "$sentinel"
    done

    (
        cd "$repo" || exit
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
    full_gate_receipt="$repo/full-gate-receipt.log"
    zero_object='0000000000000000000000000000000000000000'
    multi_ref_input="refs/heads/main $good_object refs/heads/main $zero_object
refs/tags/v0.1.1 $tag_object refs/tags/v0.1.1 $zero_object"

    write_full_gate_receipt "$repo" "$full_gate_receipt"
    pre_push_output="$(assert_success_output \
        'pre-push attests a matching full-gate receipt without rerunning it' \
        run_pre_push_path \
        "$repo" "$multi_ref_input" "$validator_log" "$full_gate_log" '' "$full_gate_receipt")"
    assert_file_content \
        'pre-push validator calls for matching receipt' \
        "validator|--scope|object|--object|$good_object
validator|--scope|object|--object|$tag_object
validator|--scope|head" \
        "$validator_log"
    assert_output_count \
        'pre-push monolith validators attest the matching receipt' \
        '^Scope: (object|head)$' \
        3 \
        "$pre_push_output"
    assert_file_content 'matching receipt skips the full gate' '' "$full_gate_log"
    grep -Fq 'attested full-gate receipt' <<<"$pre_push_output" \
        || die 'matching receipt did not emit an attestation'

    : >"$validator_log"
    : >"$full_gate_log"
    assert_failure_matching \
        'pre-push fails closed without an exact full-gate receipt' \
        'missing full-gate receipt' \
        run_pre_push_path "$repo" "$multi_ref_input" "$validator_log" "$full_gate_log"
    assert_file_content 'missing receipt does not invoke the full gate' '' "$full_gate_log"

    write_full_gate_receipt "$repo" "$full_gate_receipt" "$unchanged_object"
    assert_failure_matching \
        'pre-push rejects a full-gate receipt for another HEAD' \
        'does not match HEAD' \
        run_pre_push_path \
        "$repo" "$multi_ref_input" "$validator_log" "$full_gate_log" '' "$full_gate_receipt"
    assert_file_content 'mismatched receipt does not invoke the full gate' '' "$full_gate_log"

    write_full_gate_receipt "$repo" "$full_gate_receipt" '' "$zero_object"
    assert_failure_matching \
        'pre-push rejects a full-gate receipt for another tree' \
        'does not match HEAD tree' \
        run_pre_push_path \
        "$repo" "$multi_ref_input" "$validator_log" "$full_gate_log" '' "$full_gate_receipt"
    assert_file_content 'tree-mismatched receipt does not invoke the full gate' '' "$full_gate_log"

    write_full_gate_receipt "$repo" "$full_gate_receipt" '' '' 1
    assert_failure_matching \
        'pre-push rejects a nonzero full-gate receipt' \
        'gate_exit did not pass' \
        run_pre_push_path \
        "$repo" "$multi_ref_input" "$validator_log" "$full_gate_log" '' "$full_gate_receipt"
    assert_file_content 'nonzero receipt does not invoke the full gate' '' "$full_gate_log"

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
    write_full_gate_receipt "$repo" "$full_gate_receipt"
    assert_success \
        'pre-push skips deletions and attests the full gate' \
        run_pre_push_path \
        "$repo" \
        "refs/heads/deleted $zero_object refs/heads/deleted $good_object" \
        "$validator_log" \
        "$full_gate_log" \
        '' "$full_gate_receipt"
    assert_file_content 'deletion invokes only the head validator' 'validator|--scope|head' "$validator_log"
    assert_file_content 'deletion skips the full gate' '' "$full_gate_log"
}
