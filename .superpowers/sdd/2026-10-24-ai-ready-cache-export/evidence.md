# AI-ready current-cache summary: implementation evidence

The owner-approved increment is manual summary JSON from the shared current connection cache. It includes a dictionary and provider-neutral analysis prompt. Persisted history, readable destinations, raw/detail modes, JSONL/CSV, provider APIs/uploads, schedules and automatic rules remain outside this increment.

## Reviewed source

- Private filesystem writer/view: PR [221](https://github.com/bearyjd/snitchwatch/pull/221), reviewed `cf2a6c5fb49249896ad8eeb3e8c6c20897035cb1`; merged `db9fbc26ff29ddcd7c220376824e8a00d75744b2`.
- Bounded projection/HMAC/grouping: PR [220](https://github.com/bearyjd/snitchwatch/pull/220), reviewed final `ebc1713e4038a4ddaf709606a9a228fc27b13121`; merged `5b0b3aa83267c0983ed210f8a288bd984bcebb90`. Its final test-only scanner fixture mutex fixes a reproduced inherited-writer `ETXTBSY` race.
- Private protocol/workers/logging: PR [222](https://github.com/bearyjd/snitchwatch/pull/222), reviewed `5671071582630f579ca5b5ea6d77cce06aefd626`; merged `db0a21667a3c1f3dea1308bf96d6c3395db502b0` after all six exact-head CI checks passed.
- Backend acceptance: reviewed `8df36f64890dc7fa23cd1bed617eae9bc58b63e1`.
- GUI and whole-feature source: independently reviewed `90f75359dc724e219e5f50dcdb415f954315990e`, with layout-only re-review at `56c86d052b05cf76680eeccadbcb0e6af3249e2a`, PR [223](https://github.com/bearyjd/snitchwatch/pull/223). The final documentation update preserves its product/test source at `56c86d0`. Exact-head CI is a required merge gate on the final PR head.

## Verification

Genuine failing assertions preceded fixes for snapshot classification, private repeated publication/diagnostic privacy, deadline invalidation after publication, reused request IDs before supervisor reap, generic GUI admission, late-reply busy state, malformed artifacts and invalid calendar dates. Subsequent reviews checked the corrected exact commits.

| Gate | Source | Result |
|---|---|---|
| Bridge/CLI, no default features | `5671071` | 1,825 passed; all-target Clippy `-D warnings` passed |
| Backend acceptance, production WS/MockOpensnitchd | `8df36f6` | 3 passed; strict Clippy passed |
| Workspace default members | `8df36f6` | 2,056 passed, one existing ignored; strict Clippy passed |
| Packaging checks | `8df36f6` | 8 Python and 90 Rust tests passed; shellcheck/systemd/YAML/JSON and release-feature Clippy passed |
| Native GUI/full integrations | `56c86d0` | 1,642 passed, four existing ignored; 88 harnesses |
| GUI all-target Clippy | `56c86d0` | `-D warnings` passed |

The three backend scenarios exercise exact nine-row filter/count/privacy expectations, correlated two-client success/refusals and fresh references, and a real 2,000-group response reduced to 500 groups. Exactly 1,000 groups/observations are omitted for artifact size and 500 for wire size. Actual artifact and escaped reply limits hold; no export-triggered daemon policy notification or provider call occurs.

Snapshot barriers order broadcast frames only. Deterministic same-ID private-slot/cancellation isolation and distinct authenticated WS request/cancel stamps supply the separate private-routing evidence. Trace capture in backend acceptance is application-target-only and bounded. Independent CLI/GUI subscriber tests cover the production transport log cap even with specific verbose directives; arbitrary library-embedding subscribers are outside that guarantee.

The registered GUI probes exercise independent UTC/app/missing-path/verdict controls, plaintext Unicode/escaped artifacts, immutable preview/save equality, explicit Save, private directory/file modes, cancellation/replacement/reconnection, stale or duplicate responses, and malformed/oversized rejection. A real Qt heartbeat continues while the writer is held; page destruction and ordinary window/tray dismissal cannot orphan the flow. An admitted save reports its committed location and Unconfirmed durability after disconnect without restoring preview.

The native test harness also emits a Qt Timer warning during process/test-thread teardown in unchanged baseline probes. This is recorded separately from passing functional completion, drained Qt callbacks and joined save work; no warning-free harness or physical Wayland acceptance is claimed.

## Delivery boundary

Merged source and green CI do not establish installation or release publication. A matching updated GUI and bridge are required; older bridges disable export honestly. Local bundle/system-overlay provenance, checksums and independent artifact inspection are retained separately under `dist/analysis-export-final-build/` when built. No host service, policy, installed application, candidate Git ref or release is changed by these tests.
