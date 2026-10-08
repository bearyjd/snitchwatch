#!/usr/bin/env bash
# Stage the system-bridge overlay into an image root.  This never enables a
# unit or changes the running host; callers copy the staged tree into an image.
set -euo pipefail

die() { printf '%s\n' "stage.sh: $*" >&2; exit 1; }

[[ $# -eq 3 ]] || die "usage: $0 DEST BINARY SHA256"
dest=$(realpath -m -- "$1") || die "cannot resolve DEST"
[[ "$dest" != / ]] || die "DEST must be a staging root, not /"
binary=$(realpath -e -- "$2") || die "cannot resolve BINARY"
expected_sha=$3
[[ -f "$binary" && -x "$binary" ]] || die "BINARY must be an executable regular file"
[[ "$expected_sha" =~ ^[0-9a-fA-F]{64}$ ]] || die "SHA256 must be 64 hexadecimal characters"
actual_sha=$(sha256sum "$binary" | awk '{print $1}')
[[ "${actual_sha,,}" == "${expected_sha,,}" ]] || die "BINARY SHA256 does not match the verified value"

# A legacy bridge binary has no system-mode guard and would silently re-open
# the old TCP listener.  Require the new public help contract before staging.
help=$("$binary" --help 2>&1) || die "BINARY --help failed"
grep -Fq 'SNITCHWATCH_SYSTEM_BRIDGE=1' <<<"$help" || \
  die "BINARY is legacy: --help does not advertise SNITCHWATCH_SYSTEM_BRIDGE=1"

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
install -Dm755 "$binary" "$dest/usr/bin/snitchwatch-bridge-cli"
install -Dm644 "$script_dir/snitchwatch-system-bridge.service" \
  "$dest/usr/lib/systemd/system/snitchwatch-system-bridge.service"
install -Dm644 "$script_dir/snitchwatch-system-bridge-grpc.socket" \
  "$dest/usr/lib/systemd/system/snitchwatch-system-bridge-grpc.socket"
install -Dm644 "$script_dir/snitchwatch-system-bridge-gui.socket" \
  "$dest/usr/lib/systemd/system/snitchwatch-system-bridge-gui.socket"
install -Dm644 "$script_dir/snitchwatch.conf" "$dest/usr/lib/tmpfiles.d/snitchwatch.conf"
install -Dm644 "$script_dir/snitchwatch.conf.sysusers" "$dest/usr/lib/sysusers.d/snitchwatch.conf"
# The one opensnitchd rule Snitchwatch ships: this account's bridge may make
# HTTPS connections, so blocklist downloads work under DefaultAction: deny.
# Same file as the bluebuild image's (docs/superpowers/plans/
# 2026-10-08-packaged-bridge-fetch-rule.md).
fetch_rule=000-snitchwatch-bridge-fetch.json
install -Dm644 "$script_dir/../bluebuild/files/system/etc/opensnitchd/rules/$fetch_rule" \
  "$dest/etc/opensnitchd/rules/$fetch_rule"
