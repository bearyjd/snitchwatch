# GUI clarity, phase 0 (r16, DRAFT)

Status: draft plan, no code. Release: r16 (owner, 2026-10-22), GUI-only, shipped
as the GUI artifact; it does not touch the image, units, sockets or daemon config.
Parent plan: `2026-10-22-gui-clarity-overhaul.md` (evidence from the owner's
screenshot). Each item below is its own small PR, test first, built in the order
0d, 0a, 0b, 0c (the owner's stated pain first; the parent plan lists them in a
different order).

## Goal
The Connections view should tell the owner what matters and what things are:
calm notices, readable program names, verdict counts for a time window, and a
default that shows exceptions instead of a running list, WITHOUT ever hiding a row
that needs a human.

## Corrections to the parent plan (from the code map, 2026-10-22)
- The "68,224 connections" figures are the count of rows the GUI currently holds
  per group (`GroupCounts`, `grouping.rs:64`), not lifetime totals. The GUI row
  store cap is not known (the bridge cache defaults to 10,000 rows,
  `snitchwatch-bridge-cli/src/config.rs:52`; a 68k group implies the GUI holds
  more). Label windowed figures honestly.
- The banner's "the firewall applied its default action 311 times (retries count
  again)" is the DAEMON'S global rule-miss delta tracked per prompt holder
  (`snitchwatch-bridge/src/prompt_slot.rs:120`). It cannot be attributed to a
  program or destination, so phase 0 cannot turn it into "this program retried N
  times". It can count repeated asks per program in the GUI.

## Safety invariant for every item (tested, mutation-checked)
These rows are NEVER hidden, collapsed or suppressed by any phase-0 default:
pending, deferred, decided by the daemon default (`decided_by_default`, or
`matched_rule = None` with an action), denied, blocklist-decided, and rows with an
unknown action. Decisions are made per ROW, not per group. A friendly name is never
an input to any of these decisions. A mutation test flips each condition and must
fail.

## Items, in build order

### 0d. A calm notice per program, without losing a pending decision
- Today: of the notice kinds in `notifier.rs:16-23` (Pending, DaemonAway,
  FilterPauseExpired, DenyScopeNarrowed, VerdictNotRemembered, PromptSlotSummary)
  only Pending carries a program and host, and `FlowKey{session, program, host}`
  is built only from a pending target (`notification_controller.rs:353`).
  `CooldownGate::should_fire_for_flow` (`notifier.rs`, used by `may_post` at
  `notification_controller.rs:319`) allows one notice per flow per 30 s. So
  "popups for every new connection" IS the Pending notice, and the policy below
  applies to Pending notices.
- State rule: "notified" is recorded ONLY when a desktop notice is actually posted
  (`Gate::Post` in `may_post`, `notification_controller.rs:306-321`), never when a
  notice is skipped because the window is active (the user is looking at the app).
  A suppressed repeat must not start a 1 h clock. The existing window-active check
  and the per-row recheck loop (`post_once_the_window_is_not_active`, `:248`) are
  unchanged.
- Precedence, keyed on (session, program, host) like `FlowKey` (so after a
  reconnect the first ask of an already-seen pair fires again; a reconnect is a new
  session with new rows, intended):
  1. The first pending ask for a (program, host) with no live posted notice fires.
  2. A repeat of the same (program, host) within 1 h of a LIVE posted notice does
     not fire; it increments a "retried N times" counter. The 1 h entry exists only
     while the notice is live, so it is cleared on EVERY exit of the notice, whatever
     the reason: `show_and_answer` (`pending_notice.rs`) returns for any `WaitEnd`
     (answered incl. once-off, row decided by the daemon after a timeout, notice
     dismissed or replaced while the row is still pending, `ServerChanged`,
     `Action(Review)`), and ALSO when `Notice::show` fails (`pending_notice.rs:
     103-109`) or the session bus is unavailable (`notification_controller.rs:
     380-384`), where nothing was shown although `may_post` already recorded the
     flow (`:319`). Hook: add `CooldownGate::clear_flow(&FlowKey)` and call it
     unconditionally at those exits; pass the `Arc<Mutex<CooldownGate>>` and the
     `FlowKey` from `maybe_dispatch` (`:347`, where the gate is in scope) through
     `dispatch_pending` into `show_and_answer`. (Note: the per-notice poll there is
     `row_stops_waiting`, `pending_notice.rs:130-138`, built on `pending_row`;
     `still_waiting` is imported in `pending_notice.rs:17`, not in the controller.)
     The next ask for that pair then needs a human and fires again. Put the clear in
     a drop guard inside `dispatch_pending`'s spawned future so it also runs if the
     task is ever aborted (today it is a detached runtime task that is not aborted
     before process exit, `bridge_runtime.rs:225`, so this is defence in depth).
  3. A new host of the same program fires normally, subject to the existing 30 s
     per-flow gate. No digest in phase 0.
- Gate shape: `CooldownGate` has a single `cooldown` shared by its maps
  (`notifier.rs:53`; `with_cooldown` sets it). The 1 h repeat window is a SEPARATE
  field/map (`repeat_window`) with its own constructor parameter, leaving the 30 s
  per-flow gate for new flows unchanged; tests inject `Instant` for both.
- Retry counter home: a daemon retry makes a NEW row and `row_id` (`notifier.rs:
  40-42`) and #167's collapse window is only 10 s (`collapse.rs:48`), so its repeat
  count cannot cover 1 h. Key the counter on `FlowKey` (suppressed-notice count) and
  render it on the NEWEST pending row of that flow (needs a model role and a QML
  test).
- DEFERRED to a later phase (own plan): a program digest for many new hosts. It
  needs a GUI-local notice kind (the bridge `Notice` enum has no digest variant), a
  gate poll method such as `due_digest(now)` driven by one per-feed-task ticker that
  applies window-active, a trailing flush, and a defined target for `Review`. That is
  new timer machinery with a real risk of losing a pending decision, so it is not
  built until the simpler fix is measured.
- Never lost: coalescing applies to desktop NOTICES only. The pending ROW stays in
  the list, the prompt-slot banner still shows the pending prompt, and "Review"
  still opens it. DaemonAway, FilterPauseExpired and the other kinds are untouched.
  A test asserts that a suppressed notice never removes or hides a pending row or
  the banner.
- Reword `prompt_slot_text.rs::banner_text` (`:44-70`) so the global default-action
  figure is not presented as per-program retries.
- File budget: `notification_controller.rs` is 616 lines; split it before adding.
- Tests: `notifier.rs` gate tests with an injected `Instant` (cases 1-3, clear-on-
  answer, clear-on-daemon-timeout, dismissed-notice-then-retry, show-failure-then-retry,
  bus-failure-then-retry, reconnect fires the first ask again; `with_cooldown` already exists),
  `may_post` tests (window-active then repeat still posts later), the
  banner/row-survives test, a model-role + QML test for the retry counter,
  `prompt_slot_text.rs`, `tests/prompt_slot_banner_qml.rs`.

### 0a. Friendly program names (labelled by how much they can be trusted)
- Today: the bridge derives `process` as the basename of `process_path`
  (`translator/connection.rs:43-61`), so versioned or odd binaries show as
  `2.1.308`, `pasta.avx2`. The GUI groups by `process_path` and labels the group
  `row.process` (`grouping/keys.rs:54`, `grouping.rs:604`).
- Sandbox facts (known, from `packaging/flatpak/org.snitchwatch.Snitchwatch.yml:
  77-83`): host `/usr/share/applications` is NOT reachable (`host-os` is
  deliberately not granted); only Flatpak exports, `xdg-data/applications` and
  `xdg-data/icons` are readable, read-only. So system RPM apps (chrome,
  NetworkManager) will not get friendly names from desktop files; Flatpak apps and
  user desktop files will, and the path heuristic carries most of the load. Size
  the benefit accordingly: it fixes `2.1.308`-style names, not `chrome`.
- Change, GUI side only (the bridge is a privileged service and must not parse
  desktop files): add a `Name=` field and a `name_for(path)` next to
  `icon_for_path` in a NEW file (`app_names.rs`; `app_icons.rs` is 479 lines).
  Fallback heuristic for unmatched paths: `.../<app>/versions/<semver>` ->
  `<app> <semver>`; strip `.avx2`-style suffixes.
- Spoofing: a program controls its own path, and a user-writable
  `xdg-data/applications` can name any binary, so a label is NOT proof of identity.
  The group row always shows the real path (or its tail) next to the label; heuristic
  and desktop-file names carry an "unverified" marker and tooltip stating the
  source. A test checks that a crafted path (`~/x/claude/versions/9.9`) is shown
  as unverified with its real path. Group keys stay path-based.
- Tests: `app_icons/tests.rs` pattern, `tests/connections_process_roles_qml.rs`,
  `connections_model/tests.rs`.

### 0b. Per-group verdict counts with a time window
- Today: `GroupCounts` holds total, pending, allowed, denied, blocklisted, other,
  bytes, updated incrementally in `upsert_row` (`grouping.rs:223`) from `RowMeta`
  (`:133`), which has no timestamp. The delegate shows only the total
  (`ConnectionRow.qml:249`).
- Change: store `started_at_ms` in `RowMeta`; compute allowed/blocked/pending for
  "last hour" and "today" (needs a clock tick, since verdicts only change on row
  updates); new roles in `connections_model/roles.rs` and `projection_diff.rs`;
  delegate shows icon + word per verdict. `Verdict::of` (`outcome.rs:36-41`) maps
  default-decided rows with an action into Allowed/Denied, so "allowed" would mix
  "allowed by a rule" and "allowed by the default action": split the counts by
  `decided_by_default` or label them so. Windowed figures are limited by what the
  row store retains. A new session starts with `ClearConnectionRows` (which must NOT reset the
  "new this session" flag or the seen set: a reconnect replay must neither re-flag
  baselined programs nor un-flag unacknowledged new ones); whether the
  bridge replays its cached rows (up to its 10,000-row default) on connect is NOT
  yet verified, so verify it first and word the note accordingly ("limited to the
  rows the bridge replays", or "empty after restart"). Never imply a true hour.
- File budget: `grouping.rs` is 704 lines, so extract `GroupCounts` first.
- Tests: grouping unit tests (incremental counts), `tests/connections_model_qml.rs`,
  `tests/connections_collapse_qml.rs`.

### 0c. Exception-first default
- Today: `ConnectionFilter` (`filter.rs:123`, `matches` at `:248`; `PartialEq` at
  `:135` must include any new field) with pending-only / denied-only / from-now
  switches in `ConnectionsFilterBar.qml`, and the Grouped switch in
  `ConnectionsHeader.qml:51`.
- Change: a `needs_attention` filter, ON by default, plus a visible "showing N of
  M, show all" control and a fourth switch for the full list. A row is "handled"
  (and may collapse into a "Background and system" group with a one-line summary)
  only if ALL hold: action is allow; it was matched by a named rule that still
  exists and is enabled in the CURRENT rules list (rule names in a row can be
  stale after a delete); that rule is a RECOMMENDED rule (name starts
  `snitchwatch-default-`, `rule_name.rs:46`, a reserved prefix; user edits of such
  rules are blocked, `send_policy_tests.rs:261`, and a hand-placed rule file with that
  prefix needs root, which is accepted); and the program
  has been seen before (below). In phase 0 ONLY recommended rules count: they
  match an exact `/usr` `process.path` (`curated/mod.rs:575-583`), so a program
  cannot spoof them by choosing its name or path. User-written rules (a host-only
  or regexp operator would hide every already-seen program talking to that host)
  and bridge once-off answers (`is_answer_name`) never count as handled. Revisit
  user rules later, and then only for a rule whose operator is an exact
  `process.path` match outside user-writable directories. First-seen beats handled:
  a new program is always an exception.
- Plumbing: `ConnectionFilter::matches(&self, row)` (`filter.rs:248`) sees only the
  row, but the rules list lives in the model (`LevelRules.store`,
  `level_rules.rs:398-418`, fed at `apply.rs:84`) and `visible` is recomputed only on
  `set_filter` or row updates. So the model computes a per-row `handled` flag at
  projection time from the current rules list and the seen sets, and MUST force
  `recompute_visible` when rules change (`SetRules`/`UpdateRules`) and when the seen
  set changes, so a rule deleted or disabled later un-hides its rows. Fail open:
  while the rules list is empty or unknown (the cache sends an empty `SetRules`
  while Unknown, `rules_sync.rs:223`), NO row is handled and every row shows.
- First-seen needs state the GUI does not have today (it only knows the rows it
  holds, and the bridge evicts history silently, `collapse.rs:446`). Two separate
  things: (a) a "new this session until acknowledged" flag, in memory, set when a
  program's first row arrives and cleared only by the user (so a new program does
  not vanish within seconds); (b) a persisted seen set of `process_path` values,
  capped (for example 5,000, least recently seen evicted), merged on write because
  two GUI instances can run. Rows with `process_path = None` (the `<unknown>`
  programs, `connection.rs:43-50`) are never handled.
- Baseline: taken once, after the first post-connect snapshot has been applied
  (the exact signal is specified in the implementation plan, with a test that a
  program appearing after the baseline is flagged). Recording before the snapshot
  baselines nothing; recording after baselines every replayed program, so say so:
  show a one-time notice "baseline recorded: programs seen so far will not be
  flagged as new; a program already running at that moment was baselined."
- A reconnect (`ClearConnectionRows` then replay) must not reset the "new this
  session" flag or the seen set; a test covers it.
- Persistence and privacy: the set is a list of executable paths. Write it with the
  GUI's private-file helpers (`private_files::write_private` / `ensure_private_dir`,
  0600 file in a 0700 dir, atomic replace; as `settings.rs` does) under
  `paths::config_dir()`. Do not rely on the manifest's `xdg-config/snitchwatch:
  create` grant: under Flatpak `XDG_CONFIG_HOME` is sandbox-private. A missing,
  corrupt or foreign-owned file is NOT read as a silent cold start: it re-baselines
  WITH the visible notice every time, so tampering cannot quietly baseline a
  malicious program.
- File budget: `filter.rs` is 719 lines, so put the predicate in a new file.
- Tests: the safety invariant as mutation tests; stale-rule, once-off, host-only-
  rule and first-seen cases; `filter.rs` unit tests; `tests/connections_filters_qml.rs`,
  `connections_filter_layout_qml.rs`, `connections_header_qml.rs`.

## Constraints and risks
- Files near the 800-line limit (`grouping.rs` 704, `filter.rs` 719,
  `notification_controller.rs` 616, `connections_model.rs` 648): extract before
  adding, never grow them past the limit.
- GUI-only: no wire or `ConnectionRow` field changes in phase 0. If an item needs
  one, stop and re-plan (that touches the bridge and the tower's review). The
  first-seen state is GUI-local by design.
- Every item keeps the headless QML tests green; run `just kirigami-screenshots`
  and attach before/after screenshots to each PR.
- Hiding by default is the risk in 0c: the invariant above, the "showing N of M"
  control and the baseline notice are the mitigations; the owner confirms the
  default.

## Open questions for the owner
1. (decided for phase 0: only recommended rules count as "handled"; user-written
   rules are revisited later with the exact-path restriction above.)
2. Is 1 h the right window for suppressing repeats of the same program+host?
3. Hide the background group by default, or only collapse it?
