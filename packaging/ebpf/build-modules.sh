#!/usr/bin/env bash
# Build the opensnitch eBPF modules (opensnitch.o, opensnitch-procs.o,
# opensnitch-dns.o) from the pinned vendor/opensnitch commit and stage them
# under <staging>/usr/lib/opensnitchd/ebpf/ with a SHA256SUMS file.
#
# Procedure: docs/superpowers/research/2026-11-07-ebpf-load-test-runbook.md
# section 5a. vendor/opensnitch is read-only: its ebpf_prog directory is copied
# to a scratch dir and built there.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd -P)"
PINS_FILE="${SW_EBPF_PINS_FILE:-$SCRIPT_DIR/pins.env}"
RELEASE_PINS="$REPO_ROOT/packaging/release/pins.env"
MODULES=(opensnitch.o opensnitch-procs.o opensnitch-dns.o)
DEST_REL="usr/lib/opensnitchd/ebpf"

usage() {
    cat <<'USAGE'
Usage: build-modules.sh --staging DIR [options]

Options:
  --staging DIR          output root; modules land in DIR/usr/lib/opensnitchd/ebpf/
  --kernel-dir DIR       kernel source/headers tree (or env KERNEL_DIR)
  --kernel-headers DIR   kernel headers tree (or env KERNEL_HEADERS; default: --kernel-dir)
  --kernel-ver VER       Makefile KERNEL_VER (default: SW_EBPF_KERNEL_DEVEL_VERSION in pins.env)
  --opensnitch-dir DIR   opensnitch checkout (default: vendor/opensnitch; never modified)
  --dry-run              validate pins and arguments, print the plan; needs no clang
  --help                 show this help

Fails if clang/llvm-strip/make are missing or not the pinned major version,
if the kernel trees lack headers, or if the opensnitch commit differs from
SW_OPENSNITCH_COMMIT (which must also equal packaging/release/pins.env).
USAGE
}

log() { echo "build-modules: $*" >&2; }
die() {
    echo "build-modules: ERROR: $*" >&2
    exit 1
}

STAGING="" DRY_RUN=0 KVER="" OS_DIR="$REPO_ROOT/vendor/opensnitch"
KERNEL_DIR="${KERNEL_DIR:-}" KERNEL_HEADERS="${KERNEL_HEADERS:-}"
while [[ $# -gt 0 ]]; do
    case "$1" in
    --help | -h) usage; exit 0 ;;
    --dry-run) DRY_RUN=1 ;;
    --staging | --kernel-dir | --kernel-headers | --kernel-ver | --opensnitch-dir)
        [[ $# -ge 2 ]] || die "$1 needs a value"
        case "$1" in
        --staging) STAGING="$2" ;;
        --kernel-dir) KERNEL_DIR="$2" ;;
        --kernel-headers) KERNEL_HEADERS="$2" ;;
        --kernel-ver) KVER="$2" ;;
        --opensnitch-dir) OS_DIR="$2" ;;
        esac
        shift
        ;;
    *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

[[ -f "$PINS_FILE" ]] || die "pins file not found: $PINS_FILE"
# Pins files are parsed, never sourced: only plain KEY=value lines count.
pin() { sed -n "s/^$1=//p" "$2" | head -n1; }
SW_OPENSNITCH_COMMIT="$(pin SW_OPENSNITCH_COMMIT "$PINS_FILE")"
SW_EBPF_CLANG_MAJOR="$(pin SW_EBPF_CLANG_MAJOR "$PINS_FILE")"
SW_EBPF_LLVM_MAJOR="$(pin SW_EBPF_LLVM_MAJOR "$PINS_FILE")"
SW_EBPF_LLC_MAJOR="$(pin SW_EBPF_LLC_MAJOR "$PINS_FILE")"
SW_EBPF_KERNEL_DEVEL_VERSION="$(pin SW_EBPF_KERNEL_DEVEL_VERSION "$PINS_FILE")"
for v in SW_OPENSNITCH_COMMIT SW_EBPF_CLANG_MAJOR SW_EBPF_LLVM_MAJOR SW_EBPF_LLC_MAJOR SW_EBPF_KERNEL_DEVEL_VERSION; do
    [[ -n "${!v:-}" ]] || die "$v not set in $PINS_FILE"
done
KVER="${KVER:-$SW_EBPF_KERNEL_DEVEL_VERSION}"

# The release pins are the source of truth for the daemon commit.
[[ -f "$RELEASE_PINS" ]] || die "release pins not found: $RELEASE_PINS"
release_commit="$(pin SW_OPENSNITCH_COMMIT "$RELEASE_PINS")"
[[ "$release_commit" == "$SW_OPENSNITCH_COMMIT" ]] ||
    die "commit pin mismatch: $PINS_FILE has $SW_OPENSNITCH_COMMIT, $RELEASE_PINS has $release_commit"

[[ -n "$STAGING" ]] || { usage >&2; die "--staging is required"; }
[[ "$STAGING" != -* ]] || die "--staging must not start with '-': $STAGING"

if [[ $DRY_RUN -eq 0 ]]; then
    [[ -n "$KERNEL_DIR" ]] || die "KERNEL_DIR not set (use --kernel-dir or env KERNEL_DIR)"
    KERNEL_HEADERS="${KERNEL_HEADERS:-$KERNEL_DIR}"
fi

# The opensnitch checkout must be exactly the pinned commit (when it is a git
# checkout; a missing submodule is tolerated only in --dry-run).
if [[ -e "$OS_DIR/.git" ]]; then
    actual="$(git -C "$OS_DIR" rev-parse HEAD)" || die "cannot read HEAD of $OS_DIR"
    [[ "$actual" == "$SW_OPENSNITCH_COMMIT" ]] ||
        die "commit pin mismatch: $OS_DIR is at $actual, pin is $SW_OPENSNITCH_COMMIT"
elif [[ $DRY_RUN -eq 1 ]]; then
    log "note: $OS_DIR is not a git checkout; commit check skipped (dry-run)"
else
    die "$OS_DIR is not a git checkout; cannot verify the commit pin"
fi

if [[ $DRY_RUN -eq 1 ]]; then
    log "dry-run: would extract ebpf_prog of commit $SW_OPENSNITCH_COMMIT (git archive) to a scratch dir, run make clean"
    log "dry-run: would run make KERNEL_DIR=${KERNEL_DIR:-<unset>} KERNEL_HEADERS=${KERNEL_HEADERS:-${KERNEL_DIR:-<unset>}} KERNEL_VER=$KVER"
    log "dry-run: would require clang/llvm-strip/llc major $SW_EBPF_CLANG_MAJOR/$SW_EBPF_LLVM_MAJOR/$SW_EBPF_LLC_MAJOR, run llvm-strip -g"
    log "dry-run: would stage ${MODULES[*]} + SHA256SUMS under $STAGING/$DEST_REL/"
    exit 0
fi

[[ -f "$KERNEL_DIR/include/linux/kconfig.h" ]] ||
    die "KERNEL_DIR=$KERNEL_DIR lacks include/linux/kconfig.h (install kernel-devel $SW_EBPF_KERNEL_DEVEL_VERSION)"
[[ -d "$KERNEL_HEADERS/include" ]] || die "KERNEL_HEADERS=$KERNEL_HEADERS lacks include/"
[[ -d "$OS_DIR/ebpf_prog" ]] || die "$OS_DIR/ebpf_prog not found"

for tool in clang llc llvm-strip make; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool not found in PATH"
done
clang_major="$(clang --version | sed -n 's/.*clang version \([0-9]*\)\..*/\1/p' | head -n1)"
[[ "$clang_major" == "$SW_EBPF_CLANG_MAJOR" ]] ||
    die "clang major is '${clang_major:-unknown}', pin is $SW_EBPF_CLANG_MAJOR"
strip_major="$(llvm-strip --version | sed -n 's/.*LLVM version \([0-9]*\)\..*/\1/p' | head -n1)"
[[ "$strip_major" == "$SW_EBPF_LLVM_MAJOR" ]] ||
    die "llvm-strip major is '${strip_major:-unknown}', pin is $SW_EBPF_LLVM_MAJOR"

llc_major="$(llc --version | sed -n 's/.*LLVM version \([0-9]*\)\..*/\1/p' | head -n1)"
[[ "$llc_major" == "$SW_EBPF_LLC_MAJOR" ]] ||
    die "llc major is '${llc_major:-unknown}', pin is $SW_EBPF_LLC_MAJOR"

WORK="$(mktemp -d)"
trap 'rm -rf -- "$WORK"' EXIT
# Exactly the pinned tree, not the working tree.
git -C "$OS_DIR" archive "$SW_OPENSNITCH_COMMIT" ebpf_prog | tar -x -C "$WORK" --strip-components=1
make -C "$WORK" clean

log "building modules (KERNEL_VER=$KVER)"
make -C "$WORK" KERNEL_DIR="$KERNEL_DIR" KERNEL_HEADERS="$KERNEL_HEADERS" KERNEL_VER="$KVER"
for m in "${MODULES[@]}"; do
    [[ -s "$WORK/$m" ]] || die "build did not produce $m"
done
(cd "$WORK" && llvm-strip -g "${MODULES[@]}")

DEST="$STAGING/$DEST_REL"
mkdir -p -- "$DEST"
for m in "${MODULES[@]}"; do install -m 0644 -- "$WORK/$m" "$DEST/$m"; done
(cd "$DEST" && sha256sum "${MODULES[@]}" >SHA256SUMS)
log "staged ${MODULES[*]} in $DEST"
cat "$DEST/SHA256SUMS"
