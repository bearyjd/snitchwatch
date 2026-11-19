# #391: Give each Unix gRPC connection its own `ConnKey`

Issue #391 (found while planning #386). The owner wants it fixed, not just documented.

Today `ConnKey = Option<SocketAddr>` (`crates/snitchwatch-bridge/src/daemon_commands.rs:84`)
comes from `request.remote_addr()` at three handler sites (`grpc_server.rs:287` ping,
`:534` subscribe, `:641` notifications). tonic 0.12.3's `Request::remote_addr` reads only
`TcpConnectInfo` (`tonic-0.12.3/src/request.rs:237-248`). So **every** Unix peer gets `None`, on
the per-user socket (`OwnerUnixIncoming`, `crates/snitchwatch-bridge-cli/src/activation.rs:155-170`)
and on the system socket (`RootUnixIncoming`, `:141-148`) alike. The same `None` is also the key
for in-process `Request::new` calls in tests.

Branch `docs/391-connkey-plan` (this doc only). Implementation: two PRs, PR 1 then PR 2. Each
can be reviewed and reverted on its own. PR 1 does not change behaviour in production.

## Verified touch points (2026-11-19, at `1b7714d1`)

### 1. The brief's test citation was wrong

`daemon_commands/version_gate.rs:112,135,154,172` (the brief cited 116/140/164) are
**policy** tests (`source.network` refusal by daemon version). None of them separates peers.
The peer-separation tests are in `crates/snitchwatch-bridge/src/grpc_server/tests/ping_subscribe.rs`:
- `:337` `only_the_stream_holders_ping_sets_the_daemon_version`: the TCP test. It builds requests
  with `version_ping(.., from)` (`:300-315`), which inserts a `TcpConnectInfo` extension.
- `:375` `a_ping_without_an_address_sets_the_version_when_the_stream_has_none`: this asserts
  today's shared-`None` behaviour. "On the Unix socket neither side has an address."
- `:391` `the_daemon_version_is_forgotten_when_the_last_current_stream_closes`.

### 2. Every `ConnKey` use

Production:
| Site | What it does with the key |
|---|---|
| `daemon_commands.rs:79-84` | type + doc ("`None` is one shared key; every system-mode peer is root"). **Doc becomes false.** |
| `daemon_commands.rs:187-194` | `compile_fail` doctest building `BecameCurrent { conn: None, .. }`. After the change it would still "fail to compile", but because of the private fields, not the reason it was written for. Update it to the new spelling. |
| `daemon_commands.rs:195-217` | `BecameCurrent.conn`, `conn()` |
| `daemon_commands.rs:221-225`, `:428-441` | `OpenStream.conn`, set by `open_stream(conn)` |
| `daemon_commands.rs:516-519` (`hello`), `:676-677` (`readopt_for_current`, trusted transport only) | pass the stream's key into `RulesSync::commit` / `readopt` |
| `daemon_commands/version_gate.rs:27-50` | `observe_daemon_version(peer, ..)`: `open.conn != peer` gate. **Doc `:27-33` becomes false** ("On the Unix socket both keys are `None`") |
| `cache/rules_sync.rs:74-107` | `PendingSnapshots` doc + `Staged.key`. **Doc `:77-84` is the load-bearing one (see section 3)** |
| `cache/rules_sync.rs:119,128-157` | `stage`/`stage_received`: keep only the key's newest snapshot (`retain(key != ..)`). **Doc `:124-127` becomes false** |
| `cache/rules_sync.rs:181-213` | `adopted_fresh`/`adopt_fresh`: match on `staged.key == *key` |
| `cache/rules_sync.rs:398,417,438,451` | `RulesSync::stage`/`stage_received`/`commit`/`readopt` |
| `grpc_server.rs:287,311,534,547,641` | the three derivation sites |

`cache/rules_count.rs:220` (`watch.key == Some(key)`) is an unrelated tuple key. It is not a
`ConnKey` use.

Tests: `daemon_commands/tests.rs:71` `addr(port)`, `cache/rules_tests.rs:291` `key(port)`,
`daemon_commands/refusal_tests.rs:123` (`Some(SocketAddr)`),
`grpc_server/tests/ping_subscribe.rs:338-342,392-395` and `rules_snapshot.rs:325` (an
`Option<SocketAddr>` passed straight to `open_stream`). There are also about 20 files that pass
a literal `None` (`git grep -l 'open_stream(None)\|stage(None\|stage_received(None'`: the
`cache/rules_*_tests.rs`, `daemon_commands/*_tests.rs`, `curated/*_tests.rs`,
`profiles/enforcer_tests.rs`, `blocklists/daemon_sink_tests.rs`,
`grpc_server/{prompt_slot,rule_hits}_tests.rs`, `grpc_server/tests/{daemon_redial,rules_snapshot}.rs`,
and in bridge-cli `src/test_daemon.rs` and `src/rules_import/tests.rs`).

### 3. What the shared key does on the SYSTEM socket today (the honest answer)

**Security: nothing breaks.** `RootUnixIncoming` admits only uid 0 (`activation.rs:146`).
`DaemonTransport::Unix::peer_is_trusted_daemon()` is the only trusted transport
(`daemon_commands.rs:112-115`). A second root peer, or a replaced daemon, can move the recorded
version, stage a snapshot over the real daemon's, or have its snapshot adopted. All of that is
within what root can already do: write `/etc/opensnitchd/rules`, kill or ptrace the bridge.

**Correctness: the shared key is load-bearing.** `rules_sync.rs:77-84` and
`daemon_commands.rs:25-32` (PR #106 OQ1/N1, #115) rely on it. A redialled daemon's old and new
streams can say HELLO in either order after the new Subscribe, and each one adopts the newest
staged snapshot *because they share a key*. When the stream that holds the list closes, a
fallback stream that adopted the same snapshot keeps the list (`readopt_for_current`,
`daemon_commands.rs:665-679`). That runs only where `peer_is_trusted_daemon()` is true, so TCP
(which already has per-connection keys) never exercises this path. The tests for it all use
`None` (`rules_staging_tests.rs:120-200`, `rules_refused_tests.rs:309-406`,
`rules_count_allowance_tests.rs:626-721`).

Does real opensnitchd ever split one Subscribe/Notifications pair across connections?
`client.go:236-243` runs `go c.Subscribe()` on every connected edge. `Subscribe`
(`notifications.go:349-368`) calls `listenForNotifications` (`:375-390`), which reads `c.client`
again when it runs. `disconnect` (`client.go:305-317`) nils `c.client` and closes `c.con`. So
after a flap (connected, lost, connected), a Subscribe that started on connection A can open its
Notifications stream on connection B. Also, one `grpc.ClientConn` can reconnect its transport
underneath. **UNVERIFIED by a run**: no live daemon here, per `CLAUDE.md`. If it happens and keys
are per connection, the HELLO on B finds no snapshot for B's key. The list is withdrawn until
the next Subscribe, which is a regression on the shipped flavour.

The other system-socket change: after a redial, the new connection's ping is ignored while the
dying connection's stream is still current. The bridge keeps that stream for about a second
(`version_gate.rs:38-39`). The next 1 s ping (`client.go:225`) after the new HELLO, or after
the old stream closes, then takes. The version shows up one or two pings later. That is harmless.

### 4. Plumbing (verified against tonic 0.12.3)

- `Connected::connect_info()` is called once per accepted IO, and the value is inserted into
  every request's extensions (`tonic-0.12.3/src/transport/server/mod.rs:1023`;
  `conn.rs:19-57` documents `request.extensions().get::<MyConnectInfo>()`).
- `run_with_incoming` is already generic over `IO: Connected` with `IO::ConnectInfo: Clone +
  Send + Sync + 'static` (`crates/snitchwatch-bridge-cli/src/lib.rs:215-226`), so swapping the
  `Item` type of the two incoming streams needs no signature change.
- The uid is already read at accept (`poll_accept_filtered`, `activation.rs:172-192`,
  `stream.peer_cred()`).
- `MockOpensnitchd::connect_unix` (`tests/mock_opensnitchd/src/lib.rs:166-196`) uses one
  `Channel` (eager `connect_with_connector`) for Ping, Subscribe and Notifications. So one mock
  is one UDS connection and gets one key. Two mocks get two keys. The #386-migrated tests keep
  their lists.

## Design

### Key type

```rust
/// `None`: no connection info (in-process `Request::new` calls, tests).
pub type ConnKey = Option<PeerKey>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeerKey {
    Tcp(SocketAddr),
    /// Server-assigned at accept, process-wide monotonic; never from the peer.
    Unix(u64),
}
```

Keeping the outer `Option` means the ~20 `None` test files compile and keep their meaning
("in-process, no connection"). Only `addr()`, `key()`, `refusal_tests.rs:123` and the
`ping_subscribe.rs`/`rules_snapshot.rs` sites change to `Some(PeerKey::Tcp(..))`. I rejected a
flat `Addr | Unix | Unknown` enum for this reason: same expressiveness, ~20 extra files of churn.

### Connection info and wrapper

- In **`snitchwatch-bridge`** (new module `daemon_peer.rs`): `pub struct UnixPeerInfo { pub id:
  u64, pub uid: u32, pub pid: Option<i32> }` and `pub fn conn_key<T>(req: &Request<T>) ->
  ConnKey`. It checks `TcpConnectInfo` first (unchanged mapping via `remote_addr()`), then
  `UnixPeerInfo` gives `Unix(id)`, and anything else gives `None`. All three `grpc_server.rs`
  sites call this one helper. The type has to live in the bridge crate because `grpc_server`
  reads it and the bridge does not depend on bridge-cli.
- In **`activation.rs`**: `pub(crate) struct UnixPeerStream { inner: UnixStream, info:
  UnixPeerInfo }`. It delegates `AsyncRead`/`AsyncWrite` (UnixStream is `Unpin`, no projection
  crate needed) and implements `Connected<ConnectInfo = UnixPeerInfo>`.
  `poll_accept_filtered` assigns `id` from a process-wide `static NEXT: AtomicU64`
  (`fetch_add(1, Relaxed)`, starting at 1) **after** the uid check passes, then returns
  `UnixPeerStream`. Both `RootUnixIncoming` and `OwnerUnixIncoming` change `Item` to it.

### Peer credentials: extra information, not the key

The key is the connection id. pid and uid are not used: a pid can be reused and has a different
meaning in another PID namespace, and two connections from one daemon share both. **Include uid
and pid in `UnixPeerInfo` for log fields only**: the `debug!` in `version_gate.rs:41-45`, and an
`info!` on stream open. No decision reads them. They are captured at accept (`SO_PEERCRED` is
fixed at `connect()`), so they cost no extra syscall. They never reach the WS wire.

### Per-socket scope (the conditional recommendation)

- **Per-user socket (`OwnerUnixIncoming`, `DaemonTransport::UnixUser`): per-connection ids.**
  This is the fix #391 asks for. It brings the socket to **TCP parity**, and no further. A
  same-uid process can still open its own connection, send a later HELLO, become current, and
  then set the version. That is the residual risk already documented for TCP
  (`daemon_commands.rs:42-56`). It is not closed here and cannot be on a same-uid socket
  (`activation.rs:150-153`).
- **System socket (`RootUnixIncoming`, `DaemonTransport::Unix`): decided by the PR 1
  characterization tests (Task 1c).**
  - **Default: per-connection ids too, if** those tests show readoption and both HELLO orders
    still hold when old and new streams have *distinct* Unix keys. In that case they hold
    because the newest snapshot is staged per connection and each connection's HELLO follows
    its own Subscribe.
  - **Fallback, if they regress:** keep transport-wide matching where `peer_is_trusted_daemon()`
    is true. Either `RootUnixIncoming` keeps plain `UnixStream` (no `UnixPeerInfo`, so the key
    stays `None`), or `PendingSnapshots`/`observe_daemon_version` compare keys only on untrusted
    transports. The first is a one-line change in `activation.rs` and is the preferred fallback.
  - My expectation, from section 3: the cross-connection Subscribe/HELLO split can happen, so
    **the fallback is the likely outcome.** The system socket would then keep today's shared
    key, explicitly documented. That loses nothing in security, because every peer is root.

### Alternative: document it as accepted for the dev-only flavour

Cheap (doc comments only). The per-user flavour ships nowhere (#35). But it leaves a
key that silently merges unrelated peers in the transport that every bridge test now uses
(#386). Any future two-mock test then means something different from what it says (learning
`2026-11-19-bridge-tests-use-the-per-user-...`). **Rejected**: the owner wants it fixed, and the
fix is small and contained.

## PR 1: `fix/391-connkey-peerkey` (bridge crate only; behaviour-neutral in production)

TDD order. Each RED is committed in its own commit so a reviewer can see it fail.

1. **RED tests** (`snitchwatch-bridge`):
   a. `ping_subscribe.rs`: add `unix_ping(version, id)` that inserts a `UnixPeerInfo` extension.
      Add `only_the_stream_holders_ping_sets_the_daemon_version_on_unix`, mirroring `:337`, with
      ids 1 and 2 on a `UnixUser` service. **Fails today**: the id is ignored, every key is
      `None`, and peer 2's 1.8.0 ping overwrites 1.9.
   b. `daemon_peer.rs` unit tests for `conn_key`: Tcp info gives `Tcp(addr)`, Unix info gives
      `Unix(id)`, no extension gives `None`, and a `Request` carrying both extensions is decided
      by Tcp (a defined precedence; it cannot happen in production).
   c. **Characterization (decides the system-socket scope):** copies of
      `rules_staging_tests.rs:126-200` and the readoption cases in `rules_refused_tests.rs`
      under `DaemonTransport::Unix` with `Some(PeerKey::Unix(1))` for the old stream and
      `Unix(2)` for the new one. Cover both HELLO orders, close-then-fallback, and the
      "Subscribe on connection 1, HELLO on connection 2" split. Record the result in the PR body.
2. **GREEN**: add `PeerKey`, `UnixPeerInfo`, `conn_key`; change `ConnKey`; update the three
   handler sites and the test helpers listed in section 2.
3. **Reframe `ping_subscribe.rs:375`** as "an in-process request (no connection info) is taken
   when the stream has none". The behaviour is unchanged; the old wording was a claim about
   Unix that is now false.
4. **Rewrite the docs** that become false: `daemon_commands.rs:11-32,57-61,79-84`,
   `version_gate.rs:27-33`, `rules_sync.rs:74-94,124-127`, the `compile_fail` doctest
   (`:187-194`). Each should state, per transport, which key applies.
5. Nothing in production builds a `UnixPeerInfo` yet, so behaviour is unchanged. Say so in the
   PR body.

Verification: `just test-bridge`, `just check`, `just test`. The `debug!` field change may alter
log text; grep for asserts on it.

## PR 2: `fix/391-unix-connection-ids` (bridge-cli: wire it up)

1. **RED** (bridge-cli, `src/lib_tests.rs` or a new `src/unix_peer_key_tests.rs`): build a
   bridge with `BridgeConfig::user_unix_for_tests(tempdir)`. Connect **two**
   `MockOpensnitchd::connect_unix` to one socket. Mock A subscribes, says HELLO and pings 1.9.
   Mock B pings 1.8.0. Assert the recorded version stays 1.9. **Fails today** (both are `None`).
   Optionally add an `activation.rs` unit test that two accepts yield distinct, increasing ids
   and that a rejected uid consumes no id.
2. **GREEN**: `UnixPeerStream` + `Connected`; id assigned in `poll_accept_filtered`; `Item`
   changed on `OwnerUnixIncoming` (and on `RootUnixIncoming` only if PR 1 Task 1c was green;
   otherwise add a comment there citing this plan and the characterization result).
3. **#386-migrated tests**: run all of bridge-cli. Single-mock tests are unaffected: one mock is
   one connection and one key. Tests that reconnect (`blocklist_enforcement.rs:453-455`,
   `curated_support/mod.rs:73,102` used twice, `rules_count_hint`) now get a new key per mock,
   as on TCP. The mocks Subscribe before HELLO, so they should keep their lists. Investigate any
   red rather than adjusting the assert. The learning's "Mutating fan-out to current-stream-only
   leaves bridge-cli green" still holds: delivery is per transport, not per key.
4. `HANDOFF.md` entry; `trail note` (system-socket decision and the reason); close #391.

Verification: `just check`, `just test` (both `--workspace`), `just test-bridge`,
`just package-check`, then the release steps below.

## Behaviour that must NOT change

- TCP: `PeerKey::Tcp(remote_addr)` is the same value as today's `Some(addr)`. All existing TCP
  tests pass unmodified apart from the helper spelling.
- A single daemon connection on either Unix socket: same stream selection, staging, adoption,
  version recording and delivery. One connection means one key, just as `None` was one key.
- Delivery policy (`Delivery`, `peer_is_trusted_daemon`) is unchanged. It switches on the
  transport, not the key.
- In-process `Request::new` calls keep `None`.

## Security considerations

- **Unforgeable id**: assigned server-side at accept from a private atomic, after the uid
  filter. It never comes from the peer: not from metadata, a header or the body.
  `request.extensions()` can only be filled by the server stack. Clients cannot set extensions
  over the wire (gRPC metadata goes into `MetadataMap`, not `Extensions`).
- **No leak**: the id is a counter. uid/pid appear only in local tracing at `debug`/`info`,
  never on the WS wire or in `ServerMessage`. The bridge log is the service's own and already
  records peer uids on rejection (`activation.rs:184`).
- **No new trust**: no gate starts trusting a Unix key. `peer_is_trusted_daemon()` stays the
  only trust switch (learning `2026-11-17-a-new-daemon-transport-variant-must-...`: audit every
  check that compares transports; this PR adds no transport comparison).
- **u64 overflow**: about 2^64 accepts. Not reachable.
- **Residual**: UnixUser reaches TCP parity, not daemon authentication (see "Per-socket scope").
- Reviewers: **opus `security-reviewer`** (extension trust, accept path, system-socket
  decision) and **`code-reviewer`** (sonnet) on each PR. PR 1's characterization result goes to
  the security reviewer explicitly.

## Release impact

Bridge production code changes, so the binary changes; PR 1 changes it too (types and logs).
No new dependency is expected (`AtomicU64` and tonic's `Connected` are already available), so
`Cargo.lock` and the MANIFEST `cargo_lock_sha256` should not change, but the binary sha256
will.
- `just release-bridge-repro`: build plus a byte-identical rebuild in the pinned Fedora image.
  Then `just release-verify <tarball> <sha256>`.
- After merge, **ask the owner before sending** (standing learning
  `2026-10-18-send-the-tower-...`). Send the tower the new `main` SHA and these checks:
  1. The system bridge plus opensnitchd connect, and the Rules page lists.
  2. `systemctl restart opensnitchd` (redial): the list comes back without a GUI restart, and
     the daemon version is recorded within a few seconds.
  3. Repeat 2 several times quickly (flap). This is the real-daemon check of section 3, which
     the mock does not model (`.agent_native/agent_roadmap.md` item 7).

## Risks and rollback

| Risk | Mitigation |
|---|---|
| System-socket redial regression (section 3) | Task 1c decides before production changes. The tower checks 2–3 above. Fallback: `RootUnixIncoming` back to plain `UnixStream` (one line) |
| A #386 test silently changes meaning | Full bridge-cli run in PR 2; investigate every red |
| Version recorded a ping or two later after a redial | Accepted; documented in `version_gate.rs` |
| Doc comments left describing the shared key | PR 1 Task 4 lists every one |

Rollback: revert PR 2 and production goes back to `None` keys. PR 1 alone changes no production
behaviour, so it can stay.

## Blockers

None that stop the work. The system-socket scope is decided by evidence in PR 1, not by the
owner. One real limit: the cross-connection Subscribe/HELLO split cannot be confirmed without a
live daemon. The tower check covers it.

## Open questions (non-blocking)

- If the tower flap check (3) shows the list withdrawn on the system socket even with the
  fallback, that was already true before this change. File it separately.
