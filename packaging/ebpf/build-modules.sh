#!/bin/bash -p
# Build the opensnitch eBPF modules (opensnitch.o, opensnitch-procs.o,
# opensnitch-dns.o) from the pinned vendor/opensnitch commit and stage them
# under <staging>/usr/lib/opensnitchd/ebpf/ with a SHA256SUMS file.
#
# Procedure: docs/superpowers/research/2026-11-07-ebpf-load-test-runbook.md
# section 5a. vendor/opensnitch is read-only: the ebpf_prog tree of the pinned
# commit is extracted from raw git blobs into a scratch dir, checked against
# SW_EBPF_PROG_TREE_SHA256 and built there.
#
# Run it directly (./build-modules.sh), never as `bash build-modules.sh`: only
# direct execution honours `bash -p` (no BASH_ENV/ENV, no exported functions,
# no SHELLOPTS/CDPATH from the environment). The next line then re-executes the
# script in an empty environment, so every input is a command-line flag.
[[ -n ${SW_EBPF_CLEAN:-} ]] || exec /usr/bin/env -i SW_EBPF_CLEAN=1 PATH=/usr/bin:/bin LC_ALL=C /bin/bash -p -- "$0" "$@"
# SW_EBPF_CLEAN can be forged by the caller: the environment must be exactly
# what the re-exec above builds (bash itself adds PWD/SHLVL/OLDPWD/_).
for _v in $(compgen -e); do
    case "$_v" in
    SW_EBPF_CLEAN | PATH | LC_ALL | PWD | SHLVL | OLDPWD | _) ;;
    *)
        echo "build-modules: ERROR: unexpected environment variable $_v (do not set SW_EBPF_CLEAN)" >&2
        exit 1
        ;;
    esac
done
if [[ $SW_EBPF_CLEAN != 1 || $PATH != /usr/bin:/bin || $LC_ALL != C || $- != *p* ]]; then
    echo "build-modules: ERROR: unexpected environment (do not set SW_EBPF_CLEAN)" >&2
    exit 1
fi
unset _v
set -euo pipefail -o noclobber

SAFE_PATH=/usr/bin:/bin
# Every external tool runs through clean(): an empty environment (no
# TAR_OPTIONS, GIT_*, MAKEFILES, TMPDIR, LD_PRELOAD, ...), a fixed PATH and no
# user/system git config or attributes. Tools outside SAFE_PATH are resolved
# to absolute paths first.
ENV_BIN=/usr/bin/env
clean() {
    local cmd="$1"
    shift
    if [[ "$cmd" != /* ]]; then # bare name: absolute trusted path, never a PATH lookup
        if [[ -x "/usr/bin/$cmd" ]]; then cmd="/usr/bin/$cmd"; else cmd="/bin/$cmd"; fi
    fi
    [[ -x "$cmd" ]] || die "tool not found: $cmd"
    "$ENV_BIN" -i PATH="$SAFE_PATH" HOME=/nonexistent LC_ALL=C \
        GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_ATTR_NOSYSTEM=1 "$cmd" "$@"
}
log() { echo "build-modules: $*" >&2; }
die() {
    echo "build-modules: ERROR: $*" >&2
    exit 1
}
[[ -x "$ENV_BIN" ]] || die "$ENV_BIN not executable"

SCRIPT_DIR="$(cd -P -- "$(clean dirname -- "$0")" && pwd -P)"
REPO_ROOT="$(cd -P -- "$SCRIPT_DIR/../.." && pwd -P)"
PINS_FILE="$SCRIPT_DIR/pins.env"
RELEASE_PINS="$REPO_ROOT/packaging/release/pins.env"
MODULES=(opensnitch.o opensnitch-procs.o opensnitch-dns.o)
DEST_REL="usr/lib/opensnitchd/ebpf"

usage() {
    cat <<'USAGE'
Usage: build-modules.sh --staging DIR --kernel-dir DIR [options]

Options:
  --staging DIR          absolute output root, absent or empty; modules land in
                         DIR/usr/lib/opensnitchd/ebpf/
  --kernel-dir DIR       kernel-devel tree, /usr/src/kernels/<release> (required)
  --kernel-headers DIR   kernel headers tree, same rules (default: --kernel-dir)
  --kernel-ver VER       Makefile KERNEL_VER (default: SW_EBPF_KERNEL_DEVEL_VERSION in pins.env)
  --kernel-arch ARCH     Makefile KERNEL_ARCH (default: uname -m; only x86_64)
  --opensnitch-dir DIR   opensnitch git checkout (default: vendor/opensnitch; never modified)
  --dry-run              run every pin/argument/source check (including the
                         ebpf_prog tree hash), print the plan; needs no clang
                         and no kernel tree
  --help                 show this help

Inputs come from flags only: the environment is discarded. Fails if
clang/llc/llvm-strip/make are missing or not the pinned major version, if the
kernel trees are not the pinned kernel-devel tree, if the opensnitch commit
differs from SW_OPENSNITCH_COMMIT (which must also equal
packaging/release/pins.env), or if the extracted ebpf_prog tree does not match
SW_EBPF_PROG_TREE_SHA256.
USAGE
}

STAGING="" DRY_RUN=0 KVER="" KARCH="" OS_DIR="$REPO_ROOT/vendor/opensnitch"
KERNEL_DIR="" KERNEL_HEADERS=""
while [[ $# -gt 0 ]]; do
    case "$1" in
    --help | -h) usage; exit 0 ;;
    --dry-run) DRY_RUN=1 ;;
    --staging | --kernel-dir | --kernel-headers | --kernel-ver | --kernel-arch | --opensnitch-dir)
        [[ $# -ge 2 ]] || die "$1 needs a value"
        case "$1" in
        --staging) STAGING="$2" ;;
        --kernel-dir) KERNEL_DIR="$2" ;;
        --kernel-headers) KERNEL_HEADERS="$2" ;;
        --kernel-ver) KVER="$2" ;;
        --kernel-arch) KARCH="$2" ;;
        --opensnitch-dir) OS_DIR="$2" ;;
        esac
        shift
        ;;
    *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

# ---- pins ----------------------------------------------------------------
[[ -f "$PINS_FILE" ]] || die "pins file not found: $PINS_FILE"
# Pins files are parsed, never sourced: only plain KEY=value lines count, and
# every value must match its format.
pin() { clean sed -n "s/^$1=//p" "$2" | clean head -n1; }
KVER_RE='^[0-9]+\.[0-9]+[0-9A-Za-z._+-]*$'
declare -A PIN_RE=(
    [SW_OPENSNITCH_COMMIT]='^[0-9a-f]{40}$'
    [SW_EBPF_PROG_TREE_SHA256]='^[0-9a-f]{64}$'
    [SW_EBPF_CLANG_MAJOR]='^[0-9]+$'
    [SW_EBPF_LLVM_MAJOR]='^[0-9]+$'
    [SW_EBPF_LLC_MAJOR]='^[0-9]+$'
    [SW_EBPF_KERNEL_DEVEL_VERSION]="$KVER_RE"
)
for v in SW_OPENSNITCH_COMMIT SW_EBPF_PROG_TREE_SHA256 SW_EBPF_CLANG_MAJOR SW_EBPF_LLVM_MAJOR SW_EBPF_LLC_MAJOR SW_EBPF_KERNEL_DEVEL_VERSION; do
    val="$(pin "$v" "$PINS_FILE")"
    [[ -n "$val" ]] || die "$v not set in $PINS_FILE"
    [[ "$val" =~ ${PIN_RE[$v]} ]] || die "invalid $v in $PINS_FILE (must match ${PIN_RE[$v]}): $val"
    printf -v "$v" '%s' "$val"
done
KVER="${KVER:-$SW_EBPF_KERNEL_DEVEL_VERSION}"

# The release pins are the source of truth for the daemon commit.
[[ -f "$RELEASE_PINS" ]] || die "release pins not found: $RELEASE_PINS"
release_commit="$(pin SW_OPENSNITCH_COMMIT "$RELEASE_PINS")"
[[ "$release_commit" == "$SW_OPENSNITCH_COMMIT" ]] ||
    die "commit pin mismatch: $PINS_FILE has $SW_OPENSNITCH_COMMIT, $RELEASE_PINS has $release_commit"

# ---- staging ---------------------------------------------------------------
[[ -n "$STAGING" ]] || { usage >&2; die "--staging is required"; }
[[ "$STAGING" == /* ]] || die "--staging must be absolute: $STAGING"
STAGING="$(clean realpath -m -- "$STAGING")" || die "cannot resolve --staging $STAGING"
if [[ -e "$STAGING" ]]; then
    [[ -d "$STAGING" ]] || die "--staging $STAGING is not a directory"
    [[ -z "$(clean find "$STAGING" -mindepth 1 -maxdepth 1 -print -quit)" ]] ||
        die "--staging $STAGING must be absent or empty"
fi
DEST="$STAGING/$DEST_REL"

# ---- make arguments ------------------------------------------------------
# Values reach make as VAR=value and make expands $(...), so they are checked
# against strict patterns first.
KARCH="${KARCH:-$(clean uname -m)}"
[[ "$KARCH" =~ ^[A-Za-z0-9_]+$ ]] || die "invalid KERNEL_ARCH (allowed: [A-Za-z0-9_]): $KARCH"
# The Makefile derives its own ARCH (and the include paths) from `uname -m`, so
# an override cannot be made consistent: only the native x86_64 build is allowed.
[[ "$KARCH" == x86_64 ]] || die "unsupported KERNEL_ARCH '$KARCH': only x86_64 is supported"
[[ "$(clean uname -m)" == x86_64 ]] || die "host is not x86_64; cross builds are unsupported"
[[ "$KVER" =~ $KVER_RE ]] || die "invalid KERNEL_VER (must match $KVER_RE): $KVER"
safe_path() { # <label> <value>: absolute, only [A-Za-z0-9._/+-]
    [[ "$2" =~ ^/[A-Za-z0-9._/+-]*$ ]] ||
        die "invalid $1 (must be absolute, allowed chars [A-Za-z0-9._/+-]): $2"
}
kver_match() { # <release>: exactly KERNEL_VER, or KERNEL_VER followed by a non-digit
    [[ "$1" == "$KVER" ]] || { [[ "$1" == "$KVER"* ]] && [[ "${1:${#KVER}:1}" != [0-9] ]]; }
}

# ---- kernel trees ----------------------------------------------------------
# The headers are an input of the build (kconfig.h is force-included), so only
# the installed kernel-devel tree of the pinned kernel is accepted.
canon() { # <label> <path>: canonical path (dry-run: need not exist)
    if [[ $DRY_RUN -eq 1 ]]; then
        clean realpath -m -- "$2" || die "cannot resolve $1: $2"
    else
        clean realpath -e -- "$2" || die "$1 does not exist: $2"
    fi
}
check_kernel_tree() { # <label> <canonical path>
    safe_path "$1" "$2"
    [[ "$2" =~ ^/usr/src/kernels/[^/]+$ ]] || die "$1 must be /usr/src/kernels/<release>: $2"
    kver_match "${2##*/}" || die "$1 $2 does not match KERNEL_VER $KVER"
    if [[ $DRY_RUN -eq 1 ]]; then return 0; fi
    local rel owner
    rel="$(clean head -n1 -- "$2/include/config/kernel.release")" ||
        die "$1 $2 lacks include/config/kernel.release"
    kver_match "$rel" || die "$1 kernel.release '$rel' does not match KERNEL_VER $KVER"
    if [[ -x /usr/bin/rpm ]]; then
        owner="$(clean rpm -qf --qf '%{NAME}\n' -- "$2")" || die "$1 $2 is not owned by an rpm package"
        [[ "$owner" =~ ^kernel(-[a-z0-9]+)*-devel$ ]] ||
            die "$1 $2 is owned by '$owner', not a kernel-devel package"
    else
        # rpm ownership is the only package-level identity check available.
        log "WARNING: rpm not found: package ownership of $1 $2 NOT verified"
    fi
}
[[ -n "$KERNEL_DIR" ]] || die "--kernel-dir is required"
KERNEL_HEADERS="${KERNEL_HEADERS:-$KERNEL_DIR}"
KERNEL_DIR="$(canon KERNEL_DIR "$KERNEL_DIR")"
KERNEL_HEADERS="$(canon KERNEL_HEADERS "$KERNEL_HEADERS")"
check_kernel_tree KERNEL_DIR "$KERNEL_DIR"
check_kernel_tree KERNEL_HEADERS "$KERNEL_HEADERS"
if [[ $DRY_RUN -eq 0 ]]; then
    [[ -f "$KERNEL_DIR/include/linux/kconfig.h" ]] ||
        die "KERNEL_DIR=$KERNEL_DIR lacks include/linux/kconfig.h (install kernel-devel $SW_EBPF_KERNEL_DEVEL_VERSION)"
    [[ -d "$KERNEL_HEADERS/include" ]] || die "KERNEL_HEADERS=$KERNEL_HEADERS lacks include/"
fi

# ---- opensnitch source -----------------------------------------------------
# The checkout must be a git checkout at exactly the pinned commit (dry-run too).
OS_DIR="$(clean realpath -e -- "$OS_DIR")" || die "opensnitch dir not found: $OS_DIR"
[[ -e "$OS_DIR/.git" ]] || die "$OS_DIR is not a git checkout; cannot verify the commit pin"
# --no-replace-objects: refs/replace must not alter the pinned tree.
GIT=(git --no-pager --no-replace-objects -c core.fsmonitor=false -C "$OS_DIR")
actual="$(clean "${GIT[@]}" rev-parse --verify HEAD)" || die "cannot read HEAD of $OS_DIR"
[[ "$actual" == "$SW_OPENSNITCH_COMMIT" ]] ||
    die "commit pin mismatch: $OS_DIR is at $actual, pin is $SW_OPENSNITCH_COMMIT"

WORK="$(clean mktemp -d)"
trap 'clean rm -rf -- "$WORK"' EXIT
SRC="$WORK/src"
# Raw blobs of the pinned tree (ls-tree + cat-file blob): no checkout, no
# `git archive`, so no attributes (filters, export-subst, export-ignore, ident)
# can change what is built. Every entry is validated before anything is written.
clean "${GIT[@]}" ls-tree -r -z --full-tree "$SW_OPENSNITCH_COMMIT" -- ebpf_prog >"$WORK/tree.lst" ||
    die "cannot list ebpf_prog of $SW_OPENSNITCH_COMMIT"
TAB=$'\t'
entry_re="^([0-9]+) ([a-z]+) ([0-9a-f]{40})${TAB}(.+)$"
path_re='^[A-Za-z0-9._+-]+(/[A-Za-z0-9._+-]+)*$'
shas=() rels=()
while IFS= read -r -d '' entry; do
    [[ "$entry" =~ $entry_re ]] || die "unexpected ls-tree entry: $entry"
    mode="${BASH_REMATCH[1]}" type="${BASH_REMATCH[2]}" sha="${BASH_REMATCH[3]}" path="${BASH_REMATCH[4]}"
    [[ "$mode $type" == "100644 blob" ]] ||
        die "refusing $path: not a regular file (mode $mode $type; symlinks, executables and submodules are refused)"
    rel="${path#ebpf_prog/}"
    [[ "$path" == ebpf_prog/* && "$rel" =~ $path_re && "/$rel/" != */./* && "/$rel/" != */../* ]] ||
        die "refusing unsafe path: $path"
    [[ "$rel" != *.o && "$rel" != *.bc ]] || die "refusing prebuilt object in source tree: $path"
    shas+=("$sha") rels+=("$rel")
done <"$WORK/tree.lst"
[[ ${#rels[@]} -gt 0 ]] || die "ebpf_prog is missing or empty at $SW_OPENSNITCH_COMMIT"
clean mkdir -- "$SRC"
for i in "${!rels[@]}"; do
    rel="${rels[i]}"
    if [[ "$rel" == */* ]]; then clean mkdir -p -- "$SRC/${rel%/*}" || die "cannot create dir for $rel"; fi
    clean "${GIT[@]}" cat-file blob "${shas[i]}" >"$SRC/$rel" || die "cannot extract $rel"
done
odd="$(clean find "$SRC" \( ! -type f ! -type d \) -o -name '*.o' -o -name '*.bc' | clean head -n1)"
[[ -z "$odd" ]] || die "refusing $odd in the extracted tree (symlink, special file or prebuilt object)"
# Paths + contents, independent of mtimes; recipe documented in pins.env.
tree_sha="$(cd -P -- "$SRC" && clean find . -type f -print0 | clean sort -z |
    clean xargs -0 /usr/bin/sha256sum | clean sha256sum)"
tree_sha="${tree_sha%% *}"
[[ "$tree_sha" == "$SW_EBPF_PROG_TREE_SHA256" ]] ||
    die "ebpf_prog tree hash mismatch: got $tree_sha, pin SW_EBPF_PROG_TREE_SHA256 is $SW_EBPF_PROG_TREE_SHA256"
log "ebpf_prog tree hash ok ($tree_sha)"

if [[ $DRY_RUN -eq 1 ]]; then
    log "dry-run: would run make -B KERNEL_DIR=$KERNEL_DIR KERNEL_HEADERS=$KERNEL_HEADERS KERNEL_VER=$KVER KERNEL_ARCH=$KARCH"
    log "dry-run: would require clang/llvm-strip/llc major $SW_EBPF_CLANG_MAJOR/$SW_EBPF_LLVM_MAJOR/$SW_EBPF_LLC_MAJOR, run llvm-strip -g"
    log "dry-run: would stage ${MODULES[*]} + SHA256SUMS under $DEST/"
    exit 0
fi

# ---- toolchain -------------------------------------------------------------
for tool in clang llc llvm-strip make; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool not found in PATH"
done
# The tools checked below are the ones make must use (Makefile: CC = clang,
# LLC ?= llc), so they are passed to make as absolute paths.
# Limit: only the reported major version is checked, not which package
# provides the binary (no `rpm -qf`/`rpm -V`); the toolchain is trusted to be
# the system one under /usr/bin.
MAKE_BIN="$(command -v make)"
safe_path make "$MAKE_BIN"
CLANG_BIN="$(command -v clang)"
LLC_BIN="$(command -v llc)"
safe_path clang "$CLANG_BIN"
safe_path llc "$LLC_BIN"
STRIP_BIN="$(command -v llvm-strip)"
safe_path llvm-strip "$STRIP_BIN"
clang_major="$(clean "$CLANG_BIN" --version | clean sed -n 's/.*clang version \([0-9]*\)\..*/\1/p' | clean head -n1)"
[[ "$clang_major" == "$SW_EBPF_CLANG_MAJOR" ]] ||
    die "clang major is '${clang_major:-unknown}', pin is $SW_EBPF_CLANG_MAJOR"
strip_major="$(clean "$STRIP_BIN" --version | clean sed -n 's/.*LLVM version \([0-9]*\)\..*/\1/p' | clean head -n1)"
[[ "$strip_major" == "$SW_EBPF_LLVM_MAJOR" ]] ||
    die "llvm-strip major is '${strip_major:-unknown}', pin is $SW_EBPF_LLVM_MAJOR"
llc_major="$(clean "$LLC_BIN" --version | clean sed -n 's/.*LLVM version \([0-9]*\)\..*/\1/p' | clean head -n1)"
[[ "$llc_major" == "$SW_EBPF_LLC_MAJOR" ]] ||
    die "llc major is '${llc_major:-unknown}', pin is $SW_EBPF_LLC_MAJOR"

# ---- build -----------------------------------------------------------------
log "building modules (KERNEL_VER=$KVER)"
# make runs with an empty environment (nothing inherited can reach the
# Makefile: MAKEFILES, GNUMAKEFLAGS, ARCH, ...), -R/-r drop built-in variables
# and rules, -B rebuilds every target regardless of timestamps, and every
# value comes from an explicit, validated assignment.
clean "$MAKE_BIN" -B -R -r -C "$SRC" CC="$CLANG_BIN" LLC="$LLC_BIN" \
    KERNEL_ARCH="$KARCH" KERNEL_DIR="$KERNEL_DIR" KERNEL_HEADERS="$KERNEL_HEADERS" KERNEL_VER="$KVER"
for m in "${MODULES[@]}"; do
    [[ -f "$SRC/$m" && ! -L "$SRC/$m" && -s "$SRC/$m" ]] || die "build did not produce a regular file $m"
done
(cd -P -- "$SRC" && clean "$STRIP_BIN" -g "${MODULES[@]}")
for m in "${MODULES[@]}"; do
    [[ -f "$SRC/$m" && ! -L "$SRC/$m" ]] || die "$m is not a regular file after strip"
done
(cd -P -- "$SRC" && clean sha256sum -- "${MODULES[@]}") >"$WORK/SHA256SUMS"

# ---- stage -----------------------------------------------------------------
# Staging was checked absent or empty; the final dir is created without -p so
# it cannot pre-exist.
clean mkdir -p -- "$STAGING/${DEST_REL%/*}"
clean mkdir -- "$DEST" || die "$DEST already exists"
for m in "${MODULES[@]}"; do clean install -m 0644 -- "$SRC/$m" "$DEST/$m"; done
clean install -m 0644 -- "$WORK/SHA256SUMS" "$DEST/SHA256SUMS"
log "staged ${MODULES[*]} in $DEST"
clean cat -- "$DEST/SHA256SUMS"
