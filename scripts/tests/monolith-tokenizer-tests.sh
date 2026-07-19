# shellcheck shell=bash
# Tokenizer-specific fixtures sourced by monolith-check-tests.sh.

write_fake_tokenizer() {
    write_strict_tokuin_fake "$1"
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
        'tokenizer protocol failure.*json-framing' \
        run_checker "$malformed_repo" "$malformed_bin" staged
}

prepare_tokenizer_dependency_fixture() {
    local name="$1"
    local repo
    repo="$(init_repo "$name")" || return
    printf 'base\n' >"$repo/src/tokenizer.rs" || return
    write_policy "$repo" 'files = []' || return
    commit_base "$repo" || return
    write_lines "$repo/src/tokenizer.rs" 801 candidate || return
    run_without_git_env git -C "$repo" add src/tokenizer.rs || return
    printf '%s\n' "$repo"
}

prepare_tokenizer_ratchet_fixture() {
    local name="$1" repo
    repo="$(init_repo "$name")" || return
    write_lines "$repo/src/tokenizer.rs" 801 base || return
    write_policy "$repo" "$(policy_row src/tokenizer.rs 20 801)" || return
    commit_base "$repo" || return
    write_lines "$repo/src/tokenizer.rs" 801 candidate || return
    run_without_git_env git -C "$repo" add src/tokenizer.rs || return
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
    if [ "$mode" = wrong-version ]; then
        TOKUIN_FAKE_VERSION=9.9.9
        export TOKUIN_FAKE_VERSION
        assert_failure_matching 'wrong tokenizer version' \
            'tokenizer version protocol failure: version-framing' \
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
        124|137) die "checker exceeded the independent ${checker_outer_timeout_seconds}-second test bound" ;;
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

run_runner_fixture() {
    local timeout_seconds="$1"
    local max_output_bytes="$2"
    local prefix="$3"
    shift 3
    python3 -B "$tokenizer_runner" run \
        --timeout-seconds "$timeout_seconds" \
        --max-output-bytes "$max_output_bytes" \
        --stdout "$prefix.stdout" \
        --stderr "$prefix.stderr" \
        --receipt "$prefix.receipt.json" \
        -- "$@"
}

assert_runner_receipt() {
    local prefix="$1"
    local expected_outcome="$2"
    local expected_status="$3"
    python3 - "$prefix.receipt.json" "$expected_outcome" "$expected_status" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
expected_outcome = sys.argv[2]
expected_status_text = sys.argv[3]
data = json.loads(path.read_text(encoding="utf-8"))
if data.get("outcome") != expected_outcome:
    raise SystemExit(
        f"receipt outcome mismatch: expected {expected_outcome}, got {data.get('outcome')}"
    )
if expected_status_text != "any":
    expected_status = None if expected_status_text == "null" else int(expected_status_text)
    if data.get("status") != expected_status:
        raise SystemExit(
            f"receipt status mismatch: expected {expected_status}, got {data.get('status')}"
        )
PY
}

assert_no_live_identities() {
    local pid_file="$1"
    local label="$2"
    python3 - "$pid_file" "$label" <<'PY'
import ctypes
import os
from pathlib import Path
import signal
import sys
import time

path = Path(sys.argv[1])
label = sys.argv[2]
identities = []
for line in path.read_text(encoding="utf-8").splitlines() if path.exists() else []:
    fields = line.split()
    if len(fields) != 2 or not all(field.isdigit() for field in fields):
        raise SystemExit(f"invalid lifecycle identity: {line!r}")
    identities.append((int(fields[0]), int(fields[1])))


def start_time(pid: int) -> int | None:
    try:
        raw = Path(f"/proc/{pid}/stat").read_text(encoding="ascii")
        return int(raw[raw.rfind(") ") + 2 :].split()[19])
    except (FileNotFoundError, ProcessLookupError):
        return None


residual = [(pid, start) for pid, start in identities if start_time(pid) == start]
libc = ctypes.CDLL(None, use_errno=True)
for pid, start in residual:
    try:
        descriptor = libc.pidfd_open(pid, 0)
        if descriptor < 0:
            raise ProcessLookupError(pid)
        if start_time(pid) == start:
            libc.pidfd_send_signal(descriptor, signal.SIGKILL, None, 0)
        os.close(descriptor)
    except (ProcessLookupError, FileNotFoundError):
        pass
for _ in range(50):
    if not any(start_time(pid) == start for pid, start in residual):
        break
    time.sleep(0.02)
print(f"RESIDUAL-SCAN: {label} identities={len(identities)} residual={len(residual)}")
if residual:
    raise SystemExit(f"tokenizer lifecycle fixture leaked identities: {residual}")
PY
}

wait_for_pid_file() {
    local pid_file="$1"
    local attempt
    for attempt in $(seq 1 100); do
        [ -s "$pid_file" ] && return 0
        sleep 0.02
    done
    die "lifecycle fixture did not publish an identity: $pid_file"
}

write_cd_tokenizer() {
    local path="$1"
    mkdir -p "$(dirname "$path")"
    cat >"$path" <<'PY'
#!/usr/bin/env python3
import json
import os
from pathlib import Path
import signal
import sys
import time

KNOWN = b"Verbatim tokenizer attestation v1"
args = sys.argv[1:]
if args == ["--version"]:
    phase = "version"
    content = None
elif len(args) == 6 and args[:5] == ["estimate", "--model", "gpt-4o", "--format", "json"]:
    content = Path(args[5]).read_bytes()
    phase = "known-answer" if content == KNOWN else "estimate"
else:
    raise SystemExit(64)
mode = os.environ.get("TOKUIN_FAKE_LIFECYCLE_MODE", "normal")
if os.environ.get("TOKUIN_FAKE_TARGET") != phase:
    mode = "normal"


def emit(tokens: int | None = None) -> None:
    if phase == "version":
        os.write(1, b"tokuin 0.3.0\n")
        return
    if tokens is None:
        tokens = 7 if content == KNOWN else 20
    payload = {"model": "gpt-4o", "tokens": tokens, "input_cost": None,
               "output_cost": None, "breakdown": None}
    os.write(1, json.dumps(payload, separators=(",", ":")).encode() + b"\n")


def publish() -> None:
    raw = Path("/proc/self/stat").read_text(encoding="ascii")
    start = raw[raw.rfind(") ") + 2 :].split()[19]
    descriptor = os.open(os.environ["TOKUIN_FAKE_PID_FILE"],
                         os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    os.write(descriptor, f"{os.getpid()} {start}\n".encode())
    os.close(descriptor)


def spawn(style: str, keep_output: bool) -> None:
    child = os.fork()
    if child != 0:
        time.sleep(0.05)
        return
    if style == "setpgid":
        os.setpgid(0, 0)
    elif style == "setsid":
        os.setsid()
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    publish()
    if not keep_output:
        os.close(1)
        os.close(2)
    while True:
        time.sleep(60)


if mode.startswith("leader-"):
    _, style, result = mode.split("-")
    spawn(style, False)
    raise SystemExit(0 if result == "success" else 23)
if mode == "retain-output":
    spawn("same", True)
    while True:
        time.sleep(60)
if mode == "timeout-tree":
    spawn("setsid", False)
    while True:
        time.sleep(60)
if mode == "failure":
    os.write(2, b"ordinary failure\n")
    raise SystemExit(23)
if mode == "exit-124":
    raise SystemExit(124)
if mode == "overflow":
    spawn("setsid", False)
    stream = 1 if os.environ.get("TOKUIN_FAKE_STREAM") == "stdout" else 2
    os.write(stream, b"x" * 8192)
    while True:
        time.sleep(60)
if mode == "raw":
    os.write(1, bytes.fromhex(os.environ.get("TOKUIN_FAKE_OUTPUT_HEX", "")))
    os.write(2, bytes.fromhex(os.environ.get("TOKUIN_FAKE_STDERR_HEX", "")))
    raise SystemExit(0)
if mode == "wrong-known-answer":
    emit(8)
    raise SystemExit(0)
if mode == "success-stderr":
    emit()
    os.write(2, b"unexpected success stderr\n")
    raise SystemExit(0)
emit()
PY
    chmod +x "$path"
}

run_cd_checker() {
    local repo="$1" bin_dir="$2" target="$3" mode="$4" pid_file="$5" stream="${6:-}"
    local timeout_seconds="${MONOLITH_FIXTURE_TIMEOUT_SECONDS:-1}"
    TOKUIN_FAKE_TARGET="$target" TOKUIN_FAKE_LIFECYCLE_MODE="$mode" \
        TOKUIN_FAKE_PID_FILE="$pid_file" TOKUIN_FAKE_STREAM="$stream" \
        MONOLITH_TOKENIZER_TIMEOUT_SECONDS="$timeout_seconds" \
        MONOLITH_TOKENIZER_MAX_OUTPUT_BYTES=1024 \
        run_checker_bounded "$repo" "$bin_dir" staged
}

assert_procfs_process_disappearance_is_ignored() {
    python3 -B - "$tokenizer_runner" <<'PY'
import errno
import importlib.util
import os
from pathlib import Path
import sys

runner_path = Path(sys.argv[1])
sys.path.insert(0, str(runner_path.parent))
spec = importlib.util.spec_from_file_location("verbatim_tokenizer_runner_probe", runner_path)
if spec is None or spec.loader is None:
    raise SystemExit("cannot load tokenizer runner probe")
runner = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = runner
spec.loader.exec_module(runner)

probe_entry = Path("/proc/424242")
real_iterdir = Path.iterdir
real_read_text = Path.read_text
hits = []


def fake_iterdir(path):
    if path == Path("/proc"):
        return iter((probe_entry,))
    return real_iterdir(path)


def fake_read_text(path, *args, **kwargs):
    if path == probe_entry / "stat":
        hits.append(path)
        raise ProcessLookupError(errno.ESRCH, os.strerror(errno.ESRCH))
    return real_read_text(path, *args, **kwargs)


Path.iterdir = fake_iterdir
Path.read_text = fake_read_text
try:
    table = runner.read_process_table()
finally:
    Path.iterdir = real_iterdir
    Path.read_text = real_read_text
if hits != [probe_entry / "stat"] or table:
    raise SystemExit(f"procfs disappearance probe failed: hits={hits!r} table={table!r}")
PY
    printf 'PROCFS-CHURN: esrch=ignored\n'
}

test_runner_process_lifecycle_matrix() {
    local repo bin_dir fake prefix pid_file runner_pid output status phase mode stream cases=0
    local MONOLITH_FIXTURE_TIMEOUT_SECONDS=3
    assert_procfs_process_disappearance_is_ignored
    repo="$(prepare_tokenizer_ratchet_fixture tokenizer-cd-process)"
    bin_dir="$test_root/tokenizer-cd-process-bin"
    fake="$bin_dir/tokuin"
    write_cd_tokenizer "$fake"
    assert_success 'normal version and estimate' run_cd_checker \
        "$repo" "$bin_dir" none normal "$test_root/normal.pids"
    for phase in version estimate; do
        for mode in \
            leader-same-success leader-same-failure \
            leader-setpgid-success leader-setpgid-failure \
            leader-setsid-success leader-setsid-failure \
            retain-output timeout-tree; do
            pid_file="$test_root/${phase}-${mode}.identities"
            : >"$pid_file"
            set +e
            output="$(run_cd_checker "$repo" "$bin_dir" "$phase" "$mode" "$pid_file" 2>&1)"
            status=$?
            set -e
            [ "$status" -ne 0 ] || die "$phase $mode unexpectedly succeeded"
            case "$status" in 124|137) die "$phase $mode exceeded the outer bound" ;; esac
            assert_no_live_identities "$pid_file" "$phase/$mode"
            cases=$((cases + 1))
        done
        for mode in failure exit-124; do
            pid_file="$test_root/${phase}-${mode}.identities"
            : >"$pid_file"
            set +e
            output="$(run_cd_checker "$repo" "$bin_dir" "$phase" "$mode" "$pid_file" 2>&1)"
            status=$?
            set -e
            [ "$status" -ne 0 ] || die "$phase $mode unexpectedly succeeded"
            ! grep -Fq 'timed out' <<<"$output" || die "$phase $mode was misclassified as timeout"
            grep -Fq "status ${mode#exit-}" <<<"$output" || {
                [ "$mode" = failure ] && grep -Fq 'status 23' <<<"$output"
            } || die "$phase $mode did not preserve the child status"
            cases=$((cases + 1))
        done
        for stream in stdout stderr; do
            pid_file="$test_root/${phase}-${stream}-overflow.identities"
            : >"$pid_file"
            assert_failure_matching "$phase $stream overflow" \
                'tokenizer (version )?output exceeded 1024-byte limit' run_cd_checker \
                "$repo" "$bin_dir" "$phase" overflow "$pid_file" "$stream"
            assert_no_live_identities "$pid_file" "$phase/$stream-overflow"
            cases=$((cases + 1))
        done
    done
    prefix="$test_root/runner-natural-124"
    run_runner_fixture 1 4096 "$prefix" bash -c 'exit 124'
    assert_runner_receipt "$prefix" exited 124
    prefix="$test_root/runner-spawn-failure"
    run_runner_fixture 1 4096 "$prefix" /definitely/missing/verbatim-tokenizer
    assert_runner_receipt "$prefix" spawn_failed null
    prefix="$test_root/runner-interrupt"
    pid_file="$prefix.identities"
    : >"$pid_file"
    TOKUIN_FAKE_TARGET=estimate TOKUIN_FAKE_LIFECYCLE_MODE=timeout-tree \
        TOKUIN_FAKE_PID_FILE="$pid_file" python3 -B "$tokenizer_runner" run \
        --timeout-seconds 10 --max-output-bytes 4096 \
        --stdout "$prefix.stdout" --stderr "$prefix.stderr" \
        --receipt "$prefix.receipt.json" -- \
        "$fake" estimate --model gpt-4o --format json "$repo/src/tokenizer.rs" &
    runner_pid=$!
    wait_for_pid_file "$pid_file"
    kill -TERM "$runner_pid"
    wait "$runner_pid"
    assert_runner_receipt "$prefix" interrupted 143
    assert_no_live_identities "$pid_file" 'wrapper/SIGTERM'
    printf 'PROCESS-MATRIX: cases=%s residual=0\n' "$cases"
}

test_runner_output_caps() {
    local prefix stdout_size stderr_size
    prefix="$test_root/runner-output-cap"
    run_runner_fixture 5 1024 "$prefix" python3 -c \
        'import sys; sys.stdout.write("o" * 8192); sys.stderr.write("e" * 8192)'
    assert_runner_receipt "$prefix" output_limit null
    stdout_size="$(wc -c <"$prefix.stdout" | tr -d '[:space:]')"
    stderr_size="$(wc -c <"$prefix.stderr" | tr -d '[:space:]')"
    [ "$stdout_size" -le 1024 ] \
        || die "runner stdout cap exceeded: $stdout_size"
    [ "$stderr_size" -le 1024 ] \
        || die "runner stderr cap exceeded: $stderr_size"
}

raw_hex() {
    local framing="$1" text="$2"
    python3 - "$framing" "$text" <<'PY'
import sys
framing, text = sys.argv[1:]
data = text.encode("utf-8")
if framing == "lf":
    data += b"\n"
elif framing == "nul":
    data += b"\0\n"
elif framing == "trailing":
    data += b"\ntrailing\n"
elif framing == "invalid-utf8":
    data += b"\xff\n"
elif framing != "none":
    raise SystemExit(f"bad framing: {framing}")
print(data.hex())
PY
}

assert_raw_rejected() {
    local label="$1" repo="$2" bin_dir="$3" target="$4" output_hex="$5"
    local output status
    set +e
    output="$(TOKUIN_FAKE_OUTPUT_HEX="$output_hex" \
        run_cd_checker "$repo" "$bin_dir" "$target" raw "$test_root/raw.identities" 2>&1)"
    status=$?
    set -e
    [ "$status" -ne 0 ] || die "invalid tokenizer bytes passed: $label"
    case "$status" in
        124|137) die "checker exceeded the independent ${checker_outer_timeout_seconds}-second test bound: $label" ;;
    esac
    grep -Eq 'tokenizer (version )?(output was unparsable|protocol failure|version mismatch)' \
        <<<"$output" || {
        printf '%s\n' "$output" >&2
        die "invalid tokenizer bytes reached policy comparison: $label"
    }
}

test_tokenizer_numeric_domain() {
    local repo bin_dir index label payload hex field policy_repo policy_bin huge=9223372036854775808
    local valid_prefix='{"model":"gpt-4o","tokens":'
    local valid_suffix=',"input_cost":null,"output_cost":null,"breakdown":null}'
    local -a labels=(wrong-model missing-model legacy-total duplicate extra bool float negative zero impossible nan infinity int64-plus-one nul trailing invalid-utf8)
    local -a payloads=(
        '{"model":"gpt-4.1","tokens":20,"input_cost":null,"output_cost":null,"breakdown":null}'
        '{"tokens":20,"input_cost":null,"output_cost":null,"breakdown":null}'
        '{"model":"gpt-4o","total":20,"input_cost":null,"output_cost":null,"breakdown":null}'
        '{"model":"gpt-4o","tokens":20,"tokens":20,"input_cost":null,"output_cost":null,"breakdown":null}'
        '{"model":"gpt-4o","tokens":20,"input_cost":null,"output_cost":null,"breakdown":null,"extra":null}'
        '{"model":"gpt-4o","tokens":true,"input_cost":null,"output_cost":null,"breakdown":null}'
        '{"model":"gpt-4o","tokens":1.5,"input_cost":null,"output_cost":null,"breakdown":null}'
        '{"model":"gpt-4o","tokens":-1,"input_cost":null,"output_cost":null,"breakdown":null}'
        '{"model":"gpt-4o","tokens":0,"input_cost":null,"output_cost":null,"breakdown":null}'
        '{"model":"gpt-4o","tokens":9000,"input_cost":null,"output_cost":null,"breakdown":null}'
        '{"model":"gpt-4o","tokens":NaN,"input_cost":null,"output_cost":null,"breakdown":null}'
        '{"model":"gpt-4o","tokens":Infinity,"input_cost":null,"output_cost":null,"breakdown":null}'
        '{"model":"gpt-4o","tokens":9223372036854775808,"input_cost":null,"output_cost":null,"breakdown":null}'
        "$valid_prefix"'20'"$valid_suffix"
        "$valid_prefix"'20'"$valid_suffix"
        "$valid_prefix"'20'"$valid_suffix"
    )
    repo="$(prepare_tokenizer_ratchet_fixture tokenizer-output-contract)"
    bin_dir="$test_root/tokenizer-output-contract-bin"
    write_cd_tokenizer "$bin_dir/tokuin"
    for index in "${!labels[@]}"; do
        label="${labels[$index]}"
        case "$label" in
            nul) hex="$(raw_hex nul "${payloads[$index]}")" ;;
            trailing) hex="$(raw_hex trailing "${payloads[$index]}")" ;;
            invalid-utf8) hex="$(raw_hex invalid-utf8 "${payloads[$index]}")" ;;
            *) hex="$(raw_hex lf "${payloads[$index]}")" ;;
        esac
        assert_raw_rejected "$label" "$repo" "$bin_dir" estimate "$hex"
    done
    for field in tokens lines; do
        policy_repo="$(init_repo "baseline-$field-overflow")"
        policy_bin="$test_root/baseline-$field-overflow-bin"
        write_fake_tokenizer "$policy_bin"
        write_lines "$policy_repo/src/large.rs" 801 base
        if [ "$field" = tokens ]; then
            write_policy "$policy_repo" "$(policy_row src/large.rs "$huge" 801)"
        else
            write_policy "$policy_repo" "$(policy_row src/large.rs 20 "$huge")"
        fi
        commit_base "$policy_repo"
        printf 'changed\n' >>"$policy_repo/src/large.rs"
        run_without_git_env git -C "$policy_repo" add src/large.rs
        assert_failure_matching "baseline $field INT64 overflow" \
            "invalid baseline_${field}" \
            run_checker "$policy_repo" "$policy_bin" staged
    done
    printf 'OUTPUT-NUMERIC-MATRIX: invalid=%s baseline-overflow=2\n' "${#labels[@]}"
}

test_natural_exit_124_is_not_timeout() {
    local repo bin_dir valid version_hex output status
    repo="$(prepare_tokenizer_ratchet_fixture tokenizer-version-contract)"
    bin_dir="$test_root/tokenizer-version-contract-bin"
    write_cd_tokenizer "$bin_dir/tokuin"
    valid='tokuin 0.3.0'
    for version_hex in \
        "$(raw_hex nul "$valid")" \
        "$(raw_hex trailing "$valid")" \
        "$(raw_hex none "$valid")"; do
        assert_raw_rejected 'invalid version framing' "$repo" "$bin_dir" version "$version_hex"
    done
    set +e
    output="$(run_cd_checker "$repo" "$bin_dir" version success-stderr \
        "$test_root/version-stderr.identities" 2>&1)"
    status=$?
    set -e
    [ "$status" -ne 0 ] || die 'successful version stderr unexpectedly passed'
    grep -Eq 'version (output was unparsable|protocol failure)' <<<"$output" \
        || die 'successful version stderr lacked protocol diagnostic'
    printf 'VERSION-FRAMING-MATRIX: invalid=4\n'
}

test_checker_output_cap() {
    local repo bin_dir output status fixture_repo fixture_bin
    repo="$(prepare_tokenizer_ratchet_fixture tokenizer-known-answer-substitute)"
    bin_dir="$test_root/tokenizer-known-answer-substitute-bin"
    write_cd_tokenizer "$bin_dir/tokuin"
    set +e
    output="$(run_cd_checker "$repo" "$bin_dir" known-answer wrong-known-answer \
        "$test_root/wrong-known.identities" 2>&1)"
    status=$?
    set -e
    [ "$status" -ne 0 ] || die 'same-semver behavior substitute passed known-answer attestation'
    grep -Eq 'known-answer|attestation' <<<"$output" || {
        printf '%s\n' "$output" >&2
        die 'behavior substitute lacked attestation diagnostic'
    }
    fixture_repo="$(init_repo fixture-tokenizer-attestation)"
    write_policy "$fixture_repo" 'files = []'
    printf 'base\n' >"$fixture_repo/src/real.rs"
    commit_base "$fixture_repo"
    printf 'candidate\n' >"$fixture_repo/src/real.rs"
    run_without_git_env git -C "$fixture_repo" add src/real.rs
    fixture_bin="$test_root/fixture-tokenizer-bin"
    write_fake_tokenizer "$fixture_bin"
    output="$(run_checker_clean "$fixture_repo" "$fixture_bin" staged 2>&1)" || {
        printf '%s\n' "$output" >&2
        die 'fixture tokenizer attestation failed'
    }
    grep -Fq 'Tokenizer attestation passed: tokuin 0.3.0 model=gpt-4o tokens=7' \
        <<<"$output" || die 'real tokenizer run lacked deterministic attestation receipt'
    printf 'FIXTURE-ATTESTATION: tokuin=0.3.0 model=gpt-4o tokens=7\n'
}
