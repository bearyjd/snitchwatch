<!-- Generated: 2026-11-06 | Token estimate: ~790 (o200k_base) -->
# Data: persisted and exchanged

## Runtime (not persisted)
| Item | Path | Writer -> reader |
|---|---|---|
| WS socket | `$XDG_RUNTIME_DIR/snitchwatch/bridge.sock` (bridge: `SNITCHWATCH_WS_SOCKET`) | `ws_server.rs:116` -> GUI `bridge_runtime.rs:268` |
| Token (32 random bytes, hex) | `token` beside the socket (`bridge-cli lib.rs:291-302`); `SNITCHWATCH_WS_TOKEN_PATH` overrides only the GUI lookup (`bridge_runtime.rs:287`) | `auth.rs:98` -> GUI |
| System mode | `/run/snitchwatch/{opensnitchd,bridge}.sock`, token `/run/snitchwatch-auth/token` 0640 (`activation.rs:10-12`) | `auth.rs:111` -> GUI `bridge_runtime.rs:23` |

## Bridge state (`$STATE_DIRECTORY`, else `SNITCHWATCH_STATE_DIR`; system only `/var/lib/snitchwatch`; none = memory only; `storage.rs:52,144`)
| File | Defined |
|---|---|
| `blocklists.sqlite3` | `bridge-cli storage.rs:54` |
| `profiles.sqlite3` | `bridge-cli profile_storage.rs:58` |
| `curated-defaults.json` | `bridge curated/store.rs:295` |
| `rule_hits.json` | `bridge-cli rule_hits_storage.rs:19` |
| `history/history.db`, `history/settings.json` (module dormant) | `bridge history/mod.rs:33-35,100` |

## Daemon-side rules
JSON files owned by opensnitchd (`/etc/opensnitchd/rules`); the bridge only sends `ChangeRule`/`DeleteRule` (`daemon_commands.rs:108`). Shipped daemon config: `packaging/system/opensnitchd-default-config.json` (`DefaultAction: allow`); `daemon_config.rs:274,277` embeds configs in `#[cfg(test)]` code only.

## Embedded data
`crates/snitchwatch-bridge/data/curated-defaults-v1.json` (`curated/mod.rs:45`); Kirigami `data/world-map.txt` (`geo/world_map.rs:15`), `data/psl-suffix-parents.txt` (`connections/level_rules.rs:303`).

## GUI files (`crates/snitchwatch-kirigami/src/paths.rs`)
`~/.config/snitchwatch/settings.json` (:43, `settings.rs:222`); `learned.json` (:49, 0600, 30-day expiry, `learned/store.rs`); `~/.local/state/snitchwatch/{crash,gui}.log` (gui.log only if `SNITCHWATCH_LOG`); `~/.local/share/snitchwatch/analysis-exports/` (:22); `~/.config/autostart/snitchwatch.desktop` (:30).

## Scanner DBs and reports
Userspace `scans.db`: `$SNITCHWATCH_SCANNER_STATE_DIR`, else `$XDG_STATE_HOME/snitchwatch-scanner` (`scanner-cli lib.rs:35`). Privileged: `/var/lib/snitchwatch-scanner/scans.db` (`scanner-privileged main.rs:21`). Userspace report is `scanner_core::ScanReport` (`finding.rs:55`); the broker validates the privileged output's own `scanner_broker::Report` (`scanner-broker lib.rs:34,63`).

## Wire formats
gRPC `ui.proto` via `crates/snitchwatch-proto/build.rs:2`; WS JSON in `ws_messages.rs`.
