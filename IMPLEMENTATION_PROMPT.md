# Implementation Prompt — Snitchwatch (Component A) + Bazzite Security Scanner (Component B)

Source: `AUDIT.md` (decisions confirmed + independent critique, both resolved
and carried-forward items). This prompt scopes the next implementation phase
branch-by-branch, PRP-style. Work the phases **in order** — each one lists
its blocking dependency on the phase(s) before it. Do not skip ahead.

Follow the existing plan-doc convention: each phase below should get its own
file at `docs/superpowers/plans/2026-MM-DD-<slug>.md` before implementation
starts, mirroring `docs/superpowers/plans/2026-04-11-blocklists.md` etc.

**Stop-and-ask protocol**: this repo's own norm (per `AUDIT.md`) is to
stop and ask on anything ambiguous rather than guess. Phase 4 below remains
a stop-and-ask gate — do not resolve it unilaterally, do not pick a default
and proceed. Surface the question, present the tradeoffs, and wait for an
explicit answer before writing code against that decision. (Phase 3, the
GUI stack, was a stop-and-ask gate as of the original audit — it has since
been decided; see the updated Phase 3 below.)

---

## Phase 1 — WS transport: Unix domain socket + handshake auth token (Component A)

**Blocks:** Phase 2 (all packaging work). A Flatpak-sandboxed GUI talking to
an unauthenticated loopback WS is not shippable — see `AUDIT.md`'s
independent-critique CRITICAL finding. **Scope expanded 2026-07-04**: the
Flatpak feasibility research
(`docs/superpowers/specs/2026-07-04-flatpak-feasibility-research.md`) found
that TCP loopback does not cross the Flatpak sandbox boundary at all —
Flatpak's default sandbox gets its own private, isolated network namespace,
so a Flatpak-sandboxed GUI cannot reach a host-bound `127.0.0.1:3031` WS
server regardless of auth. This is not just an auth gap, it's a transport
gap. Fix both in this phase.

**Goal:** The WS server moves from a TCP loopback socket to a **Unix domain
socket** under `$XDG_RUNTIME_DIR` (crosses the Flatpak sandbox boundary via
the well-precedented `--filesystem=xdg-run/<name>` permission — see the
feasibility doc's §3 precedent, `virt-manager`'s Flathub manifest uses this
exact pattern for a structurally identical daemon-over-socket case), and
any local process must still present a shared-secret token before the
server accepts messages from it.

**Files/crates touched:**
- `crates/snitchwatch-bridge/src/ws_server.rs` — `handle_socket`, `ws_handler`,
  `serve_with_blocklists` (and the equivalent path used by
  `snitchwatch-bridge-cli`/`lib.rs` in production, not just the test helper)
  — switch the listener from `TcpListener` to `tokio::net::UnixListener`
  (axum/hyper both support serving over a Unix listener; check
  `axum::serve` version compatibility against this workspace's pinned
  `axum = "0.7"`)
- `crates/snitchwatch-bridge-cli/src/main.rs` (or `lib.rs::run`) — socket
  path + token generation at startup, socket file permissions (0700 on the
  parent dir under `$XDG_RUNTIME_DIR/snitchwatch/`, 0600 on the socket/token
  files), and where the token gets written for the GUI to read
- `crates/snitchwatch-tauri/src/*` (or the Kirigami shell, depending on
  Phase 3 timing) — connect via the Unix socket path instead of
  `ws://127.0.0.1:PORT/stream`, and read the token
- New: a token module, e.g. `crates/snitchwatch-bridge/src/auth.rs`

**Design constraints (from the coordinator's resolution, updated for the
transport change):**
- Generate a random token at bridge startup (not a fixed/default value).
- Socket and token both live under `$XDG_RUNTIME_DIR/snitchwatch/` (mode
  0700 dir, 0600 files) — this is now also the mechanism the Flatpak
  manifest grants access to via `--filesystem=xdg-run/snitchwatch`, so the
  file-vs-env question from the original resolution is settled by the
  transport change: it must be a file, not an env var, since the consuming
  process is in a different sandbox and won't share the bridge's
  environment.
- **Worth a quick sanity check, not a full stop-and-ask:** with a Unix
  socket, `SO_PEERCRED` gives a verified UID for free — the shared-secret
  token becomes a defense-in-depth layer on top of that, not the sole
  guard it would have been over TCP. Keep the token per the standing
  decision (simpler to reason about, doesn't require the extra
  `SO_PEERCRED` syscall plumbing), but note this in the code comment/PR
  description so it's not mistaken for the only thing standing between an
  attacker and the WS.
- The token must be presented **before** any `ClientMessage` is processed —
  reject and close the connection otherwise. Minimal-diff approach: expect
  the token as the first WS text frame after upgrade (a lightweight
  handshake message) — a query-param-on-URL approach doesn't map as
  cleanly onto a Unix socket connection (there's no URL), so the handshake
  message is now the clearer default.
- Existing tests in `ws_server.rs` (`server_binds_to_ephemeral_port`,
  `server_serves_index_html_at_root`, etc.) construct `WsHandles` directly
  over TCP — these need updating for the `UnixListener` switch, not just
  the token addition. Check whether the token check only applies to the
  `/stream` route (it should; `/`, `/assets/*`, and fallback stay
  unauthenticated since they only serve static frontend assets, not
  firewall-rule-writing messages).

**Acceptance criteria:**
- New unit test: connecting over the Unix socket to `/stream` without the
  token is rejected (connection closed, no `ClientMessage` reaches
  `handles.inbound`).
- New unit test: a client presenting the correct token over the Unix
  socket can send/receive as before (extend or duplicate the existing
  round-trip coverage).
- New unit test: wrong/malformed token is rejected the same as no token.
- `just test-bridge` passes.
- Manual verification: update the README's manual-testing section (lines
  ~77–81, ~127–129) — the `websocat ws://127.0.0.1:3031/stream` example no
  longer applies; document the Unix-socket-equivalent invocation (`websocat
  --unix-listen`/`ws-l:unix:` or equivalent, with the token) instead.
- Tauri shell (or Kirigami shell, if Phase 3 has landed first) still
  connects successfully in `just tauri-dev` (manual smoke, plus
  update/verify `tests/tauri_smoke`).

---

## Phase 2 — Distribution: bluebuild (primary) + rpm-ostree layering (documented alternative) + Flatpak, with fail-open stance (Component A)

**Blocks on:** Phase 1 (auth token must exist before shipping any packaged
build that's reachable outside a single trusted dev machine).

**Goal:** A user can install Snitchwatch on Bazzite two ways — batteries-
included (custom bluebuild image with opensnitchd baked in) or lightweight
(rpm-ostree-layer opensnitchd onto stock Bazzite) — and get the GUI/bridge
via Flatpak in both cases, with a documented answer for what happens when
the GUI/bridge isn't running.

**Files/crates touched:**
- New: `packaging/bluebuild/` (or similar) — bluebuild recipe baking
  opensnitchd + a systemd unit that starts it with `Server.Address` pointed
  at the bridge's default bind
- New: `packaging/flatpak/org.snitchwatch.Snitchwatch.yml` (or similar) —
  Flatpak manifest for the GUI shell only (see updated architecture note
  below — `snitchwatch-bridge-cli` is NOT inside this Flatpak). Grants
  `--filesystem=xdg-run/snitchwatch` to reach the Unix socket + token from
  Phase 1; does **not** grant `--share=network` for reaching the bridge
  (per `docs/superpowers/specs/2026-07-04-flatpak-feasibility-research.md`,
  TCP loopback doesn't cross the sandbox boundary at all, so that
  permission wouldn't even help — Unix socket + filesystem permission is
  the only mechanism that works here). If blocklist-fetching or other
  future GUI-side features genuinely need internet access, that's a
  separate, explicit `--share=network` grant to evaluate on its own merits
  later, not bundled into this phase.
- New: `docs/packaging/rpm-ostree-layering.md` (or fold into `README.md`) —
  step-by-step `rpm-ostree install opensnitch` + config for the
  `Server.Address` dial-in, since opensnitchd is already packaged for
  Fedora (confirmed during the audit's independent critique)
- `crates/snitchwatch-bridge-cli/` — needs a way to run as an autostarted
  user service independent of the GUI window (see fail-open stance below)
- New: a systemd **user** unit for the bridge (e.g.
  `packaging/systemd/snitchwatch-bridge.service`), separate from the
  existing `~/.config/autostart/snitchwatch.desktop` GUI autostart
  mechanism already implemented in the Tauri shell (see `README.md`
  "Autostart" section)

**Fail-open stance — resolve explicitly, don't assume:**
opensnitchd applies its configured default action when no UI is connected.
Today, "no UI connected" happens whenever the GUI window (and therefore
the in-process bridge — see `README.md`: "The bridge runs in-process on
127.0.0.1:3031" under Tauri) isn't running. That conflates "user closed the
window" with "the firewall decision channel is down." Decide and document:
does the bridge become a standalone autostarted user service (decoupled
from the GUI window, so closing the window doesn't kill the AskRule path),
with the GUI just being a client that can be closed/reopened freely? This
is the natural fix and is consistent with the M3 architecture already
separating `snitchwatch-bridge-cli` from `snitchwatch-tauri` as distinct
crates.

**Confirmed: opensnitchd's actual behavior is fail-open by default, and
it's a user-configurable setting, not a hardcoded daemon default.**
`vendor/opensnitch/daemon/data/default-config.json:18` ships
`"DefaultAction": "allow"`. This value is exactly what's applied when no
UI client is connected — `vendor/opensnitch/daemon/ui/config_utils.go:178-180`
sets `clientDisconnectedRule.Action = rule.Action(newConfig.DefaultAction)`,
and `vendor/opensnitch/daemon/main.go:431-433`'s `applyDefaultAction` is
what fires when there's no rule match and no connected UI, applying
whatever `uiClient.DefaultAction()` currently resolves to. So out of the
box, opensnitchd silently **allows** all traffic once the bridge/GUI goes
away — the exact fail-open behavior the original concern worried about,
confirmed rather than assumed.

Since `DefaultAction` is a plain JSON config key (`ui/config.go:81`), not
a compiled-in constant, Snitchwatch's packaging (bluebuild image /
rpm-ostree-layered config / install script) should **ship
`"DefaultAction": "deny"`** in the daemon's `default-config.json` rather
than accept upstream's `"allow"` default — a firewall product whose whole
premise is "ask before allowing" should fail closed when its own decision
channel is down, not silently permit everything. This recommendation is
specific to Component A's interactive-firewall use case; it has no
bearing on Component B (the scanner doesn't touch opensnitchd's config or
depend on its default action at all).

**Acceptance criteria:**
- Bluebuild recipe builds an image (CI or local) with opensnitchd present
  and enabled, `Server.Address` pre-configured to match the bridge's
  default bind (`127.0.0.1:50051` per `README.md`).
- `rpm-ostree-layering.md` walkthrough is followed manually end-to-end on a
  stock Bazzite VM/container and results in a working `opensnitchd` that
  the Flatpak bridge can dial into — record this as a manual verification
  step, not just a doc that's never been run.
- Flatpak manifest builds (`flatpak-builder` or `flatpak run
  org.flatpak.Builder`) and the resulting sandboxed GUI can reach the
  host-side bridge over the Unix socket + token from Phase 1 (via
  `--filesystem=xdg-run/snitchwatch`) — verify this concretely by running
  the built Flatpak against a running `snitchwatch-bridge` systemd user
  service and confirming `AskRule` round-trips end to end. The bridge
  itself (and its gRPC connection to opensnitchd) stays entirely host-side
  and outside the Flatpak sandbox, per the fail-open stance above — no
  network permission is needed in the manifest for this to work.
- systemd user unit for the bridge: `systemctl --user status
  snitchwatch-bridge` shows it running independent of whether the Tauri
  GUI window is open; closing the GUI window does not stop the bridge
  process (verify by watching `AskRule` still round-trips via a raw
  `websocat` client while the GUI is closed).
- README updated with both install paths, explicitly presented as
  "batteries-included" vs "lightweight/DIY," not primary-vs-unsupported.

---

## Phase 3 — Qt6/QML + Kirigami rewrite of the shell (Component A)

**DECIDED 2026-07-04** (was a stop-and-ask gate; see
`docs/superpowers/specs/2026-07-04-gui-stack-decision.md` for the full
Options A/B/C/D analysis). The human owner chose **Option D**: a full
Qt6/QML+Kirigami rewrite, on the reasoning that it's the only option that
actually matches Bazzite's default desktop (KDE Plasma) natively. Accepted
tradeoffs going in: the ~6,939-line vendored web frontend is fully
discarded and rebuilt from scratch, and `cxx-qt` (the Rust↔Qt binding) is
meaningfully less proven than `gtk4-rs` would have been, with zero prior
Qt/QML work anywhere in this repo's history.

**Blocks:** Phase 6's report UI (needs this shell's design system to exist
first). Does not block Phases 1, 2, 4, or 5 — those proceed independently.

**Because binding-layer maturity was the single biggest named risk in the
decision doc, do not start the full rewrite directly. Sub-phase this:**

**Phase 3a — `cxx-qt` feasibility spike.** Before committing further,
build the smallest possible `cxx-qt`-based Rust↔QML round trip: a throwaway
binary that opens a Kirigami `ApplicationWindow`, wires one Rust-owned
property or signal through `cxx-qt`, and confirms the build/packaging story
(does this coexist cleanly with the existing Cargo workspace, what's the
`build.rs` story compared to `snitchwatch-tauri`'s and `snitchwatch-proto`'s
existing `build.rs` usage). **Stop-and-ask if this spike surfaces a hard
blocker** (e.g., `cxx-qt` can't do something Phase 3b will need, like
async Rust↔QML signal delivery for the bridge's WS-driven state updates) —
do not silently downgrade to Option A/B/C without going back to the owner.

**Phase 3b — full rewrite**, once 3a de-risks the binding layer. New
workspace member(s), e.g. `crates/snitchwatch-kirigami/` (or replace
`crates/snitchwatch-tauri/` in place — decide naming when this phase
starts). Needs, at minimum, feature parity with what `web/` provides today
before it can replace it: the three-tab Connections/Blocklists/Rules UI,
the inspector pane, the pending-`AskRule`-decision prompt (this is the
highest-priority parity item — it's the core interactive loop), the
traffic chart (`QtCharts` or `QtQuick` in place of uPlot), onboarding
wizard, tray via `KStatusNotifierItem`, desktop notifications, autostart,
crash log — i.e., everything `crates/snitchwatch-tauri/`'s 891 lines and
`web/`'s ~6,939 lines currently do, re-wired against Qt/Kirigami idioms
rather than Tauri's.

**Author a dedicated migration plan doc** (e.g.
`docs/superpowers/plans/2026-MM-DD-kirigami-shell-rewrite.md`, following the
existing `docs/superpowers/plans/` convention) before starting 3b — this is
too large a scope to implement directly off this prompt's bullet list. That
plan doc should break the feature-parity list above into its own ordered
sub-phases with acceptance criteria per feature, the way `AUDIT.md`/this
prompt did for the overall project.

**Acceptance criteria for Phase 3 as a whole:**
- 3a: a working `cxx-qt` spike exists, committed or documented, with an
  explicit go/no-go note on whether it de-risks the binding layer enough
  to proceed.
- 3b: only starts after 3a's go decision and the migration plan doc exists.
- Existing `tests/tauri_smoke`/`tests/web_smoke` Playwright suites are not
  deleted until the new shell has equivalent coverage for the same
  scenarios (wizard branches, tray states, AskRule round trip) — parity in
  tests, not just in features.
- Old `crates/snitchwatch-tauri/` and `web/` are only removed once the
  Kirigami shell has shipped feature parity and its own test coverage —
  don't leave the repo without a working shell mid-migration.

---

## Phase 4 — Component B baseline design session

**Blocks:** every other Component B phase (5, 6). No Component B code gets
written before this lands — per `AUDIT.md`'s decision #4, unchanged by the
independent critique.

**This is a design deliverable, not implementation.** Scope it as its own
session/plan doc, e.g. `docs/superpowers/specs/2026-MM-DD-scanner-baseline-design.md`.

**Must resolve, per `AUDIT.md`'s "Deferred: Component B baseline design"
section (carry these questions forward verbatim, they haven't changed):**
- What counts as expected drift on an rpm-ostree atomic system (layered
  packages per `rpm-ostree status --json`, `usroverlay`-writable paths,
  systemd-generated units/tmpfiles) vs. genuine anomaly.
- Fresh-each-scan baseline (recompute against the signed commit's
  manifest) vs. cached-and-invalidated-on-deployment-hash-change.
- How layered-package allowlisting interacts with AIDE-style integrity
  checks — whole-RPM trust vs. per-file checksum matching.
- Output shape for "what changed since last scan" — requires stored
  prior-scan state.
- Evaluate the two candidate approaches already named in `AUDIT.md`:
  signed-commit-diff + allowlist + usroverlay exclusions, vs. delegating
  to `rpm-ostree db diff` primitives directly.

**Acceptance criteria:** a design doc exists answering all four bullets
above with a concrete, implementable answer (not "TBD") for each, reviewed
and approved before Phase 5 starts.

---

## Phase 5 — Component B userspace tier (Component B)

**Blocks on:** Phase 4 (baseline design must be approved). Also has an
internal precondition below regarding the shared-signal channel.

**Goal:** implement the userspace-default tier of the scanner per the
approved baseline design from Phase 4.

**Precondition — connection-log persistence (Component A):**
`AUDIT.md`'s independent critique flagged that decision #1's "Component
B's userspace tier may read Component A's connection log as one input
signal" is currently fictional: `crates/snitchwatch-bridge/src/cache/`
(connections + traffic_bins) is in-memory only; only blocklists persist
(`rusqlite` in `crates/snitchwatch-bridge/src/blocklists/store.rs`).

**Stop-and-ask before building this:** confirm the shared-signal channel
is still wanted before building connection-log persistence in Component A.
If yes:
- Add persistence to `crates/snitchwatch-bridge/src/cache/` (follow the
  existing `blocklists/store.rs` rusqlite pattern for consistency)
- Define the read contract Component B uses (a stable on-disk schema/path,
  or an API — do not have B read the bridge's internal cache structs
  directly, that couples the two components' internals despite the
  "no shared daemon" decision)
If the answer is "not needed yet," Phase 5 proceeds without this and
Component B's userspace tier stands alone for its first release.

**Files/crates touched:**
- New workspace member(s), e.g. `crates/scanner-core/`,
  `crates/scanner-cli/` (mirrors the `snitchwatch-bridge` /
  `snitchwatch-bridge-cli` split already established for Component A)
- Conditionally: `crates/snitchwatch-bridge/src/cache/` (persistence, only
  if the stop-and-ask above resolves "yes")

**Acceptance criteria:**
- Userspace tier runs without elevated privileges and produces a scan
  report matching the baseline design's output shape.
- Unit + integration tests covering: expected-drift classification per the
  approved design, "what changed since last scan" diffing against a stored
  prior state.
- If persistence was built: a test proving Component B can read Component
  A's persisted connection log through the defined read contract, and that
  Component A functions unaffected with Component B entirely absent
  (decoupling must actually hold, not just be asserted).

---

## Phase 6 — Component B privileged tier + report UI (Component B)

**Blocks on:** Phase 5.

**Goal:** the on-demand privileged tier (AIDE-style integrity check,
rootkit scan, rpm-ostree diff) invoked via polkit/pkexec as a separate
host-level binary — never a persistent daemon — plus a report UI sharing
Component A's design system.

**Concretely specified in `docs/superpowers/specs/2026-07-04-scanner-privileged-tier-design.md`
— read it before implementing this phase.** Summary of its recommendations:
wrap **`chkrootkit`**, not `rkhunter` (`rkhunter`'s last major release was
2018 and it has no meaningful eBPF-rootkit coverage; `chkrootkit` is
actively maintained with current-threat signatures, e.g. the XZ backdoor
and the Bootkitty UEFI bootkit). Add three concrete sub-checks the
original one-liner didn't itemize: **kargs drift** (`/proc/cmdline` vs.
`rpm-ostree kargs`'s committed set), **loaded-module audit** (each entry
in `/proc/modules` classified via the baseline doc's own three-tier
base-tree/layered-package/anomalous provenance model, not a separate
hardware-dependent allowlist), and **Secure Boot/lockdown state** (flag on
*transition*, e.g. enabled→disabled, not on absolute state). All checks
run as ordered sub-steps of one polkit-gated invocation (not separate
binaries/prompts), and all findings extend the baseline doc's existing
`scans.db` `findings` table with a `check_type` discriminator column
rather than a parallel schema.

**Files/crates touched:**
- New: `crates/scanner-privileged/` (separate binary, polkit policy file,
  e.g. `packaging/polkit/org.snitchwatch.scanner.policy`)
- New: report UI, built in **Kirigami** (per Phase 3's decision) to share
  Component A's design system per `AUDIT.md`'s decision #1. **Only
  buildable once Phase 3b has shipped** — this UI depends on the Kirigami
  shell's own components/conventions existing first, not just the Phase 3
  decision being made. Do not start this UI work while Phase 3b is still
  in progress.

**Acceptance criteria:**
- Privileged binary only runs on-demand via `pkexec`/polkit — verify no
  systemd service, timer, or long-running privileged process is
  introduced (this is the property decision #1 was protecting; a
  regression here silently reintroduces the "always-on privileged daemon"
  Component B was explicitly designed to avoid).
- polkit policy correctly gates the privileged actions (manual
  verification: non-privileged user is prompted; denying the prompt
  blocks the scan).
- Report UI visually consistent with the Kirigami shell from Phase 3.
- End-to-end manual test: userspace tier flags something needing deeper
  inspection → user triggers privileged scan → polkit prompt → report
  renders with consistent design language to Component A.

---

## Cross-cutting notes for whoever executes this

- `ui.proto` instability and the "conflicts with official opensnitch-ui if
  both installed" issue (`AUDIT.md` independent critique) isn't blocking
  but should get a install-time detection check or at least a README
  callout during Phase 2's packaging work — cheap to add while already
  touching install docs.
- Vendored `web/` frontend licensing/provenance against this repo's
  GPL-2.0 (`web/rebrand.sh`) is still unaddressed. With Option D decided,
  `web/` is slated for full removal once Phase 3b ships parity, which
  retires the question for any future release — but resolve it now anyway
  for any interim release cut while `web/` is still in the tree during the
  migration window.
- Every phase above should follow the existing `docs/superpowers/plans/`
  naming and structure convention (`2026-MM-DD-<slug>.md`) already used for
  M0–M4, and each should get its own PR/branch rather than one giant diff —
  matches the "decouples release cadence and risk profile" rationale in
  `AUDIT.md` decision #1.
