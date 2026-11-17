#!/usr/bin/env bash
# Run the root-only tests from #35 as root and fail if any was skipped.
# Built as the unprivileged user (no root-owned files in target/); only the
# produced test binary runs under sudo.
set -euo pipefail

filters=(
  activation::tests::owner_incoming_drops_a_foreign_uid_peer_and_keeps_accepting
  user_socket::tests::a_directory_owned_by_someone_else_is_refused
  tests::root_unix_incoming_rejects_non_root_and_keeps_accepting_every_peer
)

bin=$(cargo test --locked -p snitchwatch-bridge-cli --lib --no-run --message-format=json |
  jq -r 'select(.profile.test == true and .target.kind[0] == "lib") | .executable // empty' | tail -n1)
[ -x "$bin" ] || { echo "test binary not found" >&2; exit 1; }

# Two tests re-exec this binary as uid 65534, which cannot traverse the
# runner's home (0750). Run a copy from a world-executable directory.
pub=$(mktemp -d)
trap 'rm -rf "$pub"' EXIT
chmod 755 "$pub"
cp "$bin" "$pub/tests-bin"
chmod 755 "$pub/tests-bin"
bin="$pub/tests-bin"

if [ "$(id -u)" -eq 0 ]; then runner=(); else runner=(sudo -n); fi
out=$("${runner[@]}" "$bin" --exact "${filters[@]}" --nocapture 2>&1) || { echo "$out"; exit 1; }
echo "$out"

if grep -q 'SKIPPED-ROOT-ONLY' <<<"$out"; then
  echo "root-only test was skipped" >&2
  exit 1
fi
for f in "${filters[@]}"; do
  grep -qF "test $f ... ok" <<<"$out" || { echo "not ok: $f" >&2; exit 1; }
done
