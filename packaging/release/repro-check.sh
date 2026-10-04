#!/usr/bin/env bash
# Reproducibility gate for the bridge release tarball. Copies the source tree
# (including .git; excluding target/, dist/, agent-tooling .omc/ dirs and any
# node_modules/) to a different absolute path, rebuilds there with
# build-bridge.sh using a fresh CARGO_TARGET_DIR, OUT_DIR, CARGO_HOME (seeded
# with a copy of the first build's crate registry only — no config.toml, no
# bin/) and HOME, and the same SW_* environment, and fails unless the two
# tarballs are byte-identical. On a mismatch it lists the members whose sha256
# differ.
#
# The different checkout path, CARGO_HOME and HOME are what prove the
# --remap-path-prefix flags make the binary path-independent.
#
# Usage (inside the builder container, after a first build-bridge.sh run):
#   repro-check.sh <first-tarball>
#
# Contract: docs/superpowers/plans/2026-10-03-bridge-release-artifact.md
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

WORK=""
cleanup() {
    if [[ -n "$WORK" && -d "$WORK" ]]; then rm -rf -- "$WORK"; fi
}
trap cleanup EXIT

log() { echo "repro-check: $*" >&2; }
die() {
    echo "repro-check: FAIL: $*" >&2
    exit 1
}

# `<sha256>  ./<path>` for every regular file under $1, sorted by path.
member_sums() {
    (cd "$1" && find . -type f -print0 | sort -z | xargs -0 -r sha256sum)
}

report_differences() {
    local first="$1" second="$2" work="$3" listing marker path
    mkdir -p "$work/a" "$work/b"
    tar -C "$work/a" -xzf "$first"
    tar -C "$work/b" -xzf "$second"
    listing="$(diff <(member_sums "$work/a") <(member_sums "$work/b") || true)"
    if [[ -z "$listing" ]]; then
        log "member contents are identical; the difference is in tar/gzip metadata"
        return
    fi
    log "members whose sha256 differ:"
    while read -r marker _ path; do
        if [[ "$marker" == "<" || "$marker" == ">" ]]; then echo "  $path"; fi
    done <<<"$listing" | sort -u >&2
}

# A fresh CARGO_HOME holding only a copy of the first build's registry (so the
# second build need not re-download), never its config.toml or bin/.
seed_cargo_home() {
    local fresh="$1" first
    first="$(cd "${CARGO_HOME:-$HOME/.cargo}" 2>/dev/null && pwd -P)" || first=""
    mkdir -m 0700 "$fresh"
    if [[ -n "$first" && -d "$first/registry" ]]; then
        log "seeding $fresh/registry from $first/registry"
        cp -a "$first/registry" "$fresh/registry"
    else
        log "no first-build registry to seed from; the second build downloads its crates"
    fi
}

main() {
    [[ $# -eq 1 && -f "${1:-}" ]] || die "usage: repro-check.sh <first-tarball>"
    local first second src copy
    first="$(cd "$(dirname "$1")" && pwd -P)/$(basename "$1")"
    src="$(cd "${SRC_DIR:-$SCRIPT_DIR/../..}" && pwd -P)"
    WORK="$(mktemp -d /tmp/sw-repro.XXXXXX)"
    copy="$WORK/src-copy"
    mkdir "$copy" "$WORK/home"

    log "copying $src -> $copy (without target/, dist/, .omc/, node_modules/)"
    # --anchored applies to the excludes after it; node_modules is matched at any depth.
    tar -C "$src" --anchored --exclude=./target --exclude=./dist --exclude=./.omc \
        --exclude=./vendor/opensnitch/.omc --no-anchored --exclude=node_modules -cf - . |
        tar -C "$copy" -xf -
    seed_cargo_home "$WORK/cargo-home"

    log "rebuilding from $copy (CARGO_HOME=$WORK/cargo-home, HOME=$WORK/home)"
    HOME="$WORK/home" CARGO_HOME="$WORK/cargo-home" SRC_DIR="$copy" OUT_DIR="$WORK/out" \
        CARGO_TARGET_DIR="$WORK/target" "$copy/packaging/release/build-bridge.sh"

    second="$WORK/out/$(basename "$first")"
    [[ -f "$second" ]] || die "the second build did not produce $(basename "$first")"
    if cmp -s "$first" "$second"; then
        log "OK: $(basename "$first") is byte-identical when rebuilt from $copy"
        return 0
    fi
    report_differences "$first" "$second" "$WORK"
    die "$(basename "$first") is NOT reproducible (first: $first, second: $second)"
}

main "$@"
