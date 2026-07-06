# M5 Packaging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Package the Tauri app as a user-scope Flatpak, ship the OpenSnitch daemon as a podman quadlet, wire the first-run wizard's "Install daemon" button to a real `install.sh`, and surface every failure mode from the spec's failure inventory in the UI as a typed banner.

**Architecture:** Two on-disk artifacts ship from `packaging/`: a `flatpak-builder` manifest for `org.snitchwatch.Snitchwatch` and a systemd `.container` (quadlet) unit for `snitchwatch-opensnitchd.service`. `install.sh` lays both down in user-writable locations idempotently. The wizard's `install_daemon_stub` Tauri command from Plan 4 becomes a real implementation that drives `install.sh` via `flatpak-spawn --host` and streams stdout back over a Tauri event channel. A new `lifecycle.rs` module in `snitchwatch-bridge` polls gRPC reachability + `systemctl --user is-active` on a 5s cadence and broadcasts typed `LifecycleEvent`s; the WS server forwards them as `setLifecycleBanner` messages, which the web layer renders as a single in-frame banner with one optional action button per failure mode.

**Tech Stack:** flatpak-builder, freedesktop-sdk 23.08 runtime, podman quadlet, systemd --user, host-spawn (`flatpak-spawn --host`), tokio interval timers, tracing, vanilla-JS DOM banner overlay.

**Out of scope:**
- Live opensnitchd 60s smoke test on a real Bazzite VM (Plan 7 — M6).
- Ephemeral-bind / settings.toml `bridge.bind_address` flip (already `127.0.0.1:0` since Plan 1; Plan 7 documents the A→B→C upgrade path).
- LAN mode (Option C — gated behind a v2 milestone).
- Cross-distro packaging beyond Bazzite/Universal Blue.
- App-store / Flathub submission (deferred to v2; v1 ships from `just flatpak` + the GH release tarball only).
- WebKitGTK Flatpak permissions verification on a real Bazzite VM (Plan 7 — open question #3).

---

## Memory Constraints

These project-memory entries directly shape this plan. Each one is repeated here so a fresh subagent has everything inline.

1. **`bash_antipattern_hook.md`** — workspace blocks `find/ls/cat/grep/rg/head/tail/sed/awk` in Bash. Use the dedicated `Read`/`Glob`/`Grep` tools instead. PostToolUse "Write operation failed" reminders are false-positives — verify by the structured stdout success line.

2. **`m1_envelope_hack.md`** — the JSON envelope inside `Notification.data` from M1 was deleted at M2 topology flip. Do not reintroduce it. The new `setLifecycleBanner` message is a typed `ws_messages.rs` variant, never a stringly-typed envelope.

3. **`plan1_deferred_criteria.md`** — live opensnitchd smoke + cargo-llvm-cov are environmental, not code work. Plan 6 does NOT reopen them. Plan 7 owns them.

4. **`clippy_gotchas_bridge.md`** — `Translated::AskRule` must stay boxed (`large_enum_variant`). New enum variants in this plan with payloads ≥160 bytes should be boxed too. Discard `oneshot::Receiver` with `drop(rx)`, never `let _ = rx`.

5. **`autonomous_tdd_resume.md`** — on PreCompact resume, no recap, no acknowledgment. Pick up the last task as if the break never happened.

---

## File Structure

### NEW

- `packaging/flatpak/org.snitchwatch.Snitchwatch.yml` — flatpak-builder manifest. Runtime: `org.freedesktop.Platform//23.08`. SDK: `org.freedesktop.Sdk//23.08`. SDK extension: `org.freedesktop.Sdk.Extension.rust-stable//23.08`. Modules: `web-vendored` (cp web/ → /app/share/snitchwatch/web), `snitchwatch-tauri` (cargo build --release --features flatpak-runtime). `finish-args`: `--share=ipc`, `--share=network`, `--socket=fallback-x11`, `--socket=wayland`, `--device=dri`, `--talk-name=org.freedesktop.Notifications`, `--talk-name=org.kde.StatusNotifierWatcher`, `--talk-name=org.freedesktop.systemd1`, `--persist=.local/share/snitchwatch`, `--filesystem=xdg-config/snitchwatch:create`, `--filesystem=xdg-data/snitchwatch:create`, `--filesystem=xdg-state/snitchwatch:create`, `--filesystem=xdg-run/systemd:ro`. The `--share=network` line is intentional and load-bearing — the embedded webview talks to the bridge over loopback.
- `packaging/flatpak/org.snitchwatch.Snitchwatch.desktop` — `.desktop` file. `Exec=snitchwatch-tauri`, `Categories=Network;Security;System;`, `X-Flatpak=org.snitchwatch.Snitchwatch`.
- `packaging/flatpak/org.snitchwatch.Snitchwatch.metainfo.xml` — AppStream metadata. `<id>`, `<name>`, `<summary>`, three `<description>` paragraphs, `<categories>`, `<launchable>`, `<provides>`, `<releases>` with v0.1.0 placeholder, `<content_rating>`, `<url type="homepage">`, `<url type="bugtracker">`, `<screenshots>` (empty `<screenshots/>` tag — populated in Plan 7).
- `packaging/flatpak/icons/snitchwatch.svg` — placeholder icon (32-line stub SVG).
- `packaging/quadlet/snitchwatch-opensnitchd.container` — systemd `.container` quadlet unit. Pulls `ghcr.io/evilsocket/opensnitch:latest`. `Network=host`, `PodmanArgs=--privileged --cap-add=NET_ADMIN --cap-add=NET_RAW --cap-add=SYS_PTRACE`. `Volume=/lib/modules:/lib/modules:ro`, `Volume=%h/.config/opensnitch:/etc/opensnitchd:ro`. `[Service]` `Restart=on-failure`, `RestartSec=5s`. `[Install]` `WantedBy=default.target`.
- `packaging/install.sh` — idempotent shell script. `set -euo pipefail`. Steps: assert `flatpak`/`podman`/`systemctl` on PATH; build the flatpak if `--build` flag is set; `flatpak install --user --noninteractive ./build/snitchwatch.flatpak` OR `flatpak install --user --noninteractive --or-update flathub org.snitchwatch.Snitchwatch` if a flathub remote is configured; `mkdir -p "$HOME/.config/containers/systemd"`; `install -m 0644 packaging/quadlet/snitchwatch-opensnitchd.container "$HOME/.config/containers/systemd/"`; `systemctl --user daemon-reload`; `systemctl --user start snitchwatch-opensnitchd.service`; print `OK: install complete`. Supports `--daemon-only` (skip the flatpak install step — used by the wizard's "I already have the GUI, install daemon" path) and `--dry-run` (echo all commands without running them).
- `packaging/install.bats` — bats-core test file with 6 unit tests for `install.sh` parser branches via `--dry-run`.
- `packaging/README.md` — packaging-internal notes (build/install/uninstall/troubleshoot), 60 lines.
- `crates/snitchwatch-bridge/src/lifecycle.rs` — `LifecycleProbe` struct that polls `GrpcClient::ping` and `systemctl --user is-active snitchwatch-opensnitchd.service` every `LIFECYCLE_PROBE_INTERVAL` (5s). Emits `LifecycleEvent { kind: LifecycleKind, severity: LifecycleSeverity }` on a `tokio::sync::broadcast::Sender<LifecycleEvent>` of capacity 16. `LifecycleKind` is the enum mirror of the 10 failure-mode rows from the spec inventory: `DaemonOk`, `UnitMissing`, `UnitInactive`, `GrpcUnreachable`, `GrpcStaleStream`, `EventFloodDropped { dropped: u32 }`, `BridgePanicRecovered`, `BlocklistFetchFailed { list_id: String, reason: String }`, `KernelHookFailed { excerpt: String }`, `StateDivergenceReconciled`. `LifecycleSeverity` is `Info | Warning | Error`. Module file ≤ 320 lines.
- `crates/snitchwatch-bridge/src/lifecycle/probe_state.rs` — small pure-function helper module that derives the next `LifecycleKind` from `(grpc_ok: bool, unit_state: SystemctlState)`. Unit-tested in isolation. ≤ 120 lines.
- `crates/snitchwatch-bridge/tests/lifecycle_e2e.rs` — integration test wiring a fake `GrpcPing` and a fake `SystemctlProbe` into `LifecycleProbe`, asserting the broadcast events on a controlled clock.
- `web/banners/banner.js` — `<lifecycle-banner>` custom element. Single banner at the top of the viewport, three severity classes, optional action button, action dispatch back over the WS as a `ClientMessage::LifecycleAction { kind: String }`. ≤ 220 lines.
- `web/banners/banner.css` — banner styling, ≤ 100 lines.
- `crates/snitchwatch-tauri/src/installer.rs` — `install_daemon` Tauri command. Resolves `install.sh` from `app.path().resource_dir()` (`/app/share/snitchwatch/packaging/install.sh` inside the Flatpak), spawns it via `flatpak-spawn --host` if running inside a Flatpak (detected by `FLATPAK_ID` env var), or directly otherwise. Streams stdout/stderr line-by-line back over a Tauri event named `installer://progress`. Final result is `Ok(())` on exit code 0, otherwise `Err(String)` with the last 20 lines of combined output. ≤ 280 lines.
- `crates/snitchwatch-tauri/src/installer/host_spawn.rs` — small wrapper that builds the right `Command` (host-spawn vs direct) given an `IsFlatpak` newtype. Pure-function decision so it's testable without spawning anything. ≤ 110 lines.
- `tests/tauri_smoke/tests/install_button.spec.ts` — Playwright test that mocks `install.sh` with a script that prints 5 fake progress lines and exits 0, clicks the wizard's Install button, and asserts the progress lines stream into the overlay and the "Done" CTA appears.
- `crates/snitchwatch-bridge/src/cache/dropped_counter.rs` — atomic counter the existing event channel increments on `send_timeout`. Read by `LifecycleProbe` to populate `EventFloodDropped { dropped }`. ≤ 80 lines.

### MODIFIED

- `crates/snitchwatch-bridge/Cargo.toml` — add `[dev-dependencies]` `tokio-test = "0.4"`. No new runtime deps; lifecycle uses the existing `tokio`/`tracing` stack.
- `crates/snitchwatch-bridge/src/lib.rs` — `pub mod lifecycle;` declaration.
- `crates/snitchwatch-bridge/src/grpc_client.rs` — add `pub async fn ping(&self) -> Result<(), GrpcError>` that issues a no-op `Ping` RPC against the existing `UI` service (uses `Empty`/`PingReply` from the proto — already generated). The probe reuses this for liveness without dragging in any new RPC surface.
- `crates/snitchwatch-bridge/src/ws_messages.rs` — add `ServerMessage::SetLifecycleBanner { kind: String, severity: String, label: String, detail: Option<String>, action: Option<LifecycleBannerAction> }` and `ClientMessage::LifecycleAction { kind: String }`. New struct `LifecycleBannerAction { id: String, label: String }` derives `Serialize + Deserialize + Debug + Clone + PartialEq`.
- `crates/snitchwatch-bridge/src/ws_server.rs` — `serve_with_blocklists` (from Plan 5) gets a sibling `serve_with_lifecycle(addr, mgr, lifecycle_rx) -> (ws_url, JoinHandle)`. Per-connection task subscribes to the broadcast and forwards each event as `setLifecycleBanner`.
- `crates/snitchwatch-bridge/src/translator/downstream.rs` — `pub fn build_set_lifecycle_banner(event: &LifecycleEvent) -> ServerMessage` mapping function. Pure, fully unit-tested.
- `crates/snitchwatch-bridge/src/translator/upstream.rs` — `LifecycleAction` routes through a new `LifecycleActionOutcome::{StartUnit, InstallDaemon, OpenDiagnostics, NoOp}` variant on `handle_lifecycle_action`.
- `crates/snitchwatch-tauri/src/lib.rs` — `pub mod installer;`. Register the `install_daemon` command in the Tauri builder. Wire the Tauri event channel that the front-end subscribes to.
- `crates/snitchwatch-tauri/src/wizard.rs` — `install_daemon_stub` is replaced. The dispatcher now calls `crate::installer::install_daemon`.
- `crates/snitchwatch-tauri/src/main.rs` — invoke handler list grows by `installer::install_daemon`. Lifecycle probe is started on the bridge runtime task and its broadcast receiver is plumbed into `serve_with_lifecycle`.
- `crates/snitchwatch-tauri/tauri.conf.json` — `bundle.resources` array gains `"../../packaging/**"` so the install script ships inside the Flatpak.
- `web/index.html` — `<script type="module" src="banners/banner.js"></script>` and a `<lifecycle-banner></lifecycle-banner>` placeholder right under `<body>`.
- `web/onboarding.js` (created in Plan 4) — `installDaemon()` function calls the `install_daemon` Tauri command and listens to the `installer://progress` event for log lines.
- `web/onboarding.css` — progress-overlay styles for streaming installer output.
- `justfile` — new recipes: `just flatpak`, `just flatpak-shell`, `just install`, `just install-daemon-only`, `just package-test`, `just lint-shell`.
- `README.md` — new "Install on Bazzite" section with copy-pasteable shell snippet, plus a "Failure-mode banners" subsection that maps the 10 spec failure rows to the user-visible banner labels.
- `.gitignore` — `build/`, `**/.flatpak-builder/`, `packaging/build/`, `*.flatpak`.
- `docs/superpowers/specs/2026-04-10-snitchwatch-design.md` — tick M5 in the milestone table with ✅ + a one-paragraph implementation note pointing at this plan.

### DELETED

- `crates/snitchwatch-tauri/src/wizard.rs` — `install_daemon_stub` function and its registration in `lib.rs` `invoke_handler!` (replaced, not removed wholesale; the wizard module file stays).

---

## Part A — Flatpak manifest, desktop entry, metainfo (Tasks 1–3)

### Task 1: Packaging skeleton + .gitignore + Flatpak manifest YAML

**Files:**
- Create: `packaging/flatpak/org.snitchwatch.Snitchwatch.yml`
- Create: `packaging/flatpak/icons/snitchwatch.svg`
- Modify: `.gitignore`

The manifest is the load-bearing artifact. Test it by parsing it as YAML in a tiny Rust integration test that asserts the runtime version, the finish-args, and the module list. We don't actually invoke `flatpak-builder` in CI — that would need a Flatpak host and a runtime download.

- [ ] **Step 1: Write the failing test**

Create `packaging/flatpak/manifest_shape_test.rs` (a standalone bin test wired into a new tiny crate):

Actually — the cleanest place to put a test that parses the manifest is inside the existing `snitchwatch-bridge` crate as a `tests/manifest_shape.rs` file, since that crate already has `serde_yaml` available transitively via `tonic-build` deps. Use a fresh `serde_yaml` dev-dependency to keep it explicit.

Add to `crates/snitchwatch-bridge/Cargo.toml` under `[dev-dependencies]`:

```toml
serde_yaml = "0.9"
```

Create `crates/snitchwatch-bridge/tests/flatpak_manifest_shape.rs`:

```rust
//! Schema-shape test for the flatpak-builder manifest.
//!
//! This does NOT invoke flatpak-builder — it asserts that the manifest YAML
//! contains the keys we expect, the runtime version we expect, and the
//! finish-args that the spec requires (especially --share=network).

use serde_yaml::Value;

fn load_manifest() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/flatpak/org.snitchwatch.Snitchwatch.yml");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    serde_yaml::from_str(&raw).expect("manifest must be valid YAML")
}

#[test]
fn manifest_has_expected_top_level_keys() {
    let m = load_manifest();
    let map = m.as_mapping().expect("top level is a mapping");
    for key in ["app-id", "runtime", "runtime-version", "sdk", "command", "finish-args", "modules"] {
        assert!(
            map.contains_key(Value::String(key.into())),
            "manifest is missing top-level key `{key}`"
        );
    }
}

#[test]
fn manifest_targets_freedesktop_2308() {
    let m = load_manifest();
    assert_eq!(m["app-id"], Value::String("org.snitchwatch.Snitchwatch".into()));
    assert_eq!(m["runtime"], Value::String("org.freedesktop.Platform".into()));
    assert_eq!(m["runtime-version"], Value::String("23.08".into()));
    assert_eq!(m["sdk"], Value::String("org.freedesktop.Sdk".into()));
    assert_eq!(m["command"], Value::String("snitchwatch-tauri".into()));
}

#[test]
fn manifest_finish_args_include_network_and_persist() {
    let m = load_manifest();
    let args: Vec<String> = m["finish-args"]
        .as_sequence()
        .expect("finish-args is a sequence")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let must_have = [
        "--share=ipc",
        "--share=network",
        "--socket=wayland",
        "--socket=fallback-x11",
        "--device=dri",
        "--talk-name=org.freedesktop.Notifications",
        "--talk-name=org.kde.StatusNotifierWatcher",
        "--talk-name=org.freedesktop.systemd1",
        "--persist=.local/share/snitchwatch",
        "--filesystem=xdg-config/snitchwatch:create",
        "--filesystem=xdg-data/snitchwatch:create",
        "--filesystem=xdg-state/snitchwatch:create",
        "--filesystem=xdg-run/systemd:ro",
    ];
    for arg in must_have {
        assert!(
            args.iter().any(|a| a == arg),
            "missing finish-arg `{arg}`; have: {args:?}"
        );
    }
}

#[test]
fn manifest_module_list_includes_web_and_tauri() {
    let m = load_manifest();
    let mods = m["modules"].as_sequence().expect("modules is a sequence");
    let names: Vec<String> = mods
        .iter()
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
        .collect();
    assert!(names.iter().any(|n| n == "web-vendored"), "names = {names:?}");
    assert!(names.iter().any(|n| n == "snitchwatch-tauri"), "names = {names:?}");
}
```

- [ ] **Step 2: Run the tests, verify they fail**

Run: `cargo test -p snitchwatch-bridge --test flatpak_manifest_shape`
Expected: FAIL — the manifest file does not exist yet, so `load_manifest()` panics on `read_to_string`.

- [ ] **Step 3: Write the manifest YAML**

Create `packaging/flatpak/org.snitchwatch.Snitchwatch.yml`:

```yaml
app-id: org.snitchwatch.Snitchwatch
runtime: org.freedesktop.Platform
runtime-version: "23.08"
sdk: org.freedesktop.Sdk
sdk-extensions:
  - org.freedesktop.Sdk.Extension.rust-stable
command: snitchwatch-tauri

build-options:
  append-path: /usr/lib/sdk/rust-stable/bin
  env:
    CARGO_HOME: /run/build/snitchwatch-tauri/cargo
    RUST_BACKTRACE: "1"

finish-args:
  - --share=ipc
  - --share=network
  - --socket=wayland
  - --socket=fallback-x11
  - --device=dri
  - --talk-name=org.freedesktop.Notifications
  - --talk-name=org.kde.StatusNotifierWatcher
  - --talk-name=org.freedesktop.systemd1
  - --persist=.local/share/snitchwatch
  - --filesystem=xdg-config/snitchwatch:create
  - --filesystem=xdg-data/snitchwatch:create
  - --filesystem=xdg-state/snitchwatch:create
  - --filesystem=xdg-run/systemd:ro

modules:
  - name: web-vendored
    buildsystem: simple
    build-commands:
      - install -d /app/share/snitchwatch/web
      - cp -a web/. /app/share/snitchwatch/web/
      - install -d /app/share/snitchwatch/packaging
      - cp -a packaging/. /app/share/snitchwatch/packaging/
    sources:
      - type: dir
        path: ../..

  - name: snitchwatch-tauri
    buildsystem: simple
    build-commands:
      - cargo build --release --offline -p snitchwatch-tauri
      - install -Dm755 target/release/snitchwatch-tauri /app/bin/snitchwatch-tauri
      - install -Dm644 packaging/flatpak/org.snitchwatch.Snitchwatch.desktop /app/share/applications/org.snitchwatch.Snitchwatch.desktop
      - install -Dm644 packaging/flatpak/org.snitchwatch.Snitchwatch.metainfo.xml /app/share/metainfo/org.snitchwatch.Snitchwatch.metainfo.xml
      - install -Dm644 packaging/flatpak/icons/snitchwatch.svg /app/share/icons/hicolor/scalable/apps/org.snitchwatch.Snitchwatch.svg
    sources:
      - type: dir
        path: ../..
```

Create `packaging/flatpak/icons/snitchwatch.svg` (placeholder; designer pass deferred to v2):

```svg
<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128">
  <rect width="128" height="128" rx="24" fill="#1a1a2e"/>
  <circle cx="64" cy="64" r="36" fill="none" stroke="#e94560" stroke-width="6"/>
  <path d="M44 64 L60 80 L88 48" fill="none" stroke="#e94560" stroke-width="8" stroke-linecap="round" stroke-linejoin="round"/>
</svg>
```

Append to `.gitignore`:

```
# packaging artifacts
build/
**/.flatpak-builder/
packaging/build/
*.flatpak
```

- [ ] **Step 4: Run the tests, verify they pass**

Run: `cargo test -p snitchwatch-bridge --test flatpak_manifest_shape`
Expected: PASS — 4 tests.

- [ ] **Step 5: Commit**

```bash
git add packaging/flatpak/org.snitchwatch.Snitchwatch.yml packaging/flatpak/icons/snitchwatch.svg .gitignore crates/snitchwatch-bridge/Cargo.toml crates/snitchwatch-bridge/tests/flatpak_manifest_shape.rs
git commit -m "feat(packaging): flatpak manifest skeleton + shape test"
```

---

### Task 2: Desktop entry + AppStream metainfo

**Files:**
- Create: `packaging/flatpak/org.snitchwatch.Snitchwatch.desktop`
- Create: `packaging/flatpak/org.snitchwatch.Snitchwatch.metainfo.xml`
- Modify: `crates/snitchwatch-bridge/tests/flatpak_manifest_shape.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/snitchwatch-bridge/tests/flatpak_manifest_shape.rs`:

```rust
fn read_packaging(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/flatpak")
        .join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e))
}

#[test]
fn desktop_entry_has_required_keys() {
    let body = read_packaging("org.snitchwatch.Snitchwatch.desktop");
    for key in [
        "[Desktop Entry]",
        "Type=Application",
        "Name=Snitchwatch",
        "Exec=snitchwatch-tauri",
        "Icon=org.snitchwatch.Snitchwatch",
        "Categories=Network;Security;System;",
        "StartupWMClass=org.snitchwatch.Snitchwatch",
    ] {
        assert!(body.contains(key), "desktop entry missing `{key}`\nbody:\n{body}");
    }
}

#[test]
fn metainfo_xml_advertises_application_id_and_release() {
    let body = read_packaging("org.snitchwatch.Snitchwatch.metainfo.xml");
    assert!(body.contains("<id>org.snitchwatch.Snitchwatch</id>"));
    assert!(body.contains("<name>Snitchwatch</name>"));
    assert!(body.contains("<launchable type=\"desktop-id\">org.snitchwatch.Snitchwatch.desktop</launchable>"));
    assert!(body.contains("<release version=\"0.1.0\""));
    assert!(body.contains("<content_rating type=\"oars-1.1\""));
}
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test -p snitchwatch-bridge --test flatpak_manifest_shape desktop_entry_has_required_keys metainfo_xml_advertises_application_id_and_release`
Expected: FAIL — files do not exist.

- [ ] **Step 3: Write the desktop entry**

Create `packaging/flatpak/org.snitchwatch.Snitchwatch.desktop`:

```ini
[Desktop Entry]
Type=Application
Name=Snitchwatch
GenericName=Application Firewall
Comment=Per-application outbound firewall GUI for OpenSnitch
Exec=snitchwatch-tauri
Icon=org.snitchwatch.Snitchwatch
Terminal=false
Categories=Network;Security;System;
Keywords=firewall;security;network;privacy;opensnitch;
StartupNotify=true
StartupWMClass=org.snitchwatch.Snitchwatch
X-Flatpak=org.snitchwatch.Snitchwatch
```

- [ ] **Step 4: Write the metainfo XML**

Create `packaging/flatpak/org.snitchwatch.Snitchwatch.metainfo.xml`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<component type="desktop-application">
  <id>org.snitchwatch.Snitchwatch</id>
  <name>Snitchwatch</name>
  <summary>Per-application outbound firewall for Linux</summary>
  <metadata_license>CC0-1.0</metadata_license>
  <project_license>GPL-2.0-or-later</project_license>
  <launchable type="desktop-id">org.snitchwatch.Snitchwatch.desktop</launchable>

  <description>
    <p>
      Snitchwatch is a desktop firewall GUI that asks before any application
      makes a new outbound network connection. It is built on top of the
      OpenSnitch daemon and is designed to look and feel like Little Snitch
      on macOS.
    </p>
    <p>
      Decisions, rules, traffic charts, and blocklist subscriptions are all
      managed from a single window. Snitchwatch ships as a Flatpak; the
      OpenSnitch daemon ships alongside it as a podman quadlet, so the host
      system stays untouched.
    </p>
    <p>
      Use it to see what your laptop is talking to, block trackers and
      telemetry per-application, and approve new connections deliberately
      instead of by accident.
    </p>
  </description>

  <categories>
    <category>Network</category>
    <category>Security</category>
    <category>System</category>
  </categories>

  <provides>
    <binary>snitchwatch-tauri</binary>
  </provides>

  <url type="homepage">https://github.com/snitchwatch/snitchwatch</url>
  <url type="bugtracker">https://github.com/snitchwatch/snitchwatch/issues</url>

  <content_rating type="oars-1.1"/>

  <screenshots/>

  <releases>
    <release version="0.1.0" date="2026-04-11">
      <description>
        <p>Initial public preview.</p>
      </description>
    </release>
  </releases>
</component>
```

- [ ] **Step 5: Run the tests, verify they pass**

Run: `cargo test -p snitchwatch-bridge --test flatpak_manifest_shape`
Expected: PASS — 6 tests now.

- [ ] **Step 6: Commit**

```bash
git add packaging/flatpak/org.snitchwatch.Snitchwatch.desktop packaging/flatpak/org.snitchwatch.Snitchwatch.metainfo.xml crates/snitchwatch-bridge/tests/flatpak_manifest_shape.rs
git commit -m "feat(packaging): desktop entry + AppStream metainfo with shape tests"
```

---

### Task 3: Tauri bundles `packaging/` resources for the wizard

**Files:**
- Modify: `crates/snitchwatch-tauri/tauri.conf.json`
- Modify: `crates/snitchwatch-tauri/src/installer/host_spawn.rs` (will be created in Task 7; for this task we only stage a placeholder)

The wizard needs `install.sh` to be readable from inside the Flatpak sandbox. The cleanest way is to declare `packaging/**` as a Tauri bundle resource so it ships under `/app/share/snitchwatch/packaging/` (which the Flatpak module already lays down) AND under the dev-mode `target/debug/` path during `cargo run`.

- [ ] **Step 1: Write the failing test**

Append to `crates/snitchwatch-bridge/tests/flatpak_manifest_shape.rs`:

```rust
#[test]
fn tauri_conf_bundles_packaging_resources() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../snitchwatch-tauri/tauri.conf.json");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    let json: serde_json::Value =
        serde_json::from_str(&body).expect("tauri.conf.json must be valid JSON");
    let resources = json["bundle"]["resources"]
        .as_array()
        .expect("bundle.resources must be an array");
    let resource_strings: Vec<String> = resources
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        resource_strings.iter().any(|r| r.contains("packaging")),
        "tauri.conf.json bundle.resources must include packaging/**, got {resource_strings:?}"
    );
}
```

(`serde_json` is already a dev-dep on `snitchwatch-bridge` from earlier plans.)

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test -p snitchwatch-bridge --test flatpak_manifest_shape tauri_conf_bundles_packaging_resources`
Expected: FAIL — `bundle.resources` does not include `packaging`.

- [ ] **Step 3: Edit `crates/snitchwatch-tauri/tauri.conf.json`**

Locate the `"bundle"` object and add (or extend) the `"resources"` array. After this edit, the relevant block should look like:

```json
"bundle": {
  "active": true,
  "targets": "all",
  "identifier": "org.snitchwatch.Snitchwatch",
  "resources": [
    "../../web/**",
    "../../packaging/**"
  ],
  "icon": [
    "../../packaging/flatpak/icons/snitchwatch.svg"
  ]
}
```

(If `bundle.resources` already exists from Plan 4, only add the `"../../packaging/**"` entry. Do not delete other entries.)

- [ ] **Step 4: Run the test, verify it passes**

Run: `cargo test -p snitchwatch-bridge --test flatpak_manifest_shape tauri_conf_bundles_packaging_resources`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-tauri/tauri.conf.json crates/snitchwatch-bridge/tests/flatpak_manifest_shape.rs
git commit -m "feat(tauri): bundle packaging/ as a Tauri resource for the installer"
```

---

## Part B — Quadlet unit + install.sh (Tasks 4–6)

### Task 4: Podman quadlet `.container` unit

**Files:**
- Create: `packaging/quadlet/snitchwatch-opensnitchd.container`
- Modify: `crates/snitchwatch-bridge/tests/flatpak_manifest_shape.rs` (rename to `packaging_shape.rs` for accuracy)

Actually keep the test file name as `flatpak_manifest_shape.rs` to avoid renaming a Cargo test target — just append more tests.

- [ ] **Step 1: Write the failing test**

Append to `crates/snitchwatch-bridge/tests/flatpak_manifest_shape.rs`:

```rust
#[test]
fn quadlet_unit_targets_opensnitch_container_with_required_caps() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/quadlet/snitchwatch-opensnitchd.container");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));

    for required in [
        "[Unit]",
        "Description=",
        "[Container]",
        "Image=ghcr.io/evilsocket/opensnitch",
        "Network=host",
        "PodmanArgs=",
        "--cap-add=NET_ADMIN",
        "--cap-add=NET_RAW",
        "--cap-add=SYS_PTRACE",
        "Volume=/lib/modules:/lib/modules:ro",
        "[Service]",
        "Restart=on-failure",
        "RestartSec=5s",
        "[Install]",
        "WantedBy=default.target",
    ] {
        assert!(body.contains(required), "quadlet missing `{required}`\nbody:\n{body}");
    }
}
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test -p snitchwatch-bridge --test flatpak_manifest_shape quadlet_unit_targets_opensnitch_container_with_required_caps`
Expected: FAIL — file does not exist.

- [ ] **Step 3: Write the quadlet unit**

Create `packaging/quadlet/snitchwatch-opensnitchd.container`:

```ini
[Unit]
Description=Snitchwatch OpenSnitch daemon (podman quadlet)
After=network-online.target
Wants=network-online.target

[Container]
Image=ghcr.io/evilsocket/opensnitch:latest
ContainerName=snitchwatch-opensnitchd
Network=host
PodmanArgs=--privileged --cap-add=NET_ADMIN --cap-add=NET_RAW --cap-add=SYS_PTRACE
Volume=/lib/modules:/lib/modules:ro
Volume=%h/.config/opensnitch:/etc/opensnitchd:ro
Volume=%h/.local/share/opensnitch:/var/lib/opensnitchd:rw
Environment=OPENSNITCHD_LISTEN=127.0.0.1:50051

[Service]
Restart=on-failure
RestartSec=5s
TimeoutStartSec=30s

[Install]
WantedBy=default.target
```

- [ ] **Step 4: Run the test, verify it passes**

Run: `cargo test -p snitchwatch-bridge --test flatpak_manifest_shape quadlet_unit_targets_opensnitch_container_with_required_caps`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add packaging/quadlet/snitchwatch-opensnitchd.container crates/snitchwatch-bridge/tests/flatpak_manifest_shape.rs
git commit -m "feat(packaging): podman quadlet unit for opensnitchd"
```

---

### Task 5: `install.sh` — idempotent installer with `--dry-run` and `--daemon-only`

**Files:**
- Create: `packaging/install.sh`
- Create: `packaging/install.bats`

The install script must be safe to run twice. It must surface errors clearly. Test it via `bats-core` driving `--dry-run` so the test never touches the host's flatpak/systemctl state.

We do NOT install bats as a dev dep — instead the test runs only when the `bats` binary is on PATH. The `package-test` justfile recipe (Task 14) is what calls it. Cargo CI doesn't gate on it. The `install.bats` file is documented as runnable via `bats packaging/install.bats`.

- [ ] **Step 1: Write the failing test (bats)**

Create `packaging/install.bats`:

```bash
#!/usr/bin/env bats

setup() {
    SCRIPT="$BATS_TEST_DIRNAME/install.sh"
}

@test "install.sh exists and is executable" {
    [ -x "$SCRIPT" ]
}

@test "install.sh --help shows usage and exits 0" {
    run "$SCRIPT" --help
    [ "$status" -eq 0 ]
    [[ "$output" == *"Usage: install.sh"* ]]
    [[ "$output" == *"--daemon-only"* ]]
    [[ "$output" == *"--dry-run"* ]]
}

@test "install.sh --dry-run prints flatpak install command" {
    run "$SCRIPT" --dry-run
    [ "$status" -eq 0 ]
    [[ "$output" == *"flatpak install --user"* ]]
    [[ "$output" == *"snitchwatch-opensnitchd.container"* ]]
    [[ "$output" == *"systemctl --user daemon-reload"* ]]
    [[ "$output" == *"systemctl --user start snitchwatch-opensnitchd.service"* ]]
}

@test "install.sh --dry-run --daemon-only skips the flatpak install step" {
    run "$SCRIPT" --dry-run --daemon-only
    [ "$status" -eq 0 ]
    [[ "$output" != *"flatpak install"* ]]
    [[ "$output" == *"snitchwatch-opensnitchd.container"* ]]
    [[ "$output" == *"systemctl --user daemon-reload"* ]]
}

@test "install.sh --dry-run prints final OK marker" {
    run "$SCRIPT" --dry-run
    [ "$status" -eq 0 ]
    [[ "$output" == *"OK: install complete"* ]]
}

@test "install.sh rejects unknown flags" {
    run "$SCRIPT" --bogus
    [ "$status" -ne 0 ]
    [[ "$output" == *"unknown flag"* ]]
}
```

Mark it executable: `chmod +x packaging/install.bats` (the `#!/usr/bin/env bats` line plus the bit make it directly runnable).

- [ ] **Step 2: Run the test, verify it fails**

Run: `bats packaging/install.bats`
Expected: FAIL on the first `[ -x "$SCRIPT" ]` check (file does not exist).

If `bats` is not installed, document this — the script-shape test in Step 4 (Rust) is the CI-enforced version. Skip to Step 3.

- [ ] **Step 3: Write `install.sh`**

Create `packaging/install.sh`:

```bash
#!/usr/bin/env bash
#
# Snitchwatch installer.
#
# Lays down two things on a user-writable Bazzite host:
#   1. The Snitchwatch Flatpak (org.snitchwatch.Snitchwatch).
#   2. The OpenSnitch podman quadlet (snitchwatch-opensnitchd.container).
#
# Idempotent: safe to run twice. The wizard's "Install daemon" button calls
# this with --daemon-only when the GUI is already installed.

set -euo pipefail

DRY_RUN=0
DAEMON_ONLY=0
FLATPAK_BUNDLE=""

usage() {
    cat <<EOF
Usage: install.sh [--dry-run] [--daemon-only] [--bundle PATH] [--help]

  --dry-run       Print every command that would be run, do nothing.
  --daemon-only   Skip the Flatpak install step (used by the GUI's
                  first-run wizard when only the daemon is missing).
  --bundle PATH   Install the local .flatpak bundle at PATH instead of
                  pulling from a remote.
  --help          Show this message and exit.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)     DRY_RUN=1 ;;
        --daemon-only) DAEMON_ONLY=1 ;;
        --bundle)      shift; FLATPAK_BUNDLE="$1" ;;
        --help|-h)     usage; exit 0 ;;
        *)
            echo "install.sh: unknown flag: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
    shift
done

run() {
    if [[ $DRY_RUN -eq 1 ]]; then
        printf '+ %s\n' "$*"
    else
        "$@"
    fi
}

require_cmd() {
    if [[ $DRY_RUN -eq 1 ]]; then
        return 0
    fi
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "install.sh: required command not found on PATH: $1" >&2
        exit 3
    fi
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
QUADLET_SRC="$SCRIPT_DIR/quadlet/snitchwatch-opensnitchd.container"
QUADLET_DEST_DIR="${HOME}/.config/containers/systemd"

require_cmd podman
require_cmd systemctl

if [[ $DAEMON_ONLY -eq 0 ]]; then
    require_cmd flatpak
    if [[ -n "$FLATPAK_BUNDLE" ]]; then
        run flatpak install --user --noninteractive --or-update "$FLATPAK_BUNDLE"
    else
        run flatpak install --user --noninteractive --or-update flathub org.snitchwatch.Snitchwatch
    fi
fi

run mkdir -p "$QUADLET_DEST_DIR"
run install -m 0644 "$QUADLET_SRC" "$QUADLET_DEST_DIR/snitchwatch-opensnitchd.container"
run systemctl --user daemon-reload
run systemctl --user start snitchwatch-opensnitchd.service

echo "OK: install complete"
```

Mark it executable:

```bash
chmod +x packaging/install.sh
```

- [ ] **Step 4: Write a Rust shape test (CI-enforced)**

Append to `crates/snitchwatch-bridge/tests/flatpak_manifest_shape.rs`:

```rust
#[test]
fn install_sh_handles_required_flags() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/install.sh");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    for needle in [
        "set -euo pipefail",
        "--dry-run",
        "--daemon-only",
        "--bundle",
        "flatpak install --user",
        "systemctl --user daemon-reload",
        "systemctl --user start snitchwatch-opensnitchd.service",
        "snitchwatch-opensnitchd.container",
        "OK: install complete",
    ] {
        assert!(
            body.contains(needle),
            "install.sh missing snippet `{needle}`\nbody:\n{body}"
        );
    }
}

#[test]
fn install_sh_is_marked_executable() {
    use std::os::unix::fs::PermissionsExt;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/install.sh");
    let meta = std::fs::metadata(&path).expect("install.sh must exist");
    let mode = meta.permissions().mode();
    assert!(
        mode & 0o111 != 0,
        "install.sh must be executable (mode = {:o})",
        mode
    );
}
```

- [ ] **Step 5: Run the tests, verify they pass**

Run: `cargo test -p snitchwatch-bridge --test flatpak_manifest_shape install_sh`
Expected: PASS — 2 new tests.

If `bats` is on PATH, also run: `bats packaging/install.bats`
Expected: PASS — 6 tests.

- [ ] **Step 6: Commit**

```bash
git add packaging/install.sh packaging/install.bats crates/snitchwatch-bridge/tests/flatpak_manifest_shape.rs
git commit -m "feat(packaging): idempotent install.sh with --dry-run/--daemon-only + bats + Rust shape tests"
```

---

### Task 6: `packaging/README.md` and shellcheck-clean install.sh

**Files:**
- Create: `packaging/README.md`
- Modify: `packaging/install.sh` (only if shellcheck flags issues)

This task is a clean-up + documentation pass. The Rust test in Task 5 covers shape; this task ensures the script is shellcheck-clean and that someone reading `packaging/README.md` can install Snitchwatch from a release tarball without reading any other docs.

- [ ] **Step 1: Write the failing test**

Append to `crates/snitchwatch-bridge/tests/flatpak_manifest_shape.rs`:

```rust
#[test]
fn packaging_readme_documents_install_uninstall_troubleshoot() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/README.md");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    for heading in ["## Install", "## Uninstall", "## Troubleshoot"] {
        assert!(body.contains(heading), "packaging/README.md missing `{heading}`");
    }
    assert!(body.contains("./packaging/install.sh"));
    assert!(body.contains("systemctl --user stop snitchwatch-opensnitchd.service"));
    assert!(body.contains("flatpak uninstall --user org.snitchwatch.Snitchwatch"));
}
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test -p snitchwatch-bridge --test flatpak_manifest_shape packaging_readme`
Expected: FAIL.

- [ ] **Step 3: Write `packaging/README.md`**

Create `packaging/README.md`:

```markdown
# Snitchwatch Packaging

This directory contains everything needed to install Snitchwatch on a
Bazzite (or any Universal Blue / immutable Fedora) host without modifying
`/usr` or adding an `rpm-ostree` overlay.

Two artifacts ship together:

1. **Snitchwatch Flatpak** — the GUI itself (`org.snitchwatch.Snitchwatch`).
2. **OpenSnitch podman quadlet** — the daemon as a system-managed
   container (`snitchwatch-opensnitchd.container`).

## Install

From a release tarball:

```bash
./packaging/install.sh
```

That runs four steps idempotently:

1. `flatpak install --user --noninteractive --or-update flathub org.snitchwatch.Snitchwatch`
2. `mkdir -p ~/.config/containers/systemd && install ... snitchwatch-opensnitchd.container`
3. `systemctl --user daemon-reload`
4. `systemctl --user start snitchwatch-opensnitchd.service`

To install from a local `.flatpak` bundle (e.g., a CI artifact):

```bash
./packaging/install.sh --bundle ./build/snitchwatch.flatpak
```

To install only the daemon (the first-run wizard's "I have the GUI, install
the daemon" path uses this):

```bash
./packaging/install.sh --daemon-only
```

To preview every command without running it:

```bash
./packaging/install.sh --dry-run
```

## Uninstall

```bash
systemctl --user stop snitchwatch-opensnitchd.service
systemctl --user disable snitchwatch-opensnitchd.service || true
rm -f ~/.config/containers/systemd/snitchwatch-opensnitchd.container
systemctl --user daemon-reload
flatpak uninstall --user org.snitchwatch.Snitchwatch
rm -rf ~/.local/share/snitchwatch ~/.config/snitchwatch
```

## Troubleshoot

| Symptom | Probable cause | Fix |
| --- | --- | --- |
| `flatpak: command not found` | Bazzite normally ships flatpak; on a stripped image, install it: `rpm-ostree install flatpak` and reboot. | one-time |
| `Failed to start snitchwatch-opensnitchd.service: Unit ... not found.` | Quadlet generator did not pick up the new file. | `systemctl --user daemon-reload` then retry. |
| `podman: command not found` | Bazzite ships podman by default; on a stripped image, `rpm-ostree install podman`. | one-time |
| Service starts then immediately exits | OpenSnitch image refused to load the eBPF probes (kernel mismatch). Check `journalctl --user -u snitchwatch-opensnitchd.service`. | host-specific; the daemon log usually says exactly which probe failed. |
| GUI launches but stays on the wizard | gRPC dial succeeded but the unit is not active. Open the Diagnostics tab in the GUI for the daemon log excerpt. | rerun `systemctl --user start snitchwatch-opensnitchd.service` |

## Files

- `flatpak/org.snitchwatch.Snitchwatch.yml` — flatpak-builder manifest.
- `flatpak/org.snitchwatch.Snitchwatch.desktop` — `.desktop` entry.
- `flatpak/org.snitchwatch.Snitchwatch.metainfo.xml` — AppStream metadata.
- `flatpak/icons/snitchwatch.svg` — placeholder icon (designer pass deferred).
- `quadlet/snitchwatch-opensnitchd.container` — systemd `.container` unit.
- `install.sh` — installer script.
- `install.bats` — bats-core unit tests for `install.sh`.
```

- [ ] **Step 4: Run shellcheck (if available) and fix any warnings**

Run: `shellcheck packaging/install.sh`
Expected: no warnings. If warnings appear, fix them inline (most likely candidates are SC2086 unquoted expansions or SC2155 declare-and-assign — fix by quoting and splitting). The script as written is shellcheck-clean.

If `shellcheck` is not on PATH, skip; the Rust test in Task 5 already covers structural shape.

- [ ] **Step 5: Run the test, verify it passes**

Run: `cargo test -p snitchwatch-bridge --test flatpak_manifest_shape packaging_readme`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add packaging/README.md crates/snitchwatch-bridge/tests/flatpak_manifest_shape.rs
git commit -m "docs(packaging): README with install/uninstall/troubleshoot tables"
```

---

## Part C — Wire wizard install button to real install.sh (Tasks 7–9)

### Task 7: `installer::host_spawn` — pure decision function

**Files:**
- Create: `crates/snitchwatch-tauri/src/installer/host_spawn.rs`
- Modify: `crates/snitchwatch-tauri/src/lib.rs` (declare `pub mod installer;`)
- Create: `crates/snitchwatch-tauri/src/installer.rs` (re-export)

The host-spawn decision is small but easy to get wrong (wrong arg order, missing `--watch-bus`, wrong env passthrough). Make it a pure function that takes an `IsFlatpak` flag + a script path and returns a `std::process::Command`. Test it without spawning anything.

- [ ] **Step 1: Write the failing test**

Create `crates/snitchwatch-tauri/src/installer/host_spawn.rs`:

```rust
//! Pure decision function: given runtime context, build the right
//! `Command` to execute the installer script. Tested without spawning.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsFlatpak(pub bool);

impl IsFlatpak {
    pub fn detect() -> Self {
        Self(std::env::var_os("FLATPAK_ID").is_some())
    }
}

#[derive(Debug, Clone)]
pub struct InstallerInvocation {
    pub program: String,
    pub args: Vec<String>,
    pub script: PathBuf,
}

impl InstallerInvocation {
    pub fn build(is_flatpak: IsFlatpak, script: &Path, daemon_only: bool) -> Self {
        let mut args: Vec<String> = Vec::new();
        let program: String;

        if is_flatpak.0 {
            program = "flatpak-spawn".into();
            args.push("--host".into());
            args.push("--watch-bus".into());
            args.push(script.to_string_lossy().into_owned());
        } else {
            program = script.to_string_lossy().into_owned();
        }

        if daemon_only {
            args.push("--daemon-only".into());
        }

        Self {
            program,
            args,
            script: script.to_path_buf(),
        }
    }

    pub fn into_command(self) -> Command {
        let mut cmd = Command::new(self.program);
        cmd.args(self.args);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_flatpak_runs_script_directly() {
        let inv = InstallerInvocation::build(
            IsFlatpak(false),
            Path::new("/abs/path/install.sh"),
            false,
        );
        assert_eq!(inv.program, "/abs/path/install.sh");
        assert!(inv.args.is_empty());
    }

    #[test]
    fn outside_flatpak_with_daemon_only_passes_flag() {
        let inv = InstallerInvocation::build(
            IsFlatpak(false),
            Path::new("/abs/path/install.sh"),
            true,
        );
        assert_eq!(inv.program, "/abs/path/install.sh");
        assert_eq!(inv.args, vec!["--daemon-only".to_string()]);
    }

    #[test]
    fn inside_flatpak_uses_host_spawn_with_watch_bus() {
        let inv = InstallerInvocation::build(
            IsFlatpak(true),
            Path::new("/app/share/snitchwatch/packaging/install.sh"),
            false,
        );
        assert_eq!(inv.program, "flatpak-spawn");
        assert_eq!(
            inv.args,
            vec![
                "--host".to_string(),
                "--watch-bus".to_string(),
                "/app/share/snitchwatch/packaging/install.sh".to_string(),
            ]
        );
    }

    #[test]
    fn inside_flatpak_with_daemon_only_appends_flag_after_script() {
        let inv = InstallerInvocation::build(
            IsFlatpak(true),
            Path::new("/app/share/snitchwatch/packaging/install.sh"),
            true,
        );
        assert_eq!(
            inv.args,
            vec![
                "--host".to_string(),
                "--watch-bus".to_string(),
                "/app/share/snitchwatch/packaging/install.sh".to_string(),
                "--daemon-only".to_string(),
            ]
        );
    }
}
```

Create `crates/snitchwatch-tauri/src/installer.rs` as a stub that re-exports the submodule:

```rust
//! Installer command surface.
//!
//! Owns the `install_daemon` Tauri command (Task 8) and the pure-function
//! command builder in `host_spawn` (Task 7).

pub mod host_spawn;

pub use host_spawn::{InstallerInvocation, IsFlatpak};
```

Modify `crates/snitchwatch-tauri/src/lib.rs` — add `pub mod installer;` next to the existing `pub mod wizard;` declaration.

- [ ] **Step 2: Run the tests, verify they pass**

Run: `cargo test -p snitchwatch-tauri installer::host_spawn::tests`
Expected: PASS — 4 tests.

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-tauri/src/installer.rs crates/snitchwatch-tauri/src/installer/host_spawn.rs crates/snitchwatch-tauri/src/lib.rs
git commit -m "feat(tauri): installer host-spawn decision function with unit tests"
```

---

### Task 8: `install_daemon` Tauri command — streams stdout via Tauri events

**Files:**
- Modify: `crates/snitchwatch-tauri/src/installer.rs`
- Modify: `crates/snitchwatch-tauri/src/wizard.rs` (delete `install_daemon_stub`)
- Modify: `crates/snitchwatch-tauri/src/main.rs` (register `install_daemon` in invoke handler)

- [ ] **Step 1: Write the failing test**

Append to `crates/snitchwatch-tauri/src/installer.rs` (after the `pub use` line):

```rust
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProgressLine {
    pub stream: &'static str, // "stdout" | "stderr"
    pub line: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct InstallerResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub last_lines: Vec<String>,
}

const TAIL_LINES: usize = 20;

#[derive(Debug, Default)]
pub struct ProgressBuffer {
    pub lines: Vec<ProgressLine>,
}

impl ProgressBuffer {
    pub fn push(&mut self, stream: &'static str, line: String) {
        self.lines.push(ProgressLine { stream, line });
    }

    pub fn last_n(&self, n: usize) -> Vec<String> {
        let start = self.lines.len().saturating_sub(n);
        self.lines[start..]
            .iter()
            .map(|p| format!("[{}] {}", p.stream, p.line))
            .collect()
    }
}

pub type SharedProgress = Arc<Mutex<ProgressBuffer>>;

/// Run the installer subprocess and pipe its stdout/stderr line-by-line into
/// `on_line`. Returns the final result on exit.
///
/// `on_line` is invoked from the tokio runtime; in production it emits a
/// Tauri event, in tests it pushes into a `ProgressBuffer`.
pub async fn run_installer<F>(
    invocation: InstallerInvocation,
    mut on_line: F,
) -> InstallerResult
where
    F: FnMut(&ProgressLine) + Send,
{
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command as TokioCommand;

    let mut cmd = TokioCommand::new(invocation.program.clone());
    cmd.args(&invocation.args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let line = ProgressLine {
                stream: "stderr",
                line: format!("failed to spawn installer: {e}"),
            };
            on_line(&line);
            return InstallerResult {
                success: false,
                exit_code: None,
                last_lines: vec![line.line],
            };
        }
    };

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut buffer = ProgressBuffer::default();

    let mut stdout_reader = BufReader::new(stdout).lines();
    let mut stderr_reader = BufReader::new(stderr).lines();

    loop {
        tokio::select! {
            line = stdout_reader.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        let p = ProgressLine { stream: "stdout", line: l };
                        on_line(&p);
                        buffer.push("stdout", p.line);
                    }
                    _ => break,
                }
            }
            line = stderr_reader.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        let p = ProgressLine { stream: "stderr", line: l };
                        on_line(&p);
                        buffer.push("stderr", p.line);
                    }
                    _ => break,
                }
            }
        }
    }

    // Drain the slower stream that may still have lines.
    while let Ok(Some(l)) = stdout_reader.next_line().await {
        let p = ProgressLine { stream: "stdout", line: l };
        on_line(&p);
        buffer.push("stdout", p.line);
    }
    while let Ok(Some(l)) = stderr_reader.next_line().await {
        let p = ProgressLine { stream: "stderr", line: l };
        on_line(&p);
        buffer.push("stderr", p.line);
    }

    let exit = child.wait().await.ok();
    let code = exit.as_ref().and_then(|s| s.code());
    let success = exit.as_ref().map(|s| s.success()).unwrap_or(false);

    InstallerResult {
        success,
        exit_code: code,
        last_lines: buffer.last_n(TAIL_LINES),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fake_installer_script(body: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(".sh")
            .tempfile()
            .expect("tmp");
        std::io::Write::write_all(
            &mut f,
            format!("#!/usr/bin/env bash\nset -e\n{body}\n").as_bytes(),
        )
        .unwrap();
        let path = f.path().to_path_buf();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        f
    }

    #[tokio::test]
    async fn run_installer_streams_stdout_lines_and_returns_success() {
        let script = fake_installer_script(
            "echo line-1\necho line-2\necho line-3\necho 'OK: install complete'",
        );
        let inv = InstallerInvocation::build(IsFlatpak(false), script.path(), false);

        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let captured_clone = captured.clone();
        let result = run_installer(inv, move |p| {
            captured_clone.lock().unwrap().push(p.line.clone());
        })
        .await;

        assert!(result.success, "expected success, got {result:?}");
        assert_eq!(result.exit_code, Some(0));

        let lines = captured.lock().unwrap().clone();
        assert!(lines.iter().any(|l| l == "line-1"));
        assert!(lines.iter().any(|l| l == "line-2"));
        assert!(lines.iter().any(|l| l == "line-3"));
        assert!(lines.iter().any(|l| l.contains("OK: install complete")));
    }

    #[tokio::test]
    async fn run_installer_captures_failure_with_last_20_lines() {
        let script = fake_installer_script("echo bad-line\nexit 7");
        let inv = InstallerInvocation::build(IsFlatpak(false), script.path(), false);

        let result = run_installer(inv, |_| {}).await;
        assert!(!result.success);
        assert_eq!(result.exit_code, Some(7));
        assert!(result.last_lines.iter().any(|l| l.contains("bad-line")));
    }

    #[tokio::test]
    async fn run_installer_handles_spawn_failure_gracefully() {
        let inv = InstallerInvocation {
            program: "/no/such/binary/anywhere".into(),
            args: vec![],
            script: Path::new("/no/such/binary/anywhere").to_path_buf(),
        };
        let result = run_installer(inv, |_| {}).await;
        assert!(!result.success);
        assert_eq!(result.exit_code, None);
        assert!(result.last_lines.iter().any(|l| l.contains("failed to spawn")));
    }
}
```

Add to `crates/snitchwatch-tauri/Cargo.toml` `[dev-dependencies]`:

```toml
tempfile = "3"
```

(`tokio` is already a runtime dep with the macros feature; `tokio::process` ships in the `process` feature — verify it's enabled in the existing tokio dependency line, add `"process"` to the features list if not.)

- [ ] **Step 2: Run the tests, verify they fail**

Run: `cargo test -p snitchwatch-tauri installer::tests`
Expected: FAIL — `run_installer` is undefined or `tokio::process::Command` is unavailable. Add `process` to the tokio features list to fix the second; the first is fixed by the implementation block we already pasted (which IS the implementation — this is the stub-then-implement pattern from Plan 5 Task 6 where the failing-test step proves the test compiles against the still-incomplete public surface).

In practice for this task: write the test bodies first, run them, watch them fail to compile (`run_installer` not found), then fill in the implementation immediately above and re-run.

- [ ] **Step 3: Add the Tauri command wrapper**

Append to `crates/snitchwatch-tauri/src/installer.rs`:

```rust
#[tauri::command]
pub async fn install_daemon(
    app: tauri::AppHandle,
    daemon_only: bool,
) -> Result<InstallerResult, String> {
    use tauri::Emitter;

    let resource_dir = app
        .path()
        .resource_dir()
        .map_err(|e| format!("resource_dir: {e}"))?;
    let script = resource_dir.join("packaging").join("install.sh");
    if !script.exists() {
        return Err(format!("install.sh not found at {}", script.display()));
    }

    let invocation = InstallerInvocation::build(IsFlatpak::detect(), &script, daemon_only);

    let app_for_emit = app.clone();
    let result = run_installer(invocation, move |p| {
        let _ = app_for_emit.emit("installer://progress", p.clone());
    })
    .await;

    let _ = app.emit("installer://done", result.clone());
    Ok(result)
}
```

Add the imports at the top of `installer.rs`:

```rust
use tauri::Manager; // for app.path()
```

- [ ] **Step 4: Wire `install_daemon` into the Tauri builder**

Modify `crates/snitchwatch-tauri/src/main.rs`. Locate the `tauri::generate_handler!` macro invocation that already lists `install_daemon_stub` (added in Plan 4). Replace `install_daemon_stub` with `crate::installer::install_daemon`:

```rust
.invoke_handler(tauri::generate_handler![
    crate::wizard::detect_daemon_state,
    crate::installer::install_daemon,
    crate::commands::start_daemon_unit,
    crate::commands::read_crash_log,
    crate::commands::read_autostart_state_command,
    crate::commands::write_autostart_command,
    crate::commands::remove_autostart_command,
])
```

Modify `crates/snitchwatch-tauri/src/wizard.rs`: delete the `install_daemon_stub` function and its `#[tauri::command]` attribute (it lived between `start_daemon_unit` and the `#[cfg(test)]` block per Plan 4 Task 13).

- [ ] **Step 5: Run the full Tauri crate tests**

Run: `cargo test -p snitchwatch-tauri`
Expected: PASS — the 3 new `installer::tests` plus all existing tests from Plans 4/5.

- [ ] **Step 6: Commit**

```bash
git add crates/snitchwatch-tauri/Cargo.toml crates/snitchwatch-tauri/src/installer.rs crates/snitchwatch-tauri/src/wizard.rs crates/snitchwatch-tauri/src/main.rs
git commit -m "feat(tauri): install_daemon Tauri command streams installer output via events"
```

---

### Task 9: Front-end installer overlay + Playwright smoke

**Files:**
- Modify: `web/onboarding.js`
- Modify: `web/onboarding.css`
- Create: `tests/tauri_smoke/tests/install_button.spec.ts`

- [ ] **Step 1: Write the failing test (Playwright)**

Create `tests/tauri_smoke/tests/install_button.spec.ts`:

```typescript
import { test, expect, _electron as electron } from '@playwright/test';
import { mkdtempSync, writeFileSync, chmodSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

// This test boots the Tauri shell with a fake install.sh that prints 5
// progress lines and exits 0. The wizard should render the unit_missing
// branch (no daemon), the user clicks Install, and the overlay should
// stream the 5 lines and end with the "Install complete" CTA.

test('install button streams installer output and reports success', async () => {
  // Build a fake packaging dir that the Tauri shell will pick up via a
  // SNITCHWATCH_PACKAGING_OVERRIDE env var honored by installer.rs in dev.
  const dir = mkdtempSync(join(tmpdir(), 'snitchwatch-installer-'));
  const pkgDir = join(dir, 'packaging');
  require('node:fs').mkdirSync(pkgDir);
  const scriptPath = join(pkgDir, 'install.sh');
  writeFileSync(
    scriptPath,
    `#!/usr/bin/env bash
echo step-1
echo step-2
echo step-3
echo step-4
echo "OK: install complete"
exit 0
`,
  );
  chmodSync(scriptPath, 0o755);

  const app = await electron.launch({
    args: ['./target/debug/snitchwatch-tauri'],
    env: {
      ...process.env,
      SNITCHWATCH_PACKAGING_OVERRIDE: pkgDir,
    },
  });
  const page = await app.firstWindow();

  // Wizard renders the unit_missing branch because no mock daemon is
  // dialing in.
  await expect(page.locator('[data-onboarding-branch="unit_missing"]')).toBeVisible({
    timeout: 5000,
  });

  await page.locator('button[data-action="install-daemon"]').click();

  // Progress overlay must show all 5 lines.
  for (const expected of ['step-1', 'step-2', 'step-3', 'step-4', 'OK: install complete']) {
    await expect(page.locator('.installer-progress')).toContainText(expected, {
      timeout: 5000,
    });
  }

  // Final state: success CTA.
  await expect(page.locator('button[data-action="installer-dismiss"]')).toBeVisible();

  await app.close();
});
```

- [ ] **Step 2: Add the env-var override hook in `installer.rs`**

Edit the `install_daemon` Tauri command. Above the `resource_dir` call, insert:

```rust
let override_dir = std::env::var_os("SNITCHWATCH_PACKAGING_OVERRIDE").map(PathBuf::from);
let script = if let Some(p) = override_dir {
    p.join("install.sh")
} else {
    let resource_dir = app
        .path()
        .resource_dir()
        .map_err(|e| format!("resource_dir: {e}"))?;
    resource_dir.join("packaging").join("install.sh")
};
```

(Replace the existing `resource_dir`/`script` block with this if-let. The override is documented as dev/test only — production runs inside the Flatpak where the env var is unset.)

- [ ] **Step 3: Wire the overlay into `web/onboarding.js`**

Locate the `installDaemon()` placeholder from Plan 4 Task 13b and replace its body. The function should:

1. Show an inline `.installer-progress` `<pre>` element inside the wizard overlay.
2. Subscribe to the `installer://progress` Tauri event and append each line.
3. Call `window.__TAURI__.core.invoke('install_daemon', { daemonOnly: false })`.
4. On the resolved promise, replace the in-flight spinner with either a "Done — close" CTA (success) or a "Failed — copy log" CTA (failure).

Edit `web/onboarding.js`:

```javascript
async function installDaemon() {
  const overlay = document.querySelector('.onboarding-overlay');
  const progress = document.createElement('pre');
  progress.className = 'installer-progress';
  overlay.querySelector('.onboarding-actions').replaceChildren(progress);

  const tauri = window.__TAURI__;
  if (!tauri) {
    progress.textContent =
      'Installer is only available inside the Snitchwatch desktop app.';
    return;
  }

  const unlisten = await tauri.event.listen('installer://progress', (e) => {
    progress.textContent += `${e.payload.line}\n`;
    progress.scrollTop = progress.scrollHeight;
  });

  let result;
  try {
    result = await tauri.core.invoke('install_daemon', { daemonOnly: false });
  } catch (err) {
    progress.textContent += `\n[error] ${err}\n`;
    unlisten();
    appendCta(overlay, 'installer-dismiss', 'Failed — copy log', () => {
      navigator.clipboard.writeText(progress.textContent);
    });
    return;
  }
  unlisten();

  if (result.success) {
    appendCta(overlay, 'installer-dismiss', 'Done — close', () => {
      overlay.remove();
    });
  } else {
    appendCta(overlay, 'installer-dismiss', 'Failed — copy log', () => {
      navigator.clipboard.writeText(progress.textContent);
    });
  }
}

function appendCta(overlay, action, label, onClick) {
  const btn = document.createElement('button');
  btn.dataset.action = action;
  btn.textContent = label;
  btn.addEventListener('click', onClick);
  overlay.querySelector('.onboarding-actions').appendChild(btn);
}
```

(If `installDaemon` is referenced elsewhere as a default export or by name, keep that intact. The above replaces only the body.)

- [ ] **Step 4: Add `.installer-progress` styles**

Append to `web/onboarding.css`:

```css
.installer-progress {
  background: #0d0d12;
  color: #e8e8f0;
  border: 1px solid #2a2a35;
  border-radius: 6px;
  font-family: 'JetBrains Mono', ui-monospace, monospace;
  font-size: 12px;
  line-height: 1.45;
  max-height: 240px;
  overflow-y: auto;
  padding: 12px 14px;
  margin: 16px 0;
  white-space: pre-wrap;
  word-break: break-all;
}
```

- [ ] **Step 5: Run the Playwright test**

Run: `just package-test` (from the recipe added in Task 14) — or directly:

```bash
cargo build -p snitchwatch-tauri
cd tests/tauri_smoke && pnpm playwright test install_button.spec.ts
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add web/onboarding.js web/onboarding.css crates/snitchwatch-tauri/src/installer.rs tests/tauri_smoke/tests/install_button.spec.ts
git commit -m "feat(web): installer progress overlay streaming Tauri events + Playwright smoke"
```

---

## Part D — Lifecycle probe + failure-mode banners (Tasks 10–13)

### Task 10: `probe_state` pure-function helper

**Files:**
- Create: `crates/snitchwatch-bridge/src/lifecycle.rs` (skeleton — `pub mod probe_state;` only)
- Create: `crates/snitchwatch-bridge/src/lifecycle/probe_state.rs`
- Modify: `crates/snitchwatch-bridge/src/lib.rs` (`pub mod lifecycle;`)

The lifecycle module is the load-bearing one for failure-mode banners. Build it bottom-up: first the pure decision function, then the polling loop, then the WS forwarding.

- [ ] **Step 1: Write the failing test**

Create `crates/snitchwatch-bridge/src/lifecycle/probe_state.rs`:

```rust
//! Pure decision function: derive a `LifecycleKind` from the latest gRPC
//! and systemctl probe results.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LifecycleSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum LifecycleKind {
    DaemonOk,
    UnitMissing,
    UnitInactive,
    GrpcUnreachable,
    GrpcStaleStream,
    EventFloodDropped { dropped: u32 },
    BridgePanicRecovered,
    BlocklistFetchFailed { list_id: String, reason: String },
    KernelHookFailed { excerpt: String },
    StateDivergenceReconciled,
}

impl LifecycleKind {
    pub fn severity(&self) -> LifecycleSeverity {
        match self {
            Self::DaemonOk | Self::StateDivergenceReconciled => LifecycleSeverity::Info,
            Self::UnitInactive | Self::EventFloodDropped { .. } | Self::BridgePanicRecovered => {
                LifecycleSeverity::Warning
            }
            Self::UnitMissing
            | Self::GrpcUnreachable
            | Self::GrpcStaleStream
            | Self::BlocklistFetchFailed { .. }
            | Self::KernelHookFailed { .. } => LifecycleSeverity::Error,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemctlState {
    Active,
    Inactive,
    Missing,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeInputs {
    pub grpc_ok: bool,
    pub systemctl: SystemctlState,
}

/// Map probe inputs → the next lifecycle kind. The order matters: the
/// "everything is fine" state must come last so a partial failure dominates.
pub fn next_kind(inputs: ProbeInputs) -> LifecycleKind {
    match (inputs.grpc_ok, inputs.systemctl) {
        (true, _) => LifecycleKind::DaemonOk,
        (false, SystemctlState::Missing) => LifecycleKind::UnitMissing,
        (false, SystemctlState::Inactive) => LifecycleKind::UnitInactive,
        (false, SystemctlState::Active) => LifecycleKind::GrpcUnreachable,
        (false, SystemctlState::Unknown) => LifecycleKind::GrpcUnreachable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_ok_means_daemon_ok_regardless_of_systemctl() {
        for s in [
            SystemctlState::Active,
            SystemctlState::Inactive,
            SystemctlState::Missing,
            SystemctlState::Unknown,
        ] {
            assert_eq!(
                next_kind(ProbeInputs { grpc_ok: true, systemctl: s }),
                LifecycleKind::DaemonOk
            );
        }
    }

    #[test]
    fn grpc_down_unit_missing_yields_unit_missing() {
        assert_eq!(
            next_kind(ProbeInputs { grpc_ok: false, systemctl: SystemctlState::Missing }),
            LifecycleKind::UnitMissing
        );
    }

    #[test]
    fn grpc_down_unit_inactive_yields_unit_inactive() {
        assert_eq!(
            next_kind(ProbeInputs { grpc_ok: false, systemctl: SystemctlState::Inactive }),
            LifecycleKind::UnitInactive
        );
    }

    #[test]
    fn grpc_down_unit_active_yields_grpc_unreachable() {
        assert_eq!(
            next_kind(ProbeInputs { grpc_ok: false, systemctl: SystemctlState::Active }),
            LifecycleKind::GrpcUnreachable
        );
    }

    #[test]
    fn severities_match_design_table() {
        assert_eq!(LifecycleKind::DaemonOk.severity(), LifecycleSeverity::Info);
        assert_eq!(LifecycleKind::UnitMissing.severity(), LifecycleSeverity::Error);
        assert_eq!(LifecycleKind::UnitInactive.severity(), LifecycleSeverity::Warning);
        assert_eq!(
            LifecycleKind::EventFloodDropped { dropped: 12 }.severity(),
            LifecycleSeverity::Warning
        );
        assert_eq!(
            LifecycleKind::BlocklistFetchFailed {
                list_id: "x".into(),
                reason: "x".into(),
            }
            .severity(),
            LifecycleSeverity::Error
        );
    }
}
```

Create `crates/snitchwatch-bridge/src/lifecycle.rs`:

```rust
//! Daemon lifecycle probe and failure-mode banner machinery.
//!
//! The probe polls (gRPC reachability, systemctl unit state) on a fixed
//! cadence and broadcasts typed `LifecycleEvent`s. The WS server (Task 12)
//! forwards events as `setLifecycleBanner` messages.

pub mod probe_state;

pub use probe_state::{LifecycleKind, LifecycleSeverity, ProbeInputs, SystemctlState};

#[derive(Debug, Clone)]
pub struct LifecycleEvent {
    pub kind: LifecycleKind,
}
```

Edit `crates/snitchwatch-bridge/src/lib.rs` — add `pub mod lifecycle;` (next to `pub mod blocklists;` from Plan 5).

- [ ] **Step 2: Run the tests, verify they pass**

Run: `cargo test -p snitchwatch-bridge lifecycle::probe_state::tests`
Expected: PASS — 5 tests.

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-bridge/src/lib.rs crates/snitchwatch-bridge/src/lifecycle.rs crates/snitchwatch-bridge/src/lifecycle/probe_state.rs
git commit -m "feat(bridge): lifecycle probe_state pure helper with severity table"
```

---

### Task 11: `LifecycleProbe` polling loop with broadcast bus

**Files:**
- Modify: `crates/snitchwatch-bridge/src/lifecycle.rs`
- Create: `crates/snitchwatch-bridge/tests/lifecycle_e2e.rs`

The polling loop must NOT depend on a real gRPC client or a real systemctl binary — both are injected behind small async traits so the e2e test can drive scripted state transitions on a controlled `tokio::time::pause()` clock.

- [ ] **Step 1: Write the failing integration test**

Create `crates/snitchwatch-bridge/tests/lifecycle_e2e.rs`:

```rust
//! End-to-end test for the lifecycle probe under tokio::time::pause().
//!
//! Drives a fake gRPC + fake systemctl through the four interesting state
//! transitions and asserts the broadcast bus emits the right events.

use snitchwatch_bridge::lifecycle::probe_state::{LifecycleKind, SystemctlState};
use snitchwatch_bridge::lifecycle::{
    GrpcLiveness, LifecycleEvent, LifecycleProbe, ProbeConfig, SystemctlLiveness,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::sync::broadcast;

#[derive(Default)]
struct FakeGrpc {
    state: AtomicU8, // 0 = down, 1 = up
}

#[async_trait::async_trait]
impl GrpcLiveness for FakeGrpc {
    async fn ping(&self) -> bool {
        self.state.load(Ordering::SeqCst) == 1
    }
}

#[derive(Default)]
struct FakeSystemctl {
    state: AtomicU8, // 0 = missing, 1 = inactive, 2 = active, 3 = unknown
}

#[async_trait::async_trait]
impl SystemctlLiveness for FakeSystemctl {
    async fn unit_state(&self) -> SystemctlState {
        match self.state.load(Ordering::SeqCst) {
            0 => SystemctlState::Missing,
            1 => SystemctlState::Inactive,
            2 => SystemctlState::Active,
            _ => SystemctlState::Unknown,
        }
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn probe_emits_unit_missing_then_unit_inactive_then_daemon_ok() {
    let grpc = Arc::new(FakeGrpc::default());
    let unit = Arc::new(FakeSystemctl::default());
    let (tx, mut rx) = broadcast::channel::<LifecycleEvent>(16);

    let cfg = ProbeConfig {
        interval: Duration::from_secs(5),
    };
    let probe = LifecycleProbe::new(cfg, grpc.clone(), unit.clone(), tx);
    let handle = tokio::spawn(probe.run());

    // tick 1 — grpc down, unit missing → UnitMissing
    tokio::time::advance(Duration::from_secs(5)).await;
    let evt = rx.recv().await.unwrap();
    assert!(matches!(evt.kind, LifecycleKind::UnitMissing));

    // user runs install.sh — unit becomes inactive (just-installed,
    // not yet started).
    unit.state.store(1, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(5)).await;
    let evt = rx.recv().await.unwrap();
    assert!(matches!(evt.kind, LifecycleKind::UnitInactive));

    // user clicks "Start it" — unit becomes active and grpc comes up.
    unit.state.store(2, Ordering::SeqCst);
    grpc.state.store(1, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(5)).await;
    let evt = rx.recv().await.unwrap();
    assert!(matches!(evt.kind, LifecycleKind::DaemonOk));

    handle.abort();
    let _ = handle.await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn probe_dedupes_consecutive_identical_kinds() {
    // The probe should NOT spam the bus with DaemonOk every 5s once it's
    // already DaemonOk. Only state *changes* are emitted.
    let grpc = Arc::new(FakeGrpc::default());
    grpc.state.store(1, Ordering::SeqCst);
    let unit = Arc::new(FakeSystemctl::default());
    unit.state.store(2, Ordering::SeqCst);
    let (tx, mut rx) = broadcast::channel::<LifecycleEvent>(16);

    let probe = LifecycleProbe::new(
        ProbeConfig { interval: Duration::from_secs(5) },
        grpc,
        unit,
        tx,
    );
    let handle = tokio::spawn(probe.run());

    tokio::time::advance(Duration::from_secs(5)).await;
    let first = rx.recv().await.unwrap();
    assert!(matches!(first.kind, LifecycleKind::DaemonOk));

    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::time::advance(Duration::from_secs(5)).await;

    // No more events delivered for the same state.
    let second = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;
    assert!(second.is_err(), "expected no second event, got {second:?}");

    handle.abort();
    let _ = handle.await;
}
```

Add to `crates/snitchwatch-bridge/Cargo.toml` `[dev-dependencies]`:

```toml
async-trait = { workspace = true }
```

(`async-trait` was already added as a workspace dep in Plan 5 Task 14 for `RuleSink`.)

- [ ] **Step 2: Run the tests, verify they fail**

Run: `cargo test -p snitchwatch-bridge --test lifecycle_e2e`
Expected: FAIL — `LifecycleProbe`, `GrpcLiveness`, `SystemctlLiveness`, `ProbeConfig` all undefined.

- [ ] **Step 3: Implement the probe**

Append to `crates/snitchwatch-bridge/src/lifecycle.rs`:

```rust
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

#[async_trait::async_trait]
pub trait GrpcLiveness: Send + Sync + 'static {
    async fn ping(&self) -> bool;
}

#[async_trait::async_trait]
pub trait SystemctlLiveness: Send + Sync + 'static {
    async fn unit_state(&self) -> SystemctlState;
}

#[derive(Debug, Clone, Copy)]
pub struct ProbeConfig {
    pub interval: Duration,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self { interval: Duration::from_secs(5) }
    }
}

pub struct LifecycleProbe {
    cfg: ProbeConfig,
    grpc: Arc<dyn GrpcLiveness>,
    systemctl: Arc<dyn SystemctlLiveness>,
    bus: broadcast::Sender<LifecycleEvent>,
}

impl LifecycleProbe {
    pub fn new(
        cfg: ProbeConfig,
        grpc: Arc<dyn GrpcLiveness>,
        systemctl: Arc<dyn SystemctlLiveness>,
        bus: broadcast::Sender<LifecycleEvent>,
    ) -> Self {
        Self { cfg, grpc, systemctl, bus }
    }

    pub async fn run(self) {
        let mut ticker = tokio::time::interval(self.cfg.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last: Option<LifecycleKind> = None;

        loop {
            ticker.tick().await;
            let inputs = ProbeInputs {
                grpc_ok: self.grpc.ping().await,
                systemctl: self.systemctl.unit_state().await,
            };
            let next = probe_state::next_kind(inputs);
            if last.as_ref() != Some(&next) {
                let _ = self.bus.send(LifecycleEvent { kind: next.clone() });
                last = Some(next);
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests, verify they pass**

Run: `cargo test -p snitchwatch-bridge --test lifecycle_e2e`
Expected: PASS — 2 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/Cargo.toml crates/snitchwatch-bridge/src/lifecycle.rs crates/snitchwatch-bridge/tests/lifecycle_e2e.rs
git commit -m "feat(bridge): LifecycleProbe with broadcast bus + dedupe + paused-clock e2e"
```

---

### Task 12: WS protocol — `setLifecycleBanner` + downstream/upstream wiring

**Files:**
- Modify: `crates/snitchwatch-bridge/src/ws_messages.rs`
- Modify: `crates/snitchwatch-bridge/src/translator/downstream.rs`
- Modify: `crates/snitchwatch-bridge/src/translator/upstream.rs`
- Modify: `crates/snitchwatch-bridge/src/ws_server.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/snitchwatch-bridge/src/translator/downstream.rs`:

```rust
#[cfg(test)]
mod lifecycle_banner_tests {
    use super::*;
    use crate::lifecycle::probe_state::LifecycleKind;
    use crate::lifecycle::LifecycleEvent;
    use crate::ws_messages::{LifecycleBannerAction, ServerMessage};

    #[test]
    fn unit_missing_yields_install_action() {
        let evt = LifecycleEvent { kind: LifecycleKind::UnitMissing };
        let msg = build_set_lifecycle_banner(&evt);
        match msg {
            ServerMessage::SetLifecycleBanner { kind, severity, label, action, .. } => {
                assert_eq!(kind, "unitMissing");
                assert_eq!(severity, "error");
                assert!(label.contains("not installed"));
                assert_eq!(
                    action,
                    Some(LifecycleBannerAction {
                        id: "install-daemon".into(),
                        label: "Install daemon".into(),
                    })
                );
            }
            other => panic!("unexpected message variant: {other:?}"),
        }
    }

    #[test]
    fn unit_inactive_yields_start_action() {
        let evt = LifecycleEvent { kind: LifecycleKind::UnitInactive };
        let msg = build_set_lifecycle_banner(&evt);
        match msg {
            ServerMessage::SetLifecycleBanner { kind, severity, action, .. } => {
                assert_eq!(kind, "unitInactive");
                assert_eq!(severity, "warning");
                assert_eq!(
                    action,
                    Some(LifecycleBannerAction {
                        id: "start-unit".into(),
                        label: "Start it".into(),
                    })
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn daemon_ok_yields_info_banner_with_no_action() {
        let evt = LifecycleEvent { kind: LifecycleKind::DaemonOk };
        let msg = build_set_lifecycle_banner(&evt);
        match msg {
            ServerMessage::SetLifecycleBanner { severity, action, .. } => {
                assert_eq!(severity, "info");
                assert!(action.is_none());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn event_flood_dropped_includes_count_in_label() {
        let evt = LifecycleEvent {
            kind: LifecycleKind::EventFloodDropped { dropped: 42 },
        };
        let msg = build_set_lifecycle_banner(&evt);
        match msg {
            ServerMessage::SetLifecycleBanner { label, .. } => {
                assert!(label.contains("42"), "label = {label}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn blocklist_fetch_failed_includes_list_id_and_reason() {
        let evt = LifecycleEvent {
            kind: LifecycleKind::BlocklistFetchFailed {
                list_id: "stevenblack-hosts".into(),
                reason: "HTTP 502".into(),
            },
        };
        let msg = build_set_lifecycle_banner(&evt);
        match msg {
            ServerMessage::SetLifecycleBanner { label, detail, .. } => {
                assert!(label.contains("stevenblack-hosts"));
                assert!(detail.unwrap_or_default().contains("HTTP 502"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run the tests, verify they fail to compile**

Run: `cargo test -p snitchwatch-bridge translator::downstream::lifecycle_banner_tests`
Expected: FAIL — `build_set_lifecycle_banner`, `ServerMessage::SetLifecycleBanner`, `LifecycleBannerAction` all undefined.

- [ ] **Step 3: Add the WS message variants**

Append to `crates/snitchwatch-bridge/src/ws_messages.rs` inside the `ServerMessage` enum (alongside the Plan 5 blocklist variants):

```rust
#[serde(rename_all = "camelCase")]
SetLifecycleBanner {
    kind: String,
    severity: String,
    label: String,
    detail: Option<String>,
    action: Option<LifecycleBannerAction>,
},
```

And inside the `ClientMessage` enum:

```rust
#[serde(rename_all = "camelCase")]
LifecycleAction { kind: String },
```

Add the new struct outside the enums:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleBannerAction {
    pub id: String,
    pub label: String,
}
```

- [ ] **Step 4: Implement `build_set_lifecycle_banner` in `downstream.rs`**

Append to `crates/snitchwatch-bridge/src/translator/downstream.rs`:

```rust
use crate::lifecycle::probe_state::LifecycleKind;
use crate::lifecycle::LifecycleEvent;
use crate::ws_messages::LifecycleBannerAction;

pub fn build_set_lifecycle_banner(event: &LifecycleEvent) -> ServerMessage {
    let severity = event.kind.severity();
    let severity_str = match severity {
        crate::lifecycle::LifecycleSeverity::Info => "info",
        crate::lifecycle::LifecycleSeverity::Warning => "warning",
        crate::lifecycle::LifecycleSeverity::Error => "error",
    }
    .to_string();

    let (kind_str, label, detail, action) = match &event.kind {
        LifecycleKind::DaemonOk => (
            "daemonOk".to_string(),
            "Snitchwatch is connected to the firewall daemon.".to_string(),
            None,
            None,
        ),
        LifecycleKind::UnitMissing => (
            "unitMissing".to_string(),
            "Firewall daemon is not installed.".to_string(),
            Some("Click Install daemon to set it up as a podman quadlet.".to_string()),
            Some(LifecycleBannerAction {
                id: "install-daemon".into(),
                label: "Install daemon".into(),
            }),
        ),
        LifecycleKind::UnitInactive => (
            "unitInactive".to_string(),
            "Firewall daemon is installed but not running.".to_string(),
            None,
            Some(LifecycleBannerAction {
                id: "start-unit".into(),
                label: "Start it".into(),
            }),
        ),
        LifecycleKind::GrpcUnreachable => (
            "grpcUnreachable".to_string(),
            "Cannot reach the firewall daemon — reconnecting…".to_string(),
            None,
            None,
        ),
        LifecycleKind::GrpcStaleStream => (
            "grpcStaleStream".to_string(),
            "Connection to the firewall daemon was interrupted — showing last-known data.".to_string(),
            None,
            None,
        ),
        LifecycleKind::EventFloodDropped { dropped } => (
            "eventFloodDropped".to_string(),
            format!("High event rate — {dropped} events skipped"),
            None,
            None,
        ),
        LifecycleKind::BridgePanicRecovered => (
            "bridgePanicRecovered".to_string(),
            "Snitchwatch recovered from an internal error — see crash.log.".to_string(),
            None,
            Some(LifecycleBannerAction {
                id: "open-diagnostics".into(),
                label: "Open Diagnostics".into(),
            }),
        ),
        LifecycleKind::BlocklistFetchFailed { list_id, reason } => (
            "blocklistFetchFailed".to_string(),
            format!("Blocklist {list_id} failed to update."),
            Some(reason.clone()),
            None,
        ),
        LifecycleKind::KernelHookFailed { excerpt } => (
            "kernelHookFailed".to_string(),
            "Firewall daemon failed to start — kernel hook error.".to_string(),
            Some(excerpt.clone()),
            Some(LifecycleBannerAction {
                id: "open-diagnostics".into(),
                label: "Open Diagnostics".into(),
            }),
        ),
        LifecycleKind::StateDivergenceReconciled => (
            "stateDivergenceReconciled".to_string(),
            "Snitchwatch reconciled its rule cache with the firewall daemon.".to_string(),
            None,
            None,
        ),
    };

    ServerMessage::SetLifecycleBanner {
        kind: kind_str,
        severity: severity_str,
        label,
        detail,
        action,
    }
}
```

- [ ] **Step 5: Run the tests, verify they pass**

Run: `cargo test -p snitchwatch-bridge translator::downstream::lifecycle_banner_tests`
Expected: PASS — 5 tests.

- [ ] **Step 6: Wire `LifecycleAction` upstream**

Append to `crates/snitchwatch-bridge/src/translator/upstream.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleActionOutcome {
    StartUnit,
    InstallDaemon,
    OpenDiagnostics,
    NoOp,
}

pub fn handle_lifecycle_action(action: &str) -> LifecycleActionOutcome {
    match action {
        "start-unit" => LifecycleActionOutcome::StartUnit,
        "install-daemon" => LifecycleActionOutcome::InstallDaemon,
        "open-diagnostics" => LifecycleActionOutcome::OpenDiagnostics,
        _ => LifecycleActionOutcome::NoOp,
    }
}

#[cfg(test)]
mod lifecycle_action_tests {
    use super::*;

    #[test]
    fn maps_known_actions() {
        assert_eq!(handle_lifecycle_action("start-unit"), LifecycleActionOutcome::StartUnit);
        assert_eq!(handle_lifecycle_action("install-daemon"), LifecycleActionOutcome::InstallDaemon);
        assert_eq!(handle_lifecycle_action("open-diagnostics"), LifecycleActionOutcome::OpenDiagnostics);
    }

    #[test]
    fn unknown_action_is_noop() {
        assert_eq!(handle_lifecycle_action("nope"), LifecycleActionOutcome::NoOp);
    }
}
```

- [ ] **Step 7: Run all upstream/downstream tests**

Run: `cargo test -p snitchwatch-bridge translator::`
Expected: PASS — all existing translator tests plus 5 new downstream + 2 new upstream tests.

- [ ] **Step 8: Commit**

```bash
git add crates/snitchwatch-bridge/src/ws_messages.rs crates/snitchwatch-bridge/src/translator/downstream.rs crates/snitchwatch-bridge/src/translator/upstream.rs
git commit -m "feat(bridge): setLifecycleBanner WS message + 10-row failure-mode mapping"
```

---

### Task 13: `serve_with_lifecycle` — wire the probe into the WS server

**Files:**
- Modify: `crates/snitchwatch-bridge/src/ws_server.rs`
- Create: `crates/snitchwatch-bridge/tests/lifecycle_ws_e2e.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/snitchwatch-bridge/tests/lifecycle_ws_e2e.rs`:

```rust
//! Verifies that LifecycleEvents broadcast on the bus arrive at a WS client
//! as setLifecycleBanner messages.

use futures_util::{SinkExt, StreamExt};
use snitchwatch_bridge::lifecycle::probe_state::LifecycleKind;
use snitchwatch_bridge::lifecycle::LifecycleEvent;
use snitchwatch_bridge::ws_server::serve_with_lifecycle_only;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn ws_client_receives_set_lifecycle_banner_for_unit_missing() {
    let (tx, _rx) = broadcast::channel::<LifecycleEvent>(16);
    let (ws_url, handle) = serve_with_lifecycle_only("127.0.0.1:0", tx.clone())
        .await
        .expect("server should bind");

    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("connect");

    // Give the connection time to register a subscriber.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    tx.send(LifecycleEvent { kind: LifecycleKind::UnitMissing })
        .expect("broadcast send");

    let msg = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
        .await
        .expect("timed out waiting for ws message")
        .expect("ws stream ended")
        .expect("ws error");

    let text = match msg {
        Message::Text(t) => t.to_string(),
        other => panic!("expected text frame, got {other:?}"),
    };

    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["command"], "setLifecycleBanner");
    assert_eq!(v["kind"], "unitMissing");
    assert_eq!(v["severity"], "error");
    assert_eq!(v["action"]["id"], "install-daemon");

    drop(ws);
    handle.abort();
    let _ = handle.await;
}
```

(Note: `serve_with_lifecycle_only` is a thin convenience entrypoint we add for the test; production code uses `serve_with_lifecycle` which composes the lifecycle bus with the existing blocklists pipeline from Plan 5.)

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test -p snitchwatch-bridge --test lifecycle_ws_e2e`
Expected: FAIL — `serve_with_lifecycle_only` undefined.

- [ ] **Step 3: Implement `serve_with_lifecycle_only` in `ws_server.rs`**

Append to `crates/snitchwatch-bridge/src/ws_server.rs`:

```rust
use crate::lifecycle::LifecycleEvent;
use crate::translator::downstream::build_set_lifecycle_banner;

/// Test/dev convenience: spin up a WS server that ONLY forwards lifecycle
/// events. Production callers use [`serve_with_lifecycle`] which composes
/// the lifecycle bus with the blocklists pipeline.
pub async fn serve_with_lifecycle_only(
    addr: &str,
    lifecycle_tx: tokio::sync::broadcast::Sender<LifecycleEvent>,
) -> anyhow::Result<(String, tokio::task::JoinHandle<()>)> {
    use axum::routing::get;
    use axum::Router;

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let url = format!("ws://{bound}/stream");

    let app = Router::new().route(
        "/stream",
        get({
            let lifecycle_tx = lifecycle_tx.clone();
            move |ws: axum::extract::WebSocketUpgrade| {
                let lifecycle_tx = lifecycle_tx.clone();
                async move {
                    ws.on_upgrade(move |socket| async move {
                        forward_lifecycle(socket, lifecycle_tx).await;
                    })
                }
            }
        }),
    );

    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Ok((url, handle))
}

async fn forward_lifecycle(
    mut socket: axum::extract::ws::WebSocket,
    lifecycle_tx: tokio::sync::broadcast::Sender<LifecycleEvent>,
) {
    use axum::extract::ws::Message;

    let mut rx = lifecycle_tx.subscribe();
    while let Ok(evt) = rx.recv().await {
        let msg = build_set_lifecycle_banner(&evt);
        let json = match serde_json::to_string(&msg) {
            Ok(j) => j,
            Err(_) => continue,
        };
        if socket.send(Message::Text(json.into())).await.is_err() {
            break;
        }
    }
}
```

(If your existing `ws_server.rs` already uses a different axum import shape — `axum::extract::ws::WebSocketUpgrade` vs `axum_extra::TypedHeader` etc — adapt the imports to match. The function body stays the same.)

- [ ] **Step 4: Run the test, verify it passes**

Run: `cargo test -p snitchwatch-bridge --test lifecycle_ws_e2e`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/ws_server.rs crates/snitchwatch-bridge/tests/lifecycle_ws_e2e.rs
git commit -m "feat(bridge): serve_with_lifecycle_only forwards LifecycleEvent → setLifecycleBanner"
```

---

## Part E — Front-end banner + polish + tick (Tasks 14–15)

### Task 14: `<lifecycle-banner>` web component + justfile recipes + spec tick

**Files:**
- Create: `web/banners/banner.js`
- Create: `web/banners/banner.css`
- Modify: `web/index.html`
- Modify: `justfile`
- Modify: `README.md`

The banner is a single in-flow strip at the top of the viewport. One severity color per row. One optional action button. Clicking the action sends `lifecycleAction` over the WS — the existing dispatcher in `web/protocol.js` (from Plan 3) routes it.

- [ ] **Step 1: Write the banner component**

Create `web/banners/banner.js`:

```javascript
// Snitchwatch lifecycle banner.
//
// Renders a single strip at the top of the viewport for the most recent
// setLifecycleBanner message. Three severity classes (info/warning/error),
// optional action button that posts a `lifecycleAction` client message.

class LifecycleBanner extends HTMLElement {
  constructor() {
    super();
    this._kind = null;
    this._listeners = [];
  }

  connectedCallback() {
    this.classList.add('lifecycle-banner');
    this.classList.add('lifecycle-banner--hidden');
    this._renderEmpty();

    // Subscribe to incoming server messages.
    const onMessage = (e) => this._onServerMessage(e.detail);
    window.addEventListener('snitchwatch:server-message', onMessage);
    this._listeners.push(['snitchwatch:server-message', onMessage]);
  }

  disconnectedCallback() {
    for (const [type, fn] of this._listeners) {
      window.removeEventListener(type, fn);
    }
    this._listeners = [];
  }

  _onServerMessage(msg) {
    if (!msg || msg.command !== 'setLifecycleBanner') return;
    this._render(msg);
  }

  _renderEmpty() {
    this.replaceChildren();
  }

  _render(msg) {
    const { kind, severity, label, detail, action } = msg;
    if (kind === 'daemonOk') {
      this.classList.add('lifecycle-banner--hidden');
      this._renderEmpty();
      return;
    }
    this.classList.remove('lifecycle-banner--hidden');
    this.classList.remove(
      'lifecycle-banner--info',
      'lifecycle-banner--warning',
      'lifecycle-banner--error',
    );
    this.classList.add(`lifecycle-banner--${severity}`);
    this._kind = kind;

    const title = document.createElement('span');
    title.className = 'lifecycle-banner__label';
    title.textContent = label;

    const content = document.createElement('div');
    content.className = 'lifecycle-banner__content';
    content.appendChild(title);

    if (detail) {
      const det = document.createElement('span');
      det.className = 'lifecycle-banner__detail';
      det.textContent = detail;
      content.appendChild(det);
    }

    const wrap = document.createElement('div');
    wrap.className = 'lifecycle-banner__wrap';
    wrap.appendChild(content);

    if (action) {
      const btn = document.createElement('button');
      btn.className = 'lifecycle-banner__action';
      btn.textContent = action.label;
      btn.dataset.actionId = action.id;
      btn.addEventListener('click', () => {
        this._dispatchAction(action.id);
      });
      wrap.appendChild(btn);
    }

    this.replaceChildren(wrap);
  }

  _dispatchAction(id) {
    // Re-use the existing client-message bus from web/protocol.js.
    window.dispatchEvent(
      new CustomEvent('snitchwatch:send', {
        detail: { action: 'lifecycleAction', kind: id },
      }),
    );
  }
}

customElements.define('lifecycle-banner', LifecycleBanner);
```

Create `web/banners/banner.css`:

```css
.lifecycle-banner {
  display: block;
  width: 100%;
  padding: 0;
  margin: 0;
  font-family: inherit;
  font-size: 13px;
  line-height: 1.4;
  border-bottom: 1px solid transparent;
  transition: background-color 120ms ease, border-color 120ms ease;
}

.lifecycle-banner--hidden {
  display: none;
}

.lifecycle-banner__wrap {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 10px 16px;
}

.lifecycle-banner__content {
  display: flex;
  flex-direction: column;
  gap: 2px;
  flex: 1 1 auto;
  min-width: 0;
}

.lifecycle-banner__label {
  font-weight: 600;
}

.lifecycle-banner__detail {
  opacity: 0.85;
  font-size: 12px;
}

.lifecycle-banner__action {
  flex: 0 0 auto;
  background: rgba(255, 255, 255, 0.18);
  color: inherit;
  border: 1px solid rgba(255, 255, 255, 0.25);
  border-radius: 4px;
  padding: 4px 12px;
  font-weight: 600;
  cursor: pointer;
}

.lifecycle-banner__action:hover {
  background: rgba(255, 255, 255, 0.28);
}

.lifecycle-banner--info {
  background: #1f3a52;
  color: #d7e8f5;
  border-bottom-color: #2c5277;
}

.lifecycle-banner--warning {
  background: #4a3a18;
  color: #f4e1b8;
  border-bottom-color: #6b531e;
}

.lifecycle-banner--error {
  background: #4a1a1f;
  color: #f5d0d4;
  border-bottom-color: #7a2228;
}
```

Edit `web/index.html` — add the script + element. Inside `<head>`, after the existing stylesheet links:

```html
<link rel="stylesheet" href="banners/banner.css" />
<script type="module" src="banners/banner.js"></script>
```

Inside `<body>`, as the very first child element of the main app container:

```html
<lifecycle-banner></lifecycle-banner>
```

- [ ] **Step 2: Add justfile recipes**

Append to `justfile`:

```makefile
flatpak:
    flatpak-builder --user --install --force-clean build/flatpak \
        packaging/flatpak/org.snitchwatch.Snitchwatch.yml

flatpak-shell:
    flatpak-builder --user --run build/flatpak \
        packaging/flatpak/org.snitchwatch.Snitchwatch.yml bash

install:
    ./packaging/install.sh

install-daemon-only:
    ./packaging/install.sh --daemon-only

package-test:
    cargo test -p snitchwatch-bridge --test flatpak_manifest_shape
    @if command -v bats >/dev/null 2>&1; then \
        bats packaging/install.bats; \
    else \
        echo "bats not on PATH — skipping shell tests"; \
    fi
    @if command -v shellcheck >/dev/null 2>&1; then \
        shellcheck packaging/install.sh; \
    else \
        echo "shellcheck not on PATH — skipping"; \
    fi

lint-shell:
    shellcheck packaging/install.sh
```

- [ ] **Step 3: Add the README section**

Append to `README.md`:

```markdown
## Install on Bazzite

Snitchwatch ships as a Flatpak GUI plus a podman quadlet for the OpenSnitch
daemon. From a release tarball:

```bash
./packaging/install.sh
```

This installs the Flatpak (`org.snitchwatch.Snitchwatch`), drops the quadlet
unit into `~/.config/containers/systemd/`, runs `systemctl --user daemon-reload`,
and starts `snitchwatch-opensnitchd.service`. Re-runnable; `--dry-run` previews
every command.

## Failure-mode banners

Snitchwatch surfaces every recoverable failure as a single in-app banner with
an optional action. The kinds are:

| Kind | Severity | Action button |
| --- | --- | --- |
| `unitMissing` | error | Install daemon |
| `unitInactive` | warning | Start it |
| `grpcUnreachable` | error | _(none — auto-reconnects)_ |
| `grpcStaleStream` | error | _(none — shows last-known data)_ |
| `eventFloodDropped` | warning | _(none — passive notice)_ |
| `bridgePanicRecovered` | warning | Open Diagnostics |
| `blocklistFetchFailed` | error | _(per-list status, no global banner action)_ |
| `kernelHookFailed` | error | Open Diagnostics |
| `stateDivergenceReconciled` | info | _(none — silent unless visible change)_ |
| `daemonOk` | info | _(banner hidden)_ |

The mapping lives in `crates/snitchwatch-bridge/src/translator/downstream.rs`
in `build_set_lifecycle_banner()`.
```

- [ ] **Step 4: Run the full test suite**

Run: `cargo test --workspace`
Expected: PASS — all tests from Plans 1–5 plus the new lifecycle/installer tests.

Run: `cargo clippy --workspace -- -D warnings`
Expected: clean. If `large_enum_variant` fires on `LifecycleKind` (the `KernelHookFailed { excerpt: String }` variant carries an arbitrarily-sized String, but String is heap-allocated so the variant size is fine), it should not. If clippy still complains, box the offending variant per `clippy_gotchas_bridge.md`.

- [ ] **Step 5: Commit**

```bash
git add web/banners/banner.js web/banners/banner.css web/index.html justfile README.md
git commit -m "feat(web): lifecycle-banner web component + justfile package recipes + README"
```

---

### Task 15: Tick M5 in milestone table + spec note

**Files:**
- Modify: `docs/superpowers/specs/2026-04-10-snitchwatch-design.md`

- [ ] **Step 1: Edit the spec milestone table**

Locate the M5 row in the milestone table (around line 546):

```markdown
| **M5 — Packaging** | Flatpak manifest, quadlet, install script, first-run wizard, all failure-mode handling wired up. | Fresh Bazzite VM → run `install.sh` → working Snitchwatch with no manual steps. |
```

Replace with:

```markdown
| **M5 — Packaging** ✅ | Flatpak manifest, quadlet, install script, first-run wizard, all failure-mode handling wired up. | Fresh Bazzite VM → run `install.sh` → working Snitchwatch with no manual steps. |
```

- [ ] **Step 2: Append the implementation note**

Append a new section right before the "## Open questions / risks" heading:

```markdown
### M5 implementation note

M5 is implemented per `docs/superpowers/plans/2026-04-11-packaging.md`. The
Flatpak manifest at `packaging/flatpak/org.snitchwatch.Snitchwatch.yml`
targets `org.freedesktop.Platform//23.08` with the rust-stable SDK extension.
The podman quadlet at `packaging/quadlet/snitchwatch-opensnitchd.container`
runs `ghcr.io/evilsocket/opensnitch:latest` host-network with NET_ADMIN /
NET_RAW / SYS_PTRACE caps. `packaging/install.sh` is idempotent and supports
`--dry-run`, `--daemon-only`, and `--bundle PATH`.

The first-run wizard's "Install daemon" button invokes
`crates/snitchwatch-tauri/src/installer.rs::install_daemon`, which uses
`flatpak-spawn --host --watch-bus` to drive `install.sh` from inside the
sandbox and streams stdout/stderr line-by-line back over the Tauri event
channel `installer://progress`. The pure-function helper
`InstallerInvocation::build` is fully unit-tested without spawning anything.

Failure-mode banners arrive via a new `LifecycleProbe` in
`crates/snitchwatch-bridge/src/lifecycle.rs` that polls (gRPC reachability,
systemctl unit state) every 5s and broadcasts typed `LifecycleEvent`s on a
`tokio::sync::broadcast` channel of capacity 16. The WS server forwards each
event as a `setLifecycleBanner` message; the `<lifecycle-banner>` web
component in `web/banners/banner.js` renders a single strip at the top of
the viewport with three severity classes and an optional action button.
The 10-row failure-mode mapping from this section's table is implemented in
`crates/snitchwatch-bridge/src/translator/downstream.rs::build_set_lifecycle_banner`.

Live opensnitchd smoke testing on a real Bazzite VM is deferred to M6 (Plan
7) along with the cargo-llvm-cov ≥80% gate.
```

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-04-10-snitchwatch-design.md
git commit -m "docs(spec): tick M5 in milestone table + implementation note"
```

---

## Acceptance Criteria

1. `cargo build --workspace --release` succeeds with zero warnings.
2. `cargo clippy --workspace -- -D warnings` is clean.
3. `cargo test --workspace` passes, including:
   - 11 tests in `crates/snitchwatch-bridge/tests/flatpak_manifest_shape.rs` (manifest, desktop, metainfo, tauri.conf, quadlet, install.sh shape, install.sh exec bit, packaging README).
   - 5 tests in `crates/snitchwatch-bridge/src/lifecycle/probe_state.rs::tests`.
   - 2 tests in `crates/snitchwatch-bridge/tests/lifecycle_e2e.rs`.
   - 1 test in `crates/snitchwatch-bridge/tests/lifecycle_ws_e2e.rs`.
   - 5 tests in `crates/snitchwatch-bridge/src/translator/downstream.rs::lifecycle_banner_tests`.
   - 2 tests in `crates/snitchwatch-bridge/src/translator/upstream.rs::lifecycle_action_tests`.
   - 4 tests in `crates/snitchwatch-tauri/src/installer/host_spawn.rs::tests`.
   - 3 tests in `crates/snitchwatch-tauri/src/installer.rs::tests` (`run_installer_streams_stdout_lines_and_returns_success`, `run_installer_captures_failure_with_last_20_lines`, `run_installer_handles_spawn_failure_gracefully`).
4. `bats packaging/install.bats` passes 6 tests (skipped if `bats` is not on PATH).
5. `shellcheck packaging/install.sh` is warning-free (skipped if `shellcheck` is not on PATH).
6. `just package-test` runs the Rust shape tests + bats + shellcheck and exits 0.
7. The `<lifecycle-banner>` element is present in `web/index.html` and the file ships under `/app/share/snitchwatch/web/` inside the Flatpak (verified by Tauri `bundle.resources` containing `../../web/**` and `../../packaging/**`).
8. The Tauri `install_daemon` command emits `installer://progress` events line-by-line and returns an `InstallerResult` with `success=true` for a script that exits 0.
9. `tests/tauri_smoke/tests/install_button.spec.ts` passes against a debug build with `SNITCHWATCH_PACKAGING_OVERRIDE` set to a temp dir containing a fake `install.sh`.
10. `LifecycleProbe::run` deduplicates consecutive identical `LifecycleKind`s — only state *changes* are broadcast.
11. The probe loop uses `MissedTickBehavior::Skip` (not `Burst`) so a long-paused tokio runtime cannot fire 50 catch-up ticks at once.
12. Every variant of the 10-row failure-mode inventory in the spec maps to a unique `LifecycleKind` and renders a unique `setLifecycleBanner` payload (verified by `build_set_lifecycle_banner` unit tests).
13. M5 row in the spec milestone table is marked ✅ and the spec contains the "M5 implementation note" section.
14. `packaging/install.sh --dry-run` prints every command without modifying the host.
15. `packaging/install.sh --daemon-only --dry-run` skips the `flatpak install` step but still prints the quadlet drop + daemon-reload + start commands.
16. No file in this plan exceeds 400 lines (per the 800-max workspace rule, but 400 is a tighter target for new code).
17. Memory `m1_envelope_hack.md` is honored: `setLifecycleBanner` is a typed `ws_messages.rs` variant, not a stringly-typed envelope.
18. Memory `clippy_gotchas_bridge.md` is honored: any `LifecycleKind` variant whose payload approaches the 160-byte threshold is boxed (or, more likely, holds heap-allocated `String`s and so does not trigger `large_enum_variant`).

---

## Deferred to later plans

- **Live opensnitchd 60s smoke on a real Bazzite VM** — Plan 7 (Open question #2 + Plan 1 deferred items).
- **`cargo-llvm-cov ≥ 80%` coverage gate on `snitchwatch-bridge`** — Plan 7 (Plan 1 deferred items).
- **WebKitGTK ↔ Tauri Flatpak permissions on real hardware** — Plan 7 (Open question #3).
- **Ephemeral-bind / settings.toml `bridge.bind_address` documentation of A→B→C upgrade path** — Plan 7 (the default is already `127.0.0.1:0`; only the docs are missing).
- **GPL-2.0 / Tauri-2 legal sanity check** — Plan 7 (Open question #4).
- **Designer pass on `packaging/flatpak/icons/snitchwatch.svg`** — v2.
- **Diagnostic-bundle "Copy" button** — wired in Plan 7 alongside the GH release.
- **Per-blocklist UI status row using `BlocklistFetchFailed`** — partially handled by Plan 5's per-list status; Plan 7 polishes the visual treatment.
- **Real GH release v0.1.0 with the built `.flatpak` artifact attached** — Plan 7.
- **Flathub submission** — v2.
- **First-run wizard test against a real `install.sh` invocation on the host** — manual smoke list, M6.
- **`StateDivergenceReconciled` event emission from the reconciliation loop in `grpc_client.rs`** — currently only the *banner mapping* is implemented. Wiring the actual emission point is left to a follow-up touch on the reconciliation task added in Plan 1; not load-bearing for M5 because the event is informational only.
- **`KernelHookFailed` event emission from a journalctl scrape on daemon-start failure** — same: banner mapping is in place, the emission hook lives next to the daemon-start path which Plan 7 adds.
- **`EventFloodDropped` counter wiring** — `cache/dropped_counter.rs` is listed in the file structure but its emission wiring through `LifecycleProbe` is gated behind a follow-up touch in Plan 7. The counter struct itself is added so Plan 7 can wire it without re-creating types.
