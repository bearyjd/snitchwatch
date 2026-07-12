# Snitchwatch — Agent Notes

Pre-alpha Rust workspace. Two independent products live in this one repo:

- **Component A ("Snitchwatch")** — a Little Snitch–style GUI on top of
  `opensnitchd`. It is a friendlier frontend, **not** a from-scratch
  interception engine: `opensnitchd` dials in as a gRPC client to a bridge
  this repo owns; the bridge translates to a Little-Snitch-v6-style
  WebSocket protocol for the frontend. Crates: `snitchwatch-proto`,
  `snitchwatch-spike`, `snitchwatch-bridge`, `snitchwatch-bridge-cli`,
  `snitchwatch-tauri`, `kirigami-spike`, `snitchwatch-kirigami`.
- **Component B (Bazzite security scanner)** — a separate, unrelated
  userspace/privileged-tier scanner for rpm-ostree/atomic-OS drift
  detection. Crates: `scanner-core`, `scanner-cli`, `scanner-privileged`. It
  intentionally shares no daemon, systemd unit, or privilege model with
  Component A — see "Settled decisions" below before proposing otherwise.

`vendor/opensnitch` is a **read-only git submodule** pinned at `v1.8.0`
(upstream reference for `ui.proto`, default config shape, etc.) — never edit
files under it. If `git status` shows it as dirty with only an untracked
`.omc/` (or similar tooling-artifact) path inside it, that's tooling noise,
not a real change; don't commit a submodule pointer bump for it.

## Build / test commands (verified 2026-07-07)

```bash
just build          # cargo build --workspace (default-members only)
just check           # cargo check --workspace && cargo clippy --workspace --all-targets -- -D warnings
just test             # cargo test --workspace
just test-bridge      # cargo test -p snitchwatch-bridge (fast, most iteration happens here)
just test-blocklists  # blocklist-specific unit + e2e suite
```

`cargo check` (no args, from repo root) and `cargo check -p snitchwatch-proto`
both verified working in this environment. Full `cargo check` (default
members) finishes in well under a minute with a warm target dir.

**`kirigami-spike` and `snitchwatch-kirigami` are excluded from
`default-members`** in `Cargo.toml` — they require system Qt6 + Kirigami dev
packages. A plain `cargo build`/`cargo check`/`just build` will *not* touch
them; this is expected, not a build failure. To work on the Kirigami shell,
build explicitly: `cargo build -p snitchwatch-kirigami`. Those tests also
need `QT_QPA_PLATFORM=offscreen` (and `QT_QUICK_CONTROLS_STYLE=Basic`) to run
headless — see `crates/snitchwatch-kirigami/tests/smoke.rs` for the pattern.
Whether these actually pass headless in this sandbox has **not** been
verified — treat a first attempt as exploratory, not "should just work."

Playwright suites (`tests/web_smoke`, `tests/tauri_smoke`) need a one-time
`just web-smoke-install` / `just tauri-smoke-install` before
`just web-smoke` / `just tauri-smoke` will pass. If one of these fails with a
missing-browser error, run the `-install` target first — it is not a code
regression.

`just package-check` validates packaging artifacts (YAML/JSON/systemd unit
syntax) without needing a Bazzite host.

`.github/workflows/ci.yml` runs four jobs on push to `main` and on PRs:
- `check`/`test` — `cargo check`/`clippy -D warnings`/`test`, **scoped to
  `default-members` only** (plain `cargo check`/`clippy`/`test`, not
  `--workspace`) so this gate never depends on Qt6/KF6 packaging succeeding.
- `package-check` — runs `just package-check` (Phase 2 packaging artifacts'
  YAML/JSON/systemd syntax + the Rust packaging shape test).
- `kirigami` — builds/lints/tests `snitchwatch-kirigami` specifically
  against Qt6 + KDE Frameworks 6 on Ubuntu, headless
  (`QT_QPA_PLATFORM=offscreen`). Addresses
  `.agent_native/agent_roadmap.md` item 6. **Its apt package names are
  best-effort** (translated from this sandbox's own Fedora packages, not
  confirmed against a live Ubuntu apt repo from here — no outbound network
  in this sandbox) — if it fails on an unknown-package error on its first
  real run, that's a package-name fix, not a `snitchwatch-kirigami` code
  problem.

Until the `kirigami` job's package names are confirmed working on a real
run, treat local `just check`/`just test` (which do pass `--workspace`) as
still mandatory before calling Kirigami-touching work done.

## Reproduction paths — use these, don't reach for a real daemon/host

- **Bridge / protocol bugs**: use `tests/mock_opensnitchd` (`MockOpensnitchd`,
  an in-process gRPC client standing in for the real daemon) plus the
  pattern in `tests/bridge_protocol_test.rs` (`ask_rule_round_trip_*`). Boot
  a real bridge in-process, connect a real WebSocket client over its Unix
  socket presenting the handshake token, drive it with scripted daemon
  events. This is the load-bearing test and the template for new scenarios.
- **Scanner (userspace) bugs**: use `scanner_core::testkit::MockInspector` —
  a programmable `SystemInspector` double (register command output /
  file contents / dir listings) with **no production call sites**, built
  specifically so tests never need a real `rpm-ostree`/`ostree`/`flatpak`/`ss`
  on the host. See `crates/scanner-core/tests/userspace_scan.rs`.
- **Scanner (privileged) bugs**: same idea via the `SystemFacts` trait — see
  the `FakeFacts` struct in `crates/scanner-privileged/tests/end_to_end.rs`.
  Integration tests compile without `#[cfg(test)]`, so the unit-test double
  isn't visible there; a separate fake exists at that layer — mirror it
  rather than trying to share the unit-test mock across the boundary.
- **Blocklist bugs**: fixtures live in `tests/fixtures/blocklists/` (tiny
  ABP/domains/StevenBlack samples + one intentionally-malformed
  `garbage.bin`); `just test-blocklists` runs both the unit and e2e suites.
- Never point any test at a live `opensnitchd`, a real Bazzite host, or
  `sudo` — every code path that needs one has a deterministic double above.
  If a bug report is specific to real-daemon behavior the mock doesn't
  model, that's a genuine gap (see `.agent_native/agent_roadmap.md` item 7)
  — flag it rather than improvising against a live system.

## Repo conventions

- **Plan-first for anything beyond a small fix**: new work gets a doc at
  `docs/superpowers/plans/2026-MM-DD-<slug>.md` *before* implementation
  starts (see the existing files there for the format/level of detail
  expected). Specs/research docs live in `docs/superpowers/specs/`.
- **Stop-and-ask is this repo's explicit norm for genuine ambiguity**
  (stated in `AUDIT.md`) — when a design question is truly open, surface
  the tradeoffs and wait rather than picking a default silently. This does
  **not** apply to the settled decisions below; don't re-litigate those
  without new evidence.
- Commit messages and PR process follow the user's standard conventional-
  commit format; no repo-specific override found.
- No `.rustfmt.toml`/`clippy.toml`/`rust-toolchain.toml` override exists —
  defaults apply. `just fmt` runs `cargo fmt --all`.

## Settled architecture decisions — do not re-derive, cite instead

These were resolved across `AUDIT.md`/`HANDOFF.md`/`IMPLEMENTATION_PROMPT.md`
after real back-and-forth. Read the source doc before proposing a change.

1. **Components A and B are two separate apps**, sharing at most a design
   system and, optionally, a one-way signal (B reading A's connection log).
   No shared daemon/systemd unit/privilege model. Rationale: B's core
   security property is "no persistent privileged daemon, on-demand only via
   polkit" — merging would break that. See `AUDIT.md` decision #1.
2. **Component A keeps riding `opensnitchd`** for interception; a
   from-scratch nfqueue/eBPF daemon is explicitly deferred (too
   security-critical to justify before the frontend is proven out).
3. **Distribution: bluebuild image (primary) + documented rpm-ostree
   layering (alternative) + Flatpak GUI.** The bridge needs no
   `CAP_NET_RAW`/privileged capability — only `opensnitchd` does — but
   Flatpak isolates the *network namespace* too, so the WS transport is a
   **Unix domain socket** under `$XDG_RUNTIME_DIR/snitchwatch/` (0700 dir,
   0600 files), not TCP loopback, gated by `--filesystem=xdg-run/snitchwatch`.
   A random handshake token (written alongside the socket, mode 0600) must
   be sent as the first WS text frame on `/stream` before the bridge treats
   a connection as trusted; `/`, `/assets/*`, and the SPA fallback stay
   unauthenticated (static assets only). See
   `docs/superpowers/specs/2026-07-04-flatpak-feasibility-research.md`.
4. **GUI stack: Qt6/QML + Kirigami**, replacing the currently-built Tauri
   shell + vendored web frontend. Bazzite's default desktop is KDE Plasma,
   not GNOME, so neither the original GTK4/libadwaita spec nor the
   already-built Tauri shell is native there. `cxx-qt` is the accepted
   Rust↔Qt binding (pre-1.0, KDAB-maintained; issue #770 confirmed not to
   bite this repo's `tests/*` convention — see `crates/kirigami-spike`).
   This is a large, still-in-progress rewrite (`docs/superpowers/plans/
   2026-07-04-kirigami-shell-rewrite.md`); the Tauri crate remains in the
   tree during the migration, not as the long-term target.
5. **opensnitchd's shipped default is fail-open (`DefaultAction: allow`)**,
   confirmed from `vendor/opensnitch/daemon/data/default-config.json:18`.
   Snitchwatch's own shipped config must override this to `deny` — don't
   assume the vendored default is what ships.
6. **Component B's atomic-baseline problem** is solved by delegating to
   `rpm-ostree status/db diff`, `rpm -V`, and OSTree's content-addressed
   store (not reimplemented manifest diffing), via a 5-step classification
   tree in `docs/superpowers/specs/2026-07-04-scanner-baseline-design.md`.
   Component B's privileged-tier tool choices (chkrootkit over rkhunter,
   `/proc/cmdline` vs `rpm-ostree kargs`, module classification) are in
   `docs/superpowers/specs/2026-07-04-scanner-privileged-tier-design.md`.

## Where to look next

- `HANDOFF.md` — current overall status and next-step pointer.
- `IMPLEMENTATION_PROMPT.md` — phased, branch-by-branch plan for
  outstanding work; work phases in the stated order, they have real
  dependencies on each other.
- `.agent_native/agent_roadmap.md` — this audit's prioritized findings on
  making the repo more autonomous-agent-friendly (CI gap, setup-script
  gaps, etc.) — not architecture, tooling/process improvements.
