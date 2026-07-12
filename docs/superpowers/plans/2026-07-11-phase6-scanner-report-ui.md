# Phase 6 — Kirigami report UI for the privileged-tier scanner

**Date:** 2026-07-11
**Spec:** `docs/superpowers/specs/2026-07-04-scanner-privileged-tier-design.md`
**Prior work:** `docs/superpowers/plans/2026-07-05-phase6-scanner-privileged.md`
(scanned/persisted the data; explicitly left the UI out of scope pending
Phase 3b)

## Why now

`IMPLEMENTATION_PROMPT.md` Phase 6 gated this UI on Phase 3b (the Kirigami
shell) shipping first — "only buildable once Phase 3b has shipped ... do not
start this UI work while Phase 3b is still in progress." Phase 3b is done
(feature parity + Task 7's fullscreen-focus safety test passed, per
`docs/superpowers/plans/2026-07-04-kirigami-shell-rewrite.md`'s status
note), so this is now unblocked.

## Scope

A new Kirigami page that triggers a privileged scan and renders its report,
sharing the shell's existing design system — nothing else. Explicitly NOT
in scope: changes to `scanner-privileged`'s scan logic, store schema, or the
polkit policy (all already correct); a live daemon/hardware round trip
(`chkrootkit`/`rpm-ostree`/`mokutil` aren't installed here — same "not
verifiable in this sandbox" boundary the scanner-privileged plan already
documented).

## Design

Mirrors this crate's existing plain-Rust-module + cxx-qt-bridge-module split
(e.g. `wizard.rs` / `wizard_controller.rs`):

- **`src/scanner.rs`** (plain Rust, Qt-free, unit-testable): resolves
  `pkexec` via `which`, shells out to the privileged binary
  (`/usr/libexec/snitchwatch-scanner-privileged` by default — matches the
  polkit policy's `exec.path` annotation — overridable via
  `SNITCHWATCH_SCANNER_BIN` for dev/manual runs, same override-env-var
  convention `bridge_runtime.rs` uses for `SNITCHWATCH_GRPC_BIND`) with
  `--json`, and returns the raw JSON stdout on exit 0/2 (2 means "new
  anomalies found" per the binary's own contract — still valid output) or an
  error string otherwise. Never panics: missing `pkexec`, missing scanner
  binary, and a denied polkit prompt are all just `Err` strings, since a
  scan is optional/on-demand by design.
- **`src/scanner_controller.rs`** (`#[cxx_qt::bridge]`): `ScannerController`
  QObject with `busy: bool`, `reportJson: QString`, `errorText: QString`
  qproperties and one `runScan()` qinvokable — same
  `qt_thread.queue`/background-thread shape as `SettingsController`.
  `reportJson` is handed to QML as-is (not modeled as Rust structs/qproperies
  per field) since this is a point-in-time report a human reads, not a live
  stream needing incremental model updates the way `ConnectionsModel` does —
  QML parses it with `JSON.parse` and renders sections directly.
- **`qml/ScannerPage.qml`**: `Kirigami.ScrollablePage`, a "Run Deep Scan"
  button (disabled while `busy`), an error `Kirigami.InlineMessage` when
  `errorText` is non-empty, and five sections mirroring
  `scanner-privileged`'s own `print_report`/`print_json` shape: New /
  Still outstanding / Resolved / Informational / Skipped checks.
- Wired into `main.qml` alongside the other controllers/pages: a
  `ScannerController` instance, a `scannerPageComponent`, and a "Security
  Scan" `Kirigami.Action` in the global drawer.
- Test: `tests/scanner_controller_qml.rs`, mirroring `wizard_qml.rs`'s
  scope — confirms the QML type registers, `ScannerPage.qml` compiles
  against a real controller, and `runScan()` doesn't panic when `pkexec`/the
  scanner binary are absent (the sandbox's actual condition — this exercises
  the real "gracefully degrade" path, not a mock).

## Non-goals

- No changes to `scanner-privileged`, `scanner-core`, the polkit policy, or
  the `scans.db` schema.
- No attempt to invoke a real `pkexec` prompt or a real scan in this
  sandbox — no polkit daemon here (documented gap already in the
  scanner-privileged plan). Manual verification on a real Bazzite host is a
  follow-up, same as Phase 2's runbook.
- No packaging changes (the scanner binary's Flatpak/bluebuild placement is
  already specified in the privileged-tier design doc and untouched here).

## Acceptance criteria

- [x] `cargo build -p snitchwatch-kirigami` succeeds with the new files
  wired into `build.rs`.
- [x] `cargo test -p snitchwatch-kirigami` passes including the new
  `scanner_controller_qml.rs` test (239 lib unit tests + 13 integration test
  files, all green; `cargo clippy -p snitchwatch-kirigami --all-targets -D
  warnings` clean; workspace-wide `just check` unaffected).
- [x] `ScannerPage.qml` reachable from the global drawer as "Security Scan".
- [ ] Manual note: real polkit-prompt + real scan round trip needs a real
  Bazzite host; record pass/fail there separately (not blocking this pass).
