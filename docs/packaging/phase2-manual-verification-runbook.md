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

**Corrected 2026-07-31 (see issue #7):** `docker.io/evilsocket/opensnitch`
does not exist, and no current opensnitch container image is published.
Build the container from a stock Fedora image plus the upstream v1.8.0
release RPM (matches the vendored submodule pin). Two hard requirements
found live: **root podman** (rootless gets `operation not permitted` on
nfqueue/conntrack even with `--privileged`), and `rpm --noscripts` (the
RPM's `%post` calls `systemctl`, absent in containers):

```bash
mkdir -p ~/.cache/snitchwatch-verify/etc-opensnitchd/rules
cp packaging/bluebuild/files/system/etc/opensnitchd/default-config.json \
   ~/.cache/snitchwatch-verify/etc-opensnitchd/
cp vendor/opensnitch/daemon/data/system-fw.json \
   ~/.cache/snitchwatch-verify/etc-opensnitchd/
curl -sL -o ~/.cache/snitchwatch-verify/etc-opensnitchd/opensnitch.rpm \
  https://github.com/evilsocket/opensnitch/releases/download/v1.8.0/opensnitch-1.8.0-1.x86_64.rpm
# The shipped config already sets "ProcMonitorMethod": "proc" — the v1.8.0
# RPM's eBPF module fails to load on kernels ≥ ~6.19 (issue #6).

sudo podman run -d --name opensnitchd-dev \
    --privileged --network=host --pid=host \
    -v ~/.cache/snitchwatch-verify/etc-opensnitchd:/etc/opensnitchd:Z \
    registry.fedoraproject.org/fedora:43 \
    sh -c 'dnf install -y libnetfilter_queue nftables info \
             && rpm -ivh --noscripts /etc/opensnitchd/opensnitch.rpm \
             && exec /usr/bin/opensnitchd \
                  -config-file /etc/opensnitchd/default-config.json \
                  -rules-path /etc/opensnitchd/rules'
```

For an unattended run, consider `DefaultAction: allow` in the copied config
so an unanswered prompt can never block live traffic; the shipped deny
default is Step 1's image concern, not this step's dial-in concern.

**False-DaemonDown caveat (issue #5) — fixed:** a healthy idle daemon sends
no `Ping` RPCs (upstream only pings when new stats events exist). The bridge
now treats any inbound gRPC activity plus an open `Notifications` stream as
proof of liveness (`docs/superpowers/plans/2026-07-31-daemon-liveness-heartbeat.md`),
so an idle-but-connected daemon reads as reachable without needing steady
traffic. Traffic generation is still useful for judging Step 5/6's
stats-driven rows (the connection table, traffic graphs), just no longer
required to keep the tray/diagnostics from false-positiving.

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

## Step 5 — Tray state on real hardware (DaemonDown / RecentBlock / FilterOff)

Added 2026-07-12 alongside the tray-state work itself
(`.agent_native/agent_roadmap.md` item 9,
`docs/superpowers/plans/2026-07-12-tray-daemon-down-recent-block.md` and
`2026-07-12-tray-filter-off.md`). All three were verified with
`tokio::time::pause`/mocked publishers in the sandbox — genuinely correct
for the logic under test, but none of that exercises the real compositor
tray icon, a real `opensnitchd` actually going silent, or a real polkit-free
click path. With `snitchwatch-kirigami` running (Step 2 or `just
kirigami-dev`) against a real bridge + `opensnitchd` (Step 3):

**DaemonDown** — stop feeding it pings without killing the bridge itself:

```bash
# Stop opensnitchd (not the bridge!) so pings actually cease:
podman stop opensnitchd-dev
```

**Pass condition:** the tray tooltip changes to "opensnitchd not reachable"
within ~10-12 seconds (the `DAEMON_DOWN_TIMEOUT` in `daemon_watchdog.rs`
plus one watchdog tick). Restart `opensnitchd-dev` and confirm the tooltip
returns to "Snitchwatch — filtering" (or the correct `Pending(n)` count if
a decision was queued while it was down) within a couple more seconds.

**RecentBlock** — trigger a Deny verdict (allow-list a connection then deny
one, or use the existing `tests/mock_opensnitchd/examples/fire_ask_rule.rs`
harness against the real running shell — see its own doc comment) and
confirm the tooltip shows "Blocked: \<process\> → \<host\>" for ~5 seconds
before reverting.

**FilterOff** — right-click the tray icon:

```
Tray menu should show "Pause filtering" (not currently paused).
Click it -> tooltip becomes "Snitchwatch — filtering disabled",
  menu item now reads "Resume filtering".
Trigger a new connection (curl to an unclassified host) -> confirm it is
  silently allowed with NO pending-decision prompt shown.
Click "Resume filtering" -> tooltip returns to normal, menu item reads
  "Pause filtering" again, and a fresh AskRule prompts normally again.
```

**If any of these fail:** first confirm the tray icon is even receiving
live updates at all (`Qt.labs.platform.SystemTrayIcon`'s tooltip should
already reflect `Pending(n)` correctly per Step 3 — if that baseline is
broken, these three won't work either and the bug is upstream of this
step). If the baseline works but a specific transition doesn't, that's a
real regression in `daemon_watchdog.rs`/`grpc_server.rs`'s `ask_rule`/the
`main.qml` tray menu wiring — not a hardware-vs-sandbox environment gap.

---

## Step 6 — Daemon Health diagnostics on real hardware

Added 2026-07-14 alongside the daemon/kernel diagnostics feature itself
(`docs/superpowers/specs/2026-07-14-daemon-kernel-diagnostics-design.md`,
`docs/superpowers/plans/2026-07-14-daemon-kernel-diagnostics.md`). Everything
about this feature was verified with `MockOpensnitchd` and a headless
`QT_QPA_PLATFORM=offscreen` QML test in the sandbox — genuinely correct for
the logic under test, but none of that exercises a real `opensnitchd`
process, a real host kernel's actual eBPF/BTF or nftables support, or the
real `Kirigami.InlineMessage` banner/page rendering on a real compositor.

**6a — Baseline: everything healthy.** With `snitchwatch-kirigami` running
(Step 2 or `just kirigami-dev`) against a real bridge + real `opensnitchd`
already dialed in (Step 3):

**Pass condition:** no "Daemon Health" warning banner appears at the top of
the window, and the "Daemon Health" drawer entry's page shows all four
checks (`opensnitchd reachable`, `firewall running`, `eBPF support`,
`nftables support`) as healthy with the all-clear message, not a spinner or
`Unknown`/blank state.

**6b — Daemon unreachable.** Reuse Step 5's `DaemonDown` trigger:

```bash
podman stop opensnitchd-dev
```

**Pass condition:** the "Daemon Health" banner appears within ~10-12
seconds (same `DAEMON_DOWN_TIMEOUT` + one watchdog tick as Step 5's tray
check), showing the `DAEMON_UNREACHABLE_TROUBLESHOOTING` text
(`opensnitchd isn't dialing in...`) on the page. Restart `opensnitchd-dev`
and confirm the banner clears and the page returns to all-clear within a
couple more seconds — this also confirms the watchdog's *recovery*
transition re-broadcasts a fresh report, not just the down transition.

**6c — Kernel prerequisite failure (the case this feature exists for).**
This is the scenario Step 5 doesn't cover at all: `opensnitchd` reachable
and healthy, but the host kernel can't satisfy what it's configured to use.
Two independent ways to force it:

- **nftables path** — temporarily rename `nft` off PATH (safe, easily
  reversible):

  ```bash
  sudo mv "$(command -v nft)" "$(command -v nft).bak"
  ```

- **eBPF path, now real-hardware-testable (issue #6, fixed by the daemon-alert
  → diagnostics overlay).** `RealKernelProbe::btf_vmlinux_exists` only stats
  `/sys/kernel/btf/vmlinux` for existence, so it can't be faked by hiding the
  path (bind-mounting `/dev/null` over it leaves the path existing and the
  check passing) — and on a kernel ≥6.19 that genuinely has BTF, the probe
  alone can't produce a failure here at all. But a live daemon on such a
  kernel can: opensnitchd v1.8.0's bundled eBPF module fails to load on
  kernel 6.19+ regardless of BTF presence, and it reports that failure itself
  via `PostAlert` (`vendor/opensnitch/daemon/main.go:176,187,645`), which the
  bridge now overlays onto `EbpfSupport` (`DaemonAlertStore` +
  `DiagnosticsCtx::report()` in `diagnostics/mod.rs`). To exercise it for
  real: set `ProcMonitorMethod: ebpf` in opensnitchd's config on a host
  running kernel ≥6.19, restart `opensnitchd-dev`, and confirm the daemon's
  own startup alert surfaces as a failed `ebpf_support` check — with the
  daemon's own alert text visible in the troubleshooting detail, not just the
  generic `EBPF_TROUBLESHOOTING` copy. (On a kernel where the module loads
  fine, this path is a no-op — use the nftables path above instead, or don't
  expect a failure here.)

**Pass condition — this is the finding the whole-branch review flagged and
a follow-up fix addressed (see PR #3):** since `opensnitchd` itself never
goes unreachable in this scenario, the watchdog never transitions, so the
banner must appear via `DaemonHealthModel::start_bridge_feed`'s
initial-recheck-on-connect instead. Confirm this actually happens: restart
`snitchwatch-kirigami` fresh (`just kirigami-dev` again) *while* the
kernel/PATH change from above is still in effect, and confirm the banner
appears **on first render**, without needing to click "Recheck" manually.
If it only appears after clicking Recheck, that's a regression of the fix
made in response to the final whole-branch review — flag it, don't treat
it as expected behavior.

Then click "Recheck" anyway and confirm the page's failed check shows the
correct troubleshooting text: `NFTABLES_TROUBLESHOOTING` for the PATH-rename
path, or `EBPF_TROUBLESHOOTING` *with the daemon's own alert text appended*
for the `ProcMonitorMethod: ebpf` path (`diagnostics/mod.rs`'s overlay always
appends "opensnitchd reports: ..." — a bare `EBPF_TROUBLESHOOTING` with no
appended daemon text there would mean the overlay isn't firing).

**Restore the host** before moving on:

```bash
sudo mv "$(command -v nft).bak" "$(dirname "$(command -v nft)")/nft" 2>/dev/null  # if renamed
# If ProcMonitorMethod was changed to ebpf, set it back to its prior value
# (proc, unless you'd deliberately overridden it) and restart opensnitchd-dev.
```

**If any of 6a-6c fail:** first confirm Step 5's tray-tooltip baseline
already works on this host — if that's broken, the Daemon Health feature
rides the same watchdog/broadcast plumbing and won't work either, and the
bug is upstream of this step. If the tray baseline is fine but only the
banner/page is wrong, that's a real regression in
`daemon_health_model.rs`/`main.qml`/`DaemonHealthPage.qml` or the kernel
probe itself — not a hardware-vs-sandbox gap.

---

## Recording results

Once all five steps are run, update
`docs/superpowers/plans/2026-07-05-phase2-packaging.md`'s "Acceptance
criteria & verification status" section — flip the unchecked box to `[x]`
and note the host/date, mirroring how Task 7's fullscreen-focus test was
recorded in `docs/superpowers/plans/2026-07-04-kirigami-shell-rewrite.md`.
