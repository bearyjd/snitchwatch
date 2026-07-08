# GUI stack decision — GTK4/libadwaita vs. Tauri (Component A shell)

**Date:** 2026-07-04
**Status:** **Decided — Option D (Qt6/QML + Kirigami rewrite).** Chosen by
the human owner on 2026-07-04 over Options A/B/C. Rationale: Bazzite's
default desktop is KDE Plasma, not GNOME, and matching that default
natively outweighed the lower-risk paths of keeping/hardening Tauri
(Options A/C) or a GTK4 rewrite that wouldn't actually be native there
either (Option B). Accepted tradeoffs going in: full rewrite of the
~6,939-line vendored frontend, and a less mature Rust↔Qt binding layer
(`cxx-qt`) than `gtk4-rs` would have offered.
**Audience:** The human owner deciding, and whoever implements the choice
afterward.

## Why this doc exists

`AUDIT.md` flagged an unresolved tension: the original brief called for a
GTK4/libadwaita-native GUI (matching Gatepath's desktop stack, explicitly
ruling out Electron/Qt), but what's actually built in this repo is a
**Tauri 2** shell (`crates/snitchwatch-tauri/`) wrapping a vendored
web-tech frontend (`web/`, HTML/CSS/JS, forked from a Little-Snitch-for-
Linux-style UI). This is not something to silently resolve one way or the
other — `IMPLEMENTATION_PROMPT.md` Phase 3 explicitly scopes it as a
stop-and-ask. This doc lays out three options with concrete costs, and
ends with a direct question for the owner to answer.

## What exists today, concretely

- **`crates/snitchwatch-tauri/`** — 891 lines of Rust: `tray.rs` (131
  lines, tray icon state derivation + `tauri::tray::TrayIcon` wiring),
  `notifier.rs` (161 lines, desktop notifications), `wizard.rs` (122
  lines, first-run onboarding flow), `bridge_runtime.rs` (in-process
  bridge lifecycle), `commands.rs` (141 lines, Tauri IPC commands),
  `panic_hook.rs` (crash-log capture), `paths.rs`, `main.rs`. Built on
  `tauri = { version = "2", features = ["tray-icon"] }`.
- **`web/`** — the vendored frontend: `~6,939` lines of vanilla JS across
  `app.js` (1,114), `connections.js` (1,523), `blocklists.js` (1,525),
  `rules.js` (1,415), `traffic.js` (609, uPlot chart binning),
  `selection.js`, `datetime.js`, `localization.js`, `onboarding.js`, plus
  the vendored `uPlot` chart library and per-tab CSS. This is the actual
  Connections/Blocklists/Rules three-tab UI, the inspector pane, the
  pending-decision prompt UI — i.e., the product's entire user-facing
  surface and its differentiation from a bare opensnitch-ui clone.
- **Test coverage riding on the current stack:** `tests/tauri_smoke/`
  (`wizard_branches.spec.ts`, `tray_states.spec.ts`) and
  `tests/web_smoke/` (`loads_index.spec.ts`,
  `round_trips_ask_rule.spec.ts`) — Playwright suites that drive the
  Tauri window and the embedded webview respectively.
- **Packaging assumption already encoded:** `tauri.conf.json` targets
  `deb`, `rpm`, `appimage` bundles; no GTK4 anywhere in the dependency
  tree today.

## The premise worth questioning first

The original brief's justification for GTK4/libadwaita was "native,
matching Gatepath's desktop stack." **Bazzite's default desktop is KDE
Plasma, not GNOME.** libadwaita is GNOME's design language, not KDE's —
running a libadwaita app on stock Bazzite means running a GNOME-styled app
in a KDE session, which is exactly the kind of visual mismatch "go native"
was supposed to prevent. This doesn't make GTK4 automatically wrong (KDE
Plasma renders GTK apps fine, and Flatpak runtimes commonly ship both
`org.gnome.Platform` and `org.kde.Platform` toolkit deps side by side), but
it means **neither stack is "the native one" on Bazzite's actual default
desktop.** Whoever decides this should weigh that the brief's core premise
— "native Bazzite desktop" — doesn't cleanly point to GTK4 the way it
would on, say, stock Fedora Workstation (GNOME). This is a genuine reason
to not treat GTK4 as the default-correct answer, not just a throwaway
aside.

## Option A — Keep Tauri + the vendored web frontend as-is

**What it costs to build (from here):** nothing new — this is the status
quo. Remaining work is what `IMPLEMENTATION_PROMPT.md` Phases 1–2 already
require (WS auth, packaging), not shell work.

**What it gets you:** the ~6,939-line vendored frontend — which is the
actual product differentiation, not incidental UI chrome — stays intact
and already works, already has smoke test coverage, and already speaks
the bridge's WS protocol end to end (`round_trips_ask_rule.spec.ts`
proves the pending-decision flow works today). Zero rework risk.

**Concrete risks:**
- **WebKitGTK CVE surface.** The embedded webview is WebKitGTK. For a
  tool whose entire purpose is making security-relevant network decisions,
  running a full browser engine (and its regularly-CVE'd JS/rendering
  stack) as the thing the user stares at to make those decisions is a
  meaningfully larger attack surface than a native toolkit. This is a real
  cost, not a hypothetical one — WebKitGTK security advisories are
  frequent enough that "keep the webview runtime patched" becomes an
  ongoing maintenance burden distinct from the app's own code.
- **Fullscreen-game focus/raise reliability.** Bazzite is a gaming-focused
  distro. The `AskRule` pending-decision prompt (`tray.rs`'s
  `TrayState::Pending`, the inspector pane in `connections.js`) has to
  reliably surface over a fullscreen exclusive game — that's the moment a
  novel connection needs a decision, often *because* a game just launched
  and phoned home somewhere new. Webview-shell window-raise-over-fullscreen
  behavior is a known weak spot for Tauri/Electron-class apps on Linux
  (compositor-dependent, DE-dependent); this hasn't been tested anywhere
  in this repo today — `tests/tauri_smoke` covers wizard branches and tray
  state derivation, not fullscreen-focus interaction.
- **Tray backend fragility.** `tray.rs` uses Tauri's own
  `tauri::tray::TrayIcon` abstraction (`tray-icon` feature), which itself
  wraps platform tray backends. Linux tray icon support is notoriously
  inconsistent across desktop environments (GNOME needs an extension for
  any tray at all; KDE's SNI/StatusNotifierItem support is generally
  better but still a second-class integration compared to a native
  toolkit's own tray primitives). This is a pre-existing risk in the
  current build, not new to this option — but it stays a risk if this
  option is chosen.
- Doesn't match the brief's literal stack requirement (GTK4/libadwaita),
  though see "premise worth questioning" above.

## Option B — GTK4/libadwaita native rewrite of the shell

**What it costs to build:** this is **not** "swap the window chrome and
keep everything else" — it is a full rebuild of the ~6,939-line frontend
as GTK4 widgets from scratch. The vendored `web/` JS is the Connections
list, the inspector pane, the traffic chart (uPlot), the blocklist
management UI, the rules table, onboarding — none of that is reusable in
a GTK4 rewrite; GTK4 widget trees, GtkListView/ColumnView for the
connection list, a native charting solution in place of uPlot, and a
rebuilt inspector pane all have to be authored new. `crates/snitchwatch-
tauri/`'s 891 lines of shell-wiring Rust (tray, notifications, autostart,
wizard, crash log) are the smaller, more portable part — but they'd still
need re-wiring against GTK4/libadwaita's own tray, notification, and
autostart idioms rather than Tauri's.

**What it gets you:** a toolkit that at least one of Bazzite's two
plausible desktops (GNOME-family, if the user runs a GNOME spin or GNOME
via Flatpak runtime) renders natively; a meaningfully smaller and more
auditable attack surface than an embedded browser engine (no WebKitGTK,
no JS engine, no HTML/CSS rendering pipeline); native GTK4 tray/
notification primitives instead of a cross-platform abstraction layer.

**Concrete risks:**
- Throws away the current product's actual differentiated UX and its
  existing smoke-test coverage (`tests/web_smoke`, `tests/tauri_smoke`),
  and replaces it with an unbuilt, untested UI that has to re-earn parity
  feature-by-feature (three tabs, inspector pane, traffic chart,
  blocklist management, onboarding wizard) before it's even at today's
  baseline.
- As established above, GTK4/libadwaita isn't actually "the native
  toolkit" on Bazzite's default KDE Plasma desktop either — so this option
  trades a real, working product for a rewrite that doesn't even
  unambiguously deliver on the "native" premise that motivates it.
- Meaningfully larger scope and timeline than either other option — this
  is the only option that requires building new UI, not just hardening or
  keeping existing UI.

## Option C — Keep Tauri, invest in hardening it (not previously considered in `AUDIT.md`)

**What it costs to build:** incremental, scoped work on top of the
existing stack rather than a rewrite: explicit fullscreen-focus/raise
testing and fixes (extend `tests/tauri_smoke` with a scenario that
launches a fullscreen test window and verifies the `AskRule` prompt still
surfaces), evaluate/harden the tray backend choice for KDE's SNI vs.
GNOME's extension-gated tray (possibly documenting a "tray unavailable,
use the window" fallback rather than assuming tray always works),
consider WebKitGTK sandboxing flags or a stricter CSP (`tauri.conf.json`
currently sets `"csp": null` — tightening this is cheap and directly
reduces the WebKitGTK risk surface without touching the frontend code).

**What it gets you:** keeps the ~6,939-line frontend and its test coverage
intact (same benefit as Option A) while directly addressing the two
concrete risk categories (fullscreen reliability, CSP hardening) that are
otherwise just accepted risk under Option A.

**Concrete risks:** doesn't reduce the WebKitGTK CVE surface itself (that
requires not using a webview at all, i.e., Option B) — it only reduces
what's exposed within that surface (CSP) and hardens the parts most likely
to actually break for Bazzite's use case (fullscreen focus). Still doesn't
match the brief's literal GTK4/libadwaita requirement.

## Option D — Qt6/QML with Kirigami (KDE-native rewrite)

Not in the original brief (which said "not Qt") and not considered in the
first draft of this doc — added after the human owner asked "is there a
better choice, Qt, that's actually KDE-native?" during review. Worth taking
seriously precisely because of the "premise worth questioning" section
above: if Bazzite's flagship/primary variant is KDE Plasma (not GNOME), and
"native" is the actual goal, Qt6+Kirigami is the toolkit that delivers on
that goal — Kirigami is KDE's own design-language framework, the direct
counterpart to libadwaita, just pointed at the desktop Bazzite actually
ships by default. The original brief's "not Qt" rule reads as inherited
from a GNOME-native assumption (matching "Gatepath's stack") that this
doc's own KDE-default finding already undermines.

**What it costs to build:** the same order of cost as Option B — this is a
full rewrite, not "swap the shell." The vendored `web/` frontend
(~6,939 lines) is not reusable in Qt/QML any more than it would be in
GTK4; the Connections list, inspector pane, traffic chart, blocklist
management, and onboarding wizard all get rebuilt against QML views
(`ListView`/`TableView` for connections, `QtCharts` or `QtQuick`-based
charting in place of uPlot). `crates/snitchwatch-tauri/`'s 891 lines of
shell-wiring Rust (tray, notifications, autostart, wizard, crash log) would
be re-wired against Qt/Kirigami idioms instead of Tauri's or GTK4's.

**What it gets you, beyond what Option B offers:**
- Actually native on Bazzite's default (Plasma) desktop, not "native to a
  toolkit Bazzite doesn't default to" — the exact gap Option B has.
- Plasma's `KStatusNotifierItem` tray integration is materially more
  mature and reliable than GTK's tray story (GNOME needs an extension for
  any tray at all; see the "tray backend fragility" risk under Option A).
  This directly resolves a risk that persists under every other option.
- Same attack-surface benefit as Option B: no embedded webview, no
  WebKitGTK, no JS engine.

**Concrete risks, specific to this option:**
- **Rust↔Qt binding maturity is a real, distinct risk.** `gtk4-rs` (used by
  Option B) is a mature, first-class binding maintained by the gtk-rs
  project with wide adoption. The Rust↔Qt story (`cxx-qt`, used by some
  KDE/Plasma Mobile projects) is meaningfully less battle-tested — smaller
  community, fewer examples, more edge cases to debug firsthand. This is
  not a free upgrade over Option B; it trades "more native" for "less
  proven tooling."
- No prior Qt/QML/Kirigami work exists anywhere in this repo or its
  history — unlike Option B, which at least benefits from GTK's more
  common presence in the broader Linux Rust ecosystem, this is starting
  from zero on both the toolkit and the binding layer.
- Same rewrite-scope and timeline cost as Option B — this is not a
  cheaper alternative to a rewrite, it's a different rewrite target.
- If the GNOME variant of Bazzite is also a real target (not just the
  Plasma flagship), this option reintroduces the exact "not native on the
  other desktop" problem it's meant to solve, just with GNOME and KDE
  swapped relative to Option B.

## The ask

This is a real decision for you to make, not a recommendation we're
making on your behalf:

- **Option A** — ship the current Tauri + vendored web frontend as-is,
  accept the WebKitGTK/fullscreen/tray risks as known tradeoffs.
- **Option B** — commit to a GTK4/libadwaita rewrite of the shell,
  understanding this discards the vendored frontend's ~6,939 lines and
  its existing test coverage, and re-derives the entire UI from scratch,
  for a toolkit that (per the KDE-default point above) isn't actually
  "native" on Bazzite's default desktop either.
- **Option C** — keep Tauri, but explicitly fund hardening work
  (fullscreen-focus testing, tray backend fallback, CSP tightening)
  before further feature work on the shell.
- **Option D** — commit to a Qt6/QML+Kirigami rewrite, understanding this
  is the same rewrite cost as Option B, on a less mature Rust binding
  layer, in exchange for actually matching Bazzite's default (Plasma)
  desktop rather than GNOME's.
- **Something else** — if none of these fit, say so; this doc doesn't
  assume the four options above are exhaustive.

Which one do you want, and should the choice be revisited later (e.g., if
Option A or C is chosen now but user feedback later shows the WebKitGTK/
fullscreen risk is unacceptable in practice)?
