#!/usr/bin/env bash
# Verify the bridge's systemd *user* unit for real. Run from `just package-check`.
#
# Why this is more than `systemd-analyze verify --user <unit>`:
#   * The shipped unit says `ExecStart=/usr/bin/snitchwatch-bridge-cli`, which
#     doesn't exist on a dev box or in CI, so verify reports "not executable".
#     We verify a copy whose ExecStart points at a freshly built binary.
#   * The copy is RENAMED. Verifying `snitchwatch-bridge.service` directly lets a
#     `~/.config/systemd/user/snitchwatch-bridge.service.d/` drop-in (or a dev
#     copy of the unit) shadow the file under test, so a broken unit passes.
#     The new name must also share NO prefix with the real unit: since
#     systemd 246 a unit `a-b-c.service` also picks up the prefix drop-ins
#     `a-.service.d/` and `a-b-.service.d/`, so a merely suffixed name such as
#     `snitchwatch-bridge-pkgcheck.service` would still be shadowed by
#     `snitchwatch-.service.d/`. A dash-free name has no prefix drop-in dirs.
#   * verify exits 0 for some problems (e.g. "Unknown key"), but its diagnostics
#     always name the unit file, so any output line naming our copy is a failure.
#     Lines about unrelated units on the machine are ignored.
#
# "Skipped" is printed only when systemd-analyze is genuinely absent.
#
# Optional env: SW_UNIT_FILE overrides the unit under test (used to prove the
# failure path against a deliberately broken scratch copy; never the real unit).
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
unit_src="${SW_UNIT_FILE:-$repo_root/packaging/systemd/snitchwatch-bridge.service}"
shipped_exec='ExecStart=/usr/bin/snitchwatch-bridge-cli'
# Dash-free and unrelated to `snitchwatch-bridge` (see above); the PID keeps
# concurrent runs apart.
unit_name="swpkgcheck$$"

if ! command -v systemd-analyze >/dev/null 2>&1; then
    echo "systemd-analyze not installed — skipped unit verify"
    exit 0
fi

cd "$repo_root"
cargo build -p snitchwatch-bridge-cli

target_dir="$(cargo metadata --format-version 1 --no-deps |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
binary="$target_dir/debug/snitchwatch-bridge-cli"
if [[ ! -x "$binary" ]]; then
    echo "verify-unit: FAIL: built binary not found at $binary" >&2
    exit 1
fi

tmp="$(mktemp -d)"
trap 'rm -r -- "$tmp"' EXIT
unit_copy="$tmp/$unit_name.service"

replaced=0
while IFS= read -r line || [[ -n "$line" ]]; do
    if [[ "$line" == "$shipped_exec" ]]; then
        printf 'ExecStart=%s\n' "$binary"
        replaced=$((replaced + 1))
    else
        printf '%s\n' "$line"
    fi
done <"$unit_src" >"$unit_copy"
if [[ "$replaced" -ne 1 ]]; then
    echo "verify-unit: FAIL: expected exactly one line '$shipped_exec' in $unit_src, found $replaced" >&2
    exit 1
fi

# `systemd-analyze verify --user` errors out without a runtime dir.
if [[ -z "${XDG_RUNTIME_DIR:-}" ]]; then
    mkdir -m 0700 "$tmp/xdg"
    export XDG_RUNTIME_DIR="$tmp/xdg"
fi

if output="$(systemd-analyze verify --user "$unit_copy" 2>&1)"; then
    status=0
else
    status=$?
fi

unit_diagnostics="$(grep -F -- "$unit_name" <<<"$output" || true)"
if [[ "$status" -ne 0 || -n "$unit_diagnostics" ]]; then
    echo "verify-unit: FAIL: systemd-analyze verify reported problems (exit status $status) for $unit_src" >&2
    echo "$output" >&2
    exit 1
fi

echo "systemd unit ok"
