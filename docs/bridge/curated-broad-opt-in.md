# Broad recommended DNS consent protocol

`curatedBroadOptInV1` is advertised only in the existing authenticated
acknowledgement. It is feature negotiation; the socket permissions, token
handshake, daemon UID admission and rule policy remain the security boundaries.
An absent capability means unsupported. There is no legacy enable fallback.

## Wire operations

All modern frames are at most 4096 bytes and reject duplicate/unknown fields,
wrong types and missing fields. `requestId` is 1–64 ASCII alphanumeric or hyphen
characters; `id` is exactly `dns-resolved`.

| Action | Required fields beyond action/requestId/id |
| --- | --- |
| `prepareCuratedBroadOptIn` | `purpose`: `enable`, `reenable`, or `keep` |
| `enableCuratedBroadDefault` | purpose `enable`/`reenable`, ticket, consent |
| `keepCuratedBroadDefault` | purpose `keep`, ticket, consent |
| `cancelCuratedBroadOptIn` | none; requestId matches the prepare being cancelled |

Consent is `resolver-any-address-v1`. A private
`curatedBroadOptInPrepared { requestId,id,purpose,ticket }` returns a 64-character
lowercase hexadecimal ticket containing 32 OS-random bytes. It expires after
120 monotonic seconds and binds one connection, exact prepare requestId, purpose, canonical SHA256 and
entry revision. Each connection has one pending ticket. Replacement,
submission (including a mismatch), malformed input, cancel or lease death
consumes it. Off/removal from another client invalidates it by entry revision.
Tickets are never logged, broadcast, saved or replayed after reconnect.

The GUI confirmation must disclose `/usr/lib/systemd/systemd-resolved`, its
account (`systemd-resolve`, whose uid the bridge looks up at run time: it is
allocated per image build), TCP/UDP 53, IPv4/IPv6, any destination and data
carriage inside DNS lookups. A grant names the account, not a uid: the
rule's fingerprint leaves the uid out, so a grant made for the earlier fixed
uid no longer matches and is asked for once more.
Bulk enable excludes DNS even when only DNS remains. The bridge cannot prove
a human clicked a button: an authenticated client can deliberately prepare
and submit a fresh opt-in.

`curatedDefaultsRequestResult { requestId?,results }` is private. Each result
has `id`, `outcome`, `reason` and optional `choiceAccepted`/`persisted` booleans.
Outcomes are `choiceAccepted`, `refusedConsent`, `unknownId`, `unavailable`,
`invalidIntent`, and `staleIntent`. Choice acceptance and durable authority
are separate from installation; only daemon OK reports Installed. Legacy
mixed requests run narrow IDs once in first-occurrence order and refuse every
broad enable before changing choices/resetting failures. Broad refusal appears
in the existing summary problem. Awaiting consent uses the known NotInstalled
wire status plus `awaitingConsent` in problem for old GUI compatibility.

## Storage and dispatch

Choices file v3 records versioned broad grants with ID, consent version,
SHA256 of deterministic canonical policy and `enable` or `adoptionOnly`
authority. Fingerprinting excludes enabled/created and includes all policy
fields and ordered conditions. Legacy v1/v2 choices preserve enabled IDs but
carry no install/re-enable/retry authority. Snapshots/adoption/capabilities
cannot mint provenance. Disabled Keep records adoption only and sends no
command; deliberate re-enable is a separate operation. Edited, unknown and
legacy path-only copies remain untouched.

Manager admission locks state then the server-created connection lease.
Death uses that same lease lock; queued unadmitted requests after death refuse.
Accepted choices that save successfully may reconcile after disconnect,
without replaying the consumed ticket. This implementation grants no session-only
activation: inert/nonpersistent/save-failed bridges do not activate DNS.
The ordered atomic owner-only/no-link save completes before any dependent
broad command. Save failure is reported as persisted=false and leaves the
manager inert. A failed revocation write blocks in-session activation; it does
not claim an unwritten revocation survived restart.

Planning and final daemon queue admission both require current authority.
The final state guard serializes revision/grant checks with dispatch; the daemon inner and rules-cache locks validate stream generation, canonical presence and deletion markers through queue admission; off and
removal revoke grants and bump revision before later sends. Stale plans,
changed policy, deletion markers and generic broad on refuse. Rules-page off
preserves the disabled rule while revoking authority. Valid saved grants never
retrofit consent onto legacy actions. Existing daemon refusal/timeouts,
uncertain outcomes, ordered saves and failure backoff retain their semantics.

## Source map and verification

- `ws_messages`/`curated_wire`: strict DTOs, correlation, results.
- `ws_server`/`curated::intent`: authenticated context, ticket, lease lifecycle.
- `curated::store`/`reconcile`: durable provenance and pure planning guard.
- `curated::manager_intent`/`manager`: admission, revocation, ordered result and final dispatch.
- `daemon_commands::curated_command` and CLI `rule_commands::gates`: reserved-name activation guard.

Run bridge and CLI package tests with an isolated Cargo target/runtime directory.
Tests cover modern parsing, ticket replay/lifecycle, queued lease death,
legacy partial refusal, saved authority/restart, adoption-only Keep, off,
stale dispatch, persistence failure and mock-daemon canonical DNS installation.
Real NFQUEUE/socket access/daemon UID/reconnect/migration/rollback runtime
acceptance remains a release gate outside these unit/mock tests.
