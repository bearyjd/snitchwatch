# r16 platform upgrade (umbrella plan) — DRAFT for owner approval

Status: proposal. No code. Each workstream gets its own plan doc under
`docs/superpowers/plans/` before implementation (repo convention). Nothing here
merges to `main` until the tower has pushed `r15-candidate` (main freeze).

Owner direction (2026-10-22): "a large upgrade before r16 ... all of this ...
on the Snitchwatch platform" = log export + analysis, rules/prompt quality, GUI,
and platform/engine foundations.

## Constraints that do not move (settled decisions, CLAUDE.md)
- Component A keeps riding `opensnitchd`; a from-scratch nfqueue/eBPF engine is
  explicitly deferred. WS5 below is a decision gate, not a build, unless the
  owner supplies new evidence.
- Components A and B stay separate apps. Kirigami is the GUI target. The WS
  transport is a Unix socket + handshake token. Shipped config overrides the
  fail-open default to `deny`. `vendor/opensnitch` is read-only.
- Packaging, units, sockets and daemon config changes go through the tower's
  cut process; plan them as their own PRs, never mixed with features.

## What r16 is for
Today the platform works but is hard to observe and hard to tune: unmatched
prompts (~2/min idle on r14) cannot be explained from logs, rules are written
by hand against uids the GUI cannot see, and each release cut is manual. r16 =
make it observable, make it smarter, make shipping routine.

## Workstreams

### WS1 Observability and export (plan exists: 2026-10-22-log-export-for-analysis.md)
Redacted-by-default export (GUI Export action; bridge redacts; GUI writes the
file), rolling on-disk history bounded by size and age, structured log fields.
This also provides the history store that WS2 and WS3 build on.
Gate: security-reviewer pass on the design before code.

### WS2 Rules and prompt quality
- Measure first: unmatched asks per minute at idle becomes a tracked number
  (WS1 history gives it). r15's log line already names path+destination.
- uid-aware recommended rules (#113): the DNS rule is already pinned by account
  NAME (`systemd-resolve`) and resolved to a uid at runtime
  (`curated/manager_resolver.rs`), not by a fixed 193. The tower sees resolved
  at uid 990 on the r14 laptop, while this dev host maps `systemd-resolve` to 193
  and 990 to `flatpak`. First task: find out what the laptop's resolver account
  is called and whether the name lookup, not the pin, is what fails. Update
  2026-10-22: the owner's `getent` shows `systemd-resolve` = uid 990 on the tower
  host, so the name is right and the lookup should give 990. Nothing in
  `packaging/` hard-codes 193. Leading hypothesis: the laptop still has an old
  fixed-uid (193) DNS rule and the name-pinned copy is waiting for the user's
  confirmation (the 'moved copy' path in `curated/`). Verify in the GUI before
  touching code; the fix may be UX (make the pending confirmation visible), not
  rule data.
- New recommended entries from tower captures (chronyd runs as uid 994, not
  root; rpm-ostree/skopeo; fwupd).
- Rule suggestions from history ("you allowed X 14 times; make it a rule"),
  depends on WS1's history store.
- Stay clear of the daemon-reload gap (#65, needs upstream).

### WS3 GUI
Needs the owner's pain points before design (ask: which screens hurt?). Likely:
the Export action (WS1), a history/pattern view and rule suggestions (WS2),
onboarding for the deny-by-default stance. Keep file sizes under the 800-line
rule; the recent splits (#173/#174/#182) are the baseline.

### WS4 Platform foundations
- Release engineering: the r15 cut was manual and a toolchain 404 stopped the
  tower's builds. Add a CI canary that fails early when the pinned rust (or
  other pinned inputs in `pins.env`) can no longer be fetched; write the cut
  checklist; keep `audit` green with a routine for advisory bumps (this week's
  rustls-webpki/webpki-roots bump was exactly that).
- Test infrastructure: recorded daemon-traffic replay fixture (agent-roadmap
  item 7), verify the Kirigami headless tests actually run in CI, watch the
  flaky-test pins (#110).
- Performance: set a budget for the bridge and GUI; re-sample opensnitchd CPU
  over long uptimes (r14 showed 2-12% early vs 40-70% earlier).
- Upstream: propose the two daemon changes #178 needs ("rules reloaded" push,
  over-budget list feedback) to opensnitch as patches; our fork stays read-only.
- Hardware proof for Phase 2/6 on a real Bazzite host.

### WS5 Engine decision gate (no build)
Write a one-page decision record: what evidence would justify revisiting the
deferral (e.g. upstream refuses the daemon changes, or opensnitchd CPU cannot be
bounded). Default outcome: stay on opensnitchd.

## Evidence from the r14 laptop (tower, uptime 12.6 h, 2026-10-22)
- Unanswered prompts are sustained, not a startup burst: 1452 "nobody answered"
  lines since boot, 115 in the last hour (~1.9/min). Which program and
  destination is still unknown until the r15 log line is on the laptop.
- opensnitchd: ~20.5% of one core on average over 12.6 h (22.8% instantaneous),
  RSS 222 MB, up from 122 MB at 6 min, so memory grows. The earlier 40-70% did
  not recur. Kirigami GUI ~8-14% CPU, 362 MB RSS. Bridge 0.1%, 46 MB.
  -> WS4 performance budget needs a long-uptime sample series (RSS over time),
  not single readings.
- NetworkManager poll WARN: once, 1 s after bridge start,
  `org.freedesktop.DBus.Error.ServiceUnknown: The name is not activatable`, i.e.
  the bridge starts before NetworkManager is on the system bus. The poll retries
  every 3 s and the warning never repeated, so it is boot ordering and harmless.
  Optional polish: log the first ServiceUnknown at `info`/`debug`.
- "feed lagged" GUI lines could not be read: the GUI runs in a bwrap/flatpak
  sandbox and the tower cannot see its journal. 10 bridge-side re-broadcasts
  since boot. -> WS1/WS4 gap: GUI-side diagnostics are invisible from outside the
  sandbox. The export and diagnostics should carry GUI-side lag events (for
  example the GUI reports its lag count to the bridge, or an in-app diagnostics
  page shows them).

## Sequencing
0. Now to freeze lift: this plan, WS1 security review, WS2 uid investigation
   (read-only), WS4 canary design. Branches only.
1. After `r15-candidate` is pushed: WS4 foundations and WS1 core (history store,
   structured logs, export request) — they unblock the rest.
2. WS2 suggestions and WS3 UI on top of the history store.
3. Hardening: hardware proof, perf pass, flake sweep, docs, then the r16 cut
   through the tower's process.

## Risks
- Scope: five workstreams cannot all land in one release. Cut line below.
- Persistent history is a new privacy surface; export and suggestions must not
  leak it. Security review is a gate, not a courtesy.
- Many changes touch the shipped binary; each needs its own tower review. Batch
  them and keep packaging/unit/socket changes out of feature PRs.

## Release shape (owner, 2026-10-22; replaces the earlier proposed cut line)
- Nothing in these workstreams is a hard "must": every item is a "could".
- r15 = the improved GUI and the Bazzite update (already in flight with the
  tower: rust 1.99.0, lock fix, log line).
- r16 = back to eBPF process monitoring instead of `proc` ("ebpf vs proc"), and
  "a big jump" (owner to say what; see questions).

### Roadmap by release (owner accepted 2026-10-22)
- r15 (frozen, shipping): improved GUI + Bazzite update. Already merged: system
  socket transport, collapsed repeat rows, "No longer recommended" list, rust
  1.99.0, lock fix, unanswered-prompt log line.
- r16: eBPF restoration as its own release (image build + daemon config + the
  tower's checks), plus the cheap measurement groundwork it needs: long-uptime
  CPU/RSS sample series, CI canary for pinned toolchains, release cut checklist.
  Optional small pieces that make the eBPF run easier to diagnose: structured
  fields on log lines, GUI-side lag diagnostics.
- r17: observability and rules: history store, redacted export + analysis, rule
  suggestions, GUI history view + Export button, uid-aware rules, upstream daemon
  patches (#65, #109.4), hardware proof.
- Engine decision gate (WS5) is revisited AFTER r16: reopen only if eBPF fails
  or CPU stays high.
- r16 gates: (1) capture the real eBPF error on the laptop; (2) build the three
  modules in the image build; (3) flip config + the tower's verifiers together
  with a tested `proc` fallback; (4) ship only if a full day on eBPF beats the
  ~20% average-core baseline.
- Open: owner has not yet said whether "the big jump" is the eBPF move or
  something else, nor whether the first export slice (redacted export, no disk
  history) should be pulled into r16.

### r16 theme: restore `ProcMonitorMethod: ebpf`
Facts from the repo: both shipped opensnitchd configs set `ProcMonitorMethod:
ebpf` (`packaging/system/opensnitchd-default-config.json:21`,
`docs/packaging/system-bridge-integration.md:102`); the tower overrides it to
`proc` after staging (learning 2026-10-18). The v1.8.0 RPM used ebpf on kernel
>= 6.19; eBPF needs `CONFIG_DEBUG_INFO_BTF=y` and a matching eBPF module
(`docs/superpowers/specs/2026-07-14-daemon-kernel-diagnostics-design.md`). The
r14 laptop runs kernel 7.2.0-ogc6.1.
- What the tower found (2026-10-22, laptop kernel 7.2.0-ogc6.1.fc44):
  * Only recorded failure: `unable to load eBPF module (opensnitch.o)`
    (snitchwatch#6), from the v1.8.0 RPM's bundled module; no errno/BTF/CO-RE
    detail kept, and ebpf has NOT been retried on 7.2.0.
  * BTF is present (`/sys/kernel/btf/vmlinux`, CONFIG_DEBUG_INFO_BTF=y, BPF_SYSCALL,
    BPF_JIT, KPROBES, BPF_EVENTS). Missing BTF is ruled out; a module/kernel
    mismatch is still open.
  * STRUCTURAL: the image no longer ships the opensnitch RPM. The tower builds a
    patched daemon from source with Go (pinned b404c4c) and nothing builds or
    installs `opensnitch.o` / `opensnitch-procs.o` / `opensnitch-dns.o`. So on
    this image ebpf currently has no module files to load.
  * The tower's stage/verify code hard-requires `ProcMonitorMethod == "proc"`
    (snitchwatch-system-stage.py:49, snitchwatch-system-verify.py:118,
    bazzite-tower-snitchwatch-common.py:267). Those and their tests change
    together with our config.
  * No ebpf CPU number exists; the earlier 40-70% was also on `proc`.
- So r16's eBPF work is a build-and-packaging project, not a config flip:
  1. Get the real error: run the daemon once with ebpf and a hand-built module on
     the laptop and capture the full opensnitchd log plus `dmesg | grep -i bpf`.
     Needs the owner (touches the firewall); the tower did not try it.
  2. Build the modules from `vendor/opensnitch/ebpf_prog` (clang/llvm + matching
     kernel headers; unverified) in the tower's image build, and install them
     under the daemon's ModulesPath.
  3. Flip the tower's verifiers/tests and our shipped config together; keep a
     tested fallback to `proc` when the modules fail to load ("the daemon
     degrades badly" without one).
  4. Measure CPU/RSS before and after with the WS4 sample series.
- Because it changes the image build, units and daemon config, it goes through
  the tower's cut process as its own plan and PRs, never mixed with features.

## Decisions needed from the owner
1. (answered: all "could"; r15 = GUI + Bazzite update; r16 = eBPF + big jump)
2. What is the "big jump" for r16: a newer opensnitch/eBPF modules, a major
   Bazzite or kernel move, a 1.0 product milestone, or something else?
2b. (answered: when ready)
3. GUI: which screens or workflows hurt today?
4. Engine gate: any new evidence that reopens the nfqueue/eBPF deferral?
