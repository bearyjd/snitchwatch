# Runbook

Each row cites its source; where there is none the
text says "Not defined in the repo". Variables: `docs/ENV.md`. Contributor
workflow: `docs/CONTRIBUTING.md`.

**Hard rule: releases (tags, r*-candidate, promotion) are the owner's/tower session's decision.**
Do not push a `v*` tag, push or move `r16.2-candidate` (the published `r16-candidate` and `r16.1-candidate` are never replaced), set
`R16_2_CANDIDATE_REVIEWED_SHA`, publish a draft release, or pin a new release in
an image on your own initiative. (The repo-side mechanics of that gate are
under "Release flow". Source: `HANDOFF.md:183`, "final r16 needs a separate owner go given to the bazzite-tower session".)

## Deployment shapes

| Shape | What it is | Source |
|---|---|---|
| Batteries-included image | Signed custom Bazzite image with `opensnitchd` baked in and enabled; built with the `bluebuild` CLI | `packaging/bluebuild/recipe.yml`; `packaging/README.md` "The two install paths"; build: `bluebuild build packaging/bluebuild/recipe.yml` (`packaging/README.md` "Build") |
| rpm-ostree layering (retired) | The DIY per-user path is gone (#35); the page is a pointer to `stage.sh` + the overlay. The system bridge is the only shipped flavour | `docs/packaging/rpm-ostree-layering.md` |
| Flatpak GUI | `packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml`; no `--share=network`; reaches the system bridge over its Unix socket | `packaging/README.md`; `README.md` "Install on Bazzite" |
| System-bridge overlay | Socket-activated system service for the `snitchwatch` account, root-only daemon socket; the only shipped flavour | `packaging/README.md` "System-bridge overlay"; `docs/packaging/system-bridge-integration.md` |
| Pre-built bridge tarball | Deterministic sha256-pinned tarball for image-baked installs | `docs/packaging/bridge-release-artifact.md` |
| Staging environment | Not defined in the repo | n/a |

Real-hardware verification steps (image build, Flatpak build, live daemon
dial-in, closed-window fail-open, tray states, Daemon Health) are in
`docs/packaging/phase2-manual-verification-runbook.md` steps 1-6.

## Runtime layout

| Item | Dev (not shipped) | System flavour (shipped) | Source |
|---|---|---|---|
| Socket dir | `$XDG_RUNTIME_DIR/snitchwatch/` (dir 0700, files 0600) | `/run/snitchwatch/` | `README.md` "Architecture"; `CLAUDE.md` "Settled" #3; `crates/snitchwatch-bridge/src/auth.rs:92-97` |
| WS socket | `.../bridge.sock` | `/run/snitchwatch/bridge.sock`, root:`snitchwatch-ui` 0660 | `README.md`; `docs/packaging/system-bridge-integration.md:19-23` |
| Handshake token | `.../token`, 0600; sent as first WS text frame on `/stream` | `/run/snitchwatch-auth/token`, mode 0640 | `README.md`; `crates/snitchwatch-bridge-cli/src/main.rs` header; `docs/packaging/system-bridge-integration.md:25-29` |
| Daemon to bridge | per-user Unix socket `$XDG_RUNTIME_DIR/snitchwatch-grpc/opensnitchd.sock` (nothing packaged dials it); loopback TCP only with `SNITCHWATCH_DEV_TCP=1` (not deny-safe, #35) | `/run/snitchwatch/opensnitchd.sock`, root:root 0600; no TCP fallback | `docs/ENV.md`; `system-bridge-integration.md:13-17` |
| Unit | none (run `just run-bridge` by hand) | `snitchwatch-system-bridge.service` + `-grpc.socket` + `-gui.socket` | `packaging/system/` |
| State | `$STATE_DIRECTORY` or `SNITCHWATCH_STATE_DIR`; `blocklists.sqlite3`, `profiles.sqlite3` (0600); none set means in-memory only | `/var/lib/snitchwatch` only | `README.md` "M4"; `cli.rs:78-82` |
| GUI access (system) | `usermod -aG snitchwatch-ui <user>`, then a new login session | same | `system-bridge-integration.md:19-23` |
| Crash log (Kirigami shell) | `$XDG_STATE_HOME/snitchwatch/crash.log` (default `~/.local/state/snitchwatch/crash.log`) | n/a | `README.md` "Crash log"; Kirigami: `src/main.rs:35`, `src/paths.rs:57-59` |
| GUI log (Kirigami) | `gui.log` in the state dir, only when `SNITCHWATCH_LOG` is set; previous run kept as `gui.log.1` | n/a | `crates/snitchwatch-kirigami/src/logging.rs:21-28` |

`DefaultAction` is `allow` as shipped (owner decision 2026-10-25), in both the
system overlay config (`packaging/system/opensnitchd-default-config.json`) and
the bluebuild recipe's identical copy
(`packaging/bluebuild/files/system/etc/opensnitchd/default-config.json`);
`deny` is an operator opt-in, never packaged
(`packaging/README.md`; `CLAUDE.md` "Settled" #5). With no bridge connected the
daemon falls back to its `DefaultAction`
(`docs/packaging/system-bridge-integration.md:160-166`).

## Release flow (documented mechanics)

These rows describe what the repo's scripts and workflows do. Running any of
them for a real release, tag or candidate is the owner's/tower session's
decision (hard rule above).

| Step | Source |
|---|---|
| Pins in `packaging/release/pins.env`; builder digest must match `release.yml` `container.image` (`release_shape.rs` fails otherwise) | `bridge-release-artifact.md` "Maintaining the pins" |
| Local: `SW_RELEASE_TAG=vX.Y.Z just release-bridge-repro`, then `just release-verify <tarball> <sha256>` with the printed `SHA256=` | `bridge-release-artifact.md` "Cutting a release" 2 |
| `release.yml` runs on `v*` tags: rebuild, reproducibility check, install check, RustSec gate, then (environment `release`) refuse if the tag moved or a release exists, attest, create a **draft** release only | `.github/workflows/release.yml` header; `bridge-release-artifact.md` "Cutting a release" 3 |
| `workflow_dispatch` of `Release` builds and verifies without publishing | `release.yml:6-9`; `bridge-release-artifact.md` |
| Maintainer compares the draft's sha256 with the local build, runs the attestation check, publishes the draft, pins the sha256 in the image | `bridge-release-artifact.md` "Cutting a release" 4 |
| Candidate GUI: `candidate-gui.yml` triggers only on a push to `r16.2-candidate`. Build runs only if repo variable `R16_2_CANDIDATE_REVIEWED_SHA` equals the pushed SHA, else a `register` job holds. Publisher needs green exact-head `ci.yml`, validates the packet, creates the lightweight tag `r16.2-candidate` if missing and a draft, uploads assets, and then **automatically makes the prerelease public** (`draft: false, prerelease: true, make_latest: 'false'`), so after the owner sets the variable, the push itself publishes with no further workflow step; it never overwrites assets and never becomes "latest" | `.github/workflows/candidate-gui.yml:4-30,245,259-260`; `docs/packaging/r13-candidate-gui-ci.md` |
| The workflow refuses an existing release or mismatched tag and does not overwrite assets; a failure after draft creation (`candidate-gui.yml:247`) leaves a draft and HOLD, earlier failures leave nothing; a full rerun builds a new candidate needing fresh acceptance | `r13-candidate-gui-ci.md` |

The file `docs/packaging/r13-candidate-gui-ci.md` is titled "r16.2 system GUI
candidate CI"; the filename is historical.

## Health checks that exist

The repo defines no HTTP health endpoint, metrics endpoint or `status`
subcommand. `snitchwatch-bridge-cli` accepts only `--help`/`--version` and ignores every
other argument (`crates/snitchwatch-bridge-cli/src/cli.rs:49-60`).

| Check | Command or place | Source |
|---|---|---|
| Bridge CLI usage | `snitchwatch-bridge-cli --help` / `--version` (safe: exits before any socket or token) | `cli.rs:49`; `main.rs:1-10` |
| Daemon service and its configured dial target | `systemctl status opensnitch.service` (should be enabled + active); `sudo jq .Server.Address /etc/opensnitchd/default-config.json`; `sudo jq .DefaultAction /etc/opensnitchd/default-config.json` | `phase2-manual-verification-runbook.md:56`; `system-bridge-integration.md:226-227` |
| Bridge running and system sockets | `systemctl status snitchwatch-system-bridge-grpc.socket snitchwatch-system-bridge.service`; `ls -l /run/snitchwatch/` | `system-bridge-integration.md:173-175` |
| Bridge logs | `journalctl -b -u snitchwatch-system-bridge.service -u snitchwatch-system-bridge-grpc.socket` | `system-bridge-integration.md:176` |
| Bridge up without GUI | closing the GUI window must not stop `snitchwatch-system-bridge.service` | `system-bridge-integration.md` |
| Daemon Health page (GUI drawer entry) | four checks: `opensnitchd reachable`, `firewall running`, `eBPF support`, `nftables support`; pass = the status summary reads `Daemon reachable; firewall checks passed` and all four check rows show healthy; absence of the banner is not a pass (`daemon_health_model.rs:12-17,48-70`, `qml/main.qml:442`) | `crates/snitchwatch-kirigami/qml/GlobalDrawer.qml:167`; `phase2-manual-verification-runbook.md:310-329` |
| Blocklist "Rule installed" is not proof the daemon loaded the hosts | `sudo grep "domains loaded" /var/log/opensnitchd.log` | `README.md` "M4" |
| Scanner (userspace) | `scanner-cli` prints JSON; exit 0 clean, 1 actionable anomalies, 2 could not run | `crates/scanner-cli/src/main.rs:12-15` |
| Scanner (privileged) | binary `snitchwatch-scanner-privileged`, broker `snitchwatch-scanner-broker`; no usage text read in this pass | `crates/scanner-privileged/Cargo.toml:17-18`; `crates/scanner-broker/Cargo.toml:7-8` |

## Scanner (privileged) `file_drift`: real-host checklist

Manual, owner-run, not blocking merge (same boundary as the Phase 6 plan). The
check runs `rpm -Va --nomtime --noscripts --nodeps` inside the existing on-demand
polkit-gated scan. Rule 2 compares each failing path with the booted commit
(`ostree ls -d -C <commit> <path>` vs `ostree checksum <path>`), so a healthy scan
lists no `file_drift_base_tree` under `skipped`; it appears only when `ostree` or a
valid booted commit is missing. `file_drift_unowned` is always listed: unowned files
under `/usr` are not enumerated. Plans:
`docs/superpowers/plans/2026-11-19-privileged-file-drift.md`,
`docs/superpowers/plans/2026-11-21-file-drift-base-tree-primitive.md`.

`scripts/file-drift-evidence.sh` (read-only, run as root on the host) captures the evidence for
Q1-Q3 below into one directory; bring back its `summary.txt`.

- [ ] Run a scan on a Bazzite host via the broker (`busctl --user call
  org.snitchwatch.Scanner ... RunDeepScan`, as in `HANDOFF.md:90`).
- [ ] Record the `rpm -Va --nomtime --noscripts --nodeps` wall time, output size and
  exit code with drift in 0, 1 and 2+ packages. This pins the exit-code
  semantics the runner assumes (any non-zero normal exit with at least one
  parseable verify line counts as drift). Compare the wall time with
  `FILE_DRIFT_BUDGET` (180 s, provisional; Q2).
- [ ] Record the real format of `rpm-ostree status --json` `packages` /
  `requested-packages` entries: plain names or NEVRAs.
  Q8 closed as label-only: parse reads packages + requested-packages + requested-local-packages (union); real layered entry format not live-verified (no layered image available); a mislabel shows base_pkg instead of layered_pkg, detection is unaffected.
- [ ] Confirm an untouched image produces no flood of `T`/`M`/`missing` lines and no
  `file_drift_unparsed` or aggregate `/` finding (Q3: is `--nomtime` enough, or do
  `--excludedocs`/`%ghost`/`/usr/etc` paths also flood?).
- [ ] Layer a package, edit one of its files, confirm exactly one anomaly naming
  `layered_pkg:<name>`; restore the file and confirm it resolves on the next scan.
- [ ] Confirm the report `skipped` list shows `file_drift_unowned` and neither
  `file_drift_base_tree` nor `file_drift_incomplete` nor `file_drift`.
- [ ] Base-tree pass (plan 2026-11-21 Task 6), on the stock image via `RunDeepScan`: no
  aggregate `/` finding; the anomaly count is the host evidence `differ` + `absent` +
  `missing+present` total; `/usr/etc/shells`-style xattr-only cases appear with both
  checksums in the detail. Record total `file_drift` wall time (`rpm -Va` about 39 s plus
  about 10.6 s of lookups measured) against the 180 s budget.
- [x] Confirmed on the host's `ostree` 2026.4 (read-only, 2026-11-21): `ostree ls -d -C <commit>
  /usr/local/no-such-file-xyz` exits 1 with empty stdout and stderr `error: Inspecting path
  '/usr/local/no-such-file-xyz': No such file or directory: /usr/local/no-such-file-xyz`; a path
  below a file (`/usr/bin/bash/x`) gives `... Not a directory`. Identical to the pinned texts
  (trailing newline not byte-verified on the host); a different text would fail closed.
- [ ] Confirm `/usr/bin/sudoreplay` (mode `--x--x--x`) matches when scanned as root.
- [ ] `ostree admin unlock` (or the equivalent dev overlay) on a test host, append to a base
  file, confirm one `base-tree mismatch` anomaly; revert and reboot, confirm it resolves.
  Delete one base file in the overlay and expect `missing (present in base commit)`.
- [ ] Open (plan 2026-11-21 Q8): the layered-entry format of `rpm-ostree status --json`
  `packages` / `requested-packages` is still unverified; layer one package, record the
  entries, edit one of its files and expect one `layered_pkg:` anomaly with both checksums.
- [ ] Capture stderr of a plain `rpm -Va --nomtime --noscripts --nodeps` on a clean image.
  The runner rejects any non-zero `rpm -Va` run that wrote to stderr (it cannot tell a
  partial run from a full one), so routine rpmdb notices would skip `file_drift` on every
  scan. If stderr is not empty, decide on an allowlist of known-benign lines.

## Common issues

Items below are the ones the repo documents.

| Symptom | Cause / fix | Source |
|---|---|---|
| Running `snitchwatch-bridge-cli` (or `just run-bridge`) replaced a running bridge's socket and token | It starts a bridge. Use only `--help`/`--version`, or an isolated `XDG_RUNTIME_DIR` | `CLAUDE.md`; issue #34 |
| System flavour: socket missing, desktop has no UI; under a `deny` opt-in the network may be closed | Console recovery in order: look, `sudo systemd-tmpfiles --create snitchwatch.conf`, `reset-failed` + start both sockets, **then** `sudo systemctl restart opensnitch.service` (required) | `system-bridge-integration.md:158-193` |
| Bridge cannot be restored under `deny` | Set `DefaultAction` back to `allow` with the `jq` snippet, restart `opensnitch.service` | `system-bridge-integration.md:201-214` |
| Crash-looping bridge | `StartLimitIntervalSec=0`, retries every 2 s; needs the journal and a fix | `system-bridge-integration.md:195-199` |
| Flatpak GUI cannot see the bridge | A successful TCP connection from inside the sandbox means `--share=network` leaked; `/run/snitchwatch/bridge.sock` should be visible (once the system bridge is running, to a member of `snitchwatch-ui`) | `phase2-manual-verification-runbook.md:81-96`; `packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml:73` |
| Host still has a per-user unit from an older install; the bridge fails with a `SNITCHWATCH_GRPC_BIND ... dev-only` error | Remove it: `systemctl --user disable --now snitchwatch-bridge.service`; delete `~/.config/systemd/user/snitchwatch-bridge.service` (and `.service.d/` drop-ins), `systemctl --user daemon-reload`; uninstall the old per-user Flatpak profile (`flatpak --user uninstall org.snitchwatch.Snitchwatch`); if `/etc/opensnitchd/default-config.json` has `Server.Address: 127.0.0.1:50051`, restore the image's `unix:opensnitchd.sock` | #35; `docs/packaging/system-bridge-integration.md` |
| Rootless daemon fails with a misleading "kernel might not be compatible" | Shipped config uses `ProcMonitorMethod: ebpf`, which loads under a root daemon; a rootless run fails with that message (issue #6) | `phase2-manual-verification-runbook.md:145-147` |
| Launching Snitchwatch only brings the existing window to the front, no second window | Intended: one GUI per session (D-Bus name `org.snitchwatch.Snitchwatch`). If the running copy is wedged, a launch waits about 4.5 s and then starts a second copy (logged as an error). `SNITCHWATCH_ALLOW_MULTIPLE=1` skips the check | `crates/snitchwatch-kirigami/src/single_instance.rs` |
| Reboot into a new image | Read "Before rebooting into the image: the /etc merge" | `system-bridge-integration.md:216` |
| Known flaky tests (resolved items) | #259 closed 2026-11-07, fixed by #321; the seven Timer-driven probes #279 listed without a re-entrancy guard were handled in #313 (five guarded, two needed none) | `HANDOFF.md:162`; `docs/superpowers/research/2026-11-06-flake-259.md`; PR #313 |

## Rollback

| Item | Status |
|---|---|
| Undo a `deny` opt-in | Documented: see "Bridge cannot be restored under `deny`" above |
| `rpm-ostree rollback`, rebasing the image back, un-layering `opensnitch` | Not defined in the repo for Snitchwatch or `opensnitch` (no match for "rollback" in `docs/packaging/` or `packaging/README.md`). Only the opt-in endpoint-events recipe has its own way back, e.g. `sudo rpm-ostree uninstall rsyslog ...` (`packaging/endpoint-events/README.md:443`) |
| Undoing a published release or candidate | Not defined in the repo; the candidate workflow only forbids overwriting or deleting (`r13-candidate-gui-ci.md`) |

## Alerting and escalation

| Item | Status |
|---|---|
| Alerting, paging, on-call, escalation path | Not defined in the repo |
| Metrics, dashboards, HTTP health endpoint | Not defined in the repo |
| Security reports | See `SECURITY.md` (not summarised here) |
