<!-- Generated: 2026-11-06 | Files scanned: 20 | Token estimate: ~900 -->
# Dependencies

## External processes and services
| What | Used by | Notes |
|---|---|---|
| `opensnitchd` (Go, root) | bridge | dials gRPC `Ui` into the bridge; submodule `vendor/opensnitch` v1.8.0 read-only |
| `rpm-ostree`, `rpm -V`, `ostree`, `flatpak`, `ss` | scanner-core via `SystemInspector` | `inspector.rs` |
| `chkrootkit`, `modinfo`, sysfs, `/proc/cmdline` | scanner-privileged | via `SystemFacts` |
| `pkexec` + polkit | kirigami native path, broker | `packaging/polkit/` |
| session D-Bus (zbus) | broker, kirigami notifications | `org.snitchwatch.Scanner` |
| systemd (socket activation, `StateDirectory=`) | system bridge | `packaging/systemd`, `packaging/system` |
| podman, Fedora 44 pinned image | release build | `packaging/release/pins.env` |
| `rdap.org`, blocklist hosts (HTTPS) | reqwest in kirigami (opt-in RDAP) and bridge fetcher | rustls |
| Qt6 + KDE Frameworks 6 (Kirigami, Qt.labs.platform) | snitchwatch-kirigami | system packages |

## Workspace pins (`Cargo.toml`)
tokio 1.40, tonic/tonic-build 0.12, prost 0.13, axum 0.7 (ws), tokio-tungstenite 0.24, rusqlite 0.32 (bundled), reqwest 0.12 (rustls), zbus 4 (workspace; scanner-broker and kirigami use 5), chrono, sha2, rand 0.8, regex, addr 0.15.

## Per-crate top-level (beyond workspace-common serde/tracing/anyhow)
| Crate | Notable deps |
|---|---|
| snitchwatch-proto | tonic, prost (+tonic-build) |
| snitchwatch-bridge | axum, hyper, hyper-util, tower, tokio-tungstenite, tonic, rusqlite, reqwest, ring, sha2, zbus, rust-embed+mime_guess (feature `web-ui`), addr, async-trait |
| snitchwatch-bridge-cli | bridge (no default features), proto, tonic, libc |
| snitchwatch-tauri | tauri 2 (tray-icon), tauri-plugin-autostart, notify-rust, bridge + bridge-cli |
| snitchwatch-kirigami | cxx =1.0.194, cxx-qt =0.9.1, cxx-qt-lib (qt_full), maxminddb 0.29, dns-lookup 2, notify-rust 4, zbus 5, which, scanner-broker, bridge, proto |
| kirigami-spike / snitchwatch-spike | throwaway probes (cxx-qt; gRPC against proto) |
| scanner-core | rusqlite, sha2, chrono, serde |
| scanner-cli | scanner-core |
| scanner-privileged | libc, rusqlite, chrono |
| scanner-broker | tokio, zbus 5 (tokio), libc |

New or unusual: exact-pinned cxx/cxx-qt (pre-1.0 binding), `dns-lookup` (libc getnameinfo, chosen to avoid an async-DNS crate), `ring`, hand-driven hyper over `UnixListener` (axum 0.7 limitation).

## CI / release / packaging
`.github/workflows/ci.yml` jobs: `check`, `test` (default-members only), `audit`, `package-check` (`just package-check`), `kirigami` (Qt6/KF6 on Ubuntu, headless), `candidate-contract`. `release.yml` (tags `v*`): `build-bridge` -> `audit-bridge` -> `publish` (draft releases only), scripts in `packaging/release/` (reproducible tarball without `web-ui`). `candidate-gui.yml` builds the r16 system GUI Flatpak candidate (`packaging/flatpak/`). Image distribution via bluebuild (`packaging/bluebuild/recipe.yml`). Local entry points: `justfile` (`build`, `check`, `test`, `test-bridge`, `kirigami-build`, `qml-test`, `release-bridge-repro`, `package-check`). Tests: `tests/*.rs` + `tests/mock_opensnitchd` (in-process fake daemon), `tests/web_smoke`, `tests/tauri_smoke` (Playwright).
