# Linux App Firewall + Bazzite Security Scanner — Handoff (updated 2026-08-05)

> **Read this first if you're picking this repo up cold.** Everything below
> the "Current status" section is the *original* handoff from 2026-07-04,
> kept for history/decision rationale — it is accurate as a record of what
> was decided and why, but stale as a status report. Trust this section for
> "what's true today."

## Start here (2026-08-05)

`main` is clean, CI green, nothing uncommitted. Last merges: PR #29 (shell
cleanup + test hardening) and PR #30 (rule enable/disable/delete).

**Only open item: [issue #31](https://github.com/bearyjd/snitchwatch/issues/31)**
— the pending-count badge renders orange-on-orange outside Plasma. Not
verified under Breeze (`org.kde.desktop` is not installed in the dev
container), so confirm that first before deciding scope.

**Live-verification is possible from this machine — do not assume otherwise.**
Three things were wrongly recorded as "needs real hardware" and each turned out
to be reachable locally:

- A real `opensnitchd` runs in the root `opensnitchd-dev` podman container
  (`distrobox-host-exec sudo -n podman ...`), dialing `127.0.0.1:50051`, with
  `DefaultAction: allow` (fail-open, so a bridge that stalls cannot lock the
  network out). Drive it with
  `cargo run -p snitchwatch-bridge-cli --example live_rule_change`.
- A real Wayland compositor is reachable (`WAYLAND_DISPLAY=wayland-0`), and the
  host has `spectacle` for screenshots. The Kirigami shell runs against it.
- `qmltestrunner-qt6` + the QtTest QML module are installed, so QML input
  routing can be measured with real synthesized clicks (`just qml-test`).

**Testing traps that cost real time here — read before writing a test:**

1. Qt routes logging to **journald**, not stderr, whenever stderr is not a TTY
   (always true under `cargo test`). `tests/common::init_headless_qt_env` sets
   `QT_FORCE_STDERR_LOGGING` to fix it. Without that, every stderr-based QML
   assertion passes *vacuously*.
2. A bare `QtObject` root under `QQmlApplicationEngine` reports a non-null root
   but **never runs `Component.onCompleted`** — probes need a `Window` root
   driven through `exec()`, or the whole test body silently never executes.
3. **Verify each new assertion by deliberately breaking the specific thing it
   asserts.** Four tests in this repo have passed while proving nothing. Two
   sabotage attempts were themselves mis-aimed (one produced a compile error,
   one hit the wrong half of a round trip) — and a mis-aimed negative control
   is indistinguishable from a passing one.
4. Run Kirigami cargo jobs **serially**. Concurrent invocations poison the
   shared cxx-qt build state (`Conflicting include_prefixes`); recover with
   `cargo clean -p cxx-qt -p cxx-qt-lib -p snitchwatch-kirigami`.

## Current status (2026-07-12)

Both components are **code-complete**. Everything reachable from a sandbox
without real Bazzite hardware has been built, tested, and verified — CI is
green on real GitHub Actions, not just locally. What's left is exclusively
real-hardware verification and one release-timing decision, both described
below.

**Component A (Snitchwatch):**
- Phases 1 (auth/socket), 2 (packaging), 3a/3b (Kirigami shell) are done.
- The shell that ships is `crates/snitchwatch-kirigami/` (Qt6/QML +
  Kirigami via `cxx-qt`) — feature-complete, including the safety-critical
  pending-decision prompt (verified to raise/focus over a fullscreen window
  on a real Plasma session) and all four tray states
  (`Idle`/`Pending(n)`/`DaemonDown`/`RecentBlock`/`FilterOff`, the last
  three added 2026-07-12, see `.agent_native/agent_roadmap.md` item 9).
- `crates/snitchwatch-tauri/` + `web/` (the original shell) still exist and
  work, but are intentionally **not** what the Flatpak packages —
  `packaging/flatpak/org.snitchwatch.Snitchwatch.yml` targets
  `snitchwatch-kirigami`. They're kept until a packaged release ships
  (explicit owner decision, 2026-07-11) — don't remove them before that.

**Component B (Bazzite scanner):** also done — Phases 4 (baseline design),
5 (userspace tier), 6 (privileged tier + Kirigami report UI) are all
code-complete. This section's original "not started, design-only" framing
below is entirely superseded.

**CI:** `.github/workflows/ci.yml` runs 4 jobs on every push/PR to `main`:
`check`/`test` (default-members, Ubuntu), `package-check` (packaging
artifact validation), and `kirigami` (builds/lints/tests
`snitchwatch-kirigami` in a Fedora container, since KF6 Kirigami packages
don't exist in Ubuntu's repos yet). All green as of the last commit.

**Update (2026-07-31): first real-hardware verification ran.** Live
opensnitchd v1.8.0 dial-in, AskRule→WS delivery, and the diagnostics
protocol are all confirmed working on a real Bazzite host — and the run
surfaced three real product findings, filed as issues #5 (daemon only
Pings when it has new stats events → the watchdog false-positives
DaemonDown on idle systems; blocks Step 5/6b pass conditions), #6
(EbpfSupport check can't see real eBPF module-load failure), and #7
(opensnitch isn't in Fedora/Bazzite repos — the recipe's install step and
both install docs were wrong; docs corrected, bluebuild recipe fix
pending). Detailed results:
`docs/superpowers/plans/2026-07-05-phase2-packaging.md` acceptance
section.

**Update (later 2026-07-31): all three findings are fixed and merged** —
#5 via PR #9 (liveness = any RPC activity + open Notifications stream;
re-verified live: idle daemon reads alive, down transition now fires in
~2s via stream-close with a consistent report, recovery pushes an
all-clear without a recheck), #7 via PR #10 (recipe/docs install the
upstream v1.8.0 release RPM), #6 via PR #11 (daemon-reported alerts
overlay onto the diagnostics checks — text-classified, since real v1.8.0
alerts are all `GENERIC`; alerts persist until a user-driven Recheck).
**Correction:** issue #6's "eBPF incompatible with kernel 6.19" premise
was a mis-diagnosis — the failure was rootless-container permissions;
under root, ebpf loads fine on 6.19 (see issue #6's post-close comment).
The shipped config keeps upstream's `ProcMonitorMethod: ebpf` (PR #10
briefly shipped `proc`; reverted). Also verified live along the way:
sustained interception under a real daemon + eBPF process monitoring.

**Correction (2026-07-31): the "verdict round-trip" claim above was
over-claimed — filed as issue #14.** A `tailscaled` connection was asked and
a `setVerdict` was sent back over WS promptly, but the daemon never actually
*applied* it: every real `AskRule` ended in `DeadlineExceeded` +
`Invalid rule received, applying default action`. Root cause: the bridge's
`verdict_to_rule` always set `operator: None` on the reply `Rule`, which
`vendor/opensnitch/daemon/rule/rule.go`'s `Deserialize` hard-rejects — so no
verdict from this bridge has ever been accepted by a real daemon. Fixed by
populating a real `Operator` per the verdict's scope (`ThisHost`/
`AnyHostOnDomain`/`AnyHost`) and by hardening `MockOpensnitchd::ask_rule` to
apply the daemon's real ~120s deadline and validate the returned `Rule`
shape, so this class of bug can't pass the sandbox suite silently again.

**Update (2026-08-01): the verdict round-trip is now verified live.** PR
#16 (the issue-#14 operator fix) merged and was re-verified against the
real daemon — it now logs `Added new rule: allow if dest.host is '<host>'`
where it previously logged `Invalid rule received, applying default
action`. The same session — the first time a human operated the Kirigami
shell on real hardware — surfaced four UI defects (issues #15, #18, #19,
#20; #17 largely retracted) and a systematic verification gap (the test
doubles are more capable than the real daemon, and no QML test simulates a
click). Full session detail, corrections to prior claims, environment
gotchas, and suggested next steps:
`docs/superpowers/HANDOFF-2026-08-01-gui-usability.md` — **read that file
next if you're picking this up.** Issues #18/#19/#20 are blocked on
design/product calls, not engineering. PR #21 (GUI tracing subscriber) and
PR #22 (the session handoff doc) are also merged. Remaining real-hardware
items are listed in the phase2 plan's acceptance section — mostly
GUI-visual checks and the sustained-interception/queue-rules question.

**Update (2026-08-05): all of the below is MERGED. Nothing is uncommitted.**
Shipped as PR #29 (`0e3d573`..) and PR #30 (`bfc088f`); `main` is clean and in
sync. The notes in this section are kept as the record of what landed and why —
read them as history plus live-verified facts, not as pending work.

Work was performed in the user's `dev` distrobox from the canonical
`/var/home/user/Documents/vibe-code/opensnitch-gui` path (that spelling
matters — see the cxx-qt path-aliasing note). `lld` is installed there;
`CCACHE_DISABLE=1 just kirigami-dev` builds without the cxx-qt linker warning.
The in-process bridge is verified live: Allow/Deny clicks resolve pending rows
and log `applied verdict and broadcast row update`.

**Only open item from this run: issue #31** (pending-count badge renders
orange-on-orange outside Plasma). Everything else below is closed.

Important current findings:

- The initial QML null-model/`setPendingOnly`/verdict-click failures are fixed
  by retaining QML object references on `main.qml`'s root and passing those
  references into dynamically-created `ConnectionsPage` instances.
- The bridge now broadcasts `UpdateConnectionRows` after a verdict. It also
  now broadcasts `UpdateRules` when a non-`once` verdict returns a daemon
  rule, so `For 5 minutes`, `Until quit`, and `Forever` decisions show in the
  Rules page immediately. This is covered by
  `persistent_allow_verdict_broadcasts_rule_for_live_clients`.
- `This time` intentionally creates no rule. The Rules empty state now says
  this explicitly. The inspector now displays destination IP and reports a
  missing PTR as `No PTR result for <ip>`; this is independent from the
  opt-in RDAP/"online research" setting.
- **Rule enable/disable/delete is now wired (2026-08-04).** The outbound
  Notifications stream no longer parks on `pending()`: it relays from a
  `broadcast::Sender<Notification>` exposed by
  `UiService::notifications_handle`, and `translator::rule_notification`
  turns `UpstreamEffect::{UpdateRule, DeleteRule, AddRule}` into
  `CHANGE_RULE`/`DELETE_RULE`. Covered by
  `rule_update_and_delete_reach_the_daemon_as_notifications`, verified by
  sabotage (drop the send → the test fails with a timeout).

  Three things to know before touching it:
  - **Never send a `NONE`-typed notification.** The daemon reads
    `ntf.Type <= Action_NONE` as "server ordered to close notifications" and
    tears the stream down
    (`vendor/opensnitch/daemon/ui/notifications.go:405-408`). The relay loop
    filters it as a second line of defence.
  - **A malformed rule fails silently on a real daemon**, which is what issue
    #14 was: `rule.Deserialize` plus `Operator.Compile()` reject it and the
    daemon falls back to its default action. `rule_from_wire` returns `Err`
    rather than ever emitting `operator: None`, and the e2e test asserts the
    outgoing rule against `mock_opensnitchd::validate_rule_shape`.
  - **Toggles only persist for `always`-duration rules.** The daemon calls
    `Replace(r, r.Duration == rule.Always)`, where the second argument is
    "save to disk" — a `once`/`30s`/`until restart` rule changes in memory
    only and reverts when the daemon restarts.

  **Live-verified against a real daemon (2026-08-05).** Not just mock-driven —
  a real `opensnitchd` (v1.6.x, running in the root `opensnitchd-dev` podman
  container, `DefaultAction: allow`, dialing `127.0.0.1:50051`) accepted and
  applied a `CHANGE_RULE`. Reproduce with:

  ```bash
  RUST_LOG=info cargo run -p snitchwatch-bridge-cli --example live_rule_change -- \
    000-allow-localhost /path/to/rule.json
  ```

  Evidence from that run:

  ```
  client subscribed client=tower version=6.19.11-ogc1.1.fc44.x86_64
  notifications stream opened
  notification reply from daemon id=0 code=0     <- HELLO (hence ids start at 1)
  sent rule command to daemon id=1 action=10 receivers=1   <- 10 = CHANGE_RULE
  notification reply from daemon id=1 code=0     <- 0 = OK: accepted, not rejected
  ```

  `code=0` is the signal issue #14 never got: a real daemon confirming it
  deserialized and applied the rule rather than silently falling back to
  `DefaultAction`. The daemon also rewrote `000-allow-localhost.json` on disk
  with `created`/`updated` stamped at the exact send time, confirming
  `Replace(r, save=true)` for an `always`-duration rule.

  **That run also proved the `precedence`/`nolog` fix in production.** The real
  `000-allow-localhost` rule carries `precedence: true` and `nolog: true`; both
  survived the round trip byte-for-byte. Before the fix the bridge would have
  written `false` for each, silently stripping priority evaluation from the
  rule governing all localhost traffic.

  **`DELETE_RULE` also live-verified (2026-08-05).** Exercised against the same
  daemon using a throwaway rule (`zzz-snitchwatch-delete-probe`: `allow` on
  `*.invalid`, `zzz-` prefix so it sorts last and can shadow nothing):

  ```
  sent rule command to daemon id=1 action=9 receivers=1   <- 9 = DELETE_RULE
  notification reply from daemon id=1 code=0              <- accepted
  ```

  The probe's `.json` was gone from `/etc/opensnitchd/rules` afterwards, and
  `000-allow-localhost.json` was untouched. Reproduce with
  `--example live_rule_change -- delete <rule-name>`; **create a throwaway rule
  first, never point it at a real one.**

  **GUI visual check done on a real Wayland compositor (2026-08-05).** The
  shell was run against the live daemon (`SNITCHWATCH_GRPC_BIND=127.0.0.1:50051`)
  and rendered a genuine intercepted `syncthing` connection: process group row,
  "1 connection", and issue #18's `Allow all (1)` / `Deny all (1)` batch buttons
  with correct counts.

  **Bug found, not yet fixed — the pending badge is unreadable off Plasma.**
  `ConnectionsPage.qml:385-398` paints the badge `neutralBackgroundColor` with
  `neutralTextColor` text; under Fusion both resolve to the same orange, so the
  pill renders solid orange with the "N pending" text invisible. `main.rs:39-43`
  only forces a style under `offscreen`, so a real session inherits the platform
  default — Breeze on Plasma (likely fine, unverified: `org.kde.desktop` is not
  installed in the dev container) but Fusion/Basic elsewhere, including a
  Flatpak without `kf6-qqc2-desktop-style`. Pre-existing, unrelated to the
  notifications work; worth its own issue.
- Remaining runtime noise: GeoLite DB absence and NetworkManager absence in
  the container are informational expected degradations; installed Kirigami
  still emits `shortHeaderMargins` and page-component placement warnings.
  Local sheets were given explicit bounds to avoid their own implicit-height
  feedback path; re-check on hardware after the next restart. Those bounds now
  live once in `qml/SizedOverlaySheet.qml` rather than copy-pasted per sheet —
  when upstream Kirigami fixes the cycle, delete the bindings there only.
- The `PendingDecision` QObject was removed: routing verdicts through
  `BridgeFeed.submitVerdict` left it with no production QML caller, and only
  its own probe test kept it alive. `pending_decision.rs`'s pure token-parsing
  and `build_verdict_message` are unchanged and still the single source of the
  verdict wire shape.
- **Latent test defect found and fixed (2026-08-04):** the headless QML probes
  assert "no QML JS error was logged" by redirecting fd 2 and scanning it.
  That assertion had **never fired**: Qt on Fedora is built with journald
  support and its default handler routes to the journal, not stderr, whenever
  stderr is not a TTY — always true under `cargo test`. The capture came back
  empty and the assertion passed unconditionally.
  `tests/common/mod.rs::init_headless_qt_env` now sets
  `QT_FORCE_STDERR_LOGGING` (not the older `QT_LOGGING_TO_CONSOLE`, which Qt
  6.10 warns is deprecated — and which, if silently dropped by a future Qt,
  would reintroduce exactly this bug).
  Both verdict probes were verified by deliberate sabotage (null feed /
  renamed invokable) and confirmed to go red. Note also that a bare `QtObject`
  root never runs `Component.onCompleted` under `QQmlApplicationEngine` — a
  probe must use a `Window` root driven through `exec()` or its QML body
  silently never executes.
- **Resolved 2026-08-04 — no compositor needed after all.** The verdict buttons
  carried *both* a `TapHandler` and an `onClicked`, and this was previously
  recorded as undecidable headlessly. It isn't: `qmltestrunner-qt6` and the
  QtTest QML module ship in `qt6-qtdeclarative-devel` (already a build
  dependency) and synthesise real `QMouseEvent`s. Measured against the exact
  delegate shape (`Item` > `MouseArea` z:0 + content z:1 > `Button`):

  | click target   | onTapped | onClicked | MouseArea |
  |----------------|----------|-----------|-----------|
  | verdict button | 1        | 1         | 0         |
  | empty row area | 0        | 0         | 1         |

  Identical on Basic/Fusion/Material/Universal, since click logic lives in the
  shared `QQuickAbstractButton` base rather than in a style. So one click
  dispatched **twice** — the double-submit was real, not hypothetical — and
  plain `onClicked` wins the grab on its own. The `TapHandler`s are removed;
  `decideOnce` stays as the single latch.

  New coverage, both verified by deliberate sabotage:
  `just qml-test` (`tests/qml/tst_delegate_input.qml`) pins the Qt input
  routing this depends on, and `tests/qml_source_guards.rs` catches a
  re-introduced `TapHandler` or a button that bypasses `decideOnce`.
  **`qmltestrunner` cannot load `com.snitchwatch.shell`** (cxx-qt links those
  types statically; no QML plugin is emitted), so the QML test exercises a
  structural mirror — the source guards cover the real file.

Verification run at the time (kept for the command reference; all of it is now
covered by CI on `main`):

```bash
CCACHE_DISABLE=1 cargo test -p snitchwatch-bridge persistent_allow_verdict_broadcasts_rule_for_live_clients
CCACHE_DISABLE=1 cargo test -p snitchwatch-bridge-cli verdict_broadcasts_an_updated_non_pending_row
CCACHE_DISABLE=1 cargo check -p snitchwatch-kirigami
CCACHE_DISABLE=1 cargo test -p snitchwatch-kirigami insight::client::tests::fetch_never_calls_rdap_source_when_disabled
git diff --check
```

Re-run after the cleanup/review pass, covering the Kirigami targets the
`cargo check` above never compiles (it omits `--all-targets`, so it does not
build `tests/` at all):

```bash
CCACHE_DISABLE=1 cargo clippy --all-targets -- -D warnings
CCACHE_DISABLE=1 cargo clippy -p snitchwatch-kirigami --all-targets -- -D warnings
CCACHE_DISABLE=1 cargo test -p snitchwatch-bridge -p snitchwatch-bridge-cli   # 293 passed
CCACHE_DISABLE=1 QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic \
  cargo test -p snitchwatch-kirigami                                          # 263 unit + integration
cargo fmt --all --check && git diff --check
```

Run the Kirigami crate's cargo jobs **serially**. Two concurrent cargo
invocations against the shared `target/` poison the cxx-qt build state
(`Conflicting include_prefixes for cxx-qt!`); recover with
`cargo clean -p cxx-qt -p cxx-qt-lib -p snitchwatch-kirigami`.

**What's actually left:**
1. **Real Bazzite hardware verification** — a bluebuild image build, a
   Flatpak build, a live `opensnitchd` dial-in, the closed-window
   fail-open proof, and the tray-state transitions (5 steps total). Full
   instructions: `docs/packaging/phase2-manual-verification-runbook.md`.
   See "Running on real hardware" below for the short version.
2. **Retiring `snitchwatch-tauri`/`web/`** — blocked on a packaged release
   actually shipping, not on any remaining code work.

Nothing else is blocked on more agent/sandbox work. If you're an agent
picking this up: don't re-derive the phase status from `IMPLEMENTATION_PROMPT.md`'s
original phase descriptions without reading each phase's own inline
"Update" note first — they're kept current.

---

## Running on real hardware

Quick-start for a Bazzite (or Fedora Silverblue/Kinoite-family) box. Full
detail, pass/fail conditions, and troubleshooting per step are in
`docs/packaging/phase2-manual-verification-runbook.md` — this is the short
version.

**Fastest path — just run the dev build, no packaging:**
```bash
git submodule update --init --recursive
just build                # needs protoc; Tauri needs webkit2gtk-4.1 dev headers
just kirigami-dev         # needs system Qt6 + KDE Frameworks 6 (Kirigami) dev packages
```
This runs the in-process bridge + Kirigami shell directly — no Flatpak, no
bluebuild image, nothing installed system-wide. Good enough to confirm the
GUI itself works on your hardware. It won't exercise real `opensnitchd`
unless you also point it at one (see README's "Running the bridge against
real opensnitchd").

**Full packaged install (batteries-included):**
```bash
bluebuild build packaging/bluebuild/recipe.yml    # bakes opensnitchd + fail-closed config
# ...rebase onto the resulting image per bluebuild's own instructions...
```

**Full packaged install (lightweight/DIY, layer onto stock Bazzite):**
Follow `docs/packaging/rpm-ostree-layering.md` step by step — it covers
`rpm-ostree install opensnitch`, the fail-closed config override, and
installing `packaging/systemd/snitchwatch-bridge.service` as your own user
service.

**GUI either way — build + install the Flatpak:**
```bash
python3 flatpak-cargo-generator.py Cargo.lock \
  -o packaging/flatpak/generated-cargo-sources.json
flatpak run org.flatpak.Builder --user --install --force-clean \
  build-dir packaging/flatpak/org.snitchwatch.Snitchwatch.yml
flatpak run org.snitchwatch.Snitchwatch
```

**Then verify it's actually working**, not just installed — the 5 steps in
`docs/packaging/phase2-manual-verification-runbook.md`:
1. Bluebuild image actually has `opensnitchd` enabled + the fail-closed
   config baked in.
2. Flatpak sandbox boundary actually holds (no stray network access, the
   Unix-socket filesystem permission actually crosses the sandbox).
3. A real `opensnitchd` dial-in round-trips a live `AskRule`.
4. Closing the GUI window does **not** kill the decision channel (the
   whole point of Phase 2 — the bridge runs as its own systemd `--user`
   service, decoupled from the GUI).
5. The tray icon actually reflects `DaemonDown`/`RecentBlock`/`FilterOff`
   in real time, not just in the sandbox's mocked tests.

Record results by flipping the checkbox in
`docs/superpowers/plans/2026-07-05-phase2-packaging.md`'s "Acceptance
criteria & verification status" section, same convention Task 7's
fullscreen-focus test used.

### Troubleshooting: GUI runs, but no visible connection to opensnitchd

**Update (2026-07-14):** this class of problem now surfaces *inside the
running GUI* — a warning banner plus a "Daemon Health" page — instead of
requiring the manual `/var/log/opensnitchd.log` triage below. See
`docs/superpowers/specs/2026-07-14-daemon-kernel-diagnostics-design.md`
for the design and `docs/superpowers/plans/2026-07-14-daemon-kernel-diagnostics.md`
for the implementation. The manual steps below remain accurate as a deeper
fallback (the in-app troubleshooting text is deliberately terser) and as
the only option before this feature shipped.

Symptom hit 2026-07-13 verifying on real hardware: `snitchwatch-kirigami`
builds and launches fine, but nothing happens and no error appears anywhere
in the GUI. Root cause is topology, not a bug: **`opensnitchd` is the gRPC
*client* — it dials out to the bridge, not the other way around** — so the
GUI has no direct line to `opensnitchd` at all; it only ever talks to the
bridge over the WS Unix socket. Two things have to line up for the dial-in
to happen:

1. `opensnitchd`'s own config (`/etc/opensnitchd/default-config.json` on
   the host) needs `Server.Address` pointing at the bridge, e.g.
   `127.0.0.1:50051`. The vendored default under
   `vendor/opensnitch/daemon/data/default-config.json`
   (`unix:///tmp/osui.sock`) is opensnitch's *own* built-in UI socket, not
   this bridge — leaving it as-is means opensnitchd never even attempts to
   dial the bridge.
2. The bridge has to actually be listening on that same address.
   `snitchwatch-bridge-cli` defaults to an **ephemeral port** unless
   `SNITCHWATCH_GRPC_BIND=127.0.0.1:50051` is set explicitly — the shipped
   `packaging/systemd/snitchwatch-bridge.service` sets this, but running
   `just run-bridge` by hand does not.

If either is off, there's silence by design — no error surfaces in
Snitchwatch because opensnitchd never connects to hand one to the bridge in
the first place. **Check `opensnitchd`'s own log**, not the GUI:

```bash
tail -f /var/log/opensnitchd.log
```

Fastest way to get a real signal:

```bash
# 1. Start the bridge with a fixed, known gRPC bind:
SNITCHWATCH_GRPC_BIND=127.0.0.1:50051 RUST_LOG=debug just run-bridge
# → watch for "GRPC_LISTEN_ADDR=127.0.0.1:50051" printed to stdout

# 2. Confirm opensnitchd's config actually points there:
grep -A2 '"Server"' /etc/opensnitchd/default-config.json

# 3. Restart opensnitchd and tail its log for the dial attempt/failure:
sudo systemctl restart opensnitchd
sudo tail -f /var/log/opensnitchd.log
```

Separately: `opensnitchd` isn't installed at all in the dev sandbox this
repo is normally worked in (no `opensnitchd` binary, and the sandbox isn't
even running systemd as PID 1) — that environment can only verify the GUI
launches, never a real daemon dial-in. Real-hardware verification is
mandatory for this step, not optional. A first attempt at compiling
`snitchwatch-tauri`/`snitchwatch-kirigami` on the reporting user's baremetal
box also failed outright (pre-dial-in, a separate build-environment issue —
missing system dev packages is the leading suspect per this file's
"Running on real hardware" prerequisites).

**Update (2026-07-30): baremetal build failure diagnosed and fixed.** It was
not missing dev packages — all prerequisites were present. `just build`
failed with both Kirigami crates' build scripts panicking:
`Conflicting include_prefixes for cxx-qt! Dependency cxx-qt-lib conflicts
with existing include path` (`cxx-qt-build 0.9.1`, `src/lib.rs:706`). Root
cause: in this box's containerized dev shell (toolbox/distrobox-style —
`systemctl` reports `offline` there), `/home/user` and `/var/home/user`
are the same directory via *bind mount* (verified: same device+inode,
`/home` a real directory) rather than the bare host's `/home → var/home`
symlink, so `fs::canonicalize` cannot unify the two spellings. (On the
bare host the symlink makes both spellings canonicalize identically and
this failure cannot happen.) Earlier builds ran from a `/home/user/…`
cwd and recorded `cxx-qt-lib`'s exported-include symlinks under that
spelling in `target/`; a later build from `/var/home/user/…` re-ran only
the consumer crates' build scripts, and cxx-qt-build's conflict check
(`canonicalize(source) == canonicalize(dest)`) saw the same physical
directory under two spellings and panicked. Fix:

```bash
cargo clean -p cxx-qt -p cxx-qt-lib -p cxx-qt-build \
  -p kirigami-spike -p snitchwatch-kirigami
just build   # from ONE consistent path spelling
```

**Gotcha to avoid recurrence:** always invoke cargo from the same path
spelling on this box (pick `/var/home/user/…` or `/home/user/…` and stick
with it — shells, IDEs, and agents included). Mixing them re-poisons
`target/` and the panic returns until the cxx-qt crates are cleaned again.

---

## Original 2026-07-04 handoff (history — decisions and rationale below are
## still accurate; status framing above supersedes anything below)

Two related but independent components, both targeting Bazzite. This
supersedes the original handoff brief — all four of its open questions are
now resolved (see below), and a scoped implementation prompt exists.

## Component A: App Firewall ("Snitchwatch") — this repo, pre-alpha

A Little Snitch–style per-process outbound firewall GUI on top of
OpenSnitch. Snitchwatch is a friendlier frontend, **not** a from-scratch
interception engine: opensnitchd dials into a bridge's gRPC `protocol.UI`
server, and the bridge translates to a Little-Snitch-v6-style WebSocket
protocol for the frontend.

**What's built (M0–M4 complete):**

```text
crates/
├── snitchwatch-proto/       # generated tonic/prost bindings for opensnitchd's ui.proto
├── snitchwatch-spike/       # M0 spike binary that probes a live daemon
├── snitchwatch-bridge/      # headless bridge lib: cache, translator, ws server, grpc client, blocklists
├── snitchwatch-bridge-cli/  # thin orchestrator (lib::run + main.rs)
└── snitchwatch-tauri/       # Tauri 2 desktop shell: tray, notifications, autostart, wizard, crash log
tests/                        # bridge_protocol_test.rs, mock_opensnitchd, tauri_smoke/, web_smoke/
web/                          # vendored Little-Snitch-for-Linux-style frontend (rebranded)
```

Blocklist subscription (M4) is done: fetch → parse → materialize into
opensnitchd's `900-blocklist:<name>:` rule band, pushed live over WS.

## Component B: Immutable-OS Security Scanner — not started, design-only

Periodic anomaly/risk scanner for Bazzite's rpm-ostree/atomic model.
Userspace tier (default, no privilege) + on-demand privileged tier
(polkit/pkexec, **no persistent root daemon** — this property is load-bearing
and protected by decision #1 below). Zero code exists; three design docs do
(see "Design docs produced" below).

## The four original open questions — all resolved

1. **One app or two?** → **Two separate apps.** Component B's core security
   property (no persistent privileged daemon) can't coexist with merging
   into Component A's always-on bridge/daemon architecture. They share only
   a design system and, optionally, a signal channel (B reading A's
   connection log — not yet built, see Phase 5).
2. **Component A's interception layer** → **keep riding opensnitchd**
   indefinitely. Snitchwatch stays a friendlier opensnitchd frontend;
   building a native nfqueue/eBPF daemon is explicitly deferred.
3. **Component B's atomic-baseline problem** → **solved**, not just scoped.
   See `docs/superpowers/specs/2026-07-04-scanner-baseline-design.md`:
   delegates to `rpm-ostree status/db diff`, `rpm -V`, and OSTree's
   content-addressed object store rather than reimplementing manifest
   diffing. A 5-step classification tree (out-of-scope path → base-tree
   match → layered-package match → curated dynamic-path allowlist → else
   anomalous) replaces the original open question outright.
4. **Distribution model** → **bluebuild image (primary) + documented
   rpm-ostree layering (alternative) + Flatpak GUI.** Finding: the bridge
   needs no privileged access (no `CAP_NET_RAW`) — only opensnitchd does —
   so the GUI/bridge can be Flatpak-sandboxed. **Caveat discovered and
   fixed:** Flatpak isolates the network namespace, not just capabilities,
   so the original TCP-loopback assumption didn't actually cross the
   sandbox boundary. Fixed by moving the WS transport to a Unix domain
   socket gated by `--filesystem=xdg-run/snitchwatch` instead of
   `--share=network`. See
   `docs/superpowers/specs/2026-07-04-flatpak-feasibility-research.md`.

## Decisions made beyond the original four questions

- **GUI stack: Qt6/QML + Kirigami** (not GTK4/libadwaita as originally
  specified, not Tauri as currently built). Bazzite's default desktop is
  KDE Plasma, not GNOME — GTK4/libadwaita wouldn't have been native there
  either. This is a full rewrite of the current Tauri shell + vendored
  ~6,939-line web frontend. `cxx-qt` (the Rust↔Qt binding) researched and
  cleared with caveats: maintained by KDAB, KDE's own official docs host a
  Rust+Kirigami tutorial, but it's pre-1.0 and has an open build/test
  linking issue (GitHub #770) relevant to this repo's `tests/` convention.
  See `docs/superpowers/specs/2026-07-04-gui-stack-decision.md` and
  `2026-07-04-cxx-qt-feasibility-research.md`.
- **WS channel had no auth at all** — fixed via a shared-secret handshake
  token, now bundled with the Unix-socket transport fix above (Phase 1).
- **opensnitchd fails open by default** (`DefaultAction: allow` in
  `vendor/opensnitch/daemon/data/default-config.json:18`), confirmed via
  source, not assumed. Snitchwatch's shipped config should override this to
  `deny`.
- **Component B's privileged tier, itemized**: `chkrootkit` (not
  `rkhunter` — stale since 2018, no eBPF-rootkit coverage) for rootkit
  scanning; `/proc/cmdline` vs. `rpm-ostree kargs` for boot-parameter
  drift; loaded-module classification via the same base-tree/layered/
  anomalous model as the file-drift baseline. See
  `docs/superpowers/specs/2026-07-04-scanner-privileged-tier-design.md`.

## Design docs produced this pass

| Doc | Purpose |
|---|---|
| `AUDIT.md` | Stage-1 audit: the four decisions + independent critique |
| `IMPLEMENTATION_PROMPT.md` | 6-phase, branch-by-branch implementation plan |
| `docs/superpowers/specs/2026-07-04-gui-stack-decision.md` | GTK4 vs. Tauri vs. Qt/Kirigami options analysis |
| `docs/superpowers/specs/2026-07-04-cxx-qt-feasibility-research.md` | Rust↔Qt binding maturity research |
| `docs/superpowers/plans/2026-07-04-kirigami-shell-rewrite.md` | 18-task ordered UI migration plan |
| `docs/superpowers/specs/2026-07-04-flatpak-feasibility-research.md` | Sandbox network/filesystem boundary research |
| `docs/superpowers/specs/2026-07-04-scanner-baseline-design.md` | Component B's atomic-baseline classification design |
| `docs/superpowers/specs/2026-07-04-scanner-privileged-tier-design.md` | Rootkit/kernel-audit tool selection for Component B |

(Later plan docs, not listed here individually, live in
`docs/superpowers/plans/` — the naming convention is
`YYYY-MM-DD-<slug>.md`; browse that directory for anything after
2026-07-04, including the Phase 2 packaging plan, the Kirigami rewrite's
final status, both scanner-tier plans, the Phase 6 report-UI plan, and the
three 2026-07-12 tray-state plans.)
