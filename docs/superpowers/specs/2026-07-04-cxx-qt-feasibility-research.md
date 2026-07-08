# `cxx-qt` feasibility research — de-risking before the Phase 3a spike

**Date:** 2026-07-04
**Status:** Research complete. Recommendation below feeds the go/no-go call
on `IMPLEMENTATION_PROMPT.md` Phase 3a (hands-on `cxx-qt` spike) before
Phase 3b (the real Qt6/QML+Kirigami rewrite) starts.
**Audience:** Whoever runs the Phase 3a spike, and the human owner who
picked Option D in `docs/superpowers/specs/2026-07-04-gui-stack-decision.md`.

## Summary

`cxx-qt` is real, actively maintained by a credible Qt consultancy (KDAB),
has an official KDE-sanctioned Rust+Kirigami tutorial, and directly
supports the specific pattern this project needs (async Tokio work
emitting Qt signals into QML). It is **not** a dead-end or vaporware. It is
also genuinely pre-1.0, has at least one build-system rough edge that
collides with this repo's own existing test-layout convention, and the
Connections-table use case needs its more advanced (not simplest)
QML-model API. Net: this clears the bar for running the Phase 3a spike,
but the spike should be scoped to specifically prove out the harder edges
below, not just a toy "click a button" demo.

## 1. Maintainer and activity health

- **Maintainer confirmed: KDAB** (Klarälvdalens Datakonsult AB), a Qt/C++
  consultancy with a long track record in the Qt ecosystem — this is not
  a hobby project maintained by one anonymous contributor.
  ([kdab.com/cxx-qt](https://www.kdab.com/cxx-qt/))
- **Repo health (as of 2026-07-04, via GitHub API):** 1,510 stars, 104
  forks, 30 distinct contributors, last push 2026-07-03 (one day before
  this research). Not stale.
- **Issue backlog:** 303 closed issues vs. 71 open (issue-only count via
  GitHub search API; the repo's raw `open_issues_count` of 100 includes
  open PRs). Recently closed issues include same-day turnaround — a
  "master doesn't build, Qt installation not found" issue was opened and
  closed on the same day (2026-07-03). This is a healthy, responsive
  backlog, not an abandoned queue.
- **Release cadence (from `CHANGELOG.md` + commit history):** 0.7.0
  (2024-10-30) → 0.7.1 (2025-03-04) → 0.7.2 (2025-04-28) → 0.8.0
  (2025-12-18) → 0.8.1 (2026-02-16) → 0.9.0 (2026-06-23) → 0.9.1
  (2026-07-03, confirmed via the actual version-bump commit). Cadence has
  been accelerating, not slowing.
- **Production usage — weaker signal than the above.** I could not find a
  shipped, flagship KDE application (Itinerary, Plasma Mobile shell, etc.)
  that uses `cxx-qt` to expose Rust `QObject`s to QML in production today.
  What KDE apps use Rust for currently (e.g., Angelfish's ad-blocker
  crate) appears to be self-contained Rust logic consumed via plain
  crates, not full `cxx-qt` QObject/QML exposure. The strongest concrete
  precedent is KDE's own **official developer documentation** hosting a
  complete Rust+Kirigami tutorial app (see §3) — real and official, but a
  tutorial, not a shipped flagship app. **Read this as: credible backing
  and real official endorsement, but thinner production-scale evidence
  than a "used by everyone" claim would imply.**

## 2. Does it support what Phase 3b needs?

**Properties, signals, invokable methods — yes, first-class and
documented.** `#[qobject]` + `#[qproperty(...)]` exposes Rust struct
fields as QML-bindable properties; `#[qinvokable]` makes Rust methods
callable from QML; `#[qsignal]` declares signals. This is the documented,
default path (confirmed via the CXX-Qt book and the KDE `simplemdviewer`
tutorial in §3).

**Async Tokio work emitting signals into QML — yes, this is a supported,
documented pattern, not just theoretically possible.** Per KDAB's own
[discussion #805](https://github.com/KDAB/cxx-qt/discussions/805):

1. Grab a `CxxQtThread` handle via `self.qt_thread()` from Qt-thread
   context (e.g., inside a `qinvokable` or constructor).
2. `tokio::spawn()` the async work; `CxxQtThread` is `Send`, so it can be
   moved into the spawned task.
3. When the async work needs to update QML-visible state, call
   `qt_thread.queue(|qobject| { ... })` — this queues a closure to run
   back on the Qt event-loop thread, where property mutation and signal
   emission are safe.

This is exactly the shape this project needs: the bridge's Tokio-driven WS
state (new connections, pending `AskRule` prompts, tray state) would live
in a background task that periodically/eventfully calls `qt_thread.queue`
to push updates into Qt-owned properties/signals. **Important constraint
to design around:** generated `QObject`s are explicitly **neither `Send`
nor `Sync`** — only the `CxxQtThread` handle is. The bridge's async runtime
must hold onto the `CxxQtThread` handle (obtained once, on the Qt thread,
at startup) and route all cross-thread updates through `.queue()`, never
attempt to touch a `QObject` directly from a spawned Tokio task.

**One real limitation, acknowledged by KDAB themselves:** true Rust
`Future`↔QML integration (e.g., via `cxx-async`, or returning something
QML could `await`) is **not** implemented — QML/Qt's event model isn't
async-native, and the maintainers say so directly in #805. The
`queue`-a-closure pattern above is the only supported mechanism; it's
sufficient for this project's "background event → push UI update" shape,
but don't expect anything more sophisticated (e.g., a QML-visible async
call chain) to be supported.

## 3. Kirigami specifically

**Direct, official precedent exists.** KDE's own developer documentation
(`develop.kde.org`) hosts ["A full Rust + Kirigami
application"](https://develop.kde.org/docs/getting-started/rust/rust-app/)
— a complete tutorial app (`simplemdviewer`) with a Rust `QObject`
(`MdConverter`) exposing `#[qproperty]`/`#[qinvokable]` members, consumed
by a Kirigami-based QML UI, packaged with desktop files, icons, and
AppStream metainfo. This is KDE's own documentation, not a third-party
blog post. A second independent precedent exists:
[antroids/cxx-qt-kirigami](https://github.com/antroids/cxx-qt-kirigami), a
minimal Rust+`cxx-qt`+Kirigami2/Qt6 project template with KI18n
dependencies wired in.

**Why there's no reported friction:** Kirigami is a QtQuick components
library — it operates entirely within QML, above the `QObject`/QtCore
layer where `cxx-qt` does its work. A Rust-exposed `QObject` is consumed
identically whether the QML file uses plain `QtQuick.Controls` or
Kirigami's `Kirigami.ApplicationWindow`/`Kirigami.Page` etc. — Kirigami
doesn't change how properties/signals/invokables cross the Rust↔QML
boundary. No source found (official docs, KDAB discussions, or issue
tracker) reporting Kirigami-specific incompatibilities with `cxx-qt`.

## 4. Build system implications

**`cxx-qt-build`'s `build.rs` codegen is real, documented, and Cargo-native**
— `CxxQtBuilder` generates and compiles the C++ glue at build time, driven
from a crate's own `build.rs`, the same mechanism `snitchwatch-proto`
already uses for `tonic-build` and `snitchwatch-tauri` uses for its own
build script. Cargo build scripts are inherently crate-scoped — each
crate's `build.rs` only affects that crate's own compilation — so there is
no structural reason a `cxx-qt`-using crate's `build.rs` would conflict
with the unrelated `build.rs` scripts already in this workspace. **I found
no specific report of cross-crate build.rs conflicts within a single
workspace**, but this is architectural reasoning from how Cargo works
generally, not a `cxx-qt`-specific confirmation — worth a quick sanity
check early in the spike (build the new crate alongside the existing
workspace members and confirm `cargo build --workspace` still works
cleanly), not just assumed.

**Real, concrete friction found: integration tests.** [Issue
#770](https://github.com/KDAB/cxx-qt/issues/770) — adding Rust integration
tests (the `tests/*.rs` directory pattern) to a `cxx-qt`-using crate
produces linker errors (`undefined reference to
'cxxbridge1$MyObject$number'`) because the separate integration-test
binary doesn't automatically link against the `build.rs`-generated C++
code. **Status: open, unresolved, no official workaround documented** as
of this research; the issue author's own suggested mitigation is moving
test code into a separate crate. **This directly matters for this repo**
— the existing convention (`tests/bridge_protocol_test.rs`,
`tests/mock_opensnitchd`, `tests/tauri_smoke`, `tests/web_smoke` at the
workspace root) is exactly the `tests/*` integration-test pattern this
issue describes. The Phase 3a spike should explicitly test whether this
bites the new crate, and if so, plan for cxx-qt-dependent tests to live as
in-crate unit tests (or a wrapper crate) rather than the workspace-root
`tests/` convention used elsewhere in this repo.

**Flatpak:** no `cxx-qt`-specific Flatpak precedent found (positive or
negative). Rust+Cargo apps in Flatpak generally require the known,
solved-but-extra-step workaround of vendoring dependencies via
`flatpak-cargo-generator.py` (from `flatpak-builder-tools`) since
`flatpak-builder`'s sandboxed build has no network access mid-build — this
is a general Rust-in-Flatpak cost, not new to `cxx-qt`. What's genuinely
untested (no precedent found) is the combination of that Cargo-vendoring
step **plus** needing system Qt6/KDE Frameworks/`extra-cmake-modules`
present via the `org.kde.Platform`/`org.kde.Sdk` Flatpak runtimes — the
KDE-runtime side is extremely well-trodden for ordinary (C++) KDE apps,
but "a Cargo-built Rust binary linking against it via `cxx-qt-build`
inside a Flatpak sandbox" has no found prior art either way. This is an
open unknown, not a known problem — flag it as something the Phase 3a
spike (or Phase 2's packaging phase) needs to prove out directly rather
than assume.

## 5. Known rough edges / dealbreaker check

- **Pre-1.0, API still churning.** [Issue
  #555](https://github.com/KDAB/cxx-qt/issues/555) is KDAB's own tracking
  issue for a 1.0 API redesign — it proposes changing how types, `#[qproperty]`,
  and `#[qsignal]` are declared, explicitly moving closer to raw `cxx`
  syntax. This confirms the project is still pre-1.0 and further breaking
  changes are expected before a stable release. Pin the exact version used
  in the spike and expect a migration step later, not a "set it and forget
  it" dependency.
- **`QAbstractListModel`-derived custom base classes — supported, but the
  more advanced API path, not the simple property path.** The Connections
  table is exactly this use case (a live, growing/shrinking list of rows).
  `cxx-qt` supports subclassing `QAbstractListModel` via `#[base =
  QAbstractListModel]` in the `extern "RustQt"` block, with a documented
  `custom_base_class` example — but using such a model as a QML-exposed
  property requires raw-pointer handling (QML expects a raw C++ pointer,
  e.g. `CustomBaseClass*`), a step up in complexity from plain
  `#[qproperty]` scalar fields. Not a blocker, but the highest-complexity
  single piece of Phase 3b, and worth exercising directly in the Phase 3a
  spike rather than discovering the friction mid-rewrite.
- **Threading constraint, not a footgun if designed around up front**
  (see §2): generated `QObject`s are neither `Send` nor `Sync`; only
  `CxxQtThread` crosses threads. This has to be an explicit part of the
  bridge's async architecture from day one of Phase 3b, not retrofitted.
- **No dealbreaker found.** Nothing in the maintainer's own issue tracker,
  discussions, or KDE's community documentation suggests a fundamental
  incompatibility with what this project needs. The risks found are real
  but scoped and known (pre-1.0 churn, the integration-test linking issue,
  list-model complexity, unverified Flatpak combination) — none of them
  contradict the choice of Option D itself; they're implementation risks
  to manage, not evidence the decision should be reopened.

## Recommendation

**Proceed to the Phase 3a spike — with caution, not a clean unconditional
go.** The fundamentals (properties/signals/invokables, the
async-Tokio-to-QML signal pattern via `CxxQtThread`, and a real official
Kirigami precedent) are solid enough that a hands-on spike is very likely
to succeed at proving basic feasibility. But scope the spike to
specifically exercise the parts this research flagged as unproven for
*this* project, not a generic "hello world" QML window:

1. Prove the async-signal pattern end-to-end: a Tokio task pushing a
   simulated WS event into a QML-visible property/signal via
   `CxxQtThread::queue`.
2. Prove a `QAbstractListModel`-backed list (even a toy 3-row list)
   exposed to a QML `ListView` — this is the Connections-table
   precursor and the single most complex API surface identified.
3. Confirm `cargo build --workspace` still works cleanly with the new
   `cxx-qt`-using crate alongside the existing `snitchwatch-proto` and
   `snitchwatch-tauri` build scripts.
4. Deliberately try adding a workspace-root-style integration test
   (matching this repo's `tests/*` convention) against the spike crate
   to confirm or rule out issue #770's linker error in this exact setup —
   this determines whether Phase 3b needs a different test-layout
   convention for the new shell crate than the rest of the repo uses.

If all four hold up, Phase 3b can proceed with confidence. If #4 confirms
the integration-test linking problem, that's a scoping note for Phase 3b
(tests live differently for this crate), not a reason to abandon Option D.

## Sources

- [KDAB — Safe Rust Bindings for Qt (cxx-qt)](https://www.kdab.com/cxx-qt/)
- [KDAB/cxx-qt GitHub repository](https://github.com/KDAB/cxx-qt)
- [KDAB/cxx-qt CHANGELOG.md](https://github.com/KDAB/cxx-qt/blob/main/CHANGELOG.md)
- [CXX-Qt + async Rust discussion #805](https://github.com/KDAB/cxx-qt/discussions/805)
- [Stabilizing 1.0 API — issue #555](https://github.com/KDAB/cxx-qt/issues/555)
- [Integration test linker error — issue #770](https://github.com/KDAB/cxx-qt/issues/770)
- [CXX-Qt Documentation book](https://kdab.github.io/cxx-qt/book/)
- [KDE Developer docs — A full Rust + Kirigami application](https://develop.kde.org/docs/getting-started/rust/rust-app/)
- [KDE Developer docs — Kirigami with Rust](https://develop.kde.org/docs/getting-started/kirigami/setup-rust/)
- [antroids/cxx-qt-kirigami template](https://github.com/antroids/cxx-qt-kirigami)
- [KDE Community Wiki — Rust](https://community.kde.org/Rust)
- GitHub public API queries against `KDAB/cxx-qt` (stars/forks/contributors/
  issue counts/commit history), run 2026-07-04.
