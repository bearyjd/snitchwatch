# Endpoint Events Packaging (Vigie) Implementation Plan

**Goal:** Ship an opt-in, documented recipe that lets `opensnitchd` share this computer's connection events with Vigie, forwarding only a privacy allowlist over TLS. Packaging and docs only.

**Architecture:** `opensnitchd`'s own `remote` logger (JSON, TCP) dials `127.0.0.1:514`; an rsyslog drop-in listens there in its own ruleset, parses the raw line, renders allowlisted fields and forwards them with `omfwd` over TLS to Vigie, then stops. No Snitchwatch code is in the data path.

**Tech Stack:** OpenSnitch v1.8.0 loggers (`vendor/opensnitch/daemon/log/`), rsyslog 8.26 (Fedora 44: `rsyslog`, `rsyslog-mmjsonparse`, `rsyslog-gnutls`), jq, rootless podman with the pinned Fedora 44 image.

**Spec:** [`../specs/2026-10-26-vigie-endpoint-events.md`](../specs/2026-10-26-vigie-endpoint-events.md) (#231). Vigie side: bearyjd/vigie-network#21.

## Global Constraints

- No Rust product code; bridge, GUI, socket/token, Flatpak sandbox and daemon defaults unchanged. Never edit `vendor/opensnitch`.
- Off by default: no shipped daemon config enables a logger; recipe, staging scripts and image don't install rsyslog or the drop-in.
- Never forwarded: `process_env`, `process_args`, `process_cwd`, `process_tree`, `process_checksums` (nor `process_id`). Forwarded: time, protocol, src/dst IP and port, `dst_host`, `process_path`, `user_id`, `Action`, `Rule`, `Type`, plus a configured host id.
- Loopback listener on a port below 1024 (#35 squatting); TLS mandatory on the forward with an authenticated server (pinned CA + `x509/name`).
- Vigie's receiver contract (2026-10-26): mutual TLS required (per-host client cert/key; host identity comes from that cert, `host_id` is informational); frames over 8 KiB are dropped; Vigie records its own receive time, so `time` is rsyslog's `timegenerated`.
- No disk-assisted queue: rsyslog would serialize the raw event (environment included) to disk. Bounded in-memory queue; drop when full.
- Daemon config is root-only 0600: edit with the validate-then-swap pattern of "Restoring the `allow` default" (`docs/packaging/system-bridge-integration.md`); restart `opensnitch.service` (issue #92).
- Tests never touch the host's `/etc`, `sudo` or a live daemon; the real-rsyslog proof runs in the pinned container only.

## Source facts (v1.8.0)

- `LoggerConfig` fields: `Name, Format, Protocol, Server, WriteTimeout, ConnectTimeout, Tag, Workers, MaxConnectAttempts` (`log/loggers/logger.go:23-51`); nested at `Server.Loggers` (`ui/config/config.go:48`).
- Only `remote` emits JSON (`remote.go:85-91`); each object ends in `"\n\n"` (`formats/json.go:77`), so blank frames arrive and must be dropped. JSON ignores `Tag`.
- `Workers` unset keeps the manager's 4-slot queue (`logger.go:97-101`) and one connection (`remote.go:113`); `Workers: 1` would shrink the queue to 1.
- Fedora 44 ships no `pmnull`: parse with `mmjsonparse useRawMsg="on" cookie=""`.
- rsyslog lowercases template outnames: `Action`/`Rule`/`Type` go out as `action`/`rule`/`type` (seen in the e2e run).
- Fedora 44's rsyslogd is `/usr/bin/rsyslogd` (bin/sbin merge); the deny-mode rule template uses it.
- The bridge never forwards `CHANGE_CONFIG` (`translator/rule_notification.rs:391`), so it can't wipe the operator's `Loggers`.

## Tasks

### Task 1: Shape tests (RED first)

- [x] `crates/snitchwatch-bridge/tests/packaging_shape/endpoint_events.rs`, wired in `main.rs`: logger is remote/json/tcp/127.0.0.1 with port < 1024 and only `LoggerConfig` keys; every input is `imtcp` on 127.0.0.1 at the logger's port in a defined ruleset, no other listener; one `omfwd`, TLS mode 1, `x509/*`, CA file, permitted peer, `stop`, bounded memory queue, no `queue.filename`; template names only allowlisted fields and no forbidden field appears outside `#` comments; no other `packaging/` JSON sets `Server.Loggers`, recipe/stage scripts don't mention it; README has the root swap and restarts `opensnitch.service`.
- [x] Watched them fail on the missing files.

### Task 2: Artifacts (GREEN)

- [x] `packaging/endpoint-events/opensnitchd-logger.json`, `snitchwatch-endpoint-events.conf`, `README.md` (install, CA/placeholders, SELinux check, start, merge, `/etc` merge note, deny-mode rule template, way back, loss model).

### Task 2b: Vigie receiver contract

- [x] Tests: client `CertFile`/`KeyFile` required, auth mode `x509/name` (or `x509/fingerprint` with SHA peers) only; `maxMessageSize` 64k in, template caps (`process_path` 768, `dst_host` 253, `rule` 128) keep the worst-case escaped frame under 8 KiB; `sample-event.json` has exactly the allowlist.
- [x] Drop-in, README (exact TLS settings table, host cert generation/registration, size bounds, time source) and `sample-event.json` from the proof's receiver.

### Task 2c: /codex review of #233 (1 HIGH, 3 MEDIUM, 2 LOW)

- [x] HIGH, local forgery: opt-in `snitchwatch-endpoint-events.nft` (own `inet` table; non-root `meta skuid` connects to 127.0.0.0/8 and ::1 tcp/514 rejected) plus a oneshot unit ordered before rsyslog and the daemon; every forwarded field validated by pattern or byte cap in the ruleset, failures dropped; README "Provenance" ("reported by this host"; root can still forge).
- [x] MEDIUM: ruleset's own memory-only bounded queue (`saveOnShutdown` off); over-cap fields drop the event instead of truncating (no split UTF-8); README restarts rsyslog after `-N1` and checks the listener (and a non-root refusal) before the logger.
- [x] LOW: verify.sh compares values/types with the sample, forwards under the stock config with a disk-only main queue and greps the sinks, adds wrong-name-server and no-key cases and UTF-8 boundary events; README scopes the privacy guarantee (other loggers, debug output).

### Task 2d: /codex review round 2 of #233 (2 HIGH, 2 MEDIUM, 2 LOW)

- [x] HIGH, fail-open activation: the listener is `snitchwatch-endpoint-events.service` (its own rsyslog instance), `BindsTo`/`After`/`PartOf` the table's unit; the table's unit reloads atomically (`ExecReload` re-applies the replace-in-one-transaction file) and deletes only on stop. Proven under systemd: broken table means no listener; no unprotected moment across restarts.
- [x] HIGH, routed ingress: input-hook rejects for tcp/514 to 127.0.0.0/8 and ::1 from any interface but `lo`.
- [x] MEDIUM, broken-config fallback: separate instance, `ExecStartPre=rsyslogd -N1`, `abortOnUncleanConfig`, a default ruleset with no actions; proven a misspelt ruleset never opens 514.
- [x] MEDIUM, path gate: accepts the daemon's `Kernel connection` and comm values (procmon/details.go:258-275); producer-derived fixtures in verify.sh.
- [x] LOW: protocol allowlist from conman/connection.go, dotted-quad IPv4, uint32 user_id; the rest documented as shape checks Vigie must validate. verify.sh: receiver readiness plus positive controls, error-aware sink reads, the live journal, the broken-config test.

### Task 3: Real config proof

- [x] `packaging/endpoint-events/verify.sh` + `just endpoint-events-verify` (podman; not in `package-check`): `-N1` under stock and minimal main configs; e2e to a TLS rsyslog receiver with a fake token in env/args, blank and non-JSON frames, a task notification; worst-case event (45 KB raw) forwarded as one frame under 8 KiB; wrong-CA receiver gets nothing; receiver requiring this host's client cert rejects one from another CA; README merge (twice) and way-back snippets run against the packaged config copy, mode stays 0600.
- [x] `package-check` gains `bash -n`/shellcheck of `verify.sh` and a JSON parse of the logger entry.

## Open items (not verifiable here)

- SELinux on a real Bazzite host: TCP 514 is `rsh_port_t`, but Fedora 44's policy has `allow syslogd_t rsh_port_t:tcp_socket { name_bind name_connect }` (sesearch in the pinned image). Not run under enforcing mode; README step 3 says how to spot a denial.
- A live `opensnitchd` dialing the listener, and Vigie's TLS syslog input (spec open question 1).
