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
