# Reviewed r13 broad consent implementation

Implement the approved Choice B contract reviewed at SHA256
`dc728e7143d6ca7a893fede89157a0fca67b8d60ffb2170ae513c2cf6e615bf6`
with independent security design PASS. Bridge/CLI scope: strict authenticated
request tickets and connection leases; versioned durable canonical provenance;
legacy broad refusal and partial private results; guarded reconciliation/final
dispatch and generic DNS re-enable refusal. GUI, installer/toolchain and
publication workflows are independent lanes. No release approval is implied.

1. Add protocol/capability and duplicate-preserving strict decoding.
2. Add bounded ticket state, death/admission serialization and revocation revisions.
3. Add v3 provenance while preserving legacy choices and existing policies.
4. Save before broad dispatch, recheck current authority atomically at queue admission.
5. Verify focused/full bridge+CLI tests and independent implementation review.

Protocol, storage, compatibility and source map are documented in
[the bridge contract](../../bridge/curated-broad-opt-in.md).
