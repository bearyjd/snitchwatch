#!/usr/bin/env bash
# Install the bridge release tarball into / of a THROWAWAY container exactly
# the way the bootc image build consumes it, then check it: SHA256SUMS from /,
# `systemd-analyze verify --user` on the installed unit, `systemctl --global
# enable`, the default.target.wants symlink, and `--version`.
#
# The tarball is verified against <expected-sha256> (from a trusted source,
# e.g. build-bridge.sh's SHA256= line) and installed from the tree that
# `bridge_artifact.py verify --extract-to` wrote from its verified in-memory
# copy — the tarball is never extracted a second time, so nothing can change
# between verification and installation.
#
# It writes to /usr and /etc, so it refuses to run unless
# SW_THROWAWAY_CONTAINER=1 AND a container marker exists.
#
# Usage: SW_THROWAWAY_CONTAINER=1 check-install.sh <tarball> <expected-sha256>
#
# Contract: docs/superpowers/plans/2026-10-03-bridge-release-artifact.md
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
UNIT_NAME=snitchwatch-bridge.service
UNIT_PATH="/usr/lib/systemd/user/$UNIT_NAME"
WANTS_LINK="/etc/systemd/user/default.target.wants/$UNIT_NAME"
DATA_DIR=usr/share/snitchwatch-bridge
LICENSE_NAME_RE='^[A-Za-z0-9][A-Za-z0-9._-]*$'

WORK=""
cleanup() {
    if [[ -n "$WORK" && -d "$WORK" ]]; then rm -rf -- "$WORK"; fi
}
trap cleanup EXIT

log() { echo "check-install: $*" >&2; }
die() {
    echo "check-install: FAIL: $*" >&2
    exit 1
}

refuse_unless_throwaway() {
    [[ "${SW_THROWAWAY_CONTAINER:-}" == 1 ]] ||
        die "refusing to run: this installs into /usr and /etc. Set SW_THROWAWAY_CONTAINER=1," \
            "and only ever inside a disposable container"
    [[ -e /run/.containerenv || -e /.dockerenv ]] ||
        die "refusing to run: no container marker (/run/.containerenv or /.dockerenv)." \
            "Never run this on a real host"
    [[ ! -e /run/.toolboxenv && ! -e /run/host ]] ||
        die "refusing to run in a toolbox/distrobox: it is a long-lived container sharing your" \
            "home, not a throwaway one"
}

manifest_field() {
    python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$1" "$2"
}

# Print the contract mode of an installable path; fail for anything outside the
# allowlist (defense in depth: verify already enforced the exact layout).
contract_mode() {
    local path="$1"
    [[ "$path" != *..* ]] || return 1
    case "$path" in
        usr/bin/snitchwatch-bridge-cli) echo 0755 ;;
        usr/lib/systemd/user/snitchwatch-bridge.service) echo 0644 ;;
        usr/share/licenses/snitchwatch-bridge/*)
            [[ "${path#usr/share/licenses/snitchwatch-bridge/}" =~ $LICENSE_NAME_RE ]] || return 1
            echo 0644
            ;;
        *) return 1 ;;
    esac
}

# Install every MANIFEST files[] entry with its MANIFEST mode, plus the two
# generated files (MANIFEST.json, SHA256SUMS) at 0644.
install_tree() {
    local top="$1" entries mode path want
    entries="$(python3 -c 'import json, sys
for entry in json.load(open(sys.argv[1]))["files"]:
    print(entry["mode"], entry["path"])' "$top/$DATA_DIR/MANIFEST.json")"
    while read -r mode path; do
        want="$(contract_mode "$path")" || die "refusing to install unexpected path '$path'"
        [[ "$mode" == 0644 || "$mode" == 0755 ]] || die "refusing mode '$mode' for $path"
        [[ "$mode" == "$want" ]] || die "$path has mode $mode, the contract says $want"
        [[ -f "$top/$path" && ! -L "$top/$path" ]] || die "$top/$path is not a regular file"
        install -D -m "$mode" "$top/$path" "/$path"
        log "installed /$path ($mode)"
    done <<<"$entries"
    for path in "$DATA_DIR/MANIFEST.json" "$DATA_DIR/SHA256SUMS"; do
        install -D -m 0644 "$top/$path" "/$path"
        log "installed /$path (0644)"
    done
}

check_unit() {
    local runtime_dir="$1" output rc=0
    output="$(XDG_RUNTIME_DIR="$runtime_dir" systemd-analyze verify --user "$UNIT_PATH" 2>&1)" || rc=$?
    if [[ -n "$output" ]]; then printf '%s\n' "$output" >&2; fi
    [[ "$rc" -eq 0 ]] || die "systemd-analyze verify --user $UNIT_PATH exited $rc"
    [[ "$output" != *"$UNIT_NAME"* ]] || die "systemd-analyze reported diagnostics for $UNIT_NAME"
    log "systemd-analyze verify --user: clean"
}

check_enable() {
    local state target
    systemctl --global enable "$UNIT_NAME"
    state="$(systemctl --global is-enabled "$UNIT_NAME" || true)"
    [[ "$state" == enabled ]] || die "systemctl --global is-enabled says '$state', want 'enabled'"
    [[ -L "$WANTS_LINK" ]] || die "$WANTS_LINK is not a symlink"
    target="$(readlink "$WANTS_LINK")"
    [[ "$target" == "$UNIT_PATH" ]] || die "$WANTS_LINK -> $target, want $UNIT_PATH"
    log "systemctl --global enable: $WANTS_LINK -> $target"
}

check_version() {
    local iso="$1" version="$2" got
    got="$(env -i PATH=/usr/bin:/bin HOME="$iso" XDG_RUNTIME_DIR="$iso" \
        SNITCHWATCH_WS_SOCKET="$iso/bridge.sock" SNITCHWATCH_GRPC_BIND=127.0.0.1:0 \
        timeout 5 /usr/bin/snitchwatch-bridge-cli --version)" ||
        die "/usr/bin/snitchwatch-bridge-cli --version failed or timed out"
    [[ "$got" == "snitchwatch-bridge-cli $version" ]] ||
        die "--version printed '$got', want 'snitchwatch-bridge-cli $version'"
    log "--version: $got"
}

main() {
    refuse_unless_throwaway
    [[ $# -eq 2 && -f "${1:-}" ]] ||
        die "usage: SW_THROWAWAY_CONTAINER=1 check-install.sh <tarball> <expected-sha256>"

    local top version
    WORK="$(mktemp -d)"
    mkdir -m 0700 "$WORK/extract" "$WORK/runtime" "$WORK/iso"
    # Never install something that fails the artifact contract or the trusted
    # digest; install only what verify itself wrote from the verified bytes.
    python3 "$SCRIPT_DIR/bridge_artifact.py" verify --tarball "$1" --expect-sha256 "$2" \
        --extract-to "$WORK/extract"
    top="$(echo "$WORK"/extract/*)"
    [[ -d "$top" ]] || die "expected exactly one top-level directory from verify --extract-to"
    version="$(manifest_field "$top/$DATA_DIR/MANIFEST.json" version)"

    install_tree "$top"
    (cd / && sha256sum -c "/$DATA_DIR/SHA256SUMS") || die "sha256sum -c /$DATA_DIR/SHA256SUMS"
    check_unit "$WORK/runtime"
    check_enable
    check_version "$WORK/iso" "$version"
    log "OK: $(basename "$1") (sha256 $2) installs, verifies and enables cleanly"
}

main "$@"
