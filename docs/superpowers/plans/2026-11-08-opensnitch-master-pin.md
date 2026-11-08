# Plan: pin vendor/opensnitch to upstream master (a1353848) to include the unreleased eBPF/daemon work

Date: 2026-11-08. Status: DRAFT, awaiting owner confirm.

**2026-11-08 update:** both read-only daemon reviews of `a1353848` are done (non-rule = GO/LOW; the rule engine refuses `network`+`source.network` and regexp/simple/range + network operands panic the daemon, so PR A's version gate must merge first). The owner chose a SOFT fork: step 0 below. The pin PR ends NOT merged (overnight-run-6 task 3).
Owner decisions taken (2026-11-08): include **all of upstream master**; **pin one master commit and
bump `vendor/`**. This supersedes the settled "pinned at v1.8.0" note in `CLAUDE.md` (the owner's call).
Claims not read from a file or `gh` output are marked **UNVERIFIED**.

## Why
Upstream's last release is v1.8.0 (2025-12-15, ~11 months ago). `master` is 224 commits ahead and
includes `614537c9` "fix kernel 6.19 build, add some hardening options" (2026-02-27), the fix for
upstream #1537, plus small `opensnitch-procs.c`, `ebpf.go`, `events.go`, `notifications.go` edits and
cilium/ebpf 0.22. No release is coming soon. Newest master commit: `a1353848ba1b660320e90cefea782c3fba272c00`
(2026-07-26).

## Facts (read-only audit of main 11db9b4c)
- `proto/ui.proto` and `daemon/data/default-config.json` are **unchanged** v1.8.0..master, so the
  bridge's gRPC contract is unchanged. `snitchwatch-proto/build.rs` compiles `ui.proto` from the
  submodule; `daemon_config.rs:274` includes `default-config.json`.
- **`packaging/release/pins.env:5-6`** holds `SW_OPENSNITCH_COMMIT` + `SW_OPENSNITCH_TAG`.
  `build-bridge.sh:119-124` and `bridge_artifact.py:304-311` **die unless** the gitlink and the checked-out
  submodule equal `SW_OPENSNITCH_COMMIT`. The bridge release tarball's MANIFEST carries both values, so
  its hash changes (`ui_proto_sha256` does not, the proto is identical). Master has no tag.
- `candidate-gui.yml:174-176` asserts only that the gitlink in the candidate commit equals what
  `build-candidate.py:54-65` recorded: nothing hard-coded, safe. Already-published candidates name the old SHA
  (a published candidate is never replaced); r16.1 is published, so this lands after it.
- `recipe.yml:58` installs the released **v1.8.0 RPM**: the bluebuild image does NOT get the master
  work from this bump. The tower image builds the daemon from Go source, so only it can.
- Upstream master `daemon/go.mod` needs **Go 1.25.0** (v1.8.0: 1.23.0). Nothing in this repo builds the
  daemon (`go` is not installed here); the tower's builder must have Go >= 1.25 (**UNVERIFIED**).
- ~21 files cite `vendor/opensnitch/...:LINE`. At risk: `notifications.go` citations in
  `grpc_server.rs:618`, `translator/rule_notification.rs:12,52`, `tests/bridge_protocol_test/rules.rs:19`,
  `HANDOFF.md:837`, plan q04:26; the eBPF runbook (`ebpf.go:56,147-187`) and plan
  `2026-10-29-ebpf-q05-q06.md:96-141`.
- "stock v1.8.0 ..." behavioural comments across crates describe the shipped daemon: leave them.

## Changes (one PR, branch `chore/pin-opensnitch-master`)
0. Switch the `.gitmodules` url to `https://github.com/bearyjd/opensnitch.git`, `git submodule sync`, and pin `snitchwatch/stable` = `a1353848` (the fork's master is identical to upstream; the 3 patch branches are NOT in it). Risk: that fork branch is unprotected.
1. `git -C vendor/opensnitch fetch origin`, check out `a1353848`, commit the gitlink bump.
2. `pins.env`: `SW_OPENSNITCH_COMMIT=a1353848...`, `SW_OPENSNITCH_TAG=master-a1353848` (non-empty label;
   master is untagged). Update the `v1.8.0` text in `docs/packaging/bridge-release-artifact.md:100,145`
   and `docs/ENV.md:95-96`.
3. Prose: `CLAUDE.md:18` and the settled-decisions text (submodule is at upstream master a1353848; the
   bluebuild RPM still ships v1.8.0); the `recipe.yml:48` comment that claims the RPM matches the vendored
   submodule becomes false: reword it (no behaviour change, the RPM line stays).
4. Verify unchanged: `proto/ui.proto` and `daemon/data/default-config.json` only. `daemon/rule` DID change (operator.go, operator_lists.go, loader.go; see the 2026-11-08 rule-engine review), so PR A's two vendor tests in `history/rule_identity_tests.rs` must be rewritten (overnight-run-6 task 3(iv)).
   (operator.go for `rule_identity_tests.rs:328`, `network_aliases.json` for `simulator/network.rs:118`).
5. Re-verify or annotate the changed-file citations (list above) against a1353848; mark the eBPF runbook's
   log-string/line cites "at v1.8.0 / re-check on the pinned commit". Add a runbook procedure: build the
   daemon + the three `.o` modules from the pin as a matched pair (Go >= 1.25 for the daemon).
6. **Review the 25 daemon commits** (`git log v1.8.0..a1353848 -- daemon`) for behaviour the bridge relies
   on (`notifications.go` +7/-3 touches CHANGE_CONFIG handling; `ui/`, `rule/`, `stats`): a read-only reviewer
   report is a precondition for merging.

## Verification
`just package-check`, `just test-bridge` (include_str! users), `cargo test -p snitchwatch-proto`, and the
reproducible release: `just release-bridge-repro` then `just release-verify <tarball> <sha256>` (podman,
heavy; confirms the new pin and MANIFEST). Expect a new tarball hash; no code change.

## Not changed / out of scope
`recipe.yml` RPM v1.8.0 (the bluebuild image keeps the released daemon); the "stock v1.8.0" behaviour
comments; the tower's daemon/module build from the pin (tower work: Go >= 1.25, build both from
a1353848, redo the load test per the runbook). Whether to also move the bluebuild image off the RPM is a
separate decision.

## Process
Executor builds the edits; I run the checks; independent reviews: code review of the doc/pins diff plus a
read-only daemon-diff review (item 6). Merge only after the owner confirms; tell the tower the new SHA.
