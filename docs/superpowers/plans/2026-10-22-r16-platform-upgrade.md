# Platform roadmap r15 / r16 / r17 (umbrella plan) — DRAFT for owner approval

Status: proposal. No code. r15 shipped on 2026-10-22 (main `bfc76e6`; the tower
promoted it and the owner accepted it). Each workstream gets its own plan doc
under `docs/superpowers/plans/` before implementation (repo convention).
Packaging, unit, socket and daemon-config changes still go through the tower's
cut process. Claims taken from the tower session are marked "(per tower)": they
could not be checked from this repo.

Owner direction (2026-10-22): "a large upgrade before r16 ... all of this ...
on the Snitchwatch platform" = log export + analysis, rules/prompt quality, GUI,
and platform/engine foundations, split across releases as below.

## Constraints that do not move (settled decisions, CLAUDE.md)
- Component A keeps riding `opensnitchd`; a from-scratch nfqueue/eBPF engine is
  explicitly deferred. WS5 below is a decision gate, not a build, unless the
  owner supplies new evidence. (Not to be confused with `ProcMonitorMethod:
  ebpf`, which is an opensnitchd option and is r16's work.)
- Components A and B stay separate apps. Kirigami is the GUI target. The WS
  transport is a Unix socket + handshake token. Shipped config overrides the
  fail-open default to `deny`. `vendor/opensnitch` is read-only.
- Plan packaging/unit/socket/config changes as their own PRs, never mixed with
  features.

## Release shape (owner, 2026-10-22)
- Nothing in these workstreams is a hard "must": every item is a "could".
- r15 (shipped): improved GUI + Bazzite update. Merged: system socket
  transport, collapsed repeat rows, "No longer recommended" list, rust 1.99.0
  (`pins.env`), lock fix (rustls-webpki, webpki-roots), the unanswered-prompt
  log line (#186).
- r16: eBPF process monitoring restored, as its own release (image build + daemon
  config + the tower's checks), plus the cheap measurement groundwork it needs:
  long-uptime CPU/RSS sample series, CI canary for pinned toolchains, release cut
  checklist. Optional small diagnosability pieces: structured fields on log
  lines, GUI-side lag diagnostics. Target date: "when ready".
- r17: observability and rules: history store, redacted export + analysis, rule
  suggestions, GUI history view + Export button, uid-aware rules, upstream daemon
  patches (the open items in #178), hardware proof.
- The engine decision gate (WS5) is revisited AFTER r16: reopen only if eBPF
  fails or CPU stays high.
- r16 gates: (1) run the daemon as root with ebpf and capture the log; (2) build
  or supply the three modules in the image build; (3) flip config + the tower's
  verifiers together with a tested `proc` fallback; (4) ship only if a full day
  on eBPF has lower mean CPU than a fresh `proc` run under comparable conditions
  (~20.5% of a core is a sanity reference), an RSS
  slope no worse than `proc`, and no daemon errors (same gate as the measurement
  note).
- Open: whether "the big jump" is the eBPF move or something else.

## Workstreams

### WS1 Observability and export (r17; plan: 2026-10-22-log-export-for-analysis.md)
Redacted-by-default export (GUI Export action; bridge redacts; GUI writes the
file), rolling on-disk history bounded by size and age (off until enabled).
Structured log fields may land earlier, in r16, as a small diagnosability piece.
This also provides the history store that WS2 and WS3 build on.
Gate: security-reviewer pass on the design before code.

### WS2 Rules and prompt quality (mostly r17)
- Measure first: unmatched asks per minute at idle becomes a tracked number
  (WS1 history gives it). The r15 log line already names path+destination.
- uid-aware recommended rules: the DNS rule is already pinned by account NAME
  (`systemd-resolve`) and resolved to a uid at runtime
  (`curated/manager_resolver.rs`), not by a fixed 193. Nothing in `packaging/`
  hard-codes 193. The owner's `getent` shows `systemd-resolve` = uid 990 on the
  tower host (this dev host maps it to 193), so the name lookup should give 990.
  Leading hypothesis (unverified): the laptop still has an old fixed-uid DNS rule
  and the name-pinned copy is waiting for the user's confirmation (the "moved
  copy" path in `curated/`). Verify in the GUI before touching code; the fix may
  be UX (make the pending confirmation visible), not rule data.
- New recommended entries from tower captures (chronyd runs as uid 994, not root,
  per tower; rpm-ostree/skopeo; fwupd). Tracked in #178.
- Rule suggestions from history ("you allowed X 14 times; make it a rule"),
  depends on WS1's history store.
- Stay clear of the daemon-reload gap (needs upstream; tracked in #178).

### WS3 GUI (r17)
Needs the owner's pain points before design (ask: which screens hurt?). Likely:
the Export action (WS1), a history/pattern view and rule suggestions (WS2),
onboarding for the deny-by-default stance. Keep file sizes under the 800-line
rule; the recent splits (#173/#174/#182) are the baseline.

### WS4 Platform foundations (measurement and canary in r16; rest r17)
- Release engineering: the r15 cut was manual and a toolchain 404 stopped the
  tower's builds (per tower). Add a CI canary that fails early when the pinned
  rust (or other pinned inputs in `pins.env`) can no longer be fetched; write the
  cut checklist; keep `audit` green with a routine for advisory bumps (this
  week's rustls-webpki/webpki-roots bump was exactly that).
- Test infrastructure: recorded daemon-traffic replay fixture (agent-roadmap
  item 7), verify the Kirigami headless tests actually run in CI, watch flaky
  tests (#110 is closed; reopen if CI flakes).
- Performance: set a budget for the bridge and GUI; see
  `docs/superpowers/specs/2026-10-22-r16-measurement-series.md`.
- Upstream: propose the daemon changes #178 lists ("rules reloaded" push,
  over-budget list feedback) to opensnitch as patches; our fork stays read-only.
- Hardware proof for Phase 2/6 on a real Bazzite host.

### WS5 Engine decision gate (no build)
Write a one-page decision record: what evidence would justify revisiting the
deferral (e.g. upstream refuses the daemon changes, or opensnitchd CPU cannot be
bounded even on eBPF). Default outcome: stay on opensnitchd.

## Evidence from the r14 laptop (per tower, uptime 12.6 h, 2026-10-22)
- Unanswered prompts are sustained, not a startup burst: 1452 "nobody answered"
  lines since boot, 115 in the last hour (~1.9/min). Which program and
  destination is unknown until the r15 log line is read on the laptop.
- opensnitchd: ~20.5% of one core on average over 12.6 h (22.8% instantaneous),
  RSS 222 MB, up from 122 MB at 6 min, so memory grows. The earlier 40-70% did
  not recur (and was also on `proc`). Kirigami GUI ~8-14% CPU, 362 MB RSS.
  Bridge 0.1%, 46 MB. -> a long-uptime sample series, not single readings.
- NetworkManager poll WARN: once, 1 s after bridge start,
  `org.freedesktop.DBus.Error.ServiceUnknown: The name is not activatable`, i.e.
  the bridge starts before NetworkManager is on the system bus. The poll retries
  every 3 s and the warning never repeated, so it is boot ordering and harmless.
  Optional polish: log the first ServiceUnknown at `info`/`debug`.
- "feed lagged" GUI lines could not be read: the GUI runs in a bwrap/flatpak
  sandbox and the tower cannot see its journal. 10 bridge-side re-broadcasts
  since boot (the bridge answering a GUI `RequestSnapshot` after the GUI's feed
  lagged; production broadcast capacity is 256, `bridge-cli/src/lib.rs:189`).
  -> GUI-side diagnostics are invisible from outside the sandbox; the export and
  diagnostics should carry GUI-side lag events (the GUI reports its lag count to
  the bridge, or an in-app diagnostics page shows them).

## r16 theme: restore `ProcMonitorMethod: ebpf`
Read-only groundwork: `docs/superpowers/specs/2026-10-22-ebpf-groundwork.md`.
Facts from the repo:
- Both shipped opensnitchd configs set `ProcMonitorMethod: ebpf`
  (`packaging/system/opensnitchd-default-config.json:21`); the tower overrides it
  to `proc` after staging (per tower; `docs/packaging/system-bridge-integration.md`).
- The "ebpf failed on the laptop" premise (issue #6, closed) was a
  rootless-container permission artifact, per `packaging/bluebuild/recipe.yml`
  and `docs/packaging/rpm-ostree-layering.md`. Its message "unable to load eBPF
  module" also covers a missing file or a verifier error. The only recorded
  failure text comes from that case, so ebpf has effectively never been tried as
  root on kernel 7.2.0.
- The modules are NOT CO-RE: they need kernel headers matching the running
  kernel. BTF is not the real precondition (the laptop has it anyway, per tower).
- The daemon falls back to `proc` on its own when the MAIN module fails to load;
  if only the procs module is missing it stays on ebpf with the proc monitors
  alongside (see the groundwork note for how to read the log).
- Per tower: the image no longer ships the opensnitch RPM; it builds a patched
  daemon from Go source (pinned b404c4c) and installs no `.o` module files, so
  ebpf currently has nothing to load. The tower's stage/verify code hard-requires
  `ProcMonitorMethod == "proc"` (three places), which must change together with
  our config.
- No ebpf CPU figure exists anywhere.
Work, in order:
1. Cheapest test: run the daemon as ROOT with ebpf and the v1.8.0 RPM's prebuilt
   modules; capture the full opensnitchd log and `dmesg | grep -i bpf`. Needs the
   owner (touches the firewall). A clean load is necessary, not sufficient: also
   check connections resolve to the right process.
2. If needed, build the modules from `vendor/opensnitch/ebpf_prog` in the tower's
   image build (clang/llvm + matching kernel-devel; both unverified for the
   custom Bazzite kernel) and install them under `/usr`.
3. Flip the tower's verifiers/tests and our shipped config together; keep the
   tested `proc` fallback.
4. Measure CPU/RSS before and after (measurement-series note).
It changes the image build, units and daemon config, so it goes through the
tower's cut process as its own plan and PRs, never mixed with features.

## Risks
- Scope: five workstreams cannot all land in one release; hence the split.
- Persistent history is a new privacy surface; export and suggestions must not
  leak it. Security review is a gate, not a courtesy.
- Many changes touch the shipped binary; each needs its own tower review. Batch
  them and keep packaging/unit/socket changes out of feature PRs.

## Decisions needed from the owner
1. What is "the big jump" for r16: the eBPF move, a newer opensnitch, a major
   Bazzite or kernel move, a 1.0 milestone, or something else?
2. GUI: which screens or workflows hurt today?
3. Engine gate: any new evidence that reopens the own-engine deferral?
