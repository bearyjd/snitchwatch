# Handoff — 2026-08-01: first real-hardware GUI session

Read this with `HANDOFF.md` (overall project status). This file covers one
session: 2026-07-31 into 2026-08-01, the first time a human actually
operated the Kirigami shell against a real `opensnitchd` on real hardware.

## The headline

**The app's core function had never worked once.** The bridge sent every
allow/deny verdict without the `operator` field the daemon requires, so
`rule.Deserialize` rejected all of them (`vendor/opensnitch/daemon/rule/
rule.go:85-89`) and every connection silently fell through to
`DefaultAction`. Fixed in PR #16, verified live: the daemon now logs
`Added new rule: allow if dest.host is '<host>'` where it previously
logged `Invalid rule received, applying default action`.

It was found because the owner sat down and clicked a button. No
automated suite caught it, and none could have — see "The verification
gap" below.

## Merged this session

| PR | What |
|----|------|
| #4 | Daemon Health runbook step; diagnosed the cxx-qt build failure |
| #8 | First real-hardware verification results; corrected install docs |
| #9 | Issue #5 — daemon liveness (open Notifications stream = alive) |
| #10 | Issue #7 — install opensnitch from the upstream release RPM |
| #11 | Issue #6 — daemon-reported alerts overlay onto diagnostics |
| #12 | Live Step 6b results |
| #13 | Reverted the eBPF mis-diagnosis (see "Corrections") |
| #16 | **Issue #14 — the verdict operator fix** |
| #21 | GUI never installed a tracing subscriber (pending at time of writing) |

## Open issues, with what's known

- **#15** — tray tooltip renders unsanitized process/host text. Low
  severity, pre-existing. Fix: route both through
  `translator::verdict::sanitize_for_display` (added in #16).
- **#17** — largely retracted; see "Corrections". The one durable
  question: is there any state where all four health checks read green
  while traffic isn't filtered (including the by-design `QueueBypass`
  window)? Needs a deliberate experiment, not an incidental observation.
- **#18** — Allow/Deny is only reachable via a per-row inspector. New
  connections raise the window but never surface the sheet, so decisions
  pile up (294 observed) with no visible affordance. **Needs a design
  call**: modal per connection, a queue with next/previous, or inline
  buttons on pending rows. Batch actions ("allow all from this process")
  are probably necessary either way at that volume.
- **#19** — Traffic tab can never populate. `translator/connection.rs:55`
  hardcodes `bytes_sent: 0` because opensnitch's `Connection` proto
  carries no byte counters. **Needs a product call**: use `Ping.Statistics`
  for an aggregate-only view, or remove the tab until a source exists.
  Note `connection.rs:142` currently *asserts* the zero, encoding the
  empty behavior as correct.
- **#20** — two independent bugs, both root-caused:
  - Collapse arrows work and are overridden. `connections/grouping.rs:402,417`
    ORs the manual toggle with "group has a pending descendant"; with a
    mostly-pending inbox that forces expanded every render. **Needs a
    design call**: suppress auto-expand after an explicit collapse until a
    *new* pending arrives, or replace force-expand with a pending-count
    badge (recommended).
  - Inspector sheet: `openInspector()` runs and calls `inspector.open()`.
    Suspect is a self-referential `implicitHeight` binding in Kirigami's
    own `OverlaySheet.qml:143` (upstream). **Unresolved**: needs one human
    observation of whether a panel visibly appears.

Also unfixed, not yet filed: the privileged scanner's binary path is
hardcoded to `/usr/libexec/snitchwatch-scanner-privileged`
(`snitchwatch-kirigami/src/scanner.rs:16`), so a dev build fails with a
bare exit 127 and no hint that `SNITCHWATCH_SCANNER_BIN` exists. Needs a
preflight check with an actionable message.

## The verification gap (the real lesson)

Four user-facing defects reached real hardware with a fully green suite.
The pattern is consistent: **the test doubles are more capable than the
real daemon.**

- `MockOpensnitchd` pinged unconditionally; the real daemon only pings
  when it has new stats events, so an idle daemon looked dead (#5).
- The mock awaited `ask_rule` with no deadline and never validated the
  returned rule, so `operator: None` read as success (#14).
- Traffic tests feed synthetic rows with non-zero byte counters that the
  real daemon never produces (#19).
- Kirigami tests instantiate QML offscreen and exercise models directly.
  **Not one simulates a click and asserts a resulting state change** —
  which is exactly why #18 and #20 shipped.

The mock is now hardened (real 120s deadline, `Operator.Compile()`-
equivalent validation) and that class is closed. The QML side is not.
Until a test clicks a delegate and asserts the model changed, the suite
proves the parts exist, not that a person can operate them.

## Corrections to prior claims (read before trusting older notes)

Recorded because they were wrong in ways a future reader would inherit:

1. **"Verdict round-trip verified live" (2026-07-31 morning)** — false. An
   ask row and an outgoing verdict were observed; daemon acceptance never
   was, and it was failing the whole time. Corrected in PR #16.
2. **"eBPF incompatible with kernel 6.19" (issue #6)** — false. That
   failure was rootless-container permissions; upstream's "kernel might
   not be compatible" message masks it. Under root, ebpf loads fine on
   6.19. Reverted in PR #13; the shipped config keeps upstream's `ebpf`.
3. **"Interception silently lapsing" (issue #17)** — false. Based on
   grepping `nft list ruleset` for `queue num`; opensnitch emits
   `queue flags bypass to 0`. The table and rules were present throughout.
4. **"The GUI emits no diagnostics" (issue #20)** — false. On KDE Plasma/
   Wayland, Qt routes QML warnings to the **systemd journal**, not process
   stderr. Use `journalctl --user`. Also `/usr/share/qt6/qtlogging.ini`
   sets `*.debug=false` system-wide, silently dropping `console.log`/
   qDebug — use `console.warn` for QML diagnostics on this host.

Common failure mode in all four: inferring from a partial or wrong-place
signal instead of reading the authoritative source. The findings that held
up came from the daemon's own log, the vendored Go source, or the journal.

## Environment notes for the next session

- **Build**: this box's dev shell bind-mounts `/home/user` and
  `/var/home/user` to the same directory without a symlink, so cxx-qt's
  include-prefix check panics if cargo is invoked from mixed path
  spellings. Use one consistently; recover with `cargo clean -p cxx-qt
  -p cxx-qt-lib -p cxx-qt-build -p kirigami-spike -p snitchwatch-kirigami`.
- **Real daemon**: no opensnitch package exists in Fedora/Bazzite repos.
  Use the upstream v1.8.0 release RPM. In a container it needs **root**
  podman (rootless fails nfqueue/conntrack with `operation not permitted`)
  and `rpm -ivh --noscripts` (its `%post` calls `systemctl`).
- **Left running on the owner's box**: `opensnitchd-dev` root container,
  the `snitchwatch-bridge` user service (currently stopped in favor of the
  GUI's in-process bridge), a debug-log drop-in at
  `~/.config/systemd/user/snitchwatch-bridge.service.d/`, and the GUI
  itself. All safe to remove.
- **Never** use `pkill -f` for these processes from an agent shell — the
  pattern matches its own wrapper and kills the launching command. Kill by
  exact PID.

## Suggested next steps

1. Land the design calls on #18 and #20's collapse behavior — both are
   blocked on product decisions, not engineering.
2. Decide #19 (aggregate traffic vs. remove the tab).
3. Add one QML interaction test that clicks a delegate and asserts a model
   change. Cheapest durable fix for the gap that produced #18 and #20.
4. Then re-run the runbook's visual steps with a human present.
