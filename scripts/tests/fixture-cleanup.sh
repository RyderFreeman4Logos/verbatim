#!/usr/bin/env bash
# Shared cleanup guard for fixture roots.

cleanup_fixture_root() {
    local fixture_root="$1"
    local repo_root="$2"
    local canonical_link="$repo_root/target"
    local canonical_target="/ssd/mirror-rootfs${repo_root}/target"

    case "$fixture_root" in
        "$repo_root"|"$canonical_link"|"$canonical_target"|\
        "$canonical_link/"*|"$canonical_target/"*)
            printf 'ERROR: refusing to remove canonical Cargo target or repository root: %s\n' \
                "$fixture_root" >&2
            return 1
            ;;
    esac
    rm -rf -- "$fixture_root"
}
