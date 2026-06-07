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
