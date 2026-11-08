# Plan: one GUI instance at a time (snitchwatch-kirigami)

Date: 2026-11-07. Crate: `crates/snitchwatch-kirigami` (the Tauri shell is being replaced; out of scope).
Status: DRAFT, awaiting owner confirm. Owner decisions already taken (2026-11-07): a second
launch **raises the running window** then exits; mechanism is a **D-Bus name** (zbus, already a
dependency); **one instance per app id**.
Evidence: read-only map of main c9807ebe; claims not read from a file are marked **UNVERIFIED**.

## Facts that shape the design
- Both Flatpak flavours use the **same app id** `org.snitchwatch.Snitchwatch`
  (`packaging/flatpak/org.snitchwatch.Snitchwatch.yml:37`, `...system.yml:42`), so "per app id" is
  in effect one GUI across flavours, matching what Flatpak itself allows.
- Nothing enforces it today. Autostart (`src/autostart.rs:31`, `~/.config/autostart/snitchwatch.desktop`)
  plus a manual launch is exactly the double-instance case. Closing the window hides to the tray
  (`qml/main.qml:729-732`), so the process lives on with no visible window.
- Startup order (`src/main.rs`): `geo::install::validation_worker_entry()` (:23) first, then
  `logging::init` (:31), `panic_hook::install` (:34), `app_icons::prewarm` (:38, spawns a thread),
  `bridge_runtime::ensure_started` (:57-65, skipped in demo mode) = first real side effect, then
  `application::new()` (QApplication), engine load (:76), `exec` (:82), shutdown flush (:85, :90, exit :92).
  **Safe guard point: after :34, before :38/:57**, and after :23 (the geo worker re-execs the binary).
- Raising a window already works: `raiseAndActivate()` (`main.qml:670-675`) fed by
  `Connections { target: notificationController; onReviewRequested }` (:680-684); the signal is
  emitted via `qt_thread.queue(...)` (`notification_controller.rs:115-120, 470-480`).
  `Labs.SystemTrayIcon` registers its own `org.kde.StatusNotifierItem-<pid>-<n>` name, so it does not
  collide with the app-id name.
- zbus 5 (lock 5.14.0, plus 4.4.0 from another crate); the GUI has no long-lived bus connection and
  registers no names/objects (`Connection::session()` is created per notice at
  `notification_controller.rs:470`). `scanner-broker` is the in-repo precedent for owning a name.
- Flatpak grants ownership of the app's own id by default (no `--own-name` line exists today).
  **UNVERIFIED** empirically.

## Design
New module `src/single_instance.rs`, pure of Qt, called from `main.rs` right after the panic hook.

1. **Skip (no guard, run normally)** when `demo::enabled()` (`SNITCHWATCH_DEMO`), or when
   `SNITCHWATCH_ALLOW_MULTIPLE=1` (new, documented in `docs/ENV.md`; for developers running two
   builds), or when no session bus is reachable (CI, `XDG_RUNTIME_DIR` unset). **Fail open**: the
   guard may only make the process exit on a *positive* result.
2. Connect with `zbus::blocking::Connection::session()`. **Register the object first, then request
   the name.** Object `/org/snitchwatch/Snitchwatch`, interface `org.freedesktop.Application`
   (`Activate(a{sv})`, `Open(as, a{sv})`, `ActivateAction(s, av, a{sv})`). `Open` and `ActivateAction`
   only raise the window: their URIs and action names are ignored (no parsing, nothing to inject).
   `request_name` with `DoNotQueue`, never `ReplaceExisting`.
3. Result of the name request:
   - primary owner: keep the connection for the process lifetime; `Activate` calls arriving before
     QML is ready set a pending flag that is delivered once the window exists (no lost raise);
   - name already owned: call `Activate` on the owner (timeout 2 s, one retry after 500 ms). Success
     => exit 0. Failure or timeout (owner wedged, or mid-shutdown) => **do not vanish**: log an error
     and run as an ordinary instance (documented duplicate), so a hung first instance never makes the
     app impossible to start.
4. Raise hand-off: add a `raiseRequested` signal next to `reviewRequested` on `NotificationController`
   (it owns the Qt thread handle, `notification_controller.rs`) and one `Connections` handler in
   `main.qml` calling `raiseAndActivate()`. Wayland focus-stealing prevention may refuse
   `requestActivate()` (comment at `main.qml:665-669`); passing an activation token is best-effort and
   **UNVERIFIED**, not required for the feature.
5. Release the name at the **start of shutdown** (before `learned::flush_and_wait`, up to 3 s, and
   `close_all_open`), so a launch during shutdown starts fresh instead of activating a dying process.

## Tests (TDD, failing-first, private buses only)
Patterned on `notification_signals/bus_tests.rs` (private `dbus-daemon`, never the real session bus):
- first guard becomes primary; a second guard against the same bus gets `Forwarded` and the owner
  sees exactly one `Activate`;
- no bus reachable => skipped, run continues; `SNITCHWATCH_DEMO` and `SNITCHWATCH_ALLOW_MULTIPLE` => skipped;
- owner holds the name but never answers => the second instance times out and runs anyway (bounded);
- `Activate` before the window is ready is delivered once after;
- dropping the primary releases the name, so a new launch becomes primary (shutdown path).
`just kirigami-screenshots` runs the real binary with `SNITCHWATCH_DEMO=1` and a fresh `XDG_RUNTIME_DIR`
but does not unset `DBUS_SESSION_BUS_ADDRESS`; the demo skip is what keeps it off the developer's real
bus. `tests/geo_validation_worker.rs` exits at :23, before the guard.
The Qt side (`raiseRequested` -> `raiseAndActivate`) needs a headless QML test
(`QT_QPA_PLATFORM=offscreen`); whether Kirigami tests run in this sandbox is **UNVERIFIED**
(CLAUDE.md), so the first build is exploratory.

## Docs
`README.md` (Autostart ~:164-168, Kirigami shell ~:189-210), `docs/CONTRIBUTING.md` (:47, :135),
`docs/ENV.md` (new variable), `docs/RUNBOOK.md` (a single-instance row).

## Risks and open points
- Any process on the same session bus can call `Activate`: the only effect is raising the window of
  the same user's app (no data, no actions). Other Flatpaks cannot reach the name without a
  `--talk-name` grant. A security review of the D-Bus surface is part of the process.
- `DBusActivatable=true` (so the desktop can start the app via D-Bus) is **out of scope**; it would
  need a service file and is a separate decision.
- A stale name cannot persist across a crash: the bus releases a name when its owner's connection
  drops, so no cleanup is needed (**UNVERIFIED** under Flatpak's `xdg-dbus-proxy`; check manually).
- If the owner is alive but its Qt event loop is blocked, the second launch waits ~2.5 s and then
  opens a duplicate; accepted.

## Process
One PR, branch `feat/single-instance`, built by an executor (opus), cargo only on a throwaway `HOME`
and outside `/tmp` (the tmpfs filled up earlier). Review: independent Opus `security-reviewer` +
`code-reviewer` on the exact head; 6/6 CI (the `kirigami` job is the one that compiles this crate).
A real-Flatpak check on the tower (second launch raises, kill -9 then relaunch works) is a manual
step after merge.
