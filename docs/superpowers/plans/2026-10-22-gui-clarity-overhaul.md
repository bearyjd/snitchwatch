# GUI clarity overhaul (WS3, DRAFT; phase 0 in r16, the rest r17)

Status: draft plan, no code. Release: phase 0 in r16 (owner decision
2026-10-22, alongside the image-side eBPF work; the GUI is a separate artifact),
phases 1-5 in r17. Input: the owner's verdict on the GUI, 2026-10-22.

## What the owner said (their words, condensed)
- "The GUI is terrible. It's overwhelming with every new connection."
- "I am really not sure what is happening ... there is no status page."
- "Maybe also a search page."
- "What is blocked, what is not."
- "What does something mean."
Likely tested on r14. r15 (shipped) already collapses repeated rows (#167) and
remembers dismissed pending rows; re-judge the "overwhelming" complaint after
living with r15 before designing fixes for it.

## Evidence from the owner's screenshot (Connections page, 2026-10-22)
- The page is a running list grouped by program: ~30 groups, each with a
  connection count of the rows the GUI currently holds (not lifetime totals; the
  GUI cap is unknown) (tailscaled 68,224; chrome 57,769; NetworkManager 45,858;
  syncthing 20,965; systemd-resolved 18,655; python3.14 16,032 ...). No verdict
  summary per group (nothing says how many were allowed or blocked), no time
  window, no "needs attention" ordering. Owner: "it's just a running list", "so
  many items on it", "popups for every new connection".
- Names are file names, not applications: groups called `2.1.307`, `2.1.308`,
  `2.1.306` are versioned Claude Code binaries (confirmed on this host:
  `~/.local/share/claude/versions/2.1.306..308`), plus `wiring_sources-973045f1...`,
  `pasta.avx2`, `PluginLoader`, `node-22`. A user cannot tell what they are.
  -> a friendly-name resolver is a must for "what does this mean".
- The banner shows one prompt at a time (the prompt slot) but reads as constant
  nagging: "2.1.308 -> http-intake.logs.us5.datadoghq.com has been waiting 29 s ...
  the firewall applied its default action 311 times (retries count again)". 311
  default-action hits on one retrying program is consistent with the sustained
  ~2/min unmatched asks seen on the r14 laptop (a hypothesis until the r15 log line
  confirms it on that host).
- (All counts above are rows currently held by the GUI, not lifetime totals.)
- Background programs the recommended rules already cover (tailscaled,
  systemd-resolved, NetworkManager, chronyd) dominate the list by count and
  bury what matters.

## What exists today (from `crates/snitchwatch-kirigami/qml`)
- Pages: Connections (with a search field, a "Grouped" switch, a pending-only
  toggle, a filter bar, an inspector panel, a map band), Rules, Recommended rules,
  Blocklists, Traffic, Geo, Profiles, Scanner, Silent mode, "Daemon Health",
  "Settings & Diagnostics", Onboarding.
- Connections search covers the live connection list only, with a small syntax
  (`app:`, `host:`, `port:`, `country:XX`; `ConnectionsHeader.qml`). There is no
  search over history, rules and blocklist hits together (history does not exist
  yet; see the export plan).
- Prompts: a prompt-slot banner and a pending-decision sheet; "Decide later";
  Silent mode with a chosen verdict or "ask me".
- Explanations exist in pieces (hover tips, inline verdicts, rule insights /
  shadow findings, the rule simulator) but there is no single "why was this
  allowed/blocked" answer.
So the gap is not missing pages. It is no overview, no unified search, no plain
answer to "what happened to this connection and why", and too many interruptions.

## Problems and proposals

### 1. A Status page (the home view)
One screen that answers "is it working and what is it doing":
- Protection state: daemon connected, interception on/paused, default action
  (deny or allow), process-monitor method (proc/ebpf), bridge and daemon versions.
- Last hour / today: allowed, blocked, pending, decided-by-default counts; top
  programs and destinations; unmatched prompts per minute (the number we cannot
  see today).
- Problems that need attention, in plain words, linking to the fix: daemon down,
  rules not loaded, a recommended rule waiting for confirmation, blocklist over
  budget, feed lagging.
Source: existing health/diagnostics reports plus counters the bridge already has;
no new history store needed for the first version.

### 1b. Exception-first Connections view (new, from the screenshot; built in phase 0 as item 0c)
Default view shows only what needs a human: pending, blocked, and programs seen
for the first time, with a one-line summary for everything else ("38 programs,
all allowed by recommended rules, 312,000 connections today"). Known-good background
traffic collapses into a "Background and system" group; per-group counts use a
time window (last hour / today), not totals of whatever rows are held; each group carries
allowed / blocked / pending counts and an icon + word verdict. The full list stays
one click away.

### 2. Fewer interruptions (prompt fatigue)
Ideas, to be chosen with the owner, not all built:
- A calm default: one summary of pending items instead of a popup per connection;
  badge counts; digest of "N programs asked in the last hour".
- Group pending asks by program, answer once for "this program, any host" with the
  recommended scope pre-selected.
- Owner chose this as the main pain: "popups for every new connection". Target:
  a new (program, host) still notifies once;
  repeats of the same pair are suppressed for 1 h and shown as a counter
  ("retried N times"); a new host of the same program still notifies; pending
  notices keep their per-flow gate; a program digest for many new hosts is deferred
  (details in the phase-0 plan).
- Rate limit and de-duplicate prompts for retrying programs (the ~2/min idle
  unmatched asks on r14 are probably one or two retrying programs).
- Make Silent mode and "recommended rules" easier to reach from the first prompt.
Measure: prompts the user sees per hour before and after.

### 3. Search everything
- Phase A (no new data): extend the existing Connections search to rules and
  blocklists, with visible filter chips (verdict, program, host, port, country,
  rule) instead of remembering the `app:` syntax.
- Phase B (after the history store, r17 WS1): search past connections, with a time
  range, across restarts.

### 4. Blocked vs allowed, at a glance
- A consistent verdict marker and colour-independent label (icon + word) on every
  row; filters for Blocked / Allowed / Pending / Decided by default.
- Say WHY on the row or its detail: matched rule name, "default action", "once-off
  answer", "blocklist X", "silent mode". The data already exists on the row
  (`matched_rule`, `auto_answer`, `decided_by_default`).

### 5. "What does this mean"
- Friendly names first: resolve a program to an application name (desktop entry,
  Flatpak id, package owner, or a versioned-directory heuristic such as
  `.../claude/versions/2.1.308` -> "claude 2.1.308"), and show the raw path
  underneath. Never show a bare version number as the program name.
- Plain-language explanation panel for a connection: who the program is (package
  or Flatpak id if known), what the destination is (hostname, country, known
  service), what the verdict and rule mean, and what the safe next step is.
- Inline help and a short glossary for the terms the UI uses (scope, duration,
  once-off, silent mode, recommended rule). Unknown or odd programs get a clear
  "unrecognised" state with what to check, not a bare path.

## Phases (each its own small PR; the plan precedes code per repo convention)
Build order inside phase 0: 0d (calm notices), 0a (names), 0b (counts), 0c
(exception-first); see the phase-0 plan for the safety invariant.
0. Phase 0 (r16; its own plan `2026-10-22-gui-phase0-clarity.md`): one calm
   notice per program, friendly program names, per-group verdict counts with a
   time window, exception-first default. GUI-only, directly answers the screenshot.
1. Status page v1 (counters + health, no history).
2. Verdict and reason labels on rows (data already present).
3. Connection explanation panel and glossary (the first slice is phase-0 item 0e,
   the right-click "What is this?" sheet).
4. Search chips over live data; prompt grouping/digest (needs owner choices).
5. History search once the store exists (r17 WS1).
Keep every file under the 800-line rule; add QML tests next to the existing ones.

## Open questions for the owner
1. (answered: popups for every new connection; also too many rows in the list)
2. Is the Status page the first screen you open the app to?
3. Should the default be quiet (digest) with an opt-in to per-connection prompts,
   or the other way round?
4. Do the pages you never use (Geo, Traffic, Scanner...) need hiding or merging?
