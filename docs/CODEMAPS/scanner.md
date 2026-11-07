<!-- Generated: 2026-11-06 | Token estimate: ~840 (o200k_base) -->
# Scanner (Component B)

Detects drift on rpm-ostree/atomic Bazzite by delegating to `rpm-ostree`, `rpm -V`, ostree (no reimplemented manifests). Specs: `docs/superpowers/specs/2026-07-04-scanner-*-design.md`.

## scanner-core (userspace tier, lib, 19 .rs files)
| Item | Where (`crates/scanner-core/src/`) |
|---|---|
| `SystemInspector` trait (shell-outs and file reads); `RealSystem` | `inspector.rs:44,67` |
| `Scanner::run` over `Vec<Box<dyn Check>>`; `Check` trait | `scanner.rs:37,73`; `checks/mod.rs:100` |
| Checks: flatpak, listeners (`ss`), shell_rc, systemd_user; outbound is a deferred stub | `checks/*.rs` |
| 5-step expected-drift-vs-anomaly `classify(PathFacts)` | `classify.rs:80` |
| `ScanStore` (new / outstanding / resolved + userspace per-surface TOFU baseline), `BaselineStore` (privileged hash cache) | `stores/scans.rs:4-13,67,216-245`, `stores/baseline.rs:64` |
| `Anomaly`, `ReportedFinding`, `ScanReport` | `finding.rs:9,45,55` |

## scanner-cli (bin)
`ScannerConfig` `crates/scanner-cli/src/lib.rs:15`; `run` :51, `run_with` :56 (injectable inspector), `report_to_json` :89.

## scanner-privileged (root, one-shot bin + lib)
- `src/main.rs` (`--db`, `--json`; default db `/var/lib/snitchwatch-scanner/scans.db` :21); `scan::run_privileged_scan` `src/scan.rs:33`.
- `SystemFacts` trait, `LiveSystem` impl: `src/facts.rs:21,64`.
- Checks (`src/checks/`): `kargs` (`/proc/cmdline` vs `rpm-ostree kargs`), `lockdown`, `modules`, `rootkit` (`chkrootkit`, skipped if absent).
- `command.rs`: bounded subprocess runner `run` :425, `trusted_program` :388 (only `/usr/bin`, `/usr/sbin`, :344), watchdog `expire_watchdog` :78.
- `containment.rs`: descendant sweep `kill_descendants` :229.
- `store.rs:92` own `ScanStore` (adds `check_type`).
- Invariant: never a daemon (`src/lib.rs` header).

## scanner-broker (unprivileged session-bus adapter)
`crates/scanner-broker/src/lib.rs`: bus `org.snitchwatch.Scanner` :19, interface `org.snitchwatch.Scanner1` :21, `run_deep_scan` :248, client `request_scan` :279. Fixed pkexec argv to `/usr/libexec/snitchwatch-scanner-privileged` :22; the method takes no exe/path/args/env. Limits: report 4 MiB :23, timeout 600 s :25. `validate_report` :62 requires JSON matching the broker's own `Report` (the privileged scanner's output, :34). Overlapping scans rejected; `claim_idle_shutdown` :235.

## Privilege model
Native: GUI `scanner.rs` -> `pkexec scanner-privileged` (polkit `packaging/polkit/org.snitchwatch.scanner.policy`). Flatpak: GUI -> broker over session bus -> pkexec on host. Watchdog timeout is reported by a stderr marker (`scanner-broker lib.rs:28`) plus SIGKILL. chkrootkit is not layered on Bazzite (owner decision 2026-10-28); the check degrades when missing.
