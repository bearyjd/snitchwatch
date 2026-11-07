<!-- Generated: 2026-11-06 | Files scanned: 34 | Token estimate: ~950 -->
# Data: persisted and exchanged

## Runtime (not persisted)
| Item | Path | Writer -> reader |
|---|---|---|
| WS socket (0600, dir 0700) | `$XDG_RUNTIME_DIR/snitchwatch/bridge.sock` (override `SNITCHWATCH_WS_SOCKET`) | `ws_server.rs:116` -> GUI `bridge_runtime.rs:266` |
| Handshake token (0600), 32 random bytes hex | `$XDG_RUNTIME_DIR/snitchwatch/token` (override `SNITCHWATCH_WS_TOKEN_PATH`) | `auth.rs:98` -> GUI `bridge_runtime.rs:290` |
| System mode sockets | `/run/snitchwatch/opensnitchd.sock`, `/run/snitchwatch/bridge.sock` (`bridge-cli activation.rs:10-11`) | systemd activation |
| System token (0640) | `/run/snitchwatch-auth/token` (`activation.rs:12`, `auth.rs:111`) | bridge -> GUI `bridge_runtime.rs:23` |
| Env discovered by wrappers | stdout lines `GRPC_LISTEN_ADDR=`, `WS_SOCKET_PATH=`, `WS_TOKEN_PATH=` | bridge-cli `main.rs` |

## Bridge state (`$STATE_DIRECTORY`, else `SNITCHWATCH_STATE_DIR`; system only `/var/lib/snitchwatch`; none = memory only; `storage.rs:52,144`)
| File | Defined | Content |
|---|---|---|
| `blocklists.sqlite3` | `bridge-cli storage.rs:54`, store `bridge blocklists/store.rs` | subscriptions, entries |
| `profiles.sqlite3` | `bridge-cli profile_storage.rs:58`, `profiles/store.rs` | profiles + rules |
| `curated-defaults.json` | `bridge curated/store.rs:295` | user choices for recommended rules |
| `rule_hits.json` | `bridge-cli rule_hits_storage.rs:19` | per-rule hit counts |
| `history.db`, `settings.json` | `bridge history/mod.rs:34-35` | connection history (module unwired, see bridge.md) |
Opened owner-only via `sqlite_file.rs`; JSON via `state_file.rs:43,104` (`read`/`write`).

## Daemon-side rules
Rules are JSON files owned by opensnitchd (`/etc/opensnitchd/rules`). The bridge never writes them directly: `ChangeRule`/`DeleteRule` notifications (`daemon_commands.rs:108`), mirrored into `cache/rules.rs`. Packaged seed rule: `packaging/bluebuild/files/system/etc/opensnitchd/rules/000-snitchwatch-bridge-fetch.json`. Default daemon config embedded: `daemon_config.rs:274,277` (vendor default + `packaging/system/opensnitchd-default-config.json`, `DefaultAction: allow`). Export/import document: versioned envelope in `rule_io.rs`.

## Embedded data
`crates/snitchwatch-bridge/data/curated-defaults-v1.json` (`curated/mod.rs:45`); `crates/snitchwatch-kirigami/data/{psl-suffix-parents.txt,world-map.txt}`; `packaging/geo/dbip-country-lite.json` (CC-BY-4.0, `provenance.json`).

## GUI files (`crates/snitchwatch-kirigami/src/paths.rs`)
| File | Fn | Writer |
|---|---|---|
| `~/.config/snitchwatch/settings.json` (silent mode, durations, RDAP opt-in) | `settings_path` :43 | `settings.rs:222` |
| `~/.config/snitchwatch/learned.json` (+ `.lock`, 0600, 30-day expiry) | `learned_path` :49 | `learned/store.rs` |
| `~/.local/state/snitchwatch/{bridge,crash,gui}.log` | :53-64 | `crash_log.rs`, `logging.rs` (gui.log only if `SNITCHWATCH_LOG`); no writer of bridge.log found in kirigami |
| `~/.local/share/snitchwatch/analysis-exports/` | :22 | analysis export controller |
| geo `.mmdb` imports in app data | `geo/install.rs:31` | geo install |
| `~/.config/autostart/snitchwatch.desktop` | :30 | `autostart.rs` |

## Scanner DBs
`scans.db` userspace: `$SNITCHWATCH_SCANNER_STATE_DIR` | `$XDG_STATE_HOME/snitchwatch-scanner` (`scanner-cli lib.rs:35`); privileged: `/var/lib/snitchwatch-scanner/scans.db` (`scanner-privileged main.rs:21`).

## Wire/proto
gRPC `ui.proto` from `vendor/opensnitch/proto/ui.proto`, compiled in `crates/snitchwatch-proto/build.rs:2` into `snitchwatch_proto::protocol` (`src/lib.rs:11`). WS JSON: `ws_messages.rs` (tag `action`, camelCase). Scan report JSON: `scanner-core finding.rs:55` `ScanReport`, validated by broker.
