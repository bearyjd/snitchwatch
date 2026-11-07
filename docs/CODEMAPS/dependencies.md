<!-- Generated: 2026-11-06 | Token estimate: ~910 (o200k_base) -->
# Dependencies

## External processes and services
| What | Used by | Notes |
|---|---|---|
| `opensnitchd` (Go, root) | bridge | dials gRPC `Ui` into the bridge; submodule `vendor/opensnitch` v1.8.0 read-only |
| `rpm-ostree`, `rpm -V`, `ostree`, `flatpak`, `ss` | scanner-core `SystemInspector` | `inspector.rs` |
| `chkrootkit`, `modinfo`, sysfs, `/proc/cmdline` | scanner-privileged | via `SystemFacts` |
| `pkexec` + polkit | kirigami native path, broker | `packaging/polkit/` |
| session D-Bus (zbus) | broker, kirigami notifications | `org.snitchwatch.Scanner` |
| systemd (socket activation, `StateDirectory=`) | system bridge | `packaging/systemd`, `packaging/system` |
| podman + pinned Fedora 45 image | release build | `packaging/release/pins.env:1` |
| `rdap.org`, blocklist hosts (HTTPS) | kirigami (opt-in RDAP), bridge fetcher | rustls |
| Qt6 + KDE Frameworks 6 | snitchwatch-kirigami | system packages |

## Workspace pins (`Cargo.toml`)
tokio 1.40, tonic/tonic-build 0.12, prost 0.13, axum 0.7 (ws), tokio-tungstenite 0.24, rusqlite 0.32 (bundled), reqwest 0.12 (rustls), zbus 4 (scanner-broker and kirigami use 5), rand 0.8, addr 0.15.

## Per-crate notable deps
| Crate | Deps |
|---|---|
| snitchwatch-proto | tonic, prost (+tonic-build) |
| snitchwatch-bridge | axum, hyper, tower, tokio-tungstenite, tonic, rusqlite, reqwest, ring, zbus, rust-embed (feature `web-ui`) |
| snitchwatch-bridge-cli | bridge (no default features), proto, tonic, libc |
| snitchwatch-tauri | tauri 2 (tray-icon), notify-rust, bridge + bridge-cli |
| snitchwatch-kirigami | cxx =1.0.194, cxx-qt =0.9.1, cxx-qt-lib (qt_full), maxminddb 0.29, dns-lookup 2, notify-rust 4, zbus 5, scanner-broker, bridge, proto |
| scanner-core / -cli / -privileged | rusqlite, sha2, chrono (core); core (cli); libc, rusqlite, chrono (privileged); none depend on proto |
| scanner-broker | tokio, zbus 5, libc |

Unusual: exact-pinned cxx/cxx-qt (pre-1.0), `dns-lookup` (getnameinfo, avoids an async-DNS crate), hand-driven hyper over `UnixListener` (axum 0.7 limit).

## CI / release / packaging
`.github/workflows/ci.yml` jobs: `check`, `test` (default members), `audit`, `package-check`, `kirigami` (runs in a `fedora:43` container on an Ubuntu runner, `dnf` Qt6/KF6, headless; `ci.yml:214-239`), `candidate-contract`. `release.yml` (tags `v*`): `build-bridge` -> `audit-bridge` -> `publish` (draft only; scripts in `packaging/release/`). `candidate-gui.yml` builds the r16 GUI Flatpak candidate. Image via bluebuild (`packaging/bluebuild/recipe.yml`). Local entry points: `justfile`. Tests: `tests/*.rs`, `tests/mock_opensnitchd`, Playwright in `tests/web_smoke`, `tests/tauri_smoke`.
