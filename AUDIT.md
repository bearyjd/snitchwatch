# AUDIT — App Firewall (Snitchwatch) + Bazzite Security Scanner

> **Update 2026-07-12: historical record — all "Next step" items below are
> now done or code-complete.** This doc is a point-in-time snapshot from
> the original audit pass; it is not a living status doc. For current
> phase-by-phase status see `IMPLEMENTATION_PROMPT.md` (which has its own
> per-phase update notes) and `HANDOFF.md`. Component B (described below as
> "does not exist yet") is also now code-complete (Phases 4-6). The
> "GTK4-vs-Tauri conflict... still unresolved" note further down is
> superseded by this same doc's own "RESOLVED 2026-07-04" GUI-stack
> decision above it — that internal contradiction is a leftover from this
> doc never being reconciled after the later decision, not a re-opening of
> the question.

Stage 1 audit: resolve the four open architecture questions from the original
handoff brief against what's actually built in this repo, flag anything that
still needs a dedicated design pass rather than guessing, and hand off a
scoped implementation prompt for what's left.

## Scope

- **Component A (App Firewall / "Snitchwatch")** — this repo. Pre-alpha,
  Plan 1–4 complete per `docs/superpowers/plans/`. Substantial groundwork
  exists: see "What's already built" below.
- **Component B (Immutable-OS Security Scanner)** — does not exist yet.
  Zero code, zero design docs. Everything about it below is proposal, not
  audit-of-existing-work.

## Decisions confirmed this pass

These four were genuinely ambiguous and are now locked for the
implementation phase. Revisit only with explicit cause.

1. **One app or two → Two separate apps.** Component A and B ship as
   separate binaries/daemons/packages. They share a design system
   (GTK4/libadwaita component conventions) and, optionally, a local signal
   channel — Component B's userspace tier may read Component A's connection
   log as one input signal. They do **not** share a daemon, a systemd unit,
   or a privilege model. Rationale: Component B's core security property is
   "no persistent privileged daemon, on-demand only via polkit." Merging
   the two either forces B to inherit A's always-on bridge/daemon (killing
   that guarantee) or forces A's real-time responsiveness to wait on B's
   on-demand invocation model. Keeping them separate also decouples release
   cadence and risk profile — a scanner bug shouldn't block a firewall
   release or vice versa.

2. **Component A's interception layer → keep riding opensnitchd.**
   Snitchwatch remains a friendlier opensnitchd frontend indefinitely. It
   inherits opensnitchd's own netfilter/nfqueue-or-eBPF choice and whatever
   kernel-version sensitivity that implies on Bazzite's kernel. A from-scratch
   nfqueue/eBPF daemon (and the parent-process-hierarchy differentiator that
   would unlock) is explicitly deferred — it's the highest-risk,
   most security-critical code in the whole project and isn't justified
   until the frontend/UX differentiation is proven out first.

3. **Distribution model → bluebuild image + Flatpak GUI.** opensnitchd (and,
   later, Component B's host-level pieces) get baked into a custom bluebuild
   Bazzite image. The GUI/bridge (`snitchwatch-tauri` + `snitchwatch-bridge`)
   ships as a **Flatpak** talking to opensnitchd over loopback gRPC. This is
   viable because of a finding from this audit pass (see below): the bridge
   itself needs no raw socket / `CAP_NET_RAW` / netfilter access — only
   opensnitchd does. Users on the custom image get it "for free"; anyone
   else installs the Flatpak against a separately-provisioned opensnitchd.

4. **Component B's atomic-baseline problem → scoped as its own design pass.**
   Not solved here. See "Deferred: Component B baseline design" below for
   what that follow-up session needs to produce before any Component B code
   is written.

## Finding: the bridge doesn't need privileged access

`snitchwatch-bridge` exposes two loopback-only servers:

- a gRPC `protocol.UI` server that opensnitchd (the privileged party) dials
  *into* as a client (`grpc_client.rs`, `grpc_server.rs`)
- a WebSocket server at `/stream` for the frontend (`ws_server.rs`,
  `ws_messages.rs`)

Neither requires elevated capabilities — opensnitchd holds all the
netfilter/eBPF privilege, and the bridge just relays translated messages.
This directly changes the risk calculus stated in the original brief (which
assumed the same `CAP_NET_RAW` wall as Gatepath's desktop side): **the GUI
and bridge can be Flatpak-sandboxed**; only opensnitchd needs to live outside
any sandbox, on the host, privileged. Same logic applies to Component B —
its privileged tier (AIDE-style integrity check, rootkit scan, rpm-ostree
diff) is invoked on-demand via polkit/pkexec as a separate host-level
binary, while its GUI can be sandboxed same as A's.

**Update 2026-07-04 — the capability-vs-network distinction turned out to
matter.** Follow-up research
(`docs/superpowers/specs/2026-07-04-flatpak-feasibility-research.md`) found
that "the bridge doesn't need privileged access" is true for *capabilities*
(no `CAP_NET_RAW`) but does not by itself make Flatpak-sandboxing work —
Flatpak's sandbox also isolates the *network namespace*, so a
Flatpak-sandboxed GUI cannot reach a host-bound TCP loopback service at
all, regardless of privilege. The fix (Unix domain socket instead of TCP,
gated by a filesystem permission rather than a network permission) is now
folded into `IMPLEMENTATION_PROMPT.md` Phases 1–2. Flagging this here so
the "no privileged access needed" framing above isn't mistaken for "no
sandboxing work needed" — they're separate claims.

## What's already built (Component A)

```text
crates/
├── snitchwatch-proto/       # generated tonic/prost bindings for opensnitchd's ui.proto
├── snitchwatch-spike/       # M0 spike binary that probes a live daemon
├── snitchwatch-bridge/      # headless bridge lib: cache, translator, ws server, grpc client, blocklists
│   └── src/
│       ├── blocklists/      # fetcher, format parsing, materializer (rules → opensnitchd), store
│       ├── cache/           # connections + traffic_bins in-memory state
│       ├── translator/      # upstream/downstream, glob matching, specificity, rule_semantics, verdict
│       ├── grpc_client.rs, grpc_server.rs, ws_server.rs, ws_messages.rs, tray_state.rs, notice.rs
├── snitchwatch-bridge-cli/  # thin orchestrator (lib::run + main.rs)
└── snitchwatch-tauri/       # Tauri 2 desktop shell: tray, notifications, autostart, wizard, crash log
tests/
├── bridge_protocol_test.rs  # full AskRule round-trip vs in-process mock opensnitchd
├── mock_opensnitchd/        # in-process gRPC mock with scripted events
├── tauri_smoke/, web_smoke/ # Playwright suites
└── fixtures/blocklists/
web/                          # vendored Little-Snitch-for-Linux-style frontend (rebranded), PWA manifest
docs/superpowers/specs+plans  # design doc + milestone plans (M0–M4 tracked, M4/blocklists done)
```

Architecture: opensnitchd dials into the bridge's gRPC `protocol.UI` server
(`Ping`, `AskRule`, `Subscribe`, `PostAlert`, bidi `Notifications` stream).
`AskRule` is a blocking unary handler — the bridge inserts a pending row,
broadcasts it over the WS `/stream`, awaits user verdict via a `oneshot`,
translates the verdict back into a `Rule` reply. The frontend speaks Little
Snitch v6 protocol over that WebSocket. Blocklist subscription (M4) fetches,
parses, and materializes entries into opensnitchd's `900-blocklist:<name>:`
rule band, pushed live via `subscribeBlocklist`/`setBlocklists`/
`setBlocklistEntries`.

Dev/test loop currently runs opensnitchd in a rootful podman container
(`--privileged --network=host --pid=host --cap-add=NET_ADMIN,SYS_ADMIN,BPF`)
— there is no native Bazzite install path today (no bluebuild image, no
rpm-ostree layering script, no systemd unit shipped). That's the concrete
gap decision #3 above closes.

## Flagged for decision — RESOLVED 2026-07-04

**GUI stack conflict — decided: Qt6/QML + Kirigami rewrite.** The original
brief called for GTK4/libadwaita native, matching Gatepath's desktop stack
and explicitly ruling out Qt. What was actually built is a Tauri 2 shell
wrapping a vendored web-tech frontend. Neither GTK4 nor Tauri is truly
"native" on Bazzite's actual default desktop (KDE Plasma, not GNOME) — see
`docs/superpowers/specs/2026-07-04-gui-stack-decision.md` for the full
options analysis (Options A/B/C/D). The human owner chose **Option D: a
full Qt6/QML+Kirigami rewrite**, on the reasoning that matching Bazzite's
actual default desktop natively outweighs keeping the lower-risk,
already-built Tauri shell (Options A/C) or a GTK4 rewrite that wouldn't be
native there either (Option B). Accepted tradeoffs: the ~6,939-line
vendored frontend gets fully rewritten, and `cxx-qt` (the Rust↔Qt binding)
is less mature than `gtk4-rs` would have been. This reverses the original
brief's "not Qt" constraint — that constraint was itself premised on a
GNOME-native assumption this audit's KDE-default finding undermines.

## Deferred: Component B baseline design

This is the crux of the whole scanner and is intentionally **not** solved
in this audit. A dedicated design session must resolve, before any
Component B code is written:

- What counts as **expected** drift on an rpm-ostree atomic system: layered
  packages (visible in `rpm-ostree status --json`), `usroverlay`-writable
  paths, systemd-generated units/tmpfiles, vs. genuinely anomalous file
  changes.
- Whether the baseline is computed fresh each scan against the signed
  commit's manifest (recompute-heavy but always correct post-upgrade) or
  cached and invalidated on `rpm-ostree status` deployment-hash change
  (cheaper, but needs correct invalidation logic).
- How layered-package allowlisting interacts with AIDE-style integrity
  checks — does every file shipped by a user-layered RPM get auto-trusted,
  or only files matching that RPM's manifest checksums?
- Output shape for "what changed since last scan" diffing — needs a stored
  prior-scan state, not just a stateless re-dump.

Candidate approaches (not chosen, for the design session to evaluate):
signed-commit-diff + layered-package allowlist + usroverlay path
exclusions; or delegate more to `rpm-ostree db diff` primitives directly
rather than reimplementing manifest diffing.

## Independent critique (Fable, round 1)

An independent pass (fresh context, no memory of the decisions above)
verified this audit's factual claims against the repo and pressure-tested
the four confirmed decisions. Findings below; two were significant enough
to resolve before proceeding, the rest are carried forward as scoped items
for the implementation phase.

**Verified accurate:** the "bridge needs no privileged access" claim (all
listeners bind `127.0.0.1` only; no raw-socket/capability code anywhere in
`crates/`); the "no native Bazzite install path exists" claim (no systemd
units, bluebuild recipe, Flatpak manifest, or `.spec` in the repo); the
architecture description of the AskRule flow and blocklist materialization.

**CRITICAL — resolved.** The WS `/stream` channel (`ws_server.rs`) has no
authentication — any local process, including malware the firewall is
meant to be blocking, can dial `ws://127.0.0.1:3031/stream` and send
`setVerdict`/`subscribeBlocklist` to allow-rule its own traffic. Flatpak-
sandboxing the GUI (decision #3) doesn't help if the channel that writes
firewall rules is open loopback with no gate. **Resolution: add a
shared-secret token to the WS handshake.** The bridge generates a token at
startup and writes it somewhere the GUI can read (local file or env var);
WS connections must present it before any message is accepted. This is a
prerequisite for shipping the Flatpak in decision #3, not a nice-to-have.

**Distribution — re-confirmed with caveat.** opensnitchd is already
packaged in Fedora, so `rpm-ostree install opensnitch` layering works on
stock Bazzite without a custom image. Re-examined this against decision #3
and **kept bluebuild as the primary path** for the "batteries included"
experience, but the implementation phase should explicitly document
rpm-ostree layering as the supported lightweight alternative for users who
don't want to rebase to a custom image — not treat it as unsupported.

**Carried forward, not blocking:**
- **Fail-open behavior is undecided.** opensnitchd applies its configured
  default action when no UI is connected — if the Flatpak GUI/bridge isn't
  running (crashed, not autostarted, user logged out), the firewall
  silently degrades to whatever opensnitchd's default is. Needs an explicit
  stance (e.g., bridge ships as its own autostarted user service, separate
  from the GUI window) before the Flatpak packaging work in decision #3.
- **Decision #1's shared signal channel is currently fictional.** The
  cache (`cache/`) is in-memory only; only blocklists persist (rusqlite in
  `blocklists/store.rs`). Component B reading "A's connection log" requires
  building connection persistence in A first — not assumed to already exist.
- **`ui.proto` is not a stable upstream API**, and opensnitchd supports only
  one configured UI address — installing Snitchwatch conflicts with the
  official opensnitch-ui if both are present on the same system. Worth a
  user-facing note/detection at install time.
- **GTK4-vs-Tauri conflict, additional considerations surfaced:** Bazzite's
  default desktop is KDE, so libadwaita isn't strictly "native" there either
  — the original brief's premise is itself worth questioning. Pro-Tauri: a
  GTK4 rewrite discards the vendored frontend, which is the actual product
  differentiation, not just the shell. Pro-GTK4: Bazzite is gaming-focused,
  so the AskRule prompt must reliably surface over fullscreen games, and
  WebKitGTK is a large CVE surface for a security-focused tool. Still
  unresolved — needs an explicit call before the implementation phase
  touches the shell.
- **Vendored `web/` frontend licensing/provenance** (`web/rebrand.sh`)
  against this repo's GPL-2.0 is unaddressed anywhere.

## Next step

Per the original plan: a follow-up Opus/Claude Code implementation prompt,
scoped branch-by-branch (PRP-style), covering:

1. Component A: WS handshake auth token (prerequisite, blocks packaging work).
2. Component A: bluebuild image (primary) + documented rpm-ostree layering
   (alternative) + Flatpak packaging for the bridge/shell, with an explicit
   fail-open stance for when the GUI/bridge isn't running.
3. Component A: resolve the GTK4-vs-Tauri conflict before further shell work.
4. Component B: the dedicated baseline-design session (blocking — nothing else in B can start first).
5. Component B: userspace tier implementation once the baseline design lands
   (including building connection-log persistence in A if the shared-signal
   channel from decision #1 is still wanted).
6. Component B: privileged tier (polkit-gated) + report UI sharing A's design system.
