# Snitchwatch — design spec

**Date:** 2026-04-10
**Status:** Draft, awaiting user review
**Audience:** Implementer (you, future-you, contributors)

## Summary

Snitchwatch is a Linux desktop application firewall GUI that looks and behaves like Little Snitch for Linux, sitting on top of an unmodified upstream OpenSnitch daemon. It runs on Bazzite (immutable Fedora) without an `rpm-ostree` overlay by packaging the GUI as a Flatpak and the daemon as a podman quadlet container.

The implementation strategy is to **fork the GPL-2.0 Little Snitch for Linux web UI**, vendor it into the repo with a scripted rebrand pass, and write a Rust bridge that translates between its WebSocket protocol and OpenSnitch's gRPC protocol. The OpenSnitch daemon stays vanilla — every piece of impedance matching lives in the bridge.

## Goals

- Polished Little Snitch–like GUI on Linux, on top of a battle-tested firewall daemon
- Runs on Bazzite without modifying `/usr` or adding rpm-ostree layers
- Daemon and GUI ship as one project — single install path, single update path
- Personal use first; public release once it's stable enough for strangers to install
- License: GPL-2.0 (forced by the LS UI fork)

## Non-goals

- Building a new firewall daemon (we use OpenSnitch unmodified)
- Reaching full Little Snitch macOS feature parity (profiles, silent mode, globe view — none of these exist in LS Linux either)
- Incoming-connection enforcement in v1 (deferred)
- Cross-distro packaging beyond Bazzite/Universal Blue family in v1 (Flatpak makes this nearly free later, but not a release target)

## Architecture

### High-level

```
┌──────────────────────────────────────────────────────────────────────────┐
│  BAZZITE HOST (immutable, /usr is read-only, no rpm-ostree overlay)      │
│                                                                          │
│  ┌────────────────────────────┐         ┌─────────────────────────────┐  │
│  │  Snitchwatch  (Flatpak)    │         │  opensnitchd  (podman      │  │
│  │                            │         │  container, quadlet)        │  │
│  │  ┌──────────────────────┐  │         │                             │  │
│  │  │  Tauri 2 shell       │  │         │  --privileged               │  │
│  │  │  (tray, window,      │  │         │  --network=host             │  │
│  │  │   autostart)         │  │         │  --pid=host                 │  │
│  │  └──────────┬───────────┘  │         │  --cap-add=NET_ADMIN,       │  │
│  │             │ embeds       │         │           SYS_ADMIN,BPF     │  │
│  │             ▼              │         │                             │  │
│  │  ┌──────────────────────┐  │         │  Listens on:                │  │
│  │  │  Webview (WebKitGTK) │  │  gRPC   │    127.0.0.1:50051          │  │
│  │  │  ─ forked LS UI      │◀─┼─────────┼─                            │  │
│  │  │  ─ vanilla JS, uPlot │  │         │                             │  │
│  │  │  ─ rebranded         │  │         │  Hooks kernel via eBPF      │  │
│  │  └──────────┬───────────┘  │         │  + NFQUEUE                  │  │
│  │             │ ws://127.0.0.1:NNNN/stream                            │  │
│  │             ▼                                                        │  │
│  │  ┌──────────────────────┐                                          │  │
│  │  │  Rust bridge         │  in-process, runs on a                   │  │
│  │  │  ─ WS server         │  background tokio task of                │  │
│  │  │  ─ gRPC client       │  the Tauri Rust core                     │  │
│  │  │  ─ rule translator   │                                          │  │
│  │  └──────────────────────┘                                          │  │
│  │  Flatpak permissions:                                              │  │
│  │    --share=network                                                 │  │
│  │    --socket=wayland --socket=fallback-x11                          │  │
│  │    --talk-name=org.freedesktop.Notifications                       │  │
│  └────────────────────────────┘                                       │  │
│                                                                          │
│  systemd: ─ snitchwatch-opensnitchd.container  (quadlet, system unit)    │
│           ─ snitchwatch.desktop                (Flatpak autostart)       │
└──────────────────────────────────────────────────────────────────────────┘
```

### Five units, one job each

1. **Tauri shell** — window, tray icon, autostart, system notifications, splash. Pure Rust. No business logic.
2. **Forked LS web UI** — three tabs (Connections / Blocklists / Rules), inspector-pane decisions, traffic chart, theme. Vanilla JS + uPlot, rebranded to Snitchwatch.
3. **Rust bridge** — runs in-process inside the Tauri Rust core on a background tokio task. Implements the LS WebSocket protocol on one side, gRPC client on the other, plus the rule-semantics translator and the connection cache.
4. **opensnitchd quadlet** — a systemd `.container` file that pulls and runs the OpenSnitch daemon as a privileged podman container, host-network, listening on `127.0.0.1:50051`. Survives reboots, no overlay needed.
5. **Packaging layer** — Flatpak manifest for Snitchwatch, quadlet file for opensnitchd, install script that drops both into the right user-writable locations on Bazzite.

**Key invariant:** opensnitchd is unmodified upstream. We never patch the daemon. The bridge does all the impedance matching.

### Why a thick bridge instead of a daemon fork

Forking opensnitchd was considered and rejected. opensnitchd is ~50k lines of Go with privileged eBPF + NFQUEUE kernel hooks; a fork makes a single developer responsible for merging upstream security fixes forever, and any bug runs as root with NET_ADMIN. Meanwhile the things the bridge owns — the rolling connection display cache, blocklist subscription management, traffic chart binning — are all UI-layer concerns that don't belong in a packet-filter daemon. With the daemon vanilla, the container image can be bumped to a newer opensnitchd with zero Snitchwatch code changes. That property is what makes the project sustainable long-term.

## Data flow & protocol bridge

### Two streams, two directions

**Downstream** (server → UI): opensnitchd's `Notifications` server-stream and `AskRule` unary calls land on the bridge's tonic client. The bridge translates them into LS WebSocket messages and pushes them to the embedded webview over `/stream`.

**Upstream** (UI → server): The webview calls `sendAction(type, payload)` over the same WebSocket. The bridge routes each action to the appropriate gRPC call — verdicts complete pending `AskRule` futures, rule mutations become `ChangeRule`, settings changes become `ChangeConfig`.

### The 22 LS WebSocket message types

Roughly half are 1:1 forwarded from opensnitchd events; the other half are synthesized by the bridge from cached + derived state. The full mapping table:

| LS WebSocket message | Source on the OpenSnitch side |
| --- | --- |
| `insertConnectionRows` | New `Connection` from `AskRule` stream + new accepted/denied events from `Ping` stats deltas |
| `updateConnectionRows` | Bridge cache mutation when a pending row gets a verdict, byte counters tick, or DNS resolves |
| `removeConnectionRows` | Bridge eviction when row ages past retention window |
| `moveConnetionRows` | Bridge re-sort after verdict / sort-key change *(typo is in upstream)* |
| `clearConnectionRows` | Bridge full-flush on filter change or "clear history" |
| `setInspector` | Bridge synthesizes from cached `Connection` + matched rule + process metadata |
| `updateRuleButtons` / `highlightRuleForRows` | Bridge derives from current rule list (loaded once at startup, refreshed on `ChangeRule` ack) |
| `trafficEvents` / `setTrafficData` / `updateTrafficData` | Bridge bins `Connection` bytes/sec into uPlot-friendly buckets — opensnitchd doesn't ship a chart series, we compute it |
| `setRules` / `updateRules` | `ListRules` / `ChangeRule` on the gRPC side |
| `setBlocklists` / `setBlocklistDetails` / `setBlocklistEntries` / `setBlocklistStatus` / `setBlocklistEntryLocation` | **Bridge-owned feature.** opensnitchd has no native blocklist concept — bridge stores subscriptions in its own SQLite, fetches lists, and synthesizes deny rules into opensnitchd |
| `setConnectionsStatus` | gRPC channel health (connected / reconnecting / daemon down) |
| `setAboutInfo` | Static at startup — Snitchwatch version, opensnitchd version (queried once), eBPF commit (from daemon Ping) |
| `setUndoStack` | Bridge-owned ring buffer of recent rule mutations |
| `localizationTable` | Static — shipped in the Flatpak as JSON, pushed once on connect |
| `globalSettings` | Bridge state (filter on/off, theme) + relevant daemon config from `GetConfig` |

### Three things the bridge owns that opensnitchd doesn't

1. **The connection cache** — rolling history of last N minutes / M rows that the Connections tab needs to render the list, the chart, and the inspector. opensnitchd is event-driven and stateless from the UI's POV.
2. **Blocklists** — subscription URLs, refresh schedules, downloaded host lists, materialized as a pile of opensnitchd deny rules. Stored in a SQLite database in the Flatpak's `$XDG_DATA_HOME`.
3. **Traffic chart series** — bridge bins per-second byte counters into uPlot buckets and pushes `updateTrafficData` deltas. The daemon never sees the chart.

### The pending-prompt mechanism

OpenSnitch's `AskRule` is a blocking unary call — the kernel packet sits in NFQUEUE until the UI returns a verdict. LS for Linux has no modal prompt; decisions happen in the inspector pane of the Connections list.

The bridge bridges these like this:

1. `AskRule` arrives → bridge inserts a **pending** Connection row with action=`null`, holds the gRPC call open in a tokio task, and remembers `(rowId → oneshot::Sender<Verdict>)`.
2. The row appears with a yellow `◐` marker. If nothing else is selected, it auto-selects in the inspector.
3. User clicks Allow / Deny in the inspector → UI sends `sendAction("setVerdict", {rowId, action})` → bridge looks up the oneshot sender, fires the verdict back, gRPC call returns, kernel releases the packet.
4. If the user does nothing for N seconds (default 30) → bridge applies the configured fallback (allow-once or deny-once) and resolves the call. **Pending packets are sacred** and never dropped from any backpressure path.

## Rule semantics mapping

This is the highest-risk area of the design. The LS rule model and the OpenSnitch rule model don't line up cleanly, and the bridge has to reconcile them.

### Side-by-side

```
LS rule                              OpenSnitch rule
─────────                            ───────────────
verdict: allow|deny|blocklist        action: allow|deny|reject
direction: outgoing|incoming|both    duration: once|until-restart|30s|5m|...|always
scope:                               operator:
  process: "/usr/bin/firefox"          type: simple|regexp|list
  remote: "*.github.com"               operand: process.path|dest.host|dest.port|...
  port: 443                            data: "..."
  protocol: tcp|udp|any              precedence: alphabetical by name
permanent: bool
priority: implicit (most-specific wins)
```

### Translation table

| LS concept | OpenSnitch translation | Loss? |
| --- | --- | --- |
| `verdict: allow` | `action: allow` | none |
| `verdict: deny` | `action: deny` | none |
| `verdict: blocklist` | `action: deny` + bridge tags rule with `__source: blocklist:<id>` | cosmetic — UI distinguishes by tag |
| `process: /usr/bin/firefox` | operator `{type: simple, operand: process.path, data: "/usr/bin/firefox"}` | none |
| `remote: github.com` | operator `{type: simple, operand: dest.host, data: "github.com"}` | none |
| `remote: *.github.com` | operator `{type: regexp, operand: dest.host, data: "^.*\\.github\\.com$"}` | none — bridge does the glob→regex conversion |
| `port: 443, protocol: tcp` | operators `dest.port=443` AND `protocol=tcp` | none |
| `direction: outgoing` | native — opensnitchd's primary mode | none |
| `direction: incoming` | **NOT enforced in v1.** Bridge stores the rule but marks it `unenforced` in the UI | functional — flagged in UI |
| `direction: both` | Bridge splits into two rules: outgoing (enforced), incoming (unenforced) | partial |
| `permanent: true` | `duration: always` | none |
| `permanent: false` (session rule) | `duration: until-restart` | none |
| `priority` (most-specific-wins) | Bridge computes specificity score (0–999), prefixes filename `{999-score:03d}-...` | see precedence section |

### Three hard edge cases

**1. Incoming connections.** opensnitchd's incoming-connection support is experimental and not enabled by default in the container. v1 accepts LS rules with `direction: incoming`, persists them faithfully, but renders them in the UI with an "*unenforced — incoming filtering not enabled*" badge. A settings toggle to enable incoming on the daemon side comes in v2.

**2. The blocklist verdict type.** LS treats `blocklist` as a peer of `allow` and `deny`. OpenSnitch only knows allow/deny/reject. The bridge materializes each subscribed blocklist as a batch of deny rules with names like `900-blocklist:stevenblack:0001234.json` and tags the description field with `{"snitchwatch": {"source": "blocklist", "list_id": "stevenblack", "entry": "doubleclick.net"}}`. The bridge groups by source tag when enumerating rules for the UI: blocklist rules → Blocklists tab, user rules → Rules tab. **One uniform deny-rule pile in the daemon, two presentations in the UI.**

**3. Rule precedence.** OpenSnitch evaluates rules in alphabetical order by filename and stops at the first match. LS implicitly resolves conflicts by "most specific wins" — a rule with process+host+port beats a rule with just process. The bridge translates LS's implicit specificity into explicit alphabetical prefixes:

```
specificity_score = (
  100 * has(process)
+  50 * has(remote_host_exact)
+  30 * has(remote_host_glob)
+  40 * has(port)
+  20 * has(protocol)
)
filename = f"{999 - score:03d}-{slug}.json"
# higher specificity → lower number → evaluated first
```

Blocklist rules are locked to the `900–999` band so user rules always win — denying github.com via a blocklist still loses to allowing Firefox to github.com.

### Ask-on-new mechanism

Tied to the pending-prompt machinery above. The opensnitchd `default_action` is set per the user's Snitchwatch preference. When set to "ask", `intercept_unknown=true` causes every novel flow to fire `AskRule`, which blocks until the bridge resolves it. When set to "allow all" or "deny all", no `AskRule` blocking happens but Connection events still stream for the live monitor.

> **⚠ Spike risk.** The exact opensnitchd config keys (`default_action`, `intercept_unknown`, `AskRule` timeout semantics) need to be verified against the actual `ui.proto` and the daemon's config schema before bridge implementation begins. **This is the #1 thing to spike in M0** — it's where the UX promise meets the daemon's actual capabilities.

## UX details

### Pending-row inspector

```
┌─────────────────────────────────────────────────────────────────────┐
│  Connections                                          [search]  ⚙   │
├──────────────────────────────┬──────────────────────────────────────┤
│  ● firefox       github.com  │  ⚠ Pending decision                  │
│  ● firefox       cdn.cf.com  │                                      │
│ ▶◐ slack         updates.sl..│  /usr/lib/slack/slack                │
│  ● spotify       audio.scd.. │  → updates.slack.com:443 (tcp)       │
│                              │                                      │
│                              │  This process has not connected here │
│                              │  before. What should Snitchwatch do? │
│                              │                                      │
│                              │  [ Allow once  ]  [ Deny once  ]     │
│                              │  [ Allow always]  [ Deny always]     │
│                              │                                      │
│                              │  Scope: ◉ this host                  │
│                              │         ○ any host on slack.com      │
│                              │         ○ any host                   │
│                              │                                      │
│                              │  Auto-deny in 28s →                  │
└──────────────────────────────┴──────────────────────────────────────┘
```

- **Row markers.** Pending rows show `◐` in yellow. Decided rows show `●` in green (allow) or red (deny). Selection marker is `▶`.
- **Auto-select.** When a pending row appears and nothing else is currently selected, it auto-selects. Won't steal focus from a row the user is investigating.
- **Auto-action countdown.** Per-rule timeout (default 30s). When it elapses, the bridge applies the configured fallback. Visible countdown when selected.
- **Scope selector.** Three radio buttons control how broad a rule we synthesize on "always" verdicts: this host (exact `dest.host`), wildcard, or any host (drop the host operator).

### Tray icon states

| State | Icon | Tooltip |
| --- | --- | --- |
| Idle, filter on | monochrome silhouette | "Snitchwatch — filtering" |
| Pending decision | silhouette + small yellow dot overlay | "3 pending decisions" |
| Recent block | silhouette + brief red flash (3s) | "Blocked: spotify → tracker.x" |
| Filter off | silhouette with strikethrough | "Snitchwatch — filtering disabled" |
| Daemon down | silhouette in red | "opensnitchd not reachable" |

Right-click menu: Show window · Pause filtering for [15m / 1h / until reboot] · Quit. Left-click = show window.

### Desktop notifications

Quiet by default. Fires only when the user actually needs to know:

- **Pending decision** — only if the Snitchwatch window is hidden AND the row has been pending for more than 5 seconds. Click → opens Snitchwatch with the row pre-selected.
- **Daemon went away** — fired once when gRPC reconnect fails for >30s. Click → opens settings.
- **Filter pause expired** — fired when the user's "pause for 15min" timer runs out.

Uses `org.freedesktop.Notifications` (allowed via the Flatpak `--talk-name` permission).

### Branding rebrand

The forked LS UI ships under "Little Snitch for Linux" branding. The name is trademarked even though the code is GPL-2.0. We rebrand to **Snitchwatch** via a scripted, idempotent rebrand pass applied at vendoring time.

- **`web/rebrand.sh`** performs all substitutions deterministically. Re-runnable: vendoring upstream = drop new files in, run `./rebrand.sh`, commit.
- Tracked in git as a separate commit so the rebrand diff is always inspectable.
- Substitutions cover: brand strings (`Little Snitch for Linux` → `Snitchwatch`, etc.), file slugs (`littlesnitch-192.png` → `snitchwatch-192.png`), `manifest.json` name fields, and the brand-laden subset of i18n strings.

**Things we deliberately do NOT change:**

- Color palette stays (`#0d6abf` accent / `#72C419` allow / `#FF3C00` deny / `#f6f8fc` bg). Functional, not trademarked.
- Layout, CSS, component structure stay. The point of forking is to inherit the work.
- SPDX `GPL-2.0` headers stay. Per GPL-2 §2(c), we append our own copyright line below the original.
- Generic SVG sprite icons (gear, magnifier, etc.) stay. Only the app silhouette gets replaced.

**App icon.** v1 placeholder is a re-rendered silhouette in a different shape (an eye / closed-circuit lens) so we're not riding the LS visual identity. 192px and 512px PNGs for the manifest, plus an SVG source. Replaceable later if a real designer wants to take a pass.

## Repository layout

```
snitchwatch/
├── Cargo.toml                       # workspace root
├── justfile                         # one-liner glue: `just build`, `just flatpak`, `just test`
├── README.md
├── LICENSE                          # GPL-2.0 (forced by LS UI fork)
│
├── crates/
│   ├── snitchwatch-tauri/           # the desktop shell — pure Tauri
│   │   ├── Cargo.toml
│   │   ├── tauri.conf.json
│   │   ├── build.rs
│   │   └── src/
│   │       ├── main.rs              # window, tray, autostart, lifecycle
│   │       └── tray.rs
│   │
│   ├── snitchwatch-bridge/          # the protocol translator — testable headless
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── ws_server.rs         # axum + tungstenite, /stream endpoint
│   │       ├── grpc_client.rs       # tonic client, reconnect loop
│   │       ├── translator/
│   │       │   ├── downstream.rs    # gRPC events → LS WS messages
│   │       │   ├── upstream.rs      # LS sendAction → gRPC calls
│   │       │   └── rule_semantics.rs# the LS↔OpenSnitch rule mapping
│   │       ├── cache/
│   │       │   ├── connections.rs   # rolling row buffer + pending oneshots
│   │       │   ├── traffic_bins.rs  # uPlot bucket synthesizer
│   │       │   └── inspector.rs
│   │       ├── blocklists/
│   │       │   ├── mod.rs           # subscription manager
│   │       │   ├── store.rs         # rusqlite — $XDG_DATA_HOME/snitchwatch/blocklists.db
│   │       │   ├── fetcher.rs       # reqwest + hosts-file parser
│   │       │   └── materializer.rs  # synthesize opensnitchd deny rules
│   │       └── settings.rs
│   │
│   └── snitchwatch-proto/           # generated tonic stubs
│       ├── Cargo.toml
│       ├── build.rs                 # tonic-build against vendor/opensnitch/proto/ui.proto
│       └── src/lib.rs               # `pub mod ui;`
│
├── web/                             # forked LS-for-Linux SPA (vendored)
│   ├── VENDORED.md                  # provenance: upstream commit, fetch date, license
│   ├── rebrand.sh                   # idempotent rebrand pass
│   ├── index.html
│   ├── manifest.json
│   ├── styles.css
│   ├── connections.css
│   ├── blocklists.css
│   ├── rules.css
│   ├── traffic.css
│   ├── uPlot.min.css
│   ├── js/
│   │   ├── app.js
│   │   ├── connections.js
│   │   ├── blocklists.js
│   │   ├── rules.js
│   │   ├── traffic.js
│   │   ├── selection.js
│   │   ├── datetime.js
│   │   ├── localization.js
│   │   └── uPlot.iife.min.js
│   └── icons/                       # Snitchwatch icons (192/512, mask-friendly)
│
├── vendor/
│   └── opensnitch/                  # git submodule pinned to a known release tag
│       └── proto/ui.proto           # only file we actually consume
│
├── packaging/
│   ├── flatpak/
│   │   ├── org.snitchwatch.Snitchwatch.yml      # flatpak-builder manifest
│   │   ├── org.snitchwatch.Snitchwatch.desktop
│   │   ├── org.snitchwatch.Snitchwatch.metainfo.xml
│   │   └── icons/
│   ├── quadlet/
│   │   └── snitchwatch-opensnitchd.container    # systemd .container unit
│   └── install.sh                                # drops both into user-writable paths
│
├── tests/                           # cross-crate integration tests
│   ├── mock_opensnitchd/            # in-process gRPC server that speaks ui.proto
│   │   └── src/lib.rs
│   ├── bridge_protocol_test.rs      # WS client ↔ bridge ↔ mock daemon
│   └── golden/                      # captured WS message sequences for regression
│       └── *.jsonl
│
└── docs/
    ├── superpowers/
    │   └── specs/2026-04-10-snitchwatch-design.md   # this file
    └── architecture.md
```

### Three deliberate layout choices

1. **Bridge is its own crate**, not nested in the Tauri crate. The whole point is that `snitchwatch-bridge` can be exercised headless against `tests/mock_opensnitchd` without ever launching a webview. If it lived inside `snitchwatch-tauri`, we'd be coupling protocol logic to the windowing system. Hard no.
2. **`web/` is vendored**, not a submodule. The LS UI is GPL-2.0 and we're going to patch it (rebrand, eventually small protocol tweaks). Submodule semantics invite confusion about whose commit is whose. Snapshot it, record provenance in `VENDORED.md`, treat it as our code. Re-syncing is a manual `git diff` exercise on upstream releases.
3. **`vendor/opensnitch/` IS a submodule** — but only because we consume one file (`ui.proto`) and want it pinned to a release tag. Pinning to a release tag is what lets us bump the container image and the proto stubs in lockstep with one PR.

### Build pipeline

```
just build       →  cargo build --release           (rust workspace)
                 →  copies web/ into target/web/    (no bundler — vanilla JS)
                 →  embeds web/ into the Tauri binary via tauri.conf.json

just test        →  cargo test --workspace          (unit + integration)
                 →  golden replay against mock_opensnitchd

just flatpak     →  flatpak-builder --user --install build-dir
                       packaging/flatpak/org.snitchwatch.Snitchwatch.yml

just install     →  ./packaging/install.sh
                       ├─ flatpak install --user ./build/snitchwatch.flatpak
                       ├─ mkdir -p ~/.config/containers/systemd/
                       ├─ cp packaging/quadlet/*.container ~/.config/containers/systemd/
                       ├─ systemctl --user daemon-reload
                       └─ systemctl --user start snitchwatch-opensnitchd.service

just regen-proto →  rebuilds snitchwatch-proto/src from vendor/opensnitch/proto/ui.proto
```

**No npm, no webpack, no node_modules.** The forked LS UI is vanilla JS — nothing to bundle. The whole `web/` directory is shippable as-is.

### Install footprint on a Bazzite host

```
~/.local/share/flatpak/app/org.snitchwatch.Snitchwatch/   ← GUI (Flatpak, user)
~/.config/containers/systemd/snitchwatch-opensnitchd.container
                                                          ← daemon quadlet (user)
~/.local/share/snitchwatch/blocklists.db                  ← bridge state
~/.config/snitchwatch/settings.toml                       ← user prefs

# nothing in /usr.   nothing in /etc.   no rpm-ostree overlay.
```

## WebSocket bind mode — and the upgrade path

The Snitchwatch bridge serves its WebSocket on a local port. **For the v1 public release (M6), the bridge binds to a random ephemeral port on `127.0.0.1` and only the embedded webview ever connects.** This is "Option A": private mode, smallest attack surface, simplest first-cut.

> **Development convenience exception.** Milestones M2 through M5 use a fixed local port (`127.0.0.1:3031`) instead of ephemeral, because attaching websocat or a real browser for debugging is much easier when the port is predictable. M6 flips the default to ephemeral as part of the public-release tightening pass. Both modes use the same code path; only the bind address differs.

Two future modes were considered and explicitly deferred:

- **Option B — Fixed local port.** Bridge binds to `127.0.0.1:3031`. Same machine browsers can connect (e.g., to use Snitchwatch in Firefox alongside the Tauri window). No LAN exposure.
- **Option C — Optional LAN mode.** Bridge binds to `0.0.0.0:3031` with a generated auth token, settings opt-in only. Lets a user remote into Snitchwatch from a laptop browser elsewhere on the LAN. Real attack surface.

### Migration path A → B → C

The bind configuration is centralized in one place: `snitchwatch-bridge/src/ws_server.rs`. The upgrade story is:

1. **A → B** (one-line change for power users): expose `bridge.bind_address` in `settings.toml`, default `127.0.0.1:0` (random ephemeral). Power users can set it to `127.0.0.1:3031`. The Tauri webview reads the actual bound port from the bridge after startup, so this is invisible to anyone who doesn't change it. **Implementation cost: ~30 minutes.**
2. **B → C** (real feature work): add an `auth_token` field to `settings.toml`, generated on first run. The bridge requires `?token=...` on the WS upgrade request. The webview is preloaded with the token via Tauri IPC at startup. The settings UI gains a "Allow LAN access" toggle that, when enabled, switches the bind address to `0.0.0.0:3031` and surfaces the URL + token as a QR code or copy button. Add per-IP rate limiting to the WS upgrade endpoint. **Implementation cost: ~1 day, gated behind a v2 milestone.**

The seam is preserved by keeping all bind logic in `ws_server.rs` and never letting any other code assume the bind address is loopback. Until someone actually wants Option C, the codebase reads it as Option A and the door stays unlocked behind a settings flag.

## Error handling & resilience

### Failure mode inventory

| Failure | Detection | Recovery | User-visible signal |
| --- | --- | --- | --- |
| Container not installed (first launch) | gRPC connect fails, `systemctl --user list-unit-files` shows no unit | First-run wizard → "Install daemon" → runs `install.sh` daemon-only path | Onboarding screen, not an error |
| Container installed but stopped | unit file exists, `ActiveState != active` | UI offers "Start opensnitchd" button → bridge runs `systemctl --user start ...` via host-spawn | Banner: "Daemon stopped — [Start it]" |
| Container running, gRPC unreachable | tonic dial timeout (3s) | Exponential backoff: 1s, 2s, 5s, 10s, 30s, 60s capped | Tray turns red after 30s; "Reconnecting…" banner |
| gRPC stream drops mid-session | tonic `Status::unavailable` or stream end | Same backoff. Bridge does NOT clear its connection cache — UI shows last-known data with a "stale" overlay | Stale-data overlay on Connections list |
| gRPC slow consumer (event flood) | tokio channel `send_timeout` hits | Bridge drops oldest non-pending events; pending rows are NEVER dropped | Status bar: "high event rate — N events skipped" |
| WebSocket drop (bridge ↔ webview) | WS close event in webview JS | Webview auto-reconnects every 1s; bridge sends a full state replay on reconnect | Brief "reconnecting" toast |
| Bridge panic | Tauri panic hook | Tauri shell stays alive, restarts the bridge tokio task | Toast: "Snitchwatch recovered from an error — see crash.log" |
| Blocklist fetch failure | `reqwest` error or HTTP non-2xx | Keep last successful copy; never delete cached entries on fetch failure | Per-list status: "last updated 4h ago — last fetch failed" |
| Kernel hook failure (eBPF verifier) | Daemon refuses to start, journalctl shows verifier error | Out of bridge's hands — surface daemon stderr in Diagnostics pane, link to troubleshooting doc | Hard error screen with daemon log excerpt |
| State divergence (bridge vs daemon) | Periodic `ListRules` reconciliation every 60s | Daemon is the source of truth — bridge replaces its cache, pushes `setRules` | Silent unless something visibly changes |

### Three principles

1. **Daemon is the source of truth.** Bridge cache is a presentation layer. On any reconnect or reconcile, the daemon's view wins. Bridge never tries to "patch up" the daemon to match its own state.
2. **Pending packets are sacred.** Anywhere we drop, evict, or backpressure, pending rows (the ones holding open `AskRule` calls) are exempt. Letting one time out is fine; silently losing one is not — the kernel packet is sitting in NFQUEUE and the user expected a decision.
3. **Degrade visibly.** Each failure mode has its own user-visible signal. No generic "everything is broken" banner — specific signals for "daemon stopped" / "reconnecting" / "stale" / "high event rate" / "rule sync error". Users can act on a specific signal; they ignore a generic one.

### First-run wizard flow

```
launch Snitchwatch
       │
       ▼
bridge attempts gRPC connect (3s timeout)
       │
       ├── success ─────────────────▶ normal startup
       │
       └── failure
              │
              ▼
       systemctl --user list-unit-files snitchwatch-opensnitchd.service
              │
              ├── unit not present ──▶ "Welcome to Snitchwatch"
              │                         "We need to install the firewall daemon
              │                          as a podman container. This is a one-time
              │                          setup. [Install] [Cancel]"
              │
              ├── unit present, inactive ──▶ "Daemon installed but not running.
              │                              [Start it] [Diagnose]"
              │
              └── unit present, active ────▶ keep retrying with backoff
                                             (something else is wrong)
```

### Logging & diagnostics

- **Bridge logs:** `tracing` crate, JSON output to `$XDG_STATE_HOME/snitchwatch/bridge.log`, rotation at 10MB × 3 files. Default level `info`, settings toggle for `debug`.
- **Crash dumps:** Tauri panic hook writes to `$XDG_STATE_HOME/snitchwatch/crash.log`. UI surfaces via toast + Diagnostics tab.
- **Daemon logs:** Diagnostics tab embeds the last 200 lines of `journalctl --user -u snitchwatch-opensnitchd.service` via host-spawn.
- **"Copy diagnostic bundle" button:** tars bridge.log + crash.log + last 500 lines of daemon log + Snitchwatch version + opensnitchd version + kernel version. For bug reports.

## Testing strategy

### Pyramid

```
                  ╱ ╲
                 ╱E2E╲           ~5 manual smoke scenarios on a Bazzite VM
                ╱─────╲          (real opensnitchd, real network), per release
               ╱       ╲
              ╱protocol ╲        Bridge ↔ mock_opensnitchd full round trips
             ╱integration╲       ~50 tests, runs in CI, no kernel/container needed
            ╱─────────────╲
           ╱               ╲
          ╱      unit       ╲    Translator pure functions, rule mapping,
         ╱                   ╲   blocklist parser, traffic binner, cache eviction
        ╱─────────────────────╲  ~200 tests, sub-second
```

### Unit tier — what it covers

- **Rule semantics translator** — for every entry in the mapping table, a property test that round-trips LS rule → opensnitchd rule → LS rule and asserts the relevant fields survive. Includes the specificity scoring formula.
- **Glob → regex conversion** — `*.github.com`, `github.*`, `**`, escaped dots, edge cases.
- **Traffic binner** — feed it a synthetic event stream with known totals, assert uPlot bucket output matches.
- **Connection cache eviction** — fill past retention window, assert oldest non-pending rows go first and pending rows are never evicted.
- **Blocklist parsers** — hosts file, domains list, ABP-style filter list, malformed lines, comments.
- **WS protocol encoder** — for each of the 22 message types, assert JSON shape matches what the LS UI's `handleServerCommand` dispatch expects. Captured from a real LS instance and stored as golden fixtures.

### Protocol integration tier — the high-value middle

`tests/mock_opensnitchd` implements `ui.proto` via tonic, takes scripted event sequences as input, and lets us drive the bridge end-to-end without ever needing root or a container.

- **Pending-prompt round trip** — mock fires `AskRule`, bridge inserts pending row over WS, test acts as the WS client, sends `setVerdict`, mock receives verdict on the gRPC reply, assert the rule was synthesized correctly.
- **Reconnect storm** — mock drops the gRPC stream every 2s for 30s; assert bridge cache never loses a pending row, WS client sees consistent state replays, no events double-processed.
- **Event flood** — mock fires 10k events/sec for 10s; assert bridge degrades gracefully and the `dropped_events` counter is exposed.
- **Reconciliation** — bridge has rule X cached, mock returns a `ListRules` without X; assert bridge replaces cache and pushes `setRules`.
- **Golden replay tests** — captured WS message sequences from the real LS instance get replayed against the bridge to verify it produces byte-for-byte compatible output for the same gRPC inputs.

### E2E smoke tier — narrow but real

Scripted scenario list, run manually on a Bazzite VM with real opensnitchd in the actual quadlet container, before each release. Not in CI.

1. **Fresh install** — launch on a box without the daemon, click through the wizard, verify the daemon starts and the UI connects.
2. **Allow flow** — open Firefox, hit a new domain, accept the prompt with "always", verify the rule appears and the next visit doesn't prompt.
3. **Deny flow** — same but Deny, verify the next visit is blocked and shows up with a red dot.
4. **Blocklist subscription** — add StevenBlack/hosts, wait for fetch, verify a known tracker domain gets blocked.
5. **Daemon kill recovery** — `systemctl --user stop snitchwatch-opensnitchd` while the UI is open, verify banner appears, restart daemon, verify auto-reconnect.

### Coverage targets

- **`snitchwatch-bridge`** — 80%+ line coverage. High-stakes code; every translator branch should be hit.
- **`snitchwatch-tauri`** — no line target. Mostly thin wiring around Tauri APIs. Smoke-tested via E2E.
- **`snitchwatch-proto`** — generated code, no test target.
- **`web/`** — out of scope for unit tests; covered transitively by golden replays.

## Phasing — six milestones

| M | Goal | Demo proves |
| --- | --- | --- |
| **✅ M0 — Spike** | Verify the riskiest assumption from the rule-semantics section: that opensnitchd's `AskRule` + `default_action` + `intercept_unknown` combo can express "ask on new connection" the way LS expects. Standalone Rust binary, no UI. | A terminal program prints "novel connection from /usr/bin/curl, allow? y/n" and the kernel respects the answer. |
| **✅ M1 — Bridge core** | Headless bridge crate with WS server + gRPC client + connection cache + pending-prompt machinery. Tested only against `mock_opensnitchd`. | A WS client (websocat) can connect, receive `insertConnectionRows` events from a scripted mock, and round-trip a verdict. |
| **✅ M1.5 — Topology Flip** | Inverts gRPC topology to match real opensnitchd protocol; deletes JSON envelope; mock becomes client. | Bridge binds gRPC `Ui` server; opensnitchd dials in as the gRPC client; all tests pass with corrected topology. |
| **✅ M2 — Vendored UI** | Pull `web/`, run rebrand script, serve it from the bridge, point a real browser at it. No Tauri yet — just a browser tab. | Open `http://127.0.0.1:3031/` in Firefox, see the LS UI rendered, see live connections from real opensnitchd, click Allow/Deny in the inspector and have it work. |
| **✅ M3 — Tauri shell** | Wrap M2 in a native window with tray icon, autostart, notifications. WS still on a fixed local port for debugging. | A real desktop app you can install and use. First version a non-developer could try. |
| **M4 — Blocklists** | Subscription manager, hosts-file fetcher, deny-rule materializer, Blocklists tab fully wired. | Subscribe to StevenBlack/hosts, watch a tracker get blocked. |
| **M5 — Packaging** | Flatpak manifest, quadlet, install script, first-run wizard, all failure-mode handling wired up. | Fresh Bazzite VM → run `install.sh` → working Snitchwatch with no manual steps. |
| **M6 — Public** | Tighten ephemeral port to Option A, document the upgrade path to Option C, polish, README, contributor docs, GitHub release. | A stranger can find the repo, follow the README, and have it running on their own Bazzite box. |

### Deliberately not in v1

- Incoming-connection enforcement (deferred to v2)
- Profiles / silent mode (LS-Mac concepts that LS-Linux doesn't have either)
- Globe / map view (LS-Linux doesn't have one — we matched LS-Linux's actual surface)
- LAN mode (Option C) — documented upgrade path, not initial implementation

## Open questions / risks

1. **opensnitchd config schema verification (M0 spike).** The exact key names and semantics for `default_action`, `intercept_unknown`, and the `AskRule` timeout behavior need to be confirmed against the actual `ui.proto` and the daemon's config file format. If any of them are different from what this design assumes, the pending-prompt mechanism may need adjustment. **This is the #1 risk in the project.**
2. **opensnitchd container image availability.** Does upstream OpenSnitch publish a usable OCI image, or do we need to build one in our packaging pipeline? Spike during M0.
3. **WebKitGTK ↔ Tauri Flatpak permissions surface.** Need to verify the `--share=network` permission is actually sufficient for the embedded webview to talk to a loopback WebSocket, or whether we need additional `--socket=session-bus` or similar. Validate during M3.
4. **GPL-2.0 compatibility of Tauri 2.** Tauri itself is dual-licensed (Apache-2.0 / MIT). Linking GPL-2.0 forked code (the bridge translation logic, if any of it ends up touching the LS UI's licensing footprint) needs a quick legal sanity check before public release. The bridge stays as our own GPL-2.0 code; the question is whether the Tauri binary as a whole becomes a GPL-2.0 derivative or if the WebView/IPC boundary is enough to keep them separate. Defer to M6.
5. **i18n strategy.** v1 ships English only (whatever's in the LS `localization.js`'s `en.json`). The rebranded keys go in. Other locales are noise we don't need yet — but the table format is preserved so future contributors can add more.

## Decision log

| Decision | Alternatives | Why this won |
| --- | --- | --- |
| Fork the LS UI, swap data layer | Build new UI from scratch; fork opensnitchd's PyQt UI | LS UI is GPL-2.0, polished, vanilla JS (no toolchain), and matches the visual identity we're targeting. Faster path with less invented work. |
| Thick bridge, vanilla daemon | Fork opensnitchd to expose LS-shaped APIs natively | A solo dev can't sustainably maintain a fork of a privileged kernel-hooking daemon. Bridge concentrates project-specific code in user-space Rust where iteration is safe. |
| Tauri 2 + WebKitGTK | Electron; native GTK4/Rust; Qt | Tauri matches the vanilla JS UI directly, is small, idiomatic on Linux, and Rust on the host side meshes with tonic. |
| Flatpak GUI + podman quadlet daemon | Single Docker container; rpm-ostree overlay; native package | Bazzite-friendly: no /usr modification, idiomatic Universal Blue, daemon survives reboots, GUI has Flatpak's standard X11/Wayland/notification integration. |
| TCP loopback IPC (`127.0.0.1:50051`) bridge ↔ daemon | Unix socket | Crosses the Flatpak sandbox boundary cleanly with `--share=network`. Unix socket would require additional Flatpak host filesystem permissions. |
| Vendor `web/` instead of submodule | Git submodule pinned to upstream | We're going to patch the UI (rebrand at minimum). Vendoring keeps history clear about whose commit is whose; rebrand becomes a re-runnable script. |
| Submodule `vendor/opensnitch/` for `ui.proto` | Vendor the proto file | Want to track the daemon version we're targeting precisely. Submodule pin = single source of truth for proto version + container image tag. |
| WebSocket bind = Option A (random ephemeral, loopback) for v1 | Option B (fixed port, loopback); Option C (LAN with auth) | Smallest attack surface for v1. The seam to upgrade is documented; B is a one-line settings change, C is a v2 feature. |
| GPL-2.0 license (forced) | MIT, Apache-2.0 | Forking GPL-2.0 LS UI requires the project to be GPL-2.0. Acceptable cost for inheriting the polished UI work. |
