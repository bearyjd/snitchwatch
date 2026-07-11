# Phase 2 packaging — manual verification runbook

Covers the four items `docs/superpowers/plans/2026-07-05-phase2-packaging.md`
flags as "not verifiable in the CI sandbox." Everything else in Phase 2
(YAML/JSON syntax, the Flatpak shape test, `systemd-analyze verify`) already
passes in CI — this runbook is only for the parts that need a real Bazzite
host. Run these in order; each one builds on the state left by the previous.

**Needs:** a real Bazzite (or Fedora Silverblue/Kinoite-family) host with
`bluebuild`, `podman`/`buildah`, `flatpak-builder`, and the KDE runtime
(`org.kde.Platform`) installed. None of this is runnable in a CI sandbox or
container without a real bootable rpm-ostree system underneath it.

**Resolved 2026-07-11:** the Flatpak manifest
(`packaging/flatpak/org.snitchwatch.Snitchwatch.yml`) now packages
`command: snitchwatch-kirigami` against the `org.kde.Platform` runtime, not
`snitchwatch-tauri`/`web/` — Kirigami is the settled GUI stack (see this
repo's `CLAUDE.md` "Settled architecture decisions" #4) and has feature
parity plus a passing Task 7 fullscreen-focus test. `snitchwatch-tauri` and
`web/` remain in the repo but are intentionally not what this manifest
builds; they're kept only until this Flatpak's first real packaged release
ships (owner decision, 2026-07-11). Verify the pinned `runtime-version`
against whatever `org.kde.Platform` release is actually current before
building (`flatpak remote-info flathub org.kde.Platform`) — it was pinned
from this repo's environment at the time of writing, not confirmed against
a live Flathub listing, and may need bumping.

---

## Step 1 — Bluebuild image build

```bash
bluebuild build packaging/bluebuild/recipe.yml
```

**Pass condition:** the image builds without error and produces an OCI
image with `opensnitchd` installed and enabled by default, plus the
fail-closed `/etc/opensnitchd/default-config.json`
(`DefaultAction: deny`, `Server.Address: 127.0.0.1:50051`) baked in at
`packaging/bluebuild/files/system/etc/opensnitchd/default-config.json`.

**Verify the baked config actually landed**, don't just trust the recipe:

```bash
# From a container/VM booted off the built image:
cat /etc/opensnitchd/default-config.json | grep -E 'DefaultAction|Server'
systemctl status opensnitchd    # should be enabled + active
```

**If it fails:** check `bluebuild`'s own error output first — most likely
causes are a `bluebuild` CLI version mismatch against `recipe.yml`'s schema,
or a missing base-image pull. Not a Rust/repo-code problem, so don't start
debugging `snitchwatch-*` crates for this step.

---

## Step 2 — Flatpak build

```bash
# 1. Vendor Cargo deps offline (flatpak-builder has no network mid-build).
python3 flatpak-cargo-generator.py Cargo.lock \
  -o packaging/flatpak/generated-cargo-sources.json

# 2. Build + install locally.
flatpak run org.flatpak.Builder --user --install --force-clean \
  build-dir packaging/flatpak/org.snitchwatch.Snitchwatch.yml
```

**Pass condition:** build completes, `flatpak run org.snitchwatch.Snitchwatch`
launches the Kirigami shell's window.

**Sandbox boundary sanity check** — confirm the isolation claims in the
manifest's own comments actually hold, don't just take the YAML's word for it:

```bash
flatpak run --command=sh org.snitchwatch.Snitchwatch -c \
  'curl -sS --max-time 2 http://127.0.0.1:50051 ; echo "exit=$?"'
# Expect a connection failure (private network namespace) — NOT a successful
# connection. If this succeeds, --share=network leaked in somewhere and the
# Flatpak isolation the whole Unix-socket design depends on is broken.

flatpak run --command=sh org.snitchwatch.Snitchwatch -c \
  'ls -la "$XDG_RUNTIME_DIR/snitchwatch/" 2>&1'
# Expect this to succeed and show bridge.sock + token (only once Step 3's
# bridge is already running) — confirms --filesystem=xdg-run/snitchwatch
# actually crosses the sandbox boundary as designed.
```

**If it fails:** a `flatpak-builder` manifest error is usually a missing
`generated-cargo-sources.json` regeneration (regenerate whenever
`Cargo.lock` changes) or a runtime/SDK version not installed locally
(`flatpak install org.kde.Platform//6.9 org.kde.Sdk//6.9
org.freedesktop.Sdk.Extension.rust-stable//6.9` — match the exact version to
whatever `runtime-version` the manifest currently pins). A build failure
specifically inside the `cxx-qt-build` step (moc/rcc/qmlcachegen not found)
means the KDE SDK's Qt6 build tools aren't on `PATH` inside the build
sandbox — check `org.kde.Sdk`'s own environment setup before assuming a
`snitchwatch-kirigami` code problem.

---

## Step 3 — Live opensnitchd dial-in + bridge systemd unit

Install and start the bridge as its own user service (independent of any
GUI):

```bash
mkdir -p ~/.config/systemd/user
cp packaging/systemd/snitchwatch-bridge.service ~/.config/systemd/user/
# If the binary isn't at /usr/bin/snitchwatch-bridge-cli (e.g. a
# lightweight/DIY install), override ExecStart first:
#   systemctl --user edit snitchwatch-bridge.service
systemctl --user daemon-reload
systemctl --user enable --now snitchwatch-bridge.service
systemctl --user status snitchwatch-bridge.service   # should be active (running)
```

Start a real `opensnitchd` (per `README.md`'s "Running the bridge against
real opensnitchd" section — this is the one step in this runbook where a
real daemon is intentionally in the loop, since that's the whole point of
this verification):

```bash
podman run -d --rm \
    --name opensnitchd-dev \
    --privileged --network=host --pid=host \
    --cap-add=NET_ADMIN,SYS_ADMIN,BPF \
    docker.io/evilsocket/opensnitch:latest
```

Confirm `opensnitchd`'s `Server.Address` in its `default-config.json` points
at `127.0.0.1:50051` (matching `SNITCHWATCH_GRPC_BIND` in the systemd unit),
then generate real traffic from a fresh process and confirm a pending
decision actually reaches a connected client:

```bash
TOKEN=$(cat "$XDG_RUNTIME_DIR/snitchwatch/token")
{ printf '%s\n' "$TOKEN"; cat; } | websocat --unix-listen -t \
    ws-c:unix:"$XDG_RUNTIME_DIR/snitchwatch/bridge.sock":/stream &
curl -sS --max-time 2 https://example.com > /dev/null &   # or any new outbound conn
```

**Pass condition:** the `websocat` client receives a pending-connection
message for the `curl` process; resolving it (allow/deny) through the WS
protocol lets the connection through or blocks it as expected.

---

## Step 4 — Closed-window fail-open fix, the actual point of Phase 2

This is the test that proves the systemd `--user` unit decoupling actually
fixes the problem it was built for — don't skip it even if Step 3 passed,
since Step 3 doesn't exercise "GUI window closed."

```bash
# With snitchwatch-bridge.service still running from Step 3, and the
# snitchwatch-kirigami GUI window explicitly NOT running:
systemctl --user status snitchwatch-bridge.service   # confirm still active

TOKEN=$(cat "$XDG_RUNTIME_DIR/snitchwatch/token")
{ printf '%s\n' "$TOKEN"; cat; } | websocat --unix-listen -t \
    ws-c:unix:"$XDG_RUNTIME_DIR/snitchwatch/bridge.sock":/stream &
curl -sS --max-time 2 https://some-new-unclassified-host.example > /dev/null &
```

**Pass condition:** the pending `AskRule` still round-trips over `websocat`
with **no GUI process running at all** — proving `opensnitchd`'s fail-closed
default (`DefaultAction: deny`) is only reached on a genuine bridge outage,
not merely because the GUI window was closed. This is the specific
regression Phase 2 exists to prevent (see the plan doc's "fail-open stance"
section and the systemd unit's own header comment).

**If it fails** (i.e. the connection silently passes through or hangs with
no pending message): check `systemctl --user status snitchwatch-bridge.service`
first — if the unit isn't actually running independent of the GUI session
(e.g. `graphical-session.target` tore it down when a desktop session ended),
that's the regression this step exists to catch, not a websocat/curl
problem. Also confirm `opensnitchd`'s live `Server.Address` still points at
the bridge's stable `127.0.0.1:50051` bind and not an ephemeral port from an
earlier bridge-cli run.

---

## Recording results

Once all four steps are run, update
`docs/superpowers/plans/2026-07-05-phase2-packaging.md`'s "Acceptance
criteria & verification status" section — flip the unchecked box to `[x]`
and note the host/date, mirroring how Task 7's fullscreen-focus test was
recorded in `docs/superpowers/plans/2026-07-04-kirigami-shell-rewrite.md`.
