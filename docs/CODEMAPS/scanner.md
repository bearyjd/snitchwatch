<!-- Generated: 2026-11-06 | Files scanned: 30 | Token estimate: ~900 -->
# Scanner (Component B)

Goal: detect drift on rpm-ostree/atomic Bazzite by delegating to `rpm-ostree`, `rpm -V`, ostree (not reimplementing manifests). Specs: `docs/superpowers/specs/2026-07-04-scanner-baseline-design.md`, `...-scanner-privileged-tier-design.md`.

## scanner-core (userspace tier, lib, 19 files)
| Item | Where |
|---|---|
| `SystemInspector` trait (all shell-outs/file reads); `RealSystem` | `crates/scanner-core/src/inspector.rs:44,67` |
| `MockInspector` double (no production callers) | `src/testkit.rs` |
| `Scanner::run` (orchestrator over `Vec<Box<dyn Check>>`) | `src/scanner.rs:37,73` |
| `Check` trait (`name`, `surface`) | `src/checks/mod.rs:100` |
| Checks: flatpak, listeners (`ss`), shell_rc, systemd_user; outbound = deferred stub | `src/checks/*.rs` |
| 5-step expected-drift-vs-anomaly `classify(PathFacts)` | `src/classify.rs:80` |
| `rpmverify.rs`, `provenance.rs`, `deployment.rs` | `src/` |
| `ScanStore` (new / outstanding / resolved diff), `BaselineStore` (TOFU per surface) | `src/stores/scans.rs:67`, `src/stores/baseline.rs:64` |
| `Anomaly`, `ReportedFinding`, `ScanReport` | `src/finding.rs:9,45,55` |

## scanner-cli (bin)
`ScannerConfig` `crates/scanner-cli/src/lib.rs:15`; `run` :51, `run_with` :56 (injectable inspector), `report_to_json` :89. Degrades to `None` deployment on non-atomic hosts.

## scanner-privileged (root, one-shot bin + lib)
- Entry `src/main.rs` (`--db`, `--json`; default db `/var/lib/snitchwatch-scanner/scans.db` :21); `scan::run_privileged_scan` `src/scan.rs:33`.
- `SystemFacts` trait (testable host reads), `LiveSystem` impl: `src/facts.rs:21,64`.
- Checks (`src/checks/`): `kargs` (`/proc/cmdline` vs `rpm-ostree kargs`), `lockdown`, `modules`, `rootkit` (wraps `chkrootkit`, skipped if absent).
- `command.rs` (1767 lines): bounded subprocess runner `run` :425, `trusted_program` :388 (only `/usr/bin`, `/usr/sbin`, :344), output caps :10-11, and the process-group watchdog `expire_watchdog` :78.
- `containment.rs` (1120 lines): subreaper + descendant sweep `kill_descendants` :229 via pidfd.
- `store.rs:92` own `ScanStore` (adds `check_type` column); requires private root-owned db dirs.
- Invariant: never a daemon; no listener (`src/lib.rs` header).

## scanner-broker (unprivileged session-bus adapter)
`crates/scanner-broker/src/lib.rs`: bus name `org.snitchwatch.Scanner` :19, interface `org.snitchwatch.Scanner1` :21, method `run_deep_scan` :248, `request_scan` :279 (client). Fixed pkexec argv to `/usr/libexec/snitchwatch-scanner-privileged` :22; method takes no exe/path/args/env. Limits: report 4 MiB :23, timeout 600 s :25; `validate_report` :62 checks JSON before presenting. Overlap rejected; idle shutdown claim `claim_idle_shutdown` :235. Activation: `packaging/scanner/org.snitchwatch.Scanner.service`.

## Privilege model
Native: GUI `scanner.rs` -> `pkexec scanner-privileged` (polkit action in `packaging/polkit/org.snitchwatch.scanner.policy`; id constant in scanner-privileged `lib.rs`). Flatpak: GUI -> broker over session bus -> pkexec on host. Watchdog timeout is reported via exact stderr marker (`scanner-broker/src/lib.rs:28`) plus SIGKILL.
Decision: chkrootkit not layered on Bazzite (owner decision 2026-10-28), check degrades when missing.
