#!/usr/bin/env bash
# shellcheck shell=bash
# Generate CycloneDX SBOMs for the Verbatim Cargo workspace (SUPPLY-001).
# Optional tool: cargo-cyclonedx. Tests MUST NOT depend on this script succeeding.
#
# Outputs land under target/sbom/ (gitignored build tree). Do not commit blobs.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

out_dir="${SBOM_OUT_DIR:-$root/target/sbom}"

die() {
    printf 'generate-workspace-sbom: ERROR: %s\n' "$*" >&2
    exit 1
}

if ! cargo cyclonedx --help >/dev/null 2>&1; then
    cat >&2 <<'EOF'
generate-workspace-sbom: ERROR: cargo-cyclonedx is not available.

Install (operators only; not a CI/gate hard dependency for unit tests):

  cargo install cargo-cyclonedx --locked

Then re-run:

  bash scripts/generate-workspace-sbom.sh

License/advisory baseline remains: deny.toml + `just deny`.
EOF
    exit 1
fi

mkdir -p -- "$out_dir" || die "cannot create output dir: $out_dir"

# Remove prior generated test artifacts with our labels so find is stable.
cleanup_label_artifacts() {
    local label="$1"
    find "$root" -type f \( -name "${label}.json" -o -name "${label}.cdx.json" \) \
        ! -path "$out_dir/*" \
        -delete 2>/dev/null || true
}

generate_one() {
    local manifest="$1"
    local label="$2"
    local dest="$out_dir/${label}.cdx.json"
    local marker
    marker="$(mktemp)" || die "mktemp failed"
    cleanup_label_artifacts "$label"

    printf 'generate-workspace-sbom: generating %s from %s\n' "$label" "$manifest"
    if ! cargo cyclonedx \
        --manifest-path "$manifest" \
        -f json \
        --all-features \
        --override-filename "$label"
    then
        rm -f -- "$marker"
        die "cargo cyclonedx failed for $manifest"
    fi

    # cargo-cyclonedx writes <override-filename>.json next to package manifests.
    # Prefer the file matching the requested manifest's directory.
    local found=""
    local manifest_dir
    manifest_dir="$(cd "$(dirname "$manifest")" && pwd)"
    if [ -f "$manifest_dir/${label}.json" ]; then
        found="$manifest_dir/${label}.json"
    elif [ -f "$manifest_dir/${label}.cdx.json" ]; then
        found="$manifest_dir/${label}.cdx.json"
    else
        found="$(find "$root" -type f \( -name "${label}.json" -o -name "${label}.cdx.json" \) \
            ! -path "$out_dir/*" -newer "$marker" 2>/dev/null | head -n 1 || true)"
    fi

    rm -f -- "$marker"

    if [ -z "$found" ] || [ ! -f "$found" ]; then
        die "could not locate generated SBOM for label=$label (manifest=$manifest)"
    fi

    mv -f -- "$found" "$dest" || die "cannot move $found -> $dest"
    # Sweep any sibling copies cargo-cyclonedx may have written into other crates.
    cleanup_label_artifacts "$label"
    printf 'generate-workspace-sbom: wrote %s\n' "$dest"
}

# Per-member crates (workspace root package is virtual — members only).
members=(
    crates/verbatim-core
    crates/verbatim-daemon
    crates/verbatim-cli
)

for member in "${members[@]}"; do
    if [ -f "$root/$member/Cargo.toml" ]; then
        generate_one "$root/$member/Cargo.toml" "$(basename "$member")"
    fi
done

printf 'generate-workspace-sbom: PASS (outputs under %s)\n' "$out_dir"
printf 'generate-workspace-sbom: do not commit generated SBOM blobs.\n'
printf 'generate-workspace-sbom: license/advisory baseline remains deny.toml (`just deny`).\n'
