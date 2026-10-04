#!/usr/bin/env bash
# Build snitchwatch-bridge-cli inside the pinned Fedora builder container,
# stage it with the systemd user unit and license files, pack the
# deterministic release tarball and self-verify it.
#
# Contract: docs/superpowers/plans/2026-10-03-bridge-release-artifact.md
# ("Interfaces" -> build-bridge.sh). Container only.
#
# Environment (all optional):
#   SRC_DIR           repo checkout (default: derived from this script's path).
#                     May be a READ-ONLY mount: nothing is ever written into it.
#   OUT_DIR           where the tarball + .sha256 land (default: $SRC_DIR/dist)
#   CARGO_TARGET_DIR  default /tmp/sw-target
#   CARGO_HOME        default $HOME/.cargo
#   SW_GIT_SHA        commit being built (default: HEAD; must equal HEAD)
#   SW_RELEASE_TAG    non-empty => release mode: must be v$VERSION, the tree
#                     must be clean (git-ignored files under the build inputs
#                     count as dirty), a repo-root LICENSE must exist, and if
#                     the tag exists in the checkout it must point at HEAD
#   ALLOW_DIRTY=1     dev mode only: allow a dirty tree (MANIFEST "dirty": true)
#   SW_INSTALL_DEPS=1 run install-deps.sh first
#
# Toolchain override variables (RUSTC_WRAPPER, RUSTC, *_RUSTFLAGS, the x86_64
# linker, ...) are always unset: the MANIFEST records one exact build command.
#
# The last two lines of stdout are `TARBALL=<path>` and `SHA256=<hex>`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=/dev/null # pins.env: plain KEY=value lines (SW_*)
. "$SCRIPT_DIR/pins.env"
ARTIFACT_TOOL="$SCRIPT_DIR/bridge_artifact.py"
# Recorded in the MANIFEST instead of the real flags: nothing in the artifact
# may depend on the absolute checkout path (repro-check builds from two).
RUSTFLAGS_TEMPLATE='--remap-path-prefix=<src>=/snitchwatch --remap-path-prefix=<cargo_home>=/cargo --remap-path-prefix=<target>=/target'
# Everything the binary is built from. rust-embed embeds EVERY file under web/
# (git-ignored *.swp/.DS_Store included), so ignored files here are dirty too.
BUILD_INPUTS=(web crates/snitchwatch-bridge crates/snitchwatch-bridge-cli crates/snitchwatch-proto
    vendor/opensnitch/proto Cargo.toml Cargo.lock packaging/systemd)
# Each would silently change what the MANIFEST's build command/rustflags say.
TOOLCHAIN_OVERRIDES=(RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER RUSTC RUSTC_WORKSPACE_WRAPPER
    CARGO_BUILD_RUSTC CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS CARGO_BUILD_RUSTFLAGS CARGO_ENCODED_RUSTFLAGS)

# Git reads a possibly read-only mount owned by another uid (CI container).
export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0='*'
export GIT_OPTIONAL_LOCKS=0

WORK=""
cleanup() {
    if [[ -n "$WORK" && -d "$WORK" ]]; then rm -rf -- "$WORK"; fi
}
trap cleanup EXIT

log() { echo "build-bridge: $*" >&2; }
die() {
    echo "build-bridge: ERROR: $*" >&2
    exit 1
}

# Create (if needed) and print the physical absolute path of a directory.
abs_dir() {
    mkdir -p "$1" && (cd "$1" && pwd -P)
}

# The MANIFEST records SW_BUILDER_IMAGE as the build environment, so refuse
# anything else: a host, or a toolbox/distrobox (long-lived, shares $HOME).
require_container() {
    [[ -e /run/.containerenv || -e /.dockerenv ]] ||
        die "run this inside the pinned builder container ($SW_BUILDER_IMAGE), not on a host"
    [[ ! -e /run/.toolboxenv && ! -e /run/host ]] ||
        die "run this in a fresh container from $SW_BUILDER_IMAGE, not a toolbox/distrobox"
}

resolve_dirs() {
    SRC_DIR="$(cd "${SRC_DIR:-$SCRIPT_DIR/../..}" && pwd -P)" || die "SRC_DIR does not exist"
    OUT_DIR="$(abs_dir "${OUT_DIR:-$SRC_DIR/dist}")" ||
        die "cannot create OUT_DIR (is SRC_DIR read-only? then pass OUT_DIR, e.g. /out)"
    [[ -w "$OUT_DIR" ]] || die "OUT_DIR $OUT_DIR is not writable"
    CARGO_TARGET_DIR="$(abs_dir "${CARGO_TARGET_DIR:-/tmp/sw-target}")"
    CARGO_HOME="$(abs_dir "${CARGO_HOME:-$HOME/.cargo}")"
    export CARGO_TARGET_DIR CARGO_HOME
    local dir
    for dir in "$SRC_DIR" "$CARGO_HOME" "$CARGO_TARGET_DIR"; do
        [[ "$dir" != *[[:space:]]* ]] || die "path '$dir' contains whitespace; RUSTFLAGS cannot carry it"
    done
}

clear_toolchain_overrides() {
    local var
    for var in "${TOOLCHAIN_OVERRIDES[@]}"; do
        if [[ -v "$var" ]]; then log "unsetting $var (it was set in the environment)"; fi
        unset "$var"
    done
    log "toolchain overrides unset: ${TOOLCHAIN_OVERRIDES[*]}"
}

check_toolchain() {
    local arch rustc_v protoc_v gcc_v
    arch="$(uname -m)"
    [[ "$arch" == "$SW_ARCH" ]] || die "machine arch is $arch, pins.env SW_ARCH is $SW_ARCH"
    rustc_v="$(rustc --version)" || die "rustc not found (SW_INSTALL_DEPS=1 installs it)"
    [[ "$rustc_v" == "rustc ${SW_RUST_VERSION} "* ]] ||
        die "builder has '$rustc_v' but pins.env pins SW_RUST_VERSION=$SW_RUST_VERSION;" \
            "the image's Fedora rust RPM moved — bump SW_RUST_VERSION in packaging/release/pins.env" \
            "(and re-run the repro check)"
    protoc_v="$(protoc --version)" || die "protoc not found (SW_INSTALL_DEPS=1 installs it)"
    [[ "$protoc_v" == "libprotoc ${SW_PROTOC_VERSION}" ]] ||
        die "builder has '$protoc_v' but pins.env pins SW_PROTOC_VERSION=$SW_PROTOC_VERSION;" \
            "bump SW_PROTOC_VERSION in packaging/release/pins.env"
    gcc_v="$(gcc --version | sed -n 1p)" || die "gcc not found (SW_INSTALL_DEPS=1 installs it)"
    log "toolchain: $rustc_v | $protoc_v | $gcc_v"
}

check_submodule() {
    local kind sha sub_head
    read -r _ kind sha _ < <(git -C "$SRC_DIR" ls-tree HEAD vendor/opensnitch) ||
        die "HEAD has no vendor/opensnitch entry"
    [[ "$kind" == commit && "$sha" == "$SW_OPENSNITCH_COMMIT" ]] ||
        die "HEAD:vendor/opensnitch gitlink is $sha ($kind), pins.env SW_OPENSNITCH_COMMIT is $SW_OPENSNITCH_COMMIT"
    sub_head="$(git -C "$SRC_DIR/vendor/opensnitch" rev-parse HEAD)" ||
        die "cannot read vendor/opensnitch HEAD"
    [[ "$sub_head" == "$SW_OPENSNITCH_COMMIT" ]] ||
        die "checked-out vendor/opensnitch is $sub_head, want $SW_OPENSNITCH_COMMIT" \
            "(git submodule update --init vendor/opensnitch)"
}

# `!! <path>` for every git-ignored file under the build inputs (`git status
# --porcelain` alone hides them). The superproject's status does not descend
# into the submodule, so vendor/opensnitch/proto (protoc's include dir) is
# asked separately — there untracked (`??`) files count too, since the main
# check deliberately ignores untracked files inside the submodule.
ignored_inputs() {
    git -C "$SRC_DIR" status --porcelain --ignored -- "${BUILD_INPUTS[@]}" | sed -n '/^!! /p' ||
        return 1
    git -C "$SRC_DIR/vendor/opensnitch" status --porcelain --ignored -- proto |
        sed -n 's#^\(!!\|??\) #\1 vendor/opensnitch/#p' || return 1
}

# Release mode: when the tag exists in this checkout it must name HEAD. A local
# pre-tag repro build has no tag yet; that is logged, not refused.
check_release_tag() {
    local head="$1" tagged
    if git -C "$SRC_DIR" rev-parse -q --verify "refs/tags/$SW_RELEASE_TAG" >/dev/null; then
        tagged="$(git -C "$SRC_DIR" rev-parse "refs/tags/$SW_RELEASE_TAG^{commit}")" ||
            die "cannot resolve tag $SW_RELEASE_TAG to a commit"
        [[ "$tagged" == "$head" ]] ||
            die "tag $SW_RELEASE_TAG points at $tagged but the checkout's HEAD is $head"
        log "tag $SW_RELEASE_TAG points at HEAD ($head)"
    else
        log "tag $SW_RELEASE_TAG does not exist in this checkout (pre-tag build?);" \
            "not checking it against HEAD"
    fi
}

# Sets GIT_SHA and DIRTY; enforces release-mode / ALLOW_DIRTY rules.
check_source() {
    local head status ignored
    head="$(git -C "$SRC_DIR" rev-parse HEAD)" || die "SRC_DIR $SRC_DIR is not a git checkout"
    GIT_SHA="${SW_GIT_SHA:-$head}"
    [[ "$GIT_SHA" == "$head" ]] || die "SW_GIT_SHA=$GIT_SHA but the checkout's HEAD is $head"
    status="$(git -C "$SRC_DIR" status --porcelain --ignore-submodules=untracked)" ||
        die "git status failed"
    ignored="$(ignored_inputs)" || die "git status --ignored failed"
    if [[ -n "$ignored" ]]; then
        log "git-ignored files inside the build inputs (they would be built/embedded):"$'\n'"$ignored"
        status+="${status:+$'\n'}$ignored"
    fi
    DIRTY=0
    [[ -z "$status" ]] || DIRTY=1
    if [[ -n "${SW_RELEASE_TAG:-}" ]]; then
        [[ "$SW_RELEASE_TAG" == "v$VERSION" ]] ||
            die "SW_RELEASE_TAG=$SW_RELEASE_TAG does not match the crate version (want v$VERSION)"
        [[ "$DIRTY" == 0 ]] || die "release builds need a clean tree; git status:"$'\n'"$status"
        [[ -f "$SRC_DIR/LICENSE" ]] ||
            die "release builds need a repo-root LICENSE and there is none: the project's own" \
                "license decision (decision G in docs/superpowers/plans/2026-10-03-bridge-release-artifact.md)" \
                "is still open"
        check_release_tag "$head"
    else
        [[ "$DIRTY" == 0 || "${ALLOW_DIRTY:-0}" == 1 ]] ||
            die "the source tree is dirty (ALLOW_DIRTY=1 permits a dev build):"$'\n'"$status"
        [[ -f "$SRC_DIR/LICENSE" ]] ||
            log "WARNING: no repo-root LICENSE (plan decision G still open); dev build ships" \
                "third-party attributions only"
    fi
}

build_binary() {
    SOURCE_DATE_EPOCH="$(git -C "$SRC_DIR" log -1 --format=%ct HEAD)"
    export SOURCE_DATE_EPOCH
    export RUSTFLAGS="--remap-path-prefix=$SRC_DIR=/snitchwatch --remap-path-prefix=$CARGO_HOME=/cargo --remap-path-prefix=$CARGO_TARGET_DIR=/target"
    log "building snitchwatch-bridge-cli $VERSION @ $GIT_SHA (dirty=$DIRTY," \
        "SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH)"
    # --no-default-features: no embedded web/ UI (GPL-2.0-only; plan decision I).
    (cd "$SRC_DIR" && cargo build --release --locked -p snitchwatch-bridge-cli --no-default-features)
    BIN="$CARGO_TARGET_DIR/release/snitchwatch-bridge-cli"
    [[ -f "$BIN" ]] || die "cargo did not produce $BIN"
}

# THIRD-PARTY-LICENSES.md for the crates statically linked into the binary.
# `cargo metadata` needs every workspace package's manifest, but the build only
# fetched the bridge's own closure, so fetch the rest of Cargo.lock first.
write_thirdparty() {
    local out="$1"
    log "cargo fetch --locked (whole lockfile, $SW_ARCH-unknown-linux-gnu) for cargo metadata"
    cargo fetch --locked --target "$SW_ARCH-unknown-linux-gnu" --manifest-path "$SRC_DIR/Cargo.toml"
    python3 "$ARTIFACT_TOOL" thirdparty --repo "$SRC_DIR" --out "$out"
}

stage_files() {
    local stage="$1" licenses="$1/usr/share/licenses/snitchwatch-bridge"
    install -D -m 0755 "$BIN" "$stage/usr/bin/snitchwatch-bridge-cli"
    install -D -m 0644 "$SRC_DIR/packaging/systemd/snitchwatch-bridge.service" \
        "$stage/usr/lib/systemd/user/snitchwatch-bridge.service"
    if [[ -f "$SRC_DIR/LICENSE" ]]; then
        install -D -m 0644 "$SRC_DIR/LICENSE" "$licenses/LICENSE"
    fi
    install -D -m 0644 "$SRC_DIR/vendor/opensnitch/LICENSE" "$licenses/LICENSE.opensnitch"
    write_thirdparty "$licenses/THIRD-PARTY-LICENSES.md"
}

pack_and_verify() {
    local work="$1" tarball sha
    local -a pack_args=(
        --stage "$work/stage" --name snitchwatch-bridge --version "$VERSION" --arch "$SW_ARCH"
        --git-commit "$GIT_SHA" --source-date-epoch "$SOURCE_DATE_EPOCH"
        --meta "$work/meta.json" --out "$OUT_DIR"
    )
    if [[ -n "${SW_RELEASE_TAG:-}" ]]; then pack_args+=(--git-tag "$SW_RELEASE_TAG"); fi
    if [[ "$DIRTY" == 1 ]]; then pack_args+=(--dirty); fi

    python3 "$ARTIFACT_TOOL" buildinfo --repo "$SRC_DIR" --binary "$work/stage/usr/bin/snitchwatch-bridge-cli" \
        --builder-image "$SW_BUILDER_IMAGE" --rustflags-template "$RUSTFLAGS_TEMPLATE" >"$work/meta.json"
    tarball="$(python3 "$ARTIFACT_TOOL" pack "${pack_args[@]}")"
    # The digest of the bytes just written is the trust anchor that lets verify
    # ldd/run the binary inside them.
    sha="$(sha256sum "$tarball" | cut -d' ' -f1)"
    python3 "$ARTIFACT_TOOL" verify --tarball "$tarball" --expect-version "$VERSION" \
        --expect-sha256 "$sha" --check-ldd --run-flags
    echo "TARBALL=$tarball"
    echo "SHA256=$sha"
}

main() {
    require_container
    if [[ "${SW_INSTALL_DEPS:-0}" == 1 ]]; then "$SCRIPT_DIR/install-deps.sh"; fi
    resolve_dirs
    clear_toolchain_overrides
    VERSION="$(python3 "$ARTIFACT_TOOL" version --repo "$SRC_DIR")"
    check_toolchain
    check_submodule
    check_source
    build_binary

    WORK="$(mktemp -d)"
    stage_files "$WORK/stage"
    pack_and_verify "$WORK"
}

main "$@"
