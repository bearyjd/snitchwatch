# Runbook

Each row cites its source; where there is none the
text says "Not defined in the repo". Variables: `docs/ENV.md`. Contributor
workflow: `docs/CONTRIBUTING.md`.

**Hard rule: releases (tags, r*-candidate, promotion) are the owner's/tower session's decision.**
Do not push a `v*` tag, push or move `r16.2-candidate` (the published `r16-candidate` and `r16.1-candidate` are never replaced), set
`R16_2_CANDIDATE_REVIEWED_SHA`, publish a draft release, or pin a new release in
an image on your own initiative. (The repo-side mechanics of that gate are
under "Release flow". Source: `HANDOFF.md:12`, "final r16 needs a separate owner go given to the bazzite-tower session".)

## Deployment shapes

| Shape | What it is | Source |
|---|---|---|
| Batteries-included image | Signed custom Bazzite image with `opensnitchd` baked in and enabled; built with the `bluebuild` CLI | `packaging/bluebuild/recipe.yml`; `packaging/README.md` "The two install paths"; build: `bluebuild build packaging/bluebuild/recipe.yml` (`packaging/README.md` "Build") |
| rpm-ostree layering (alternative) | Layer `opensnitchd` onto stock Bazzite, install the bridge as a `systemd --user` service, GUI as Flatpak | `docs/packaging/rpm-ostree-layering.md` steps 1-5 |
| Flatpak GUI | `packaging/flatpak/org.snitchwatch.Snitchwatch.yml`; no `--share=network`; reaches the bridge via `--filesystem=xdg-run/snitchwatch` | `packaging/README.md`; `README.md` "Install on Bazzite" |
| System-bridge overlay | Socket-activated system service for the `snitchwatch` account, root-only daemon socket; "pending a new release"; uses `org.snitchwatch.Snitchwatch.system.yml`, not coinstallable with the primary profile | `packaging/README.md` "System-bridge overlay"; `docs/packaging/system-bridge-integration.md` |
| Pre-built bridge tarball | Deterministic sha256-pinned tarball for image-baked installs | `docs/packaging/bridge-release-artifact.md` |
| Staging environment | Not defined in the repo | n/a |

Real-hardware verification steps (image build, Flatpak build, live daemon
dial-in, closed-window fail-open, tray states, Daemon Health) are in
`docs/packaging/phase2-manual-verification-runbook.md` steps 1-6.

## Runtime layout

| Item | User flavour | System flavour | Source |
|---|---|---|---|
| Socket dir | `$XDG_RUNTIME_DIR/snitchwatch/` (dir 0700, files 0600) | `/run/snitchwatch/` | `README.md` "Architecture"; `CLAUDE.md` "Settled" #3; `crates/snitchwatch-bridge/src/auth.rs:90` |
| WS socket | `.../bridge.sock` | `/run/snitchwatch/bridge.sock`, root:`snitchwatch-ui` 0660 | `README.md`; `docs/packaging/system-bridge-integration.md:18-22` |
| Handshake token | `.../token`, 0600; sent as first WS text frame on `/stream` | `/run/snitchwatch-auth/token`, mode 0640 | `README.md`; `crates/snitchwatch-bridge-cli/src/main.rs` header; `docs/packaging/system-bridge-integration.md:24-28` |
| Daemon to bridge | gRPC TCP `127.0.0.1:50051` (set by the unit; dev/per-user only, not deny-safe, #35) | `/run/snitchwatch/opensnitchd.sock`, root:root 0600; no TCP fallback | `packaging/systemd/snitchwatch-bridge.service:33`; `system-bridge-integration.md:12-16` |
| Unit | `snitchwatch-bridge.service` (`systemctl --user`) | `snitchwatch-system-bridge.service` + `-grpc.socket` + `-gui.socket` | `packaging/systemd/`, `packaging/system/` |
| State | `$STATE_DIRECTORY` or `SNITCHWATCH_STATE_DIR`; `blocklists.sqlite3`, `profiles.sqlite3` (0600); none set means in-memory only | `/var/lib/snitchwatch` only | `README.md` "M4"; `cli.rs:71-75` |
| GUI access (system) | `usermod -aG snitchwatch-ui <user>`, then a new login session | same | `system-bridge-integration.md:18-22` |
| Crash log (Tauri and Kirigami shells) | `$XDG_STATE_HOME/snitchwatch/crash.log` (default `~/.local/state/snitchwatch/crash.log`) | n/a | `README.md` "Crash log"; Kirigami: `src/main.rs:35`, `src/paths.rs:57-59` |
| GUI log (Kirigami) | `gui.log` in the state dir, only when `SNITCHWATCH_LOG` is set; previous run kept as `gui.log.1` | n/a | `crates/snitchwatch-kirigami/src/logging.rs:21-28` |

`DefaultAction` is `allow` as shipped (owner decision 2026-10-25), in both the
system overlay config (`packaging/system/opensnitchd-default-config.json`) and
the per-user recipe config; `deny` is an operator opt-in, never packaged
(`packaging/README.md`; `CLAUDE.md` "Settled" #5). With no bridge connected the
daemon falls back to its `DefaultAction`
(`docs/packaging/rpm-ostree-layering.md` step 5).

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
| Bridge running (user) | `systemctl --user is-active snitchwatch-bridge.service` | `rpm-ostree-layering.md` step 5 |
| Daemon up and dialing | `systemctl is-active opensnitch.service`; `jq` on `/etc/opensnitchd/default-config.json` (`Server.Address`, `DefaultAction`) | `rpm-ostree-layering.md` step 5 |
| System sockets | `systemctl status snitchwatch-system-bridge-grpc.socket snitchwatch-system-bridge.service`; `ls -l /run/snitchwatch/` | `system-bridge-integration.md:168-172` |
| Bridge logs | `journalctl -b -u snitchwatch-system-bridge.service -u snitchwatch-system-bridge-grpc.socket` | `system-bridge-integration.md:172` |
| Bridge up without GUI | closing the GUI window must not stop `snitchwatch-bridge.service` | `rpm-ostree-layering.md` step 5 |
| Daemon Health page (GUI drawer entry) | four checks: `opensnitchd reachable`, `firewall running`, `eBPF support`, `nftables support`; pass = the status summary reads `Daemon reachable; firewall checks passed` and all four check rows show healthy; absence of the banner is not a pass (`daemon_health_model.rs:12-16,48-65`, `qml/main.qml:440`) | `crates/snitchwatch-kirigami/qml/GlobalDrawer.qml:167`; `phase2-manual-verification-runbook.md:315-335` |
| Blocklist "Rule installed" is not proof the daemon loaded the hosts | `sudo grep "domains loaded" /var/log/opensnitchd.log` | `README.md` "M4" |
| Scanner (userspace) | `scanner-cli` prints JSON; exit 0 clean, 1 actionable anomalies, 2 could not run | `crates/scanner-cli/src/main.rs:12-15` |
| Scanner (privileged) | binary `snitchwatch-scanner-privileged`, broker `snitchwatch-scanner-broker`; no usage text read in this pass | `crates/scanner-privileged/Cargo.toml:17-18`; `crates/scanner-broker/Cargo.toml:7-8` |

## Common issues

Items below are the ones the repo documents.

| Symptom | Cause / fix | Source |
|---|---|---|
| Running `snitchwatch-bridge-cli` (or `just run-bridge`) replaced a running bridge's socket and token | It starts a bridge. Use only `--help`/`--version`, or an isolated `XDG_RUNTIME_DIR` | `CLAUDE.md`; issue #34 |
| System flavour: socket missing, desktop has no UI; under a `deny` opt-in the network may be closed | Console recovery in order: look, `sudo systemd-tmpfiles --create snitchwatch.conf`, `reset-failed` + start both sockets, **then** `sudo systemctl restart opensnitch.service` (required) | `system-bridge-integration.md:154-195` |
| Bridge cannot be restored under `deny` | Set `DefaultAction` back to `allow` with the `jq` snippet, restart `opensnitch.service` | `system-bridge-integration.md:197-210` |
| Crash-looping bridge | `StartLimitIntervalSec=0`, retries every 2 s; needs the journal and a fix | `system-bridge-integration.md:191-195` |
| Flatpak GUI cannot see the bridge | A successful TCP connection from inside the sandbox means `--share=network` leaked; the socket under `xdg-run/snitchwatch` should be visible | `phase2-manual-verification-runbook.md:89-97` |
| Per-user unit shadows the image's unit | Remove `~/.config/systemd/user/snitchwatch-bridge.service` (and drop-ins) | `bridge-release-artifact.md` "Known limitations" |
| Several users: only the first user's bridge binds `127.0.0.1:50051`; any local process can bind it first | Not deny-safe (#35); use the system flavour | `bridge-release-artifact.md` "Known limitations" |
| Rootless daemon fails with a misleading "kernel might not be compatible" | Shipped config uses `ProcMonitorMethod: ebpf`, which loads under a root daemon; a rootless run fails with that message (issue #6) | `phase2-manual-verification-runbook.md:150-152` |
| Playwright "missing browser" | Run `just web-smoke-install` / `just tauri-smoke-install`; not a code regression | `README.md` |
| Launching Snitchwatch only brings the existing window to the front, no second window | Intended: one GUI per session (D-Bus name `org.snitchwatch.Snitchwatch`). If the running copy is wedged, a launch waits about 4.5 s and then starts a second copy (logged as an error). `SNITCHWATCH_ALLOW_MULTIPLE=1` skips the check | `crates/snitchwatch-kirigami/src/single_instance.rs` |
| Reboot into a new image | Read "Before rebooting into the image: the /etc merge" | `system-bridge-integration.md:212` |
| Known flaky tests | #259 open; seven Timer-driven probes lack the re-entrancy guard (#279) | `HANDOFF.md:29` |

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
