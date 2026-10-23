# GUI learning mode and bulk rule creation (DRAFT, r16 GUI item)

Status: draft plan, no code. GUI-only: no bridge, wire message or packaging change (one
documented cosmetic limit in the bridge's import preview, below, is accepted, not fixed
here). Owner decisions (2026-10-22): the shipped `DefaultAction` STAYS `deny` (settled
decision 5 in CLAUDE.md is not reopened); the clicking is fixed by a learning mode; the end
goal is "to find bad behavior and not normal browsing". Parent: the GUI clarity plans
(`2026-10-22-gui-clarity-overhaul.md`, `gui-phase0-clarity.md`).

## Problem
Every new program-to-destination connection needs a human, so the owner spends their life
clicking. Calm notices (0d) and friendly names (0a) reduce the noise but not the number of
decisions. We want one decision per PROGRAM, made once, from evidence, with the unusual
programs standing out, and with nothing approved that the owner did not explicitly tick.

## What exists today (verified in the code map and two reviews, 2026-10-22; paths under crates/)
- Silent mode "Allow and record": the feed task (`snitchwatch-kirigami/src/bridge_feed.rs:
  269-312`) answers each NEW pending row at arrival with a once-only
  `SetVerdict(allow, this_host, this_time)` (`silent_mode.rs:121-227`) and adds the row id to
  an in-memory NEEDS_RULE set (cap 4,096). The daemon never stores a `once` rule
  (`snitchwatch-bridge/src/translator/connection.rs`, #102), so a silent allow leaves no
  rule. Only the mode is persisted (`settings.rs:105`); the sets are lost on restart; no
  time bound; `STATE` is global and in memory; `set_silent_state` (`bridge_feed.rs:251`)
  pushes the mode and sweeps waiting rows.
- `handles.subscribe()` is a broadcast channel of 1,024 messages; the feed task discards
  `RecvError::Lagged(n)` (`bridge_feed.rs:283`), so rows missed under lag are neither
  answered nor recorded: they are denied by the default after 30 s. `PendingRows`
  (`bridge_runtime/pending_rows.rs`) is fed from the runtime pump (`bridge_runtime.rs:446`),
  NOT the lossy broadcast, so a re-sweep after a lag is feasible; it holds only `process`,
  `process_path` and `dst_host` per waiting row (`pending_rows.rs:20-24`).
- With the GUI closed nothing answers: the bridge times the prompt out after 30 s
  (`snitchwatch-bridge/src/deferred_answers.rs:30`) and the daemon applies the default, deny.
- Ask rows carry NO matched rule (`translator/connection.rs:88`); a matched rule only
  appears on sampled ping `Statistics.events` rows. What the aggregator sees are connections
  that were NOT matched.
- The per-row make-a-rule flow (`make_rule.rs:64`) only offers a rule for rows that are
  `deferred` or `decided_by_default`, and its builder takes a connections-tree node and the
  settings duration (`make_rule.rs:193-207`); a silently allowed row is neither, so learning
  mode needs its own rule builder.
- Rule levels (`connections/level_rules.rs`: Program = any host, Domain, Host), refusals
  `TooBroad`/`UnknownProgram` (GUI-side `LevelRuleRefusal`s; `UnknownProgram` is raised at
  apply time, `make_rule.rs:205`). A `regexp` on `process.path` passes policy when it narrows
  (`rule_policy/regexp.rs`, `narrowing.rs`) and `sensitive` is supported
  (`rule_policy.rs:293`); `process.path` regexps are NOT limited to `/usr` like curated
  entries. Match-everything rules are forbidden (`MATCHES_EVERYTHING`).
- Display of a regexp `process.path` rule: the GUI Rules list prints the raw operator
  (`rules/row_store.rs:204`, `process.path = <regexp>`), which is honest. The BRIDGE's import
  preview and the rule editor's cautions call it "applies to all apps"
  (`binds_to_programs` is false for a regexp: `snitchwatch-bridge/src/rule_policy/
  narrowing.rs:142`, `rule_io/preview.rs:240-258`, `edit_cautions` `preview.rs:311`). That is
  bridge wording; this item does not change it. KNOWN LIMIT: exporting and re-importing a
  learned directory rule, or opening it in the editor, shows the all-apps caution. It is
  recorded for the r17 bridge batch.
- No bulk call: `AddRule` carries one rule and a `request_id`
  (`snitchwatch-bridge/src/ws_messages.rs:476`); the bridge answers one `RuleCommandResult`
  per request. `RuleCommandOutcome` (`rule_wire.rs:13-33`) includes success, `OkWithNote`,
  `Refused{problems}`, `Timeout` ("may or may not have been applied"), `Unsure`, `NoDaemon`
  and `Rejected`. `AddRule` is `CHANGE_RULE`: it REPLACES a same-name rule
  (`rule_notification.rs:41-47`). The inbound queue is `mpsc(256)` with `try_send`
  (`bridge_runtime.rs:291`). The `snitchwatch-allow-` prefix is reserved for bridge
  once-off answers (`is_answer_name`; `refuse_reserved_name`). `ApplyRulesImport`
  (`ws_server.rs:388`) is NOT relied on.
- Recommended (curated) rules cover exact `/usr` `process.path` programs only
  (`curated/mod.rs:575-583`); names start `snitchwatch-default-`.

## Design

### Mode: learning (a time-bounded Silent mode)
- Setting `learning: Option<{ started_ms, ends_ms, duration_ms, elapsed_ms, previous_mode }>`
  next to `silent_mode` (serde default, old files load). Learning = "Allow and record" plus
  the aggregator plus a banner. Default length 24 h, maximum 7 days, never extendable from
  the banner, only restartable by the owner.
- The window can only END EARLY: it ends when the wall clock passes `ends_ms` OR the
  accumulated running time (`elapsed_ms`, saved about once a minute while the GUI runs)
  reaches `duration_ms`, whichever comes first, so a clock set backwards cannot extend it.
  The GUI needs its own injected wall clock and monotonic clock for tests (the bridge's
  `rules_sync` clock seam is thread-local and bridge-only; `notifier.rs`' injected `Instant`
  is the nearest pattern). Switching the silent mode by hand mid-window ENDS learning.
- FAIL CLOSED on the end: learning persists `silent_mode = Allow`, so ending it must not be
  undoable by an unrelated write. The end check runs in three places under the SAME
  settings lock (`SETTINGS_WRITE`): at startup in the `refresh_preferences` thread before
  `apply_preferences`; inside `persist_preferences` (every settings change re-reads the
  file and calls `apply_preferences`, `settings_controller.rs:326-335`, so any unrelated
  write such as the default-duration chooser would otherwise re-apply a stale `allow`); and
  on the expiry timer. It restores `previous_mode` first. `previous_mode` is
  `Option<SilentMode>`, because `Settings.silent_mode` is `Option` (`settings.rs:105`, `None`
  = unchosen) and restoring must not turn "unchosen" into Off. The Rust `STATE` is fed only
  by the QML property `settingsController.silentMode` (`AlertHost.qml:30-41`) and
  `apply_preferences` re-reads the disk after a write ("so a failed write still reports the
  on-disk state", `:329-336`), so a failed restore sets an in-process FORCED-MODE override
  that `apply_preferences` and the silent-state push both honor. The override holds the
  mode the OWNER chose when they explicitly chose one (a failed `chooseSilentMode(Deny)`
  must stay Deny, never the old mode), otherwise `previous_mode`; while it holds, EVERY write under
  `SETTINGS_WRITE` also re-applies the intended state (`silent_mode` = the override,
  `learning` = None), and it is cleared ONLY when such a write succeeds. So a failed explicit
  Deny followed by an unrelated successful write cannot leave `allow` plus an active window
  on disk, and a restart cannot apply it (the restart-after-failed-write case is tested). Allow is NOT applied while it holds, and a visible notice says
  the settings file could not be written. The handler is idempotent. Tests drive the failure
  through the QML property end to end, including an unrelated successful write while the
  override holds.
- A banner on every page ("Learning: new connections are allowed once and recorded. Ends in
  5 h 10 min. 31 programs seen. [Stop learning] [Review]") using the existing banner pattern
  (`PromptSlotBanner.qml`). Stop is always visible.
- Blocklists and deny rules still win (decided before the GUI is asked). Learning creates NO
  rule by itself; nothing is created without the owner's explicit tick.

### Aggregator (what was seen), persisted and capped
- New `src/learned.rs`. `PendingRow` (GUI struct in `bridge_runtime/pending_rows.rs`) is
  EXTENDED with `dst_ip`, `port` and `protocol` from the row it is built from, so a sweep of
  the rows already waiting has the full key. Learning start and end go through
  `set_silent_state`; the same sweep runs after a `Lagged` (the feed task re-reads
  `PendingRows`, which the runtime pump feeds without loss). A row is RECORDED only after its
  answer was sent successfully, and the aggregator dedupes by session-qualified row id,
  because `Answered::forget` re-plans failed sends and would otherwise double-count. Rows
  lost under lag before a sweep are denied after 30 s and not recorded; the review says so
  ("some connections were missed under heavy load") and never claims to be exhaustive.
- Key = (program, destination, port, protocol): program = `process_path` else the raw name;
  destination = the normalised hostname else the IP (a bare IP is recorded as such). Per
  program: first/last seen, connection count, distinct-destination count (the first 64 kept,
  the rest only counted), ports seen (capped), the count of bare-IP connections and of
  uncommon-port connections.
- Disk writes happen off the feed task (a writer thread fed by a channel; precedent in
  `settings_controller.rs`), so persistence cannot cause more `Lagged`. File `learned.json`
  via `private_files::write_private` (0600, atomic replace) under `paths::config_dir()`;
  read-merge-write under an advisory lock on a SEPARATE lock file (`write_private` renames
  over the target; no flock precedent exists in the crate, `libc` is available; whether
  locking works on the Flatpak config dir is UNVERIFIED and tested; on failure it degrades
  to last-writer-wins with a notice). A missing, corrupt or foreign-owned file starts empty
  WITH a visible notice.
- Caps and eviction: 2,000 entries, 64 destinations each, in four classes recomputed on every
  update. FLAG PRECEDENCE A > B > D > C: an entry with network flags is class A even if it
  also has no path or a temporary path. (A) NETWORK-behaviour flags (bare-IP or
  uncommon-port connections): cap 700, NEVER evicted; when A is full a new candidate is
  REFUSED. (B) path-flagged only (user-writable or temporary directory): cap 500, and B is
  keyed per PARENT DIRECTORY (one entry per directory, with its programs and counts) rather
  than per path, so junk paths in one directory collapse into a single entry instead of
  evicting others; evicted oldest-LAST-SEEN first. (C) unflagged with a path: cap 600,
  least-active first. (D) no path (a raw comm name an attacker can choose): cap 200,
  oldest-last-seen first. Each class evicts only within itself (the caps sum to 2,000). An
  entry that BECOMES network-flagged while A is full stays in its old class, marked flagged,
  and is evicted last in that class. Nothing is dropped silently: the review keeps a ring of
  the last 50 refused or evicted keys with counts and shows "N programs were not recorded or
  were dropped (M flagged): see the list", and a full class A is called out prominently as
  itself unusual.
- Privacy and retention: the file lists programs and destinations. It is deleted when the
  owner applies or dismisses the review AND learning is not running (if learning is still
  running the file is kept), or after 30 days, whichever is first; the review page has a Clear
  button; the banner and the page say where it lives.
- Coverage is stated in the UI and docs: recording happens ONLY while the GUI runs.
  Connections made while it is closed still get the default deny and are not recorded.

### Review screen: "Make rules from what I saw"
A new page (its own Component file; `main.qml` is 756 lines, so only a navigation entry is
added there). Every recorded program had at least one UNMATCHED connection; there is no
"already covered" bucket. A proposal that an existing enabled rule already makes (same
program, any destination) is skipped on a best-effort comparison with the rules list. Each
row reuses the 0a program label, its "unverified" marker and the real-path display.
- Nothing is ticked by default. "Any destination" is NEVER ticked by anything but the owner's
  own tick on that row, shown with the program's flags, its 0a label and marker and its real
  path. A "Select suggested" button, an explicit act, ticks ONLY narrow rules: the
  "only these destinations" rules (one per host seen at least twice, up to 10, with the host
  list shown inline on the row) of a program that runs from a
  direct system path (a file directly under `/usr/bin`, `/usr/sbin`, `/usr/libexec` or
  `/usr/lib*`), has no flag, is not on the DUAL-USE list and is not high fan-out. No
  desktop-entry match is used as identification at all: the name index also scans the
  user-writable `~/.local/share/applications` and user Flatpak ids
  (`app_icons.rs:246-260, 378, 393, 417`), `matching_entry` returns the FIRST match with
  user directories first (`:374-402`), a packaged entry can name an interpreter
  (`Exec=gjs ...` would "identify" `/usr/bin/gjs`), and the Flatpak GUI cannot see the
  host's desktop files anyway (no `host-os`, `org.snitchwatch.Snitchwatch.system.yml:84-91`).
  A path alone is never proof either (a `process_path` can be an `argv[0]`/comm fallback that
  is still absolute, `process_binding.rs`; Flatpak exports are wrappers; `/opt`,
  `/usr/local` and the user Flatpak tree never qualify). So the one-decision-per-program
  cost for broad programs (a browser, a sync tool) is the owner's explicit tick, once.
- Per-program actions, all explicit (owner, 2026-10-22: "block or allow this program should
  be allowed"): for ANY listed program the owner can choose Allow (any destination), Allow
  (only these destinations) or BLOCK THIS PROGRAM. Block creates one DENY rule on the exact
  `process.path`, any destination, name `snitchwatch-learned-block-<hash>`, duration
  "always"; a deny rule wins over allows (`docs/packaging/system-bridge-integration.md:116`:
  "Denies and blocklists still win"). Block is never ticked by default and never by "Select
  suggested"; a flagged program shows Block prominently as one of its choices, nothing more.
  Because a block can break the machine, blocking a program on a system path or one whose
  name is a known system service (for example `NetworkManager`, `systemd-*`, `dbus-*`,
  `chronyd`, `rpm-ostree`) shows a plain warning ("blocking this may stop networking or
  updates") and needs its own confirmation in the summary. A program with no bindable path
  cannot be blocked (non-selectable, as for allows). Block rules go through the same
  sequencer and outcomes as allow rules.
- A tick on a row IS the per-row confirmation of a Program-wide rule (the summary dialog
  repeats every rule in plain words before anything is sent; it does not replace the tick).
- DUAL-USE list: a tested set, matched by basename AND by path, a SECOND filter on top of
  the filters on "Select suggested" (it is NOT an identification list): interpreters, shells, toolchains, package tools, remote-access
  and general network tools: `python*`, `perl`, `ruby`, `node*`, `bun`, `deno`, `java`,
  `lua*`, `php*`, `dotnet`, `go`, `cargo`, `rustc`, `tclsh`, `awk`, `gawk`, `bash`, `sh`,
  `zsh`, `fish`, `dash`, `busybox`, `env`, `xargs`, `sudo`, `pkexec`, `vim`, `vi`, `emacs`,
  `curl`, `wget`, `aria2c`, `ssh`, `scp`, `sftp`, `rsync`, `telnet`, `ftp`, `lftp`, `nc`,
  `ncat`, `netcat`, `socat`, `nmap`, `openssl`, `gdb`, `pip*`, `pipx`, `uv`, `git`, `podman`,
  `docker`, `qemu*`, `wine*`, `flatpak`, `rpm-ostree`. Anything on it only ever proposes
  "only these destinations". The list lives in one source file with one test per class;
  adding to it is a reviewed change, and it is not claimed to be exhaustive.
- Proposal shapes (a new builder from a learned key, because `make_rule.rs` takes a tree node
  and the settings duration): (a) "any destination": one rule, `process.path` exact, no
  destination operand; (b) "only these destinations": ONE rule per destination host, each
  `process.path` exact + `dest.host` exact, any port, offered only up to 10 hosts, beyond
  that only (a) (or none for a dual-use program); (c) the directory rule (below). Duration
  is "always". The builder emits the same `AddRule` shape as `make_rule.rs` and is tested
  against it.
- A program with `process_path` None, non-bindable or ending in `(deleted)` is shown as
  NON-SELECTABLE ("cannot make a rule: the program path is unknown") and is never offered a
  proposal; it is not left to fail with `UnknownProgram` at apply time.
- Programs are sorted by activity. For each: destinations (top N, with counts), flags, the
  proposal. A high-fan-out program (at least 25 distinct destinations, tunable) is shown with
  its flags: the user-writable/temporary-path flag and the bare-IP and uncommon-port counts
  stay visible, "show top destinations" expands the list on demand, and it is never ticked
  by default and never selected by "Select suggested".
  A program can reach 25 destinations on purpose, so fan-out is never read as normal.
- Objective flags, shown as facts and never as verdicts (the 0e wording guard): runs from a
  user-writable or temporary directory (`/home`, `/var/home` (the Bazzite/atomic home),
  `/root`, `/var/roothome`, `/tmp`, `/var/tmp`, `/dev/shm`, `/run/user`, `/run/media`,
  `/var/mnt`, `/mnt`, `/srv`, a `(deleted)` path); connects to bare IPs (n of m); an uncommon port (not 53, 80, 123, 443,
  853, 5353); a name that equals a well-known system program but a path outside
  `/usr`/`/bin`. Example: "runs from /tmp, 3 connections to bare IPs on port 4444". (A "first
  seen in this window" flag is dropped; it returns with a persisted baseline, r17.)
- Versioned binaries (the owner's `2.1.308` case): when the 0a heuristic matches
  `<dir>/<app>/versions/<semver>`, the review OFFERS one directory rule. The heuristic is a
  SUGGESTION only and never a decision input: the owner sees "ANY file in this folder:
  /home/u/.local/share/claude/versions/ (a folder your user can write to)". Escaped,
  anchored, `sensitive=true`, refuses a `(deleted)` suffix and any further `/`, shown in
  full. It is flagged as user-writable, never selected by "Select suggested", and ticked
  only by the owner. The GUI Rules list prints it honestly as the raw `process.path`
  regexp; the bridge's all-apps wording is the KNOWN LIMIT above.
- "Apply N rules": the sequencer (`src/bulk_rules.rs`) sends ONE `AddRule` at a time with a
  `request_id` and waits for its `RuleCommandResult`. Rule names use a DISTINCT prefix,
  `snitchwatch-learned-` (not reserved: `rule_name.rs`), then a short HASH of
  (program, scope) so the name is stable, within the 200-byte limit (`rule_name.rs:16`) and
  free of path characters; never `snitchwatch-allow-` (reserved). Because `AddRule`
  replaces a same-name rule, BEFORE sending, the sequencer issues `RequestSnapshot` and
  requires a non-Unknown `SetRules` (the bridge broadcasts an empty list while the cache is
  Unknown, `rules_sync.rs:223-230`, and an empty list must NOT read as "no such rule") and
  no `RulesNotShown` entry for the name; if the list is unavailable the batch ABORTS. It then
  looks the name up (`RulesStore::find_by_name`): identical content, compared with a
  normalising comparator over the GUI `Rule`'s operator tree -> skipped as already present;
  different content (the owner edited it) -> NOT overwritten, reported per row. Outcomes, all
  shown per row: success and `OkWithNote`; `Refused{problems}`, `Rejected` (reported, the
  rest continue); `Timeout` and `Unsure` (wait a bounded time for the rules list, requesting
  a snapshot, then compare CONTENT before going on; never blindly retry); `NoDaemon` aborts
  the WHOLE batch, and so do 3 consecutive Timeouts. A full queue (`try_send`) is retried
  with backoff. Progress and Cancel are visible.
- After learning: anything new and unmatched prompts again, calmly (0d), and is surfaced
  by the exception-first view (0c). That is where bad behaviour shows up. Weeks-long drift
  detection needs the r17 history store and is NOT part of this item.

## Honest limits (stated in the UI and docs)
- During the window EVERYTHING is allowed once, including anything hostile running then.
  The flags and the owner's review are the safeguard; blocklists and deny rules still apply.
  An interpreter or shell ticked as "any destination" lets every script run through it
  connect anywhere, which is why those are never suggested.
- Not recorded while the GUI is closed, after a bridge restart (in-memory sets) or under load
  before a re-sweep; those connections are denied by the default.
- A path is not proof of identity; "Select suggested" never ticks "any destination" and
  never uses a desktop-entry match as identification.
- Records can be lost: a full flagged class refuses newcomers and the other classes evict
  within themselves; the owner sees a counted list of what was dropped, but a flood can still
  push a real program out of a class. The flood is itself evidence.
- A bulk "only these destinations" rule leaves a SHARED host (a cloud or CDN host such as
  `s3.amazonaws.com`) open to a program that is later compromised, and to one compromised
  during the learning window; each suggested row shows its host list inline so the owner can
  untick it.
- The bridge's import preview and rule editor call a directory rule "all apps" (known limit).

## Files and budgets (800-line rule)
New: `src/learning.rs` (mode, clocks, end handler), `src/learned.rs` (aggregator,
persistence, writer), `src/learning_review.rs` (pure proposal, flag and dual-use logic),
`src/bulk_rules.rs` (sequencer, builder), `qml/LearningBanner.qml`,
`qml/LearningReviewPage.qml`. Touched: `settings.rs` (298), `settings_controller.rs` (355: the end check, the forced-mode
override and the `refresh_preferences` ordering), `qml/AlertHost.qml` (the silent-mode
binding), `silent_mode.rs`,
`bridge_feed.rs` (hook and the lag re-sweep), `bridge_runtime/pending_rows.rs` (the three
extra fields), `main.qml` (756: navigation entry only; extract first if more is needed).
Each file stays under 800 lines.

## Tests (TDD)
- Mode and fail-closed end (L1): serde defaults and old settings files; `previous_mode` is an
  `Option` and "unchosen" is restored as unchosen; the end handler runs before the saved mode
  is applied at startup, inside `persist_preferences` and on the expiry timer; a failed
  settings write leaves Allow NOT applied; a failed explicit Deny STAYS Deny (never the old
  mode); an unrelated successful write while the override holds re-applies the intended state
  and clears the override only when that write succeeds; the restart-after-failed-write case;
  the clock cannot extend the window (stepped back, suspend); a manual silent-mode change
  ends learning; the end is idempotent; Stop.
- Aggregator (L2): dedupe by session-qualified row id (including a failed send then a
  re-plan); the extended `PendingRow`; the start-of-learning sweep and the post-lag re-sweep;
  `Lagged` sets `incomplete`; the four classes with flag precedence A > B > D > C, class B
  keyed per parent directory (junk paths in one directory collapse into one entry), A never
  evicted and refusing newcomers, each class evicting only within itself, an entry that
  becomes network-flagged while A is full; the ring of the last 50 refused or evicted keys
  with counts; merge under the lock and its degrade path; atomic file; corrupt and foreign
  file; retention (30 days; delete on apply only when learning is not running); the writer
  runs off the feed task.
- Proposals (L3, pure): fan-out threshold; each flag; nothing ticked by default; "Select
  suggested" ticks only per-host rules for hosts seen at least twice (up to 10) of unflagged,
  non-dual-use programs directly under a system path, never "any destination", never a
  desktop-entry match (a user-writable `.desktop` or a forged user Flatpak id changes
  nothing); the dual-use list by basename and path; the 10-host limit; the three rule shapes;
  directory-rule escaping, `sensitive=true`, `(deleted)`/extra-slash refusal and RE2
  validation against the GUI simulator; non-selectable unknown-path programs; hostile paths.
- Block action (L3/L4): the deny rule shape (exact `process.path`, any destination, the
  `snitchwatch-learned-block-` name), never ticked or selected by default, the system-service
  warning and separate confirmation, non-bindable paths are non-selectable.
- Sequencer (L4): one-at-a-time ordering; every `RuleCommandOutcome` (including `OkWithNote`
  and `Refused`); name-collision skip and no overwrite; an empty or Unknown rules list aborts
  the batch; Timeout reconciliation by content; `NoDaemon` and 3 consecutive Timeouts abort;
  queue-full backoff; cancel; the reserved prefix is never used.
- QML: banner and page source guards, plain-text guards for every dynamic text, the
  verdict-word guard, a probe test. Headless QML tests are exploratory in this sandbox
  (CLAUDE.md); `just check`/`just test` stay mandatory.

## Phases (each its own small PR, plan first)
L1 the learning setting, clocks, the fail-closed end (startup, `persist_preferences`, expiry
timer, forced-mode override) and the manual-change rule, with NO user-facing way to start
learning; L2 `PendingRow` extension, aggregator, sweeps, persistence, and the banner (with
its "N programs seen" counter, which needs the aggregator; the banner has NO `[Review]`
button until L4 builds the review page); L3 proposal, flag, dual-use and
builder logic (pure); L4 the review page, the sequencer AND the "Start learning" control, so
learning can only be switched on once recording and review both exist (an allow-everything
window that records nothing never ships); L5 documentation and the user-facing limits.

## Out of scope (r17 or needs a bridge change)
Fixing the bridge's all-apps wording for regexp rules, history-based drift detection, a
first-seen baseline, a blocklist-membership flag (needs a new bridge request), a PID,
exporting the learned data, per-user scoping on a multi-user host.

## Decided by the owner (2026-10-22)
1. Learning length: default 24 h, maximum 7 days.
2. The 25-destination line is accepted as a starting display threshold (it only decides
   whether the review shows a program's hosts or one "any destination" choice; it approves
   nothing and is adjustable; a program that reaches 25 hosts on purpose still shows its
   flags).
3. The review also offers an explicit "Block this program" (and Allow) for any listed
   program: designed above.
4. Order: build learning mode BEFORE phase-0 items 0b and 0c (the planner's recommendation,
   accepted).
