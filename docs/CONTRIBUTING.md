# Contributing

Everything here is taken from `CLAUDE.md`, `README.md`, `justfile`,
`.github/workflows/ci.yml` and the source files cited inline. Where the repo
is silent the text says "Not defined in the repo".

## Dev environment

| Need | Detail | Source |
|---|---|---|
| Rust | README declares no minimum version; no `rust-version`, `rust-toolchain.toml` or clippy/rustfmt override exists; CI uses `dtolnay/rust-toolchain@stable`; release builds pin 1.99.1 | `README.md` "Building"; `Cargo.toml:38-42`; `ci.yml`; `packaging/release/pins.env:2` |
| `protoc` | `snitchwatch-proto/build.rs` compiles `vendor/opensnitch/proto/ui.proto`; `snitchwatch-bridge` and `-bridge-cli` depend on it, so the default build needs it | `CLAUDE.md` (build commands); `ci.yml` (`protobuf-compiler`) |
| Submodule | `git submodule update --init --recursive`; `vendor/opensnitch` is read-only, pinned `v1.8.0` | `README.md` "Building"; `CLAUDE.md`; `.gitmodules` |
| `just` | All commands below | `justfile` |
| Tauri build deps | `snitchwatch-tauri` is a default member; its build links webkit2gtk even for `cargo check` | `ci.yml` (`libwebkit2gtk-4.1-dev`, `libsoup-3.0-dev`, ...) |
| Qt6 + KDE Frameworks 6 (Kirigami) dev packages | Needed for `kirigami-spike` and `snitchwatch-kirigami`, which are excluded from `default-members`; plain `cargo build`/`cargo check`/`cargo test` (what CI runs) skip them, but `just build`, `just check` and `just test` use `--workspace` (`justfile:24-25,27-28,33-35`) and DO compile them, so they need Qt6/KF6. Without Qt, run the plain cargo commands. Build Kirigami explicitly with `cargo build -p snitchwatch-kirigami` | `CLAUDE.md`; `Cargo.toml:24-36`; `justfile` `kirigami-dev` |
| Fedora package set known to build and test Kirigami | `git gcc gcc-c++ make protobuf-compiler dbus-daemon xdg-dbus-proxy qt6-qtbase-devel qt6-qtbase-private-devel qt6-qtdeclarative-devel qt6-qttools-devel plasma-integration kf6-kirigami-devel` | `ci.yml` job `kirigami` |
| `qmltestrunner-qt6` | Ships in `qt6-qtdeclarative-devel` | `justfile` `qml-test`; `ci.yml` |
| Playwright (one time) | `just web-smoke-install`, `just tauri-smoke-install`; a "missing browser" failure is a setup gap, not a regression. `just doctor` reports what is missing | `README.md` "Playwright smoke suites"; `justfile` `doctor` |
| podman | Used by the `release-*` and `endpoint-events-verify` recipes | `justfile` |
| `python3` + PyYAML, `shellcheck` (optional) | `just package-check` | `justfile` `package-check`; `ci.yml` job `package-check` |

## Commands

<!-- AUTO-GENERATED: justfile (just --list; descriptions are the recipe's leading comment, or the first line of its body when it has none) -->
| Recipe | What it does |
|---|---|
| `default` | Runs `just --list`. |
| `doctor` | Checks for missing Playwright installs in `tests/web_smoke` and `tests/tauri_smoke`; exits non-zero with a fix hint; modifies nothing. |
| `build` | `cargo build --workspace` |
| `test` | `cargo test --workspace` |
| `test-bridge` | `cargo test -p snitchwatch-bridge` |
| `check` | `cargo check --workspace` then `cargo clippy --workspace --all-targets -- -D warnings` |
| `fmt` | `cargo fmt --all` |
| `regen-proto` | `cargo build -p snitchwatch-proto` |
| `run-bridge` | `RUST_LOG=info cargo run -p snitchwatch-bridge-cli` (starts a real bridge: see Test safety) |
| `run-spike endpoint="http://127.0.0.1:50051"` | `cargo run -p snitchwatch-spike -- <endpoint>` with `RUST_LOG=info`. |
| `web-rebrand` | Run `./web/rebrand.sh`, then `git diff --stat web/`, over the vendored `web/` tree. |
| `web-smoke` | Run the Playwright smoke tests against a freshly built bridge. |
| `web-smoke-install` | Install the Playwright Firefox channel into `tests/web_smoke/node_modules`. |
| `tauri-dev` | Run the Tauri shell in dev mode (kept until Tauri/`web/` are retired; Kirigami is the shell that ships). |
| `tauri-build` | `cargo build -p snitchwatch-tauri --release`. Its comment mentions deb/rpm/appimage, but the body runs no bundler. |
| `tauri-smoke` | Playwright smoke test for the Tauri shell (needs `npm install` in `tests/tauri_smoke` first). |
| `tauri-smoke-install` | One-time install of the Playwright deps. |
| `kirigami-dev` | Run the Kirigami shell (needs Qt6 + KF6 dev packages). |
| `kirigami-build` | Build a release Kirigami shell binary. |
| `kirigami-screenshots dir="target/screenshots"` | Demo-mode (`SNITCHWATCH_DEMO=1`) offscreen render of the pages to PNGs in `dir`; Basic style, so colours differ slightly from the Flatpak. |
| `qml-test` | `qmltestrunner-qt6` input-routing tests in `crates/snitchwatch-kirigami/tests/qml` (real mouse events via QtTest). |
| `test-blocklists` | Blocklist unit + e2e suites, offline (fixture fetchers and local TLS servers). |
| `package-check` | Validates packaging artifacts without a Bazzite host: `packaging/geo/test-country-data.py`, `packaging/scanner/test-staging.py`, `scripts/ebpf/test_measure.py`, `bash -n` on `packaging/scanner/stage.sh` and `packaging/endpoint-events/verify.sh`, YAML/JSON parse, `verify-unit.sh`, shellcheck (skipped if absent), packaging/release shape tests, `--no-default-features` clippy/test of the tarball feature set. |
| `endpoint-events-verify` | Real-rsyslog proof of the opt-in endpoint-events recipe; needs rootless podman and Fedora mirrors; not part of `package-check`. |
| `release-bridge` | Build `dist/snitchwatch-bridge-<version>-x86_64.tar.gz{,.sha256}` in the pinned Fedora 45 container. |
| `release-bridge-repro` | Build, rebuild from another path, require byte-identical tarballs. |
| `release-verify tarball sha256` | Verify a tarball against a known sha256 in a throwaway Fedora 45 container. |
<!-- /AUTO-GENERATED -->

## Testing

| Suite | Where | How |
|---|---|---|
| Bridge unit + integration | `crates/snitchwatch-bridge` | `just test-bridge` (fast, "most iteration happens here", `CLAUDE.md`) |
| Bridge protocol end to end | `tests/bridge_protocol_test.rs` and `tests/bridge_protocol_test/*.rs`, wired via `tests/integration/Cargo.toml:7-10` | `MockOpensnitchd` (`tests/mock_opensnitchd`) stands in for the daemon; a real in-process bridge plus a real WebSocket client over the Unix socket with the handshake token. Template: `ask_rule_round_trip_*` |
| Blocklists | `tests/fixtures/blocklists/` (small samples such as `abp-tiny.txt` and `stevenblack-tiny.txt`, plus the malformed `garbage.bin`) | `just test-blocklists` |
| Scanner, userspace | `scanner_core::testkit::MockInspector`; example `crates/scanner-core/tests/userspace_scan.rs` | no production call sites; no real `rpm-ostree`/`ostree`/`flatpak`/`ss` needed |
| Scanner, privileged | `FakeFacts` (the `SystemFacts` trait) in `crates/scanner-privileged/tests/end_to_end.rs` | mirror it; the unit-test mock is not visible to integration tests |
| Kirigami Rust tests | `crates/snitchwatch-kirigami/tests/*.rs` (headless Qt) | `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic cargo test -p snitchwatch-kirigami --locked` (as CI) |
| QML probes | `crates/snitchwatch-kirigami/tests/qml` | `just qml-test` |
| Playwright | `tests/web_smoke`, `tests/tauri_smoke` | `just web-smoke`, `just tauri-smoke` after the one-time install |
| Packaging | `just package-check` | see Commands |

Headless Kirigami passing locally in this sandbox "has not been verified"
(`CLAUDE.md`); treat a first local attempt as exploratory. CI job `kirigami`
is the confirmed environment.

Coverage: the global rules ask for 80 percent; no coverage tool or threshold is
configured in the repo (no match in `justfile` or `ci.yml`).

## Repo conventions

| Convention | Source |
|---|---|
| Plan first for anything beyond a small fix: a doc at `docs/superpowers/plans/2026-MM-DD-<slug>.md` before implementation; specs/research in `docs/superpowers/specs/` | `CLAUDE.md` "Repo conventions" |
| Stop and ask on genuine design ambiguity; do not re-litigate settled decisions | `CLAUDE.md`; `AUDIT.md` |
| Commit messages: conventional commits (`<type>: <description>`); no repo-specific override | `CLAUDE.md` |
| Never edit anything under `vendor/opensnitch`. A dirty submodule showing only an untracked `.omc/` path is tooling noise; do not commit a pointer bump for it | `CLAUDE.md` |
| `DefaultAction: allow` is the shipped default (owner decision 2026-10-25); deny is an explicit operator opt-in, never packaged | `CLAUDE.md` "Settled" #5; `packaging/README.md` |
| Components A and B stay separate apps (no shared daemon/unit/privilege model) | `CLAUDE.md` "Settled" #1 |
| Releases (tags, `r*-candidate`, promotion) are the owner's decision | `HANDOFF.md:12`; `docs/RUNBOOK.md` |

## Code style, as enforced

| Check | Enforced by | Notes |
|---|---|---|
| `cargo clippy ... -D warnings` | `just check` (`--workspace --all-targets`); CI `check` job (default members, `--locked`); CI `kirigami` and `package-check` jobs for their crates | warnings fail the build |
| `cargo fmt --all` | `just fmt` only | No CI job runs `cargo fmt --check` (grep of `ci.yml`: none). Run it yourself |
| Config overrides | none: no `.rustfmt.toml`, `clippy.toml`, `rust-toolchain.toml` | `CLAUDE.md`; verified absent |
| Dependency audit | CI `audit` job: `cargo audit` with two documented ignores | `ci.yml` |
| Shell scripts | `shellcheck` inside `just package-check` when installed | `justfile` |

## CI jobs (`.github/workflows/ci.yml`)

Runs on push to `main` and `r16-candidate`, and on every pull request.

| Job | What it gates |
|---|---|
| `check` | `cargo check --locked` and `cargo clippy --all-targets --locked -- -D warnings`, default members only (no Qt) |
| `test` | `cargo test --locked`; then `dbus-run-session -- cargo test -p scanner-broker private_bus --locked -- --ignored` |
| `audit` | `quick-xml` must stay unreachable on Linux, then `cargo audit` with `RUSTSEC-2026-0194`/`-0195` ignored |
| `package-check` | `just package-check` |
| `kirigami` | Fedora 43 container: native KDE tray fixture, check, clippy, `cargo test -p snitchwatch-kirigami`, `qmltestrunner-qt6` |
| `candidate-contract` | `python3 packaging/flatpak/test-candidate-publication.py` |

## PR checklist (what is actually enforced)

- [ ] `just check` clean (clippy `-D warnings`); CI re-runs it on default members.
- [ ] `just test` passes (`--workspace`, so it includes Kirigami and needs Qt6/KF6); the default-members CI jobs do not build Kirigami, only the separate `kirigami` job does (`CLAUDE.md`).
- [ ] `just fmt` run (not enforced by CI).
- [ ] `just package-check` if you touched `packaging/`, the justfile, or release scripts.
- [ ] A plan doc exists for anything beyond a small fix.
- [ ] Nothing under `vendor/` changed; no submodule pointer bump from tooling noise.
- [ ] No release, tag or candidate-branch action without the owner.
- [ ] Review sign-off requirements, branch protection, CODEOWNERS: Not defined in the repo (no such file under `.github/`).

## Test safety

| Rule | Source |
|---|---|
| Never run `snitchwatch-bridge-cli` without `--help`/`--version` outside an isolated `XDG_RUNTIME_DIR`: it starts a bridge and replaces a running bridge's socket and token (issue #34). `just run-bridge` does exactly that | `CLAUDE.md` |
| Any test that spawns a bridge must point `XDG_RUNTIME_DIR` at a tempdir (`crates/snitchwatch-tauri/tests/bridge_runtime_starts.rs` once did not) | `CLAUDE.md` |
| Run Kirigami and lib tests with a throwaway `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME` and `XDG_RUNTIME_DIR`. Some probes set `XDG_CONFIG_HOME` to a tempdir themselves (`crates/snitchwatch-kirigami/tests/learning_end_qml.rs:37`, `crates/snitchwatch-kirigami/tests/level_rule_unavailable_qml.rs:31`, `crates/snitchwatch-kirigami/tests/silent_mode_onboarding_qml.rs:33`) but not all of them, and the GUI derives its config, state and data paths from these variables or `HOME` (`src/paths.rs:14-50`). Owner instruction: a plain run can rewrite `~/.config/snitchwatch` before the #259 fix lands | `crates/snitchwatch-kirigami/src/paths.rs`; owner instruction |
| #259 status: `HANDOFF.md:29` lists "#259 (learned/silent tests) is still open". The research note `docs/superpowers/research/2026-11-06-flake-259.md` does not exist on this branch's base (only three `scan-page-flicker`/`title-flicker` notes do) | `HANDOFF.md:29`; `ls docs/superpowers/research` |
| `history::dormant_tests` (`crates/snitchwatch-bridge/src/history/dormant_tests.rs`, test `primitives_have_no_production_call_sites`) scans every `crates/*/src/**.rs` and fails if a file outside the history owner files mentions the whole identifiers `lifecycle` or `pins` (code only; comments and string literals are stripped) or the types `PinRegistry`, `PinId`, `ReaderGate`, `ReaderPermit`, `AcceptanceSeq`, `Generation` (`dormant_tests.rs:8-19,100-113,120-126,140-163`). Do not use those words as identifiers in other `.rs` sources | that file |
| Never point a test at a live `opensnitchd`, a real Bazzite host, or `sudo`; use the doubles above. A real-daemon-only behaviour is a gap to flag, not to improvise (`.agent_native/agent_roadmap.md` item 7) | `CLAUDE.md` |
| `package-check` verifies a renamed copy of the unit so a dev machine's `~/.config/systemd/user/snitchwatch-bridge.service` cannot shadow it | `CLAUDE.md`; `justfile` |
| `kirigami-screenshots` already isolates `XDG_RUNTIME_DIR` with `mktemp -d` and uses `SNITCHWATCH_DEMO=1` | `justfile` |

Environment variables used by tests: see `docs/ENV.md`.
