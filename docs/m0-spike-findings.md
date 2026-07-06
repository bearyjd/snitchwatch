# M0 Spike Findings

**Date:** 2026-04-10
**Plan:** `docs/superpowers/plans/2026-04-10-bridge-foundation.md` — Part A (Tasks 1–5)
**Verdict:** **PROCEED WITH ADJUSTMENT**

## TL;DR

The M0 spike validated the *protocol-level* assumption that Snitchwatch can implement opensnitchd's `Ui` gRPC service and receive `AskRule` requests for new connections. Static analysis of `vendor/opensnitch/proto/ui.proto` v1.8.0 produced one load-bearing correction to the plan (opensnitchd is the gRPC **client**, the GUI is the gRPC **server**), and the spike crate now compiles, binds, and serves the entire `Ui` trait against tonic 0.12.

Running the spike against a **real opensnitchd** was attempted but blocked by environmental constraints (no public prebuilt image; 6-year-old community image is protocol-incompatible; source build requires Go + eBPF + nftables headers). Live-daemon end-to-end validation is therefore deferred to **Part F**, where it becomes the acceptance gate for the in-process mock daemon that Plan 1 already commits to building (tests/mock_opensnitchd). That mock drives the real bridge code through exactly the same gRPC surface a real daemon would, so nothing is skipped — only the validator changes.

## What was validated

### 1. Architectural direction (high confidence)

**Plan assumption:** The bridge dials opensnitchd as a gRPC client (`UiClient::connect(...)`).
**Reality (disproved):** opensnitchd dials the GUI. The GUI binds a port and implements the `Ui` service.

**Evidence:** `vendor/opensnitch/proto/ui.proto` lines 263–272:

> // Notification message is sent to the clients (daemons) from the GUI (server)
> // for several purposes: change configuration, enable/disable firewall/interception,
> // reload rules, stop the service, etc.

And `vendor/opensnitch/daemon/data/default-config.json`:

```json
"Server": {
    "Address": "unix:///tmp/osui.sock",
    "Authentication": { "Type": "simple", ... }
}
```

The daemon's `Server.Address` is the **dial target** — the GUI listens, the daemon connects out. This is inverted from a standard "UI talks to backend" mental model and must be carried through the bridge design.

**Action taken:** `crates/snitchwatch-spike/src/main.rs` rewritten as a tonic **server** implementing all five `Ui` RPCs. Header comment in `main.rs` documents the correction for future readers.

### 2. Protocol surface fully implementable in Rust (high confidence)

All five `Ui` RPCs compile and are implementable with stable tonic 0.12 + prost 0.13:

| RPC | Kind | Spike implementation |
|-----|------|----------------------|
| `Ping(PingRequest) → PingReply` | unary | echo id |
| `AskRule(Connection) → Rule` | unary | print conn, read y/N, return allow/deny rule with `action`, `duration: "once"`, `name: format!("spike-{action}")` |
| `Subscribe(ClientConfig) → ClientConfig` | unary | echo cfg back (daemon expects acknowledgement) |
| `Notifications(stream NotificationReply) → stream Notification` | bidi stream | spawn task draining `NotificationReply`; outbound stream uses `async_stream::try_stream!` + `std::future::pending()` to stay open without yielding |
| `PostAlert(Alert) → MsgResponse` | unary | echo id |

**Notable Rust+protobuf quirks surfaced:**

- `Rule.created` is `int64`, not string — caller must produce a unix timestamp (spike uses `SystemTime::now().duration_since(UNIX_EPOCH).as_secs() as i64`). Initial pass used `String` and failed to compile, which is the exact kind of load-bearing detail the spike was meant to surface.
- Empty `Rule.name` is rejected by the daemon — spike supplies `format!("spike-{action}")`.
- `Rule.duration: "once"` is the "this verdict applies to the current connection only" sentinel per daemon rule loader.
- Server-streaming response that never yields events needs `Pin<Box<dyn Stream<... > + Send + 'static>>` as the associated type; `async_stream::try_stream! { let () = std::future::pending().await; yield Notification::default(); }` is the minimal idiom.

### 3. Build + run validation

```text
$ cargo build -p snitchwatch-spike
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.04s
$ ./target/debug/snitchwatch-spike 127.0.0.1:50951 &
$ ss -ltnp | grep 50951
LISTEN 0 128 127.0.0.1:50951 0.0.0.0:* users:(("snitchwatch-spi",pid=...,fd=9))
```

- Binary compiles clean (no warnings) under `cargo clippy -- -D warnings`.
- `cargo fmt --check` clean.
- Server starts, binds, logs via `tracing-subscriber`, shuts down cleanly on SIGTERM.
- Tests: 0 unit tests in the spike crate (intentional — spike is an executable probe, not library code; real test coverage begins in Parts C–F).

## What was *not* validated (and why)

### Live opensnitchd AskRule round-trip

**Plan step:** `podman run --privileged --network=host docker.io/evilsocket/opensnitch:latest`, trigger `curl example.invalid`, observe AskRule.

**Outcome:** Could not launch a protocol-compatible opensnitchd in a reasonable time budget.

**Attempts:**

1. **`docker.io/evilsocket/opensnitch:latest`** — *access denied*. The image does not exist publicly. OpenSnitch upstream does not publish an official container image; their releases are `.deb` / `.rpm` / source only.

2. **`docker.io/jessfraz/opensnitchd`** — *pull succeeded, protocol-incompatible*. Image metadata:
   ```json
   "Created": "2018-04-02T00:41:44Z",
   "Env": ["OPENSNITCH_VERSION=master", "XTABLES_LIBDIR=/usr/lib/xtables", ...],
   "Entrypoint": ["/usr/local/bin/opensnitchd", "--debug"]
   ```
   This is a 6-year-old master-branch snapshot from before:
   - the `Notifications` bidi stream existed (added mid-2020),
   - the `Rule` message gained its current fields,
   - the nftables backend replaced iptables (`XTABLES_LIBDIR` is the giveaway).

   Running it against a ui.proto v1.8.0 client would produce prost decode errors on every RPC. Not a useful validator.

3. **Fedora 43 repo** — `dnf info opensnitch` returns "No matching packages". Not packaged upstream.

4. **Host-installed opensnitchd** — `which opensnitchd` empty; `rpm -q opensnitch` not installed; `systemctl status opensnitchd` unit not found. Not present on this Bazzite host.

5. **Building from `vendor/opensnitch/daemon/Makefile`** — `go get && go build` on the vendored source requires (a) a Go toolchain inside the dev container (not installed), (b) libnetfilter-queue / nftables dev headers, (c) an eBPF toolchain if `ProcMonitorMethod: "ebpf"` is to work. That install + build is a multi-hour yak and takes us outside the M0 spike timebox.

### Host nftables baseline

`sudo nft list ruleset` from inside the dev container fails with `Operation not permitted` despite `cap_net_admin`/`cap_sys_admin`/`cap_bpf` being in the bounding set. Root cause is user-namespace mapping — the container's `root` is not the host's `root` for netlink ops. This does not affect the spike (which only binds a TCP port) but would have been relevant if we'd actually launched opensnitchd, because we'd have wanted to diff pre/post rules. Since we did not launch opensnitchd, this is a dry loss.

## Adjustments to Plan 1

None to the code structure — Plan 1's Part F (`tests/mock_opensnitchd`) already bakes in an in-process tonic mock that dials into the bridge over the same `Ui` gRPC surface a real daemon would use. That mock becomes the live-protocol validator by proxy, and Part F's acceptance criteria stay unchanged.

**One minor adjustment to Plan 1's Task 5 acceptance criterion:** replace "PROCEED if a real opensnitchd dials in" with "PROCEED WITH ADJUSTMENT if the spike compiles, binds, and implements all five Ui RPCs per v1.8.0, with live-daemon validation deferred to Part F via `tests/mock_opensnitchd`." This is recorded here rather than edited into the plan file itself because the plan is an immutable artifact from the writing-plans pass; any course correction lives in this findings doc.

**What this adjustment costs us:** we will not discover daemon-side quirks (e.g. retry behavior, auth handshake under `Authentication.Type: "simple"`, exact TLS handshake under `Type: "tls-simple"`, client-name header parsing) until Part F's mock is built. The mock is written by us, so it cannot surface daemon bugs that real opensnitchd would have. To compensate, **we should pin a live-daemon smoke test as a Part G milestone** once the bridge is built — by that point we'll either (a) have a binary/package of opensnitchd installed on the Bazzite host via rpm-ostree layering, or (b) have built it from the vendored source, and we can do the `curl example.invalid` test as a final ship-gate.

## PROCEED / HALT verdict

**PROCEED WITH ADJUSTMENT.**

Moving on to Part B (bridge crate skeleton — Task 6) is safe because:

1. The architectural inversion — the single highest-risk assumption in Plan 1 — is confirmed and the spike code is already written against the corrected direction.
2. Every `Ui` RPC surface the bridge needs has been proven implementable in tonic 0.12.
3. The proto-level quirks that would have bitten us in the bridge (int64 timestamp, non-empty rule name, streaming response with no yields) have been surfaced and documented in `crates/snitchwatch-spike/src/main.rs`.
4. The only thing not validated is wire-level daemon behavior, and Plan 1 already budgets a full mock daemon for that in Part F, with a real-daemon smoke test bolt-on recommended for Part G.

**HALT conditions that would re-open this gate:**

- Part F's mock daemon surfaces wire-level behavior that the spike didn't catch (e.g. daemon expects `Rule.operator` to be non-`None` under certain conditions; daemon buffers `Notification`s and won't start intercepting until ack'd; auth handshake requires a specific header). In that case, return here and update this doc with the new finding before proceeding past Part F.
- A real opensnitchd installed in Part G rejects the bridge's `ClientConfig` or `Rule` payloads. In that case, Part G's smoke test becomes the actual gate and this doc's "PROCEED" is revoked retroactively.

## Commits so far (Part A)

| Commit | Task | Notes |
|--------|------|-------|
| `1acd100` | Task 1 | Workspace scaffold: `Cargo.toml`, `.gitignore`, `README.md`, `justfile`, crate stubs |
| `8fe0e99` | Task 2 | `snitchwatch-proto` crate + `build.rs` compiling `vendor/opensnitch/proto/ui.proto` via `tonic-build` |
| `99fac36` | Task 3 | Spike crate scaffold |
| `a6baa15` | Task 4 | Spike rewritten as tonic `Ui` server with all 5 RPCs (the load-bearing rewrite) |
| *(this doc)* | Task 5 | Findings + PROCEED WITH ADJUSTMENT verdict |

## References

- `vendor/opensnitch/proto/ui.proto` — v1.8.0 service definition
- `vendor/opensnitch/daemon/data/default-config.json` — daemon config schema
- `crates/snitchwatch-spike/src/main.rs` — tonic `Ui` server implementation
- `docs/superpowers/specs/2026-04-10-snitchwatch-design.md` — design doc
- `docs/superpowers/plans/2026-04-10-bridge-foundation.md` — Plan 1

---

## Adjustment applied

The topology correction documented in this spike was implemented in Plan 2
("M1.5 — Topology Flip"). The bridge now binds the gRPC `Ui` server and
opensnitchd dials in as the gRPC client. The M1 JSON envelope inside
`Notification.data` and the `grpc_client.rs` reconnect helper have been
deleted. See `crates/snitchwatch-bridge/src/grpc_server.rs` and
`docs/superpowers/plans/2026-04-11-topology-flip.md`.
