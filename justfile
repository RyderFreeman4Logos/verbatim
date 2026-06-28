# Justfile for Verbatim Workspace
# Quality gates for Rust monorepo (crates under ./crates/).
# AI AGENT: Do NOT modify this file or use `git commit -n`/`--no-verify`.

set shell := ["bash", "-c"]
set tempdir := "."
set dotenv-load := true

_repo_root := `git rev-parse --show-superproject-working-tree 2>/dev/null | grep . || git rev-parse --show-toplevel`
export MISE_TRUSTED_CONFIG_PATHS := _repo_root

default: pre-commit

# ==============================================================================
# Core Workflow
# ==============================================================================

# Fast pre-commit: formatting, linting, static analysis only (no tests).
pre-commit-fast:
    just fmt
    just clippy
    just deny

# Full pre-commit: formatting, linting, and tests.
pre-commit:
    just pre-commit-fast
    just test

# ==============================================================================
# Versioning
# ==============================================================================

# Exit 0 when the workspace version differs from the default branch.
check-version-bumped:
    #!/usr/bin/env bash
    set -euo pipefail
    default_branch="${DEFAULT_BRANCH:-main}"
    base_ref="${default_branch}"
    if ! git rev-parse --verify "${base_ref}:Cargo.toml" >/dev/null 2>&1; then
        base_ref="origin/${default_branch}"
    fi
    current_version="$(python3 - <<'PY'
    import tomllib
    from pathlib import Path

    print(tomllib.loads(Path("Cargo.toml").read_text())["workspace"]["package"]["version"])
    PY
    )"
    base_version="$(git show "${base_ref}:Cargo.toml" | python3 -c 'import sys, tomllib; print(tomllib.loads(sys.stdin.read())["workspace"]["package"]["version"])')"
    if [[ "${current_version}" == "${base_version}" ]]; then
        echo "Workspace version unchanged from ${base_ref}: ${current_version}" >&2
        exit 1
    fi
    echo "Workspace version bumped: ${base_version} -> ${current_version}"

# Bump the workspace patch version and refresh Cargo.lock.
bump-patch:
    #!/usr/bin/env bash
    set -euo pipefail
    python3 - <<'PY'
    import re
    from pathlib import Path

    path = Path("Cargo.toml")
    text = path.read_text()
    pattern = re.compile(
        r'(?ms)^(\[workspace\.package\]\s*(?:(?!^\[).)*?^version\s*=\s*")'
        r'(\d+)\.(\d+)\.(\d+)'
        r'("\s*$)'
    )
    match = pattern.search(text)
    if match is None:
        raise SystemExit("workspace.package.version must be a simple MAJOR.MINOR.PATCH version")

    major, minor, patch = (int(match.group(i)) for i in range(2, 5))
    old_version = f"{major}.{minor}.{patch}"
    new_version = f"{major}.{minor}.{patch + 1}"
    path.write_text(text[:match.start()] + match.group(1) + new_version + match.group(5) + text[match.end():])
    print(f"Workspace version bumped: {old_version} -> {new_version}")
    PY
    cargo metadata --format-version 1 >/dev/null

# ==============================================================================
# Quality Gates
# ==============================================================================

# Format code and re-stage only .rs files that were already staged.
# Abort when any staged Rust file also has unstaged hunks.
fmt:
    #!/usr/bin/env bash
    set -euo pipefail
    staged_rs=()
    while IFS= read -r -d '' path; do
        staged_rs+=("$path")
    done < <(git diff --cached --name-only -z -- '*.rs')
    unstaged_rs=()
    while IFS= read -r -d '' path; do
        unstaged_rs+=("$path")
    done < <(git diff --name-only -z -- '*.rs')
    partial=()
    for staged in "${staged_rs[@]}"; do
        for unstaged in "${unstaged_rs[@]}"; do
            if [[ "$staged" == "$unstaged" ]]; then
                partial+=("$staged")
                break
            fi
        done
    done
    if (( ${#partial[@]} > 0 )); then
        printf 'just fmt: refusing -- these files are partially staged:\n' >&2
        printf '  %q\n' "${partial[@]}" >&2
        exit 1
    fi
    if (( ${#staged_rs[@]} == 0 )); then
        exit 0
    fi
    cargo fmt --all
    printf '%s\0' "${staged_rs[@]}" | xargs -0 git add --

# Clippy for entire workspace (strict).
clippy:
    cargo clippy --workspace --all-features -- -D warnings

# Clippy for a specific crate.
# Usage: just clippy-p verbatim-core
clippy-p package:
    cargo clippy -p {{package}} --all-features -- -D warnings

# Security audit (requires cargo-deny).
deny:
    cargo deny check --hide-inclusion-graph

# ==============================================================================
# Testing
# ==============================================================================

# Run the Qdrant primary vector sink spike harness.
bench-qdrant-spike *args:
    python3 scripts/qdrant_spike.py {{args}}

# Run all workspace tests.
test:
    cargo nextest run --workspace --no-tests=warn
    cargo nextest run --workspace --all-features --no-tests=warn

# Test a specific crate.
# Usage: just test-p verbatim-core
test-p package:
    cargo nextest run -p {{package}} --all-features --no-tests=warn

# Test by name pattern.
# Usage: just test-f chunk_overlap
test-f pattern:
    cargo nextest run --workspace --all-features -E 'test({{pattern}})' --no-tests=warn

# ==============================================================================
# Build & Install
# ==============================================================================

# Build all workspace members.
build:
    cargo build --workspace --all-features

# Install release binaries to /usr/local/bin.
install:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release --all-features -p verbatim-daemon -p verbatim-cli
    target_dir="${CARGO_TARGET_DIR:-{{_repo_root}}/target}"
    install -m 755 "${target_dir}/release/verbatim-daemon" /usr/local/bin/verbatim-daemon
    install -m 755 "${target_dir}/release/verbatim" /usr/local/bin/verbatim
    echo "Installed verbatim + verbatim-daemon"
    verbatim --version || true
    verbatim-daemon --version || true

# Build release binaries, install them for the current user, restart the user daemon, and verify health.
install-local-daemon:
    #!/usr/bin/env bash
    set -euo pipefail
    bin_dir="${VERBATIM_LOCAL_BIN_DIR:-${HOME}/.local/bin}"
    service="${VERBATIM_SYSTEMD_USER_SERVICE:-verbatim}"
    write_unit="${VERBATIM_LOCAL_DAEMON_WRITE_UNIT:-auto}"

    case "${write_unit}" in
        auto|0|1|true|false) ;;
        *)
            echo "VERBATIM_LOCAL_DAEMON_WRITE_UNIT must be auto, 0, 1, true, or false" >&2
            exit 2
            ;;
    esac

    case "${service}" in
        ""|*/*)
            echo "VERBATIM_SYSTEMD_USER_SERVICE must be a systemd user service name, not a path" >&2
            exit 2
            ;;
    esac
    service_unit="${service}"
    if [[ "${service_unit}" != *.service ]]; then
        service_unit="${service_unit}.service"
    fi
    unit_path="${XDG_CONFIG_HOME:-${HOME}/.config}/systemd/user/${service_unit}"

    install -d -m 755 "${bin_dir}"
    bin_dir="$(cd "${bin_dir}" && pwd -P)"
    daemon_bin="${bin_dir}/verbatim-daemon"
    cli_bin="${bin_dir}/verbatim"
    expected_exec="ExecStart=${daemon_bin}"
    should_write_unit=0
    if [[ "${write_unit}" == "1" || "${write_unit}" == "true" ]]; then
        should_write_unit=1
    elif [[ "${write_unit}" == "auto" && ! -f "${unit_path}" ]]; then
        should_write_unit=1
    fi

    if [[ ! -f "${unit_path}" && "${should_write_unit}" == "0" ]]; then
        echo "${unit_path} does not exist and unit writes are disabled." >&2
        echo "Set VERBATIM_LOCAL_DAEMON_WRITE_UNIT=auto or 1, or set VERBATIM_SYSTEMD_USER_SERVICE to an existing user service." >&2
        exit 1
    fi
    if [[ -f "${unit_path}" ]] && ! grep -Fxq "${expected_exec}" "${unit_path}" && [[ "${should_write_unit}" == "0" ]]; then
        echo "${unit_path} does not use ${daemon_bin}." >&2
        echo "Set VERBATIM_LOCAL_DAEMON_WRITE_UNIT=1 to regenerate it, or set VERBATIM_LOCAL_BIN_DIR to the unit's ExecStart directory." >&2
        exit 1
    fi

    cargo build --release --all-features -p verbatim-daemon -p verbatim-cli
    target_dir="${CARGO_TARGET_DIR:-{{_repo_root}}/target}"
    install -m 755 "${target_dir}/release/verbatim-daemon" "${daemon_bin}"
    install -m 755 "${target_dir}/release/verbatim" "${cli_bin}"

    if [[ "${should_write_unit}" == "1" ]]; then
        install -d -m 755 "$(dirname "${unit_path}")"
        tmp_unit="$(mktemp "${unit_path}.XXXXXX")"
        trap 'rm -f "${tmp_unit:-}"' EXIT
        cat > "${tmp_unit}" <<UNIT
    [Unit]
    Description=Verbatim RAG daemon
    After=network-online.target

    [Service]
    Type=simple
    ExecStart=${daemon_bin}
    Restart=on-failure
    RestartSec=5
    Environment=VERBATIM_CONFIG=%h/.config/verbatim/config.toml

    [Install]
    WantedBy=default.target
    UNIT
        chmod 644 "${tmp_unit}"
        mv "${tmp_unit}" "${unit_path}"
        tmp_unit=""
    fi
    if ! grep -Fxq "${expected_exec}" "${unit_path}"; then
        echo "${unit_path} does not use ${daemon_bin} after unit update." >&2
        exit 1
    fi

    systemctl --user daemon-reload
    systemctl --user restart "${service_unit}"
    systemctl --user is-active --quiet "${service_unit}"

    "${cli_bin}" --version
    "${daemon_bin}" --version
    for _ in {1..20}; do
        if PATH="${bin_dir}:${PATH}" "${cli_bin}" daemon status; then
            echo "Local ${service_unit} daemon deployed from ${daemon_bin}"
            exit 0
        fi
        sleep 1
    done
    echo "${service_unit} restarted but did not pass daemon status within 20 seconds" >&2
    exit 1

# Install an opt-in local post-merge or post-push hook that runs `just install-local-daemon`.
install-local-daemon-hook hook="post-merge":
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{hook}}" in
        post-merge|post-push) ;;
        *)
            echo "hook must be post-merge or post-push" >&2
            exit 2
            ;;
    esac
    repo_root="$(git rev-parse --show-toplevel)"
    hook_path="$(git rev-parse --git-path 'hooks/{{hook}}')"
    marker="# Generated by just install-local-daemon-hook"
    if [[ -e "${hook_path}" ]] && ! grep -Fq "${marker}" "${hook_path}"; then
        if [[ "${VERBATIM_LOCAL_DAEMON_HOOK_FORCE:-0}" != "1" ]]; then
            echo "Refusing to overwrite non-Verbatim hook: ${hook_path}" >&2
            echo "Set VERBATIM_LOCAL_DAEMON_HOOK_FORCE=1 to replace it." >&2
            exit 1
        fi
    fi
    mkdir -p "$(dirname "${hook_path}")"
    {
        echo "#!/usr/bin/env bash"
        echo "set -euo pipefail"
        echo "${marker}"
        printf 'cd %q\n' "${repo_root}"
        echo "exec just install-local-daemon"
    } > "${hook_path}"
    chmod +x "${hook_path}"
    echo "Installed {{hook}} hook: ${hook_path}"

# Remove a hook installed by `just install-local-daemon-hook`.
remove-local-daemon-hook hook="post-merge":
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{hook}}" in
        post-merge|post-push) ;;
        *)
            echo "hook must be post-merge or post-push" >&2
            exit 2
            ;;
    esac
    hook_path="$(git rev-parse --git-path 'hooks/{{hook}}')"
    marker="# Generated by just install-local-daemon-hook"
    if [[ ! -e "${hook_path}" ]]; then
        echo "No {{hook}} hook installed."
        exit 0
    fi
    if ! grep -Fq "${marker}" "${hook_path}"; then
        echo "Refusing to remove non-Verbatim hook: ${hook_path}" >&2
        exit 1
    fi
    rm "${hook_path}"
    echo "Removed {{hook}} hook: ${hook_path}"

# ==============================================================================
# Git Helpers
# ==============================================================================

# Install git hooks via lefthook.
install-hooks:
    @git config --unset core.hooksPath 2>/dev/null || true
    lefthook install
    @echo "Lefthook hooks installed."

# Show staged/unstaged diff summary.
review:
    @echo "=== Staged ==="
    git diff --cached --stat
    @echo ""
    @echo "=== Unstaged ==="
    git diff --stat
