#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
tmp_root="$(mktemp -d)"
trap 'rm -rf "${tmp_root}"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_contains() {
    local file="$1"
    local needle="$2"

    if ! grep -Fq "${needle}" "${file}"; then
        echo "Expected ${file} to contain: ${needle}" >&2
        echo "--- ${file} ---" >&2
        sed -n '1,220p' "${file}" >&2
        fail "missing expected text"
    fi
}

assert_status_count() {
    local state_dir="$1"
    local expected="$2"
    local count_file="${state_dir}/status-count"
    local actual="0"

    if [[ -f "${count_file}" ]]; then
        actual="$(<"${count_file}")"
    fi
    [[ "${actual}" == "${expected}" ]] || fail "expected ${expected} status calls, got ${actual}"
}

write_fake_tools() {
    local fake_bin="$1"
    mkdir -p "${fake_bin}"

    cat > "${fake_bin}/cargo" <<'FAKE_CARGO'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" != "build" ]]; then
    echo "unexpected cargo command: $*" >&2
    exit 64
fi

release_dir="${CARGO_TARGET_DIR:?}/release"
mkdir -p "${release_dir}"

cat > "${release_dir}/verbatim" <<'FAKE_VERBATIM'
#!/usr/bin/env bash
set -euo pipefail

state_dir="${VERBATIM_TEST_STATE_DIR:?}"
case "$*" in
    "--version")
        echo "verbatim test"
        exit 0
        ;;
    "daemon status")
        count_file="${state_dir}/status-count"
        count=0
        if [[ -f "${count_file}" ]]; then
            count="$(<"${count_file}")"
        fi
        count=$((count + 1))
        printf '%s\n' "${count}" > "${count_file}"

        success_on="${VERBATIM_TEST_STATUS_SUCCESS_ON:-0}"
        if [[ "${success_on}" =~ ^[0-9]+$ ]] && (( success_on > 0 && count >= success_on )); then
            echo "Daemon status: ok"
            exit 0
        fi
        echo "Daemon status: unavailable" >&2
        exit 1
        ;;
    *)
        echo "unexpected verbatim command: $*" >&2
        exit 64
        ;;
esac
FAKE_VERBATIM
chmod +x "${release_dir}/verbatim"

cat > "${release_dir}/verbatim-daemon" <<'FAKE_DAEMON'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--version" ]]; then
    echo "verbatim-daemon test"
    exit 0
fi
echo "unexpected verbatim-daemon command: $*" >&2
exit 64
FAKE_DAEMON
chmod +x "${release_dir}/verbatim-daemon"
FAKE_CARGO
    chmod +x "${fake_bin}/cargo"

    cat > "${fake_bin}/systemctl" <<'FAKE_SYSTEMCTL'
#!/usr/bin/env bash
set -euo pipefail

state_dir="${VERBATIM_TEST_STATE_DIR:?}"
printf '%s\n' "$*" >> "${state_dir}/systemctl-args"

if [[ "${1:-}" == "--user" && "${2:-}" == "daemon-reload" ]]; then
    exit 0
fi
if [[ "${1:-}" == "--user" && "${2:-}" == "restart" ]]; then
    exit 0
fi
if [[ "${1:-}" == "--user" && "${2:-}" == "is-active" && "${3:-}" == "--quiet" ]]; then
    exit 0
fi
if [[ "${1:-}" == "--user" && "${2:-}" == "status" ]]; then
    echo "fake systemctl status diagnostic: $*" >&2
    if [[ "${VERBATIM_TEST_DIAGNOSTICS_FAIL:-0}" == "1" ]]; then
        exit 42
    fi
    exit 0
fi

echo "unexpected systemctl command: $*" >&2
exit 64
FAKE_SYSTEMCTL
    chmod +x "${fake_bin}/systemctl"

    cat > "${fake_bin}/journalctl" <<'FAKE_JOURNALCTL'
#!/usr/bin/env bash
set -euo pipefail

state_dir="${VERBATIM_TEST_STATE_DIR:?}"
printf '%s\n' "$*" >> "${state_dir}/journalctl-args"

if [[ "${1:-}" == "--user" && "${2:-}" == "-u" ]]; then
    echo "fake journal diagnostic: $*" >&2
    if [[ "${VERBATIM_TEST_DIAGNOSTICS_FAIL:-0}" == "1" ]]; then
        exit 43
    fi
    exit 0
fi

echo "unexpected journalctl command: $*" >&2
exit 64
FAKE_JOURNALCTL
    chmod +x "${fake_bin}/journalctl"

    cat > "${fake_bin}/sleep" <<'FAKE_SLEEP'
#!/usr/bin/env bash
set -euo pipefail

state_dir="${VERBATIM_TEST_STATE_DIR:?}"
printf '%s\n' "$*" >> "${state_dir}/sleep-args"
exit 0
FAKE_SLEEP
    chmod +x "${fake_bin}/sleep"
}

new_case_dir() {
    local name="$1"
    local case_dir="${tmp_root}/${name}"

    mkdir -p "${case_dir}/home" "${case_dir}/xdg" "${case_dir}/target" "${case_dir}/bin" "${case_dir}/state" "${case_dir}/fake-bin"
    write_fake_tools "${case_dir}/fake-bin"
    printf '%s\n' "${case_dir}"
}

run_recipe() {
    local case_dir="$1"
    shift

    local stdout="${case_dir}/stdout"
    local stderr="${case_dir}/stderr"
    set +e
    (
        cd "${repo_root}"
        env \
            HOME="${case_dir}/home" \
            XDG_CONFIG_HOME="${case_dir}/xdg" \
            CARGO_TARGET_DIR="${case_dir}/target" \
            PATH="${case_dir}/fake-bin:${PATH}" \
            VERBATIM_LOCAL_BIN_DIR="${case_dir}/bin" \
            VERBATIM_TEST_STATE_DIR="${case_dir}/state" \
            "$@" \
            just install-local-daemon
    ) >"${stdout}" 2>"${stderr}"
    local status=$?
    set -e
    printf '%s\n' "${status}"
}

test_default_timeout_uses_90_attempts() {
    local case_dir
    case_dir="$(new_case_dir default-timeout)"

    status="$(run_recipe "${case_dir}" \
        VERBATIM_LOCAL_DAEMON_WRITE_UNIT=1 \
        VERBATIM_TEST_STATUS_SUCCESS_ON=0)"

    [[ "${status}" == "1" ]] || fail "default timeout case exited ${status}, expected 1"
    assert_status_count "${case_dir}/state" 90
    assert_contains "${case_dir}/stderr" "verbatim.service restarted but did not pass daemon status within 90 seconds"
}

test_delayed_success() {
    local case_dir
    case_dir="$(new_case_dir delayed-success)"

    status="$(run_recipe "${case_dir}" \
        VERBATIM_LOCAL_DAEMON_WRITE_UNIT=1 \
        VERBATIM_LOCAL_DAEMON_READINESS_TIMEOUT_SECONDS=3 \
        VERBATIM_TEST_STATUS_SUCCESS_ON=3)"

    [[ "${status}" == "0" ]] || fail "delayed success case exited ${status}, expected 0"
    assert_status_count "${case_dir}/state" 3
    assert_contains "${case_dir}/stdout" "Local verbatim.service daemon deployed"
}

test_timeout_prints_diagnostics() {
    local case_dir
    case_dir="$(new_case_dir timeout-diagnostics)"

    status="$(run_recipe "${case_dir}" \
        VERBATIM_LOCAL_DAEMON_WRITE_UNIT=1 \
        VERBATIM_LOCAL_DAEMON_READINESS_TIMEOUT_SECONDS=2 \
        VERBATIM_TEST_STATUS_SUCCESS_ON=0)"

    [[ "${status}" == "1" ]] || fail "timeout diagnostics case exited ${status}, expected 1"
    assert_status_count "${case_dir}/state" 2
    assert_contains "${case_dir}/stderr" "verbatim.service restarted but did not pass daemon status within 2 seconds"
    assert_contains "${case_dir}/stderr" "systemctl --user status verbatim.service --no-pager -l"
    assert_contains "${case_dir}/stderr" "fake systemctl status diagnostic"
    assert_contains "${case_dir}/stderr" "journalctl --user -u verbatim.service --no-pager"
    assert_contains "${case_dir}/stderr" "fake journal diagnostic"
}

test_diagnostic_failures_do_not_mask_timeout() {
    local case_dir
    case_dir="$(new_case_dir diagnostic-failures)"

    status="$(run_recipe "${case_dir}" \
        VERBATIM_LOCAL_DAEMON_WRITE_UNIT=1 \
        VERBATIM_LOCAL_DAEMON_READINESS_TIMEOUT_SECONDS=1 \
        VERBATIM_TEST_STATUS_SUCCESS_ON=0 \
        VERBATIM_TEST_DIAGNOSTICS_FAIL=1)"

    [[ "${status}" == "1" ]] || fail "diagnostic failure case exited ${status}, expected 1"
    assert_contains "${case_dir}/stderr" "within 1 seconds"
    assert_contains "${case_dir}/stderr" "fake systemctl status diagnostic"
    assert_contains "${case_dir}/stderr" "fake journal diagnostic"
}

test_invalid_timeout_values() {
    local value case_dir status

    for value in "" "0" "-1" "abc"; do
        case_dir="$(new_case_dir "invalid-${value:-empty}")"
        status="$(run_recipe "${case_dir}" \
            VERBATIM_LOCAL_DAEMON_WRITE_UNIT=1 \
            VERBATIM_LOCAL_DAEMON_READINESS_TIMEOUT_SECONDS="${value}" \
            VERBATIM_TEST_STATUS_SUCCESS_ON=1)"

        [[ "${status}" == "2" ]] || fail "invalid timeout '${value}' exited ${status}, expected 2"
        assert_status_count "${case_dir}/state" 0
        assert_contains "${case_dir}/stderr" "VERBATIM_LOCAL_DAEMON_READINESS_TIMEOUT_SECONDS"
    done
}

test_existing_configuration_knobs_still_work() {
    local case_dir unit_path
    case_dir="$(new_case_dir existing-knobs)"
    unit_path="${case_dir}/xdg/systemd/user/custom-verbatim.service"
    mkdir -p "$(dirname "${unit_path}")"
    cat > "${unit_path}" <<UNIT
[Unit]
Description=Custom Verbatim test unit

[Service]
ExecStart=${case_dir}/bin/verbatim-daemon
UNIT

    status="$(run_recipe "${case_dir}" \
        VERBATIM_LOCAL_DAEMON_WRITE_UNIT=0 \
        VERBATIM_SYSTEMD_USER_SERVICE=custom-verbatim \
        VERBATIM_LOCAL_DAEMON_READINESS_TIMEOUT_SECONDS=2 \
        VERBATIM_TEST_STATUS_SUCCESS_ON=1)"

    [[ "${status}" == "0" ]] || fail "existing knob case exited ${status}, expected 0"
    [[ -x "${case_dir}/bin/verbatim" ]] || fail "custom bin dir did not receive verbatim"
    [[ -x "${case_dir}/bin/verbatim-daemon" ]] || fail "custom bin dir did not receive verbatim-daemon"
    [[ ! -e "${case_dir}/xdg/systemd/user/verbatim.service" ]] || fail "default unit was unexpectedly written"
    assert_status_count "${case_dir}/state" 1
    assert_contains "${case_dir}/state/systemctl-args" "restart custom-verbatim.service"
}

test_default_timeout_uses_90_attempts
test_delayed_success
test_timeout_prints_diagnostics
test_diagnostic_failures_do_not_mask_timeout
test_invalid_timeout_values
test_existing_configuration_knobs_still_work

echo "install-local-daemon readiness checks passed"
