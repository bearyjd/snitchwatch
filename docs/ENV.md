# Environment variables

The variables the program code and shipped units read, plus the notable build, release and
test knobs, with the file and line that reads each (line numbers are for commit 606083d2;
re-run the greps if they drift). Not exhaustive: Cargo-provided `CARGO_*` variables, GitHub
Actions context variables (`GITHUB_*`, `RUNNER_TEMP`) and generic shell variables are not
listed one by one. Never
put a secret value in this file, in a unit, or in a shell history: the bridge
handshake token is a generated file, not a variable (see `docs/RUNBOOK.md`).

Required column: "no" means the code has a default or tolerates absence.

## Runtime: bridge (`snitchwatch-bridge-cli`)

Help text source: `crates/snitchwatch-bridge-cli/src/cli.rs:61-83`.

<!-- AUTO-GENERATED: crates/snitchwatch-bridge-cli/src/{config,main,storage,activation,logging}.rs, crates/snitchwatch-bridge/src/auth.rs -->
| Variable | Read at | Required | Purpose / values / default |
|---|---|---|---|
| `SNITCHWATCH_GRPC_BIND` | `crates/snitchwatch-bridge-cli/src/config.rs:40` | no | gRPC bind address (user mode). Default `127.0.0.1:0`; must parse as a socket address or startup fails (`config.rs:41-43`). |
| `SNITCHWATCH_WS_SOCKET` | `crates/snitchwatch-bridge-cli/src/config.rs:45` | no | WS Unix socket path. Default `$XDG_RUNTIME_DIR/snitchwatch/bridge.sock`. |
| `SNITCHWATCH_SYSTEM_BRIDGE` | `crates/snitchwatch-bridge-cli/src/main.rs:89` | no | Exactly `1` selects system mode (strict systemd socket activation); any other set value is an error (`main.rs:93`); unset selects user mode. |
| `STATE_DIRECTORY` | `crates/snitchwatch-bridge-cli/src/storage.rs:146` | no | Set by systemd `StateDirectory=`; wins over `SNITCHWATCH_STATE_DIR`; empty counts as unset (`storage.rs:166-168`). System mode accepts only `/var/lib/snitchwatch` (`cli.rs:71-73`). |
| `SNITCHWATCH_STATE_DIR` | `crates/snitchwatch-bridge-cli/src/storage.rs:147` | no | Used when `STATE_DIRECTORY` is unset. With neither, blocklists and profiles are kept in memory only. |
| `XDG_RUNTIME_DIR` | `crates/snitchwatch-bridge/src/auth.rs:90` | no | Base of the `snitchwatch/` socket and token directory. If unset, falls back to `<OS temp dir>/snitchwatch`. |
| `LISTEN_PID`, `LISTEN_FDS`, `LISTEN_FDNAMES` | `crates/snitchwatch-bridge-cli/src/activation.rs:32-34` | system mode only | systemd socket-activation protocol; missing values are an error (`.context("... missing")`). Set by systemd, not by hand. |
| `RUST_LOG` | `crates/snitchwatch-bridge-cli/src/logging.rs:13` | no | `tracing` EnvFilter; default `info` when unset or unparsable. Shipped user unit sets `RUST_LOG=info` (`packaging/systemd/snitchwatch-bridge.service:40`). |
| `PATH` | `crates/snitchwatch-bridge/src/diagnostics/kernel_probe.rs:19` | no | Searched for `nft` by the kernel probe; unset means "not found". (`scanner-core/src/inspector.rs:127` does the same lookup for `SystemInspector::which`.) |
<!-- /AUTO-GENERATED -->

Variables fixed by the shipped units (not read from your shell):

| Unit | Sets | Source |
|---|---|---|
| user unit | `SNITCHWATCH_GRPC_BIND=127.0.0.1:50051`, `SNITCHWATCH_WS_SOCKET=%t/snitchwatch/bridge.sock`, `RUST_LOG=info` | `packaging/systemd/snitchwatch-bridge.service:33,39,40` |
| system unit | `SNITCHWATCH_SYSTEM_BRIDGE=1`, `SNITCHWATCH_WS_SOCKET=/run/snitchwatch/bridge.sock`, `SNITCHWATCH_WS_TOKEN_PATH=/run/snitchwatch-auth/token`, `HOME=/var/lib/snitchwatch`, `XDG_STATE_HOME=/var/lib` | `packaging/system/snitchwatch-system-bridge.service:20-24` |
| system Flatpak | `SNITCHWATCH_SYSTEM_BRIDGE=1` (`--env=`) | `packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml:79` |

## Runtime: GUI (`snitchwatch-kirigami`)

<!-- AUTO-GENERATED: crates/snitchwatch-kirigami/src/*.rs -->
| Variable | Read at | Required | Purpose / values / default |
|---|---|---|---|
| `SNITCHWATCH_WS_SOCKET` | `crates/snitchwatch-kirigami/src/bridge_runtime.rs:268` | no | Client socket override. |
| `SNITCHWATCH_SYSTEM_BRIDGE` | `.../bridge_runtime.rs:269` | no | Exactly `1` selects `/run/snitchwatch/bridge.sock`; otherwise `$XDG_RUNTIME_DIR/snitchwatch/bridge.sock`. |
| `SNITCHWATCH_WS_TOKEN_PATH` | `.../bridge_runtime.rs:287` | no | Token file override; custom sockets default to a sibling token, the system socket to `/run/snitchwatch-auth/token` (`cli.rs:81-83`). |
| `SNITCHWATCH_LOG` | `.../logging.rs:110` | no | EnvFilter for the GUI log; when set to a non-blank value, also writes `gui.log` in the state dir (`logging.rs:21-28,66-69`). Takes priority over `RUST_LOG`. A blank or whitespace-only value counts as unset (`logging.rs:60-76`). |
| `RUST_LOG` | `.../logging.rs:111` | no | Used if `SNITCHWATCH_LOG` is unset or blank; no file log. Default `info`; an unparsable filter falls back to `info`. |
| `SNITCHWATCH_SCANNER_BIN` | `.../scanner.rs:124` | no | Scanner binary for the native transport; default `scanner_broker::SCANNER`. Ignored inside a Flatpak (`/.flatpak-info` selects the broker, `scanner.rs:120`). |
| `SNITCHWATCH_GEOIP_DB` | `.../geo/resolver.rs:423` | no | Explicit country DB path; highest priority, even when stale (`resolver.rs:432-435`). |
| `SNITCHWATCH_GEOIP_CITY_DB` | `.../geo/resolver.rs:436` | no | Explicit city DB path (ignored if `SNITCHWATCH_GEOIP_DB` is set). |
| `HOME` | `.../paths.rs:133`, `.../app_icons.rs:408` | no | Base for per-user paths. In `paths.rs`, unset or empty falls back to a checked per-user root under `/tmp` (`paths.rs:121-127`); `app_icons.rs` has no such fallback and simply skips the HOME-derived dirs (`app_icons.rs:408,417`). |
| `XDG_STATE_HOME` | `.../paths.rs:14` | no | State dir (crash log, `gui.log`); default under `HOME`. |
| `XDG_DATA_HOME` | `.../paths.rs:18`, `.../geo/resolver.rs:425`, `.../geo/install.rs:34`, `.../app_icons.rs:413` | no | Data dir (GeoIP databases, `.desktop` icon lookup). |
| `XDG_CONFIG_HOME` | `.../paths.rs:27,31,39,44,50` | no | Config dir (settings, autostart file, learned data). |
| `XDG_DATA_DIRS` | `.../app_icons.rs:418` | no | `.desktop` search dirs; default `/usr/local/share:/usr/share`. |
| `XDG_RUNTIME_DIR` | `crates/snitchwatch-bridge/src/auth.rs:90` via `.../bridge_runtime.rs:270` | no | Base of the GUI's default socket path (`<dir>/snitchwatch/bridge.sock`). |
| `QT_QPA_PLATFORM` | `.../main.rs:46` | no | Only compared with `offscreen` to decide whether to default `QT_QUICK_CONTROLS_STYLE`. |
| `QT_QUICK_CONTROLS_STYLE` | `.../main.rs:45` | no | Defaulted to `Basic` only when `QT_QPA_PLATFORM=offscreen` and it is unset (`main.rs:45-48`). |
<!-- /AUTO-GENERATED -->

## Debug flags (GUI)

| Variable | Read at | Rule |
|---|---|---|
| `SNITCHWATCH_DEMO` | `crates/snitchwatch-kirigami/src/demo.rs:24` | Exactly `1` enables demo mode (fake rows, bridge never started); anything else, including `true`, is off (`demo.rs:18-20`). |
| `SNITCHWATCH_SCREENSHOT_DIR` | `.../demo.rs:33` | Honoured only with `SNITCHWATCH_DEMO=1` and a non-empty value (`demo.rs:27-29`). |
| `SNITCHWATCH_TEXT_TRACE` | `.../bridge_bindings.rs:61` | Exactly `1` turns the on-screen text change log on; `""`, `0`, `true`, `yes` are off (test at `bridge_bindings.rs:71-76`). |

## Scanner (`scanner-cli`)

<!-- AUTO-GENERATED: crates/scanner-cli/src/{lib,main}.rs -->
| Variable | Read at | Required | Purpose |
|---|---|---|---|
| `HOME` | `crates/scanner-cli/src/lib.rs:31` | yes | User whose surfaces are scanned; exit 2 with "HOME is not set" if absent. |
| `SNITCHWATCH_SCANNER_STATE_DIR` | `crates/scanner-cli/src/lib.rs:35` | no | Directory for `scans.db`; overrides the next two. |
| `XDG_STATE_HOME` | `crates/scanner-cli/src/lib.rs:37` | no | `$XDG_STATE_HOME/snitchwatch-scanner`; else `$HOME/.local/state/snitchwatch-scanner`. |
| `RUST_LOG` | `crates/scanner-cli/src/main.rs:28` | no | EnvFilter, default `info`, to stderr. |
<!-- /AUTO-GENERATED -->

## Build and release pins

`packaging/release/pins.env` is the single source of truth (plain `KEY=value`
lines, sourced by the justfile and scripts).

<!-- AUTO-GENERATED: packaging/release/pins.env, packaging/release/build-bridge.sh, justfile -->
| Variable | Where | Value / meaning |
|---|---|---|
| `SW_BUILDER_IMAGE` | `pins.env:1` | `registry.fedoraproject.org/fedora:45@sha256:ea0998f4...`; must equal `container.image` in `release.yml` (`docs/packaging/bridge-release-artifact.md` "Maintaining the pins"). |
| `SW_RUST_VERSION` | `pins.env:2` | `1.99.1`; the build dies if the builder's rustc differs (`build-bridge.sh:103`). |
| `SW_PROTOC_VERSION` | `pins.env:3` | `33.5` (`build-bridge.sh:108`). |
| `SW_ARCH` | `pins.env:4` | `x86_64`. |
| `SW_OPENSNITCH_COMMIT` | `pins.env:5` | Must equal the `vendor/opensnitch` gitlink and checked-out HEAD (`build-bridge.sh:119,123`). |
| `SW_OPENSNITCH_TAG` | `pins.env:6` | `v1.8.0`; recorded as `opensnitch_tag` in the MANIFEST (`bridge_artifact.py:311`), not compared with the gitlink. |
| `RUSTC_WRAPPER`, `RUSTC`, `CARGO_BUILD_RUSTC(_WRAPPER)`, `CARGO_BUILD_RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_{LINKER,RUSTFLAGS}`, `RUSTC_WORKSPACE_WRAPPER` | `build-bridge.sh:41-43,91-93` | Always unset by the build (logged if they were set) so the MANIFEST's build command stays exact. |
| `TMPDIR` | `packaging/scanner/stage.sh:20` | Parent of the staging snapshot dir; default `/tmp`. |
| `SW_RELEASE_TAG` | `build-bridge.sh:171`, justfile `release-bridge` | Non-empty means release mode: must be `v<crate version>`, clean tree, repo-root `LICENSE`. |
| `ALLOW_DIRTY` | `build-bridge.sh:181` | `1` allows a dirty tree (dev only; recorded as `dirty` in the MANIFEST). |
| `SW_GIT_SHA` | `build-bridge.sh:160` | Commit being built; default HEAD, must equal HEAD. CI sets it in the "Resolve the commit to build" step (`release.yml:99`). |
| `SW_INSTALL_DEPS` | `build-bridge.sh:247` | `1` runs `install-deps.sh` first. |
| `SRC_DIR`, `OUT_DIR`, `CARGO_HOME`, `CARGO_TARGET_DIR` | `build-bridge.sh:9-14,76-81` | Defaults: derived from script path, `$SRC_DIR/dist`, `$HOME/.cargo`, `/tmp/sw-target`. |
| `SW_THROWAWAY_CONTAINER` | `check-install.sh:41` | Must be `1`; guard that the install check runs only in a throwaway container. |
| `SW_UNIT_FILE` | `verify-unit.sh:27` | Unit file to verify; default `packaging/systemd/snitchwatch-bridge.service`. |
| `SW_VERIFY_DNF_CACHE` | `packaging/endpoint-events/verify.sh:57` | Optional podman volume name for the dnf cache. |
| `SNITCHWATCH_BUILD_COMMIT` | `crates/snitchwatch-kirigami/build.rs:238` | Fallback commit id for the About page when git is unavailable; used only if it passes `is_commit_id`. |
<!-- /AUTO-GENERATED -->

## Tests and CI

| Variable | Read / set at | Purpose |
|---|---|---|
| `QT_QPA_PLATFORM=offscreen`, `QT_QUICK_CONTROLS_STYLE=Basic` | `crates/snitchwatch-kirigami/tests/smoke.rs:28-32` (set if unset); `.github/workflows/ci.yml` kirigami job; justfile `qml-test` | Headless Qt. Default Kirigami style hangs offscreen (`main.rs:40-44` comment). |
| `QT_FORCE_STDERR_LOGGING=1` | justfile `qml-test`; `ci.yml` qmltestrunner step | QML test output to stderr. |
| `QT_QUICK_BACKEND=software` | justfile `kirigami-screenshots`; `crates/snitchwatch-kirigami/tests/native-tray/run.sh:46,57` | Software Qt Quick renderer for the screenshot recipe and the native tray fixture. |
| `QT_LOGGING_RULES`, `QML_XHR_ALLOW_FILE_READ/WRITE` | `crates/snitchwatch-kirigami/tests/geo_qml.rs:29`, `crates/snitchwatch-kirigami/tests/status_pages_keyboard_qml.rs:219`, `crates/snitchwatch-kirigami/tests/level_rule_unavailable_once_qml.rs:33` | Set inside individual probes. |
| `CI` | `crates/snitchwatch-kirigami/src/notification_signals/bus_tests.rs:33,156`; `ci.yml` kirigami job (`CI: "true"`) | When set, missing `dbus-daemon`/`xdg-dbus-proxy` fails the private-bus tests instead of skipping. |
| `SNITCHWATCH_MOCK_ASK_RULE_DEADLINE_MS` | `tests/mock_opensnitchd/src/lib.rs:75` | Shrinks the mock's AskRule deadline (clamped; cannot exceed the real 120 s). Read once per process. |
| `SNITCHWATCH_DBIP_PROOF`, `SNITCHWATCH_DBIP_CITY_PROOF` | `crates/snitchwatch-kirigami/src/geo/install/tests.rs:158,540`; `crates/snitchwatch-kirigami/tests/geo_validation_worker.rs:177` | Path to a real DB-IP file for proof tests; the tests `expect` it, so leave them unset in normal runs. |
| `XDG_RUNTIME_DIR`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `SNITCHWATCH_WS_SOCKET`, `SNITCHWATCH_WS_TOKEN_PATH` | set to tempdirs by many `crates/snitchwatch-kirigami/tests/*_qml.rs` (e.g. `tray_lifecycle_qml.rs:33-35`, `learning_end_qml.rs:37`) | Test isolation; see `docs/CONTRIBUTING.md` "Test safety". |
| `SNITCHWATCH_TEST_*`, `*_CHILD` | `scanner-privileged/src/{command,containment,main}.rs`, `snitchwatch-bridge/src/history/tests.rs:502`, others | Internal re-exec markers for child-process tests; not user-facing. |
| `SW_STEPS` | `crates/snitchwatch-kirigami/tests/support/probe_runner.rs:64` | QML probe step limit; default `1000000`. |
| `SNITCHWATCH_TRAY_TEST_SDK` | `crates/snitchwatch-kirigami/tests/native-tray/run.sh:58` | Flatpak SDK for the native tray fixture; default `org.kde.Sdk//6.11`. |
| `SNITCHWATCH_WEB_BASE`, `SNITCHWATCH_TAURI_BASE` | `tests/web_smoke/playwright.config.ts:11`, `tests/tauri_smoke/playwright.config.ts:10` | Playwright base URL; default `http://127.0.0.1:3031`. |
| `SNITCHWATCH_MOCK_DAEMON_RUNNING` | `tests/tauri_smoke/tests/wizard_branches.spec.ts:26`, `tray_states.spec.ts:25` | When unset, those specs skip (they need `mock_opensnitchd` on `127.0.0.1:50051`). |
| `R16_1_CANDIDATE_REVIEWED_SHA` | repository variable (`vars.`), `.github/workflows/candidate-gui.yml` | Owner-set full SHA authorising the candidate build and publish. Not a secret; the owner sets it (see `docs/RUNBOOK.md`). |
| `XDG_{DATA,CONFIG,CACHE}_HOME` | `.github/workflows/candidate-gui.yml:53-55` | Job-local dirs under `runner.temp`. |

No `.env.example` exists in the repo and no `.env` file is read by the code.
