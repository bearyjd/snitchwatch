# Snitchwatch competitive feature roadmap — refreshed

**Date:** 2026-10-24 UTC. Primary-source research checked October 23; this document
was prepared across the UTC date boundary. **Status:** research and proposed
backlog, not approval to implement every item. The owner approved the direction
of manual AI-ready log export; its design still needs the existing security gate.
Scope is Component A: Kirigami GUI + bridge on OpenSnitch, including host/image
dependencies. The owner-requested repair to the existing GUI/host-scanner bridge
is included; broader Component B scanner redesign, a replacement firewall engine,
and router features are outside this roadmap.

The highest-value sequence is: finish validating the current GUI fixes; deliver
r16's process-monitoring reliability and GUI clarity; then make r17 history and
manual analysis export trustworthy. Rule ergonomics, profiles and deeper app
identity follow that evidence. More features should reduce uncertainty and work
for the user, rather than merely add screens.

## Baseline and evidence limits

The [October 7 roadmap](2026-10-07-competitive-feature-roadmap.md) is a historical
snapshot. Its claims that the rule editor/import/export are missing, profiles
and blocklists are no-ops, and learning mode is absent must not be reused.
Current source includes rule editing, simulation, insights and import/export;
capability-aware profiles and blocklists; learning/review; connection grouping;
and optional GeoIP/RDAP context. The current GUI fix series also supplies app,
domain and host actions through both the context menu and Actions menu, including
bulk paths. The menu fixes merged in PR #206 at `70ac8fcafc82b690e55ae5972f606e38e207c84d`;
the process-monitor health fixes merged in PR #207 at
`bf81d779b3f258c27d173cd199c79729ba696c2d`. Merged source is not a claim that
a published Flatpak or the owner's installed release has caught up.

Local baseline anchors: [rules UI](../../../crates/snitchwatch-kirigami/qml/RulesPage.qml),
[profiles UI](../../../crates/snitchwatch-kirigami/qml/ProfilesPage.qml),
[blocklists UI](../../../crates/snitchwatch-kirigami/qml/BlocklistsPage.qml),
[learning UI](../../../crates/snitchwatch-kirigami/qml/LearningReviewPage.qml),
[connections UI](../../../crates/snitchwatch-kirigami/qml/ConnectionsPage.qml),
[connection cache](../../../crates/snitchwatch-bridge/src/cache/connections.rs).
Persistent connection history remains r17 work; rule/profile persistence is a
different capability. Aggregate daemon statistics are not per-app byte counters.

Competitor capabilities below are documented, not installation-tested. A missing
claim in a source means unknown, not unsupported. Paid tiers, platform differences
and beta features matter. Current upstream docs can describe functionality newer
than the pinned OpenSnitch v1.8.0: verify the actual operand/protocol before
classifying a feature as bridge-only. References identify a useful precedent;
they do not claim exact parity or certify the proposed implementation.

## Product landscape

| Product | Category / platform | Representative documented capabilities | Qualification / sources |
|---|---|---|---|
| Little Snitch 6 | Commercial desktop; macOS | Nested connection grouping, traffic history/map, rich rules, subscriptions, profiles, maintenance insights | Stable reference; 6.5 integrity monitoring protects its own bundle, not all installed apps. [LS-M], [LS-R], [LS-G], [LS-I], [LS-P], [LS-REL] |
| Little Snitch Mini | Commercial vendor, tiered desktop; macOS | Simple app stop control, category blocklists, map, traffic chart | Progressive-disclosure reference; history/filtering/blocking vary by tier. [MINI] |
| Little Snitch for Linux | Free, mixed-license desktop; Linux web/PWA | App→domain→host drilldown, chart time selection, one-click rules, refreshed endpoint lists | GPL kernel/UI components plus proprietary daemon; do not equate its rule UI with prompt-before-connect. [LSL-C], [LSL-A] |
| GlassWire | Commercial desktop; Windows | Timeline, app/host/country breakdown, CSV, first-use/change alerts, profiles, lockdown | Monitoring/security reference; premium gating varies. LAN-device/ARP/evil-twin detection is additional scope. [GW] |
| NetLimiter | Commercial desktop; Windows | Long-term statistics, connection log, user/app filters, quotas/schedules, speed/priority controls | Bandwidth enforcement needs engine support, not just GUI work. [NL] |
| Windows Firewall Control | Free desktop utility; Windows | Duplicate/invalid-rule cleanup, merge, partial import/export, rules from recent activity, settings lock | Notifications report blocked attempts; do not infer an indefinitely held connection. [WFC] |
| LuLu + Netiquette | Open-source desktop; macOS | Signing/process context, temporary rules, manual profiles, import/export; bundled live monitor and JSON export | Old no-profile/no-monitor claims are stale. Outgoing filtering; host-name matching has documented limits. [LULU], [NETIQ] |
| OpenSnitch stock GUI | Open-source desktop; Linux | Rich operands, multi-node control, batch rules, separate nftables system rules, SIEM export | System inbound policy is distinct from inbound app prompts; newer wiki features need v1.8 verification. [OS-R], [OS-N], [OS-S], [OS-X] |
| Portmaster | Open-source desktop with paid tiers; Linux/Windows | Global/per-app policy, country/ASN lists, advanced UI levels, history, prompt queue, local API | History/bandwidth/reporting can be tier-gated. Import/export and validate-only APIs exist; corresponding GUI is unverified. Ask may block before the user answers and require app retry. [PM-S], [PM-F], [PM-A] |
| simplewall | Open-source desktop; Windows | Custom rules, telemetry lists, allowed/dropped logs, service/Store/WSL attribution, portable mode | Dropped notifications are not hold-and-ask prompts. [SW] |
| TinyWall | Open-source desktop; Windows | No-popup learning, temporary/LAN-only exceptions, process/window picker, tamper lock, connection views | Official FAQ confirms open source; current homepage announces 3.6, so the old “maintenance mode” claim is stale. [TW] |
| Fort Firewall | Open-source desktop; Windows | Program groups, schedules, wildcard/parent/service matching, learning, refreshed zones, simulator, speed limits | Speed limits and Windows identity mechanisms do not transfer directly to OpenSnitch. [FORT], [FORT-REL] |

**Beta, kept separate:** Little Snitch 7 beta 1 (October 20) adds a user-invoked AI
assistant with a technical-data preview and an Anthropic relay. This is a privacy
UX precedent, not stable Little Snitch 6 parity or evidence of OpenAI/OpenRouter
support. [LS7]

**Adjacent references:** OPNsense contributes live-log filters, aliases and rule
diagnostics; pfSense contributes rule metadata and configuration diff/restore;
Wazuh contributes saved investigations and report export. These are router/SIEM
patterns, not desktop-firewall parity targets. [OPN], [PFS], [WAZ]

## Candidate backlog: 64 concrete features

Baseline: **E** = existing main/source behavior to preserve or validate;
**Extend** = existing foundation, proposed addition; **New** = new planned surface;
**Research** = inventory or backend feasibility must be established first.
Boundary: **UI** = Kirigami; **Bridge** = authenticated policy/translation/service;
**Data** = history/schema/indexes/local stores; **Upstream** = daemon or host/image
capability, requiring its own validation and packaging boundary.
Priority: **P0** = current stabilization; **P1** = r16 candidate within its agreed
shape; **P2** = r17 candidate; **P3** = later candidate; **Gate** = separate scope
decision. Dependencies follow the semicolon. IDs are discussion anchors, not
implementation commitments. Every row cites a precedent, including adjacent
patterns explicitly labeled above.

### Owner-requested repairs and additions (2026-10-24)

These are active requirements, separate from the proposed competitor backlog.
Source changes, reviewed commits and installed artifacts must remain distinct.

| Item | Required outcome | Validation boundary |
|---|---|---|
| Live context menus | Keep right-click, Actions and rule menus usable while traffic updates, reorders or recreates rows; act on the original scope | Real clicks, injected updates and session changes; no stale-target dispatch |
| Dual process monitoring | The same GUI works with `/proc` and eBPF; optional eBPF readiness is not a GUI prerequisite | Core health, unknown mode, daemon warnings and fallback tests; host mode is never changed by the GUI |
| GeoIP setup | Bundle DB-IP Country Lite with attribution and release-based updates; retain optional MaxMind and validated user imports | Official pinned input and real format compatibility; user overrides preserved; corrupt inputs preserve the existing database; no broad sandbox permissions |
| Map drilldown | Owner-approved country → city → observed IP map navigation, exact geographic scope AND existing search, synchronized tree/inspector, coincident IP stacks and honest unavailable-location lists | City data/camera merged (#218/#217); semantic navigation implemented pending review/CI; [approved implementation and evidence](../plans/2026-10-24-geographic-map-drilldown.md); full GUI verification, independent review and CI remain pending; no release availability claim |
| Deep Scan | Replace the invalid in-sandbox `pkexec` launch with a constrained host integration and useful unavailable/denied/error states | Separately installed host helper and authorization, fixed command, bounded results; host packaging and GUI proof are separate gates |
| AI-ready export | Manual provider-independent files and an analysis prompt for Claude, ChatGPT/OpenAI or OpenRouter | Existing r17 export plan, redaction preview, explicit export; no automatic rule application |

The owner subsequently approved the [bounded overnight implementation plan](../plans/2026-10-24-overnight-gui-fixes-and-features.md),
including offline explanations (D01), scope previews (P03), and copy-details
actions after the repair gates. The remaining candidate backlog is still proposed.
DB-IP's monthly Lite databases permit application use with attribution under
CC BY 4.0; the packaging work must pin actual bytes and install that attribution.
[DB-IP terms and monthly downloads](https://db-ip.com/db/lite.php)

### Connections, applications and everyday actions

| ID | Candidate / user outcome | Baseline | Boundary | Priority; dependency | Precedent |
|---|---|---|---|---|---|
| C01 | Reliable app/domain/host allow, deny and temporary actions from both menus, including bulk selections | E | UI + Bridge | P0; current fix-series CI and real GUI proof | [LSL-C], [WFC] |
| C02 | Friendly program names with inspectable executable identity | Extend | UI + Bridge | P1; identity mapping with safe path fallback | [LS-M], [LULU] |
| C03 | Exception-first groups with allow/deny counts and an explicit time window | Extend | UI + Data | P1; agreed GUI clarity phase 0 | [LS-M], [GW] |
| C04 | App→domain→host drilldown and reverse destination→apps pivot | Extend | UI + Data | P2; stable grouping keys | [LSL-C], [LS-M] |
| C05 | Saved user app groups with explicit membership and affected-app preview | Research | UI + Bridge + Data | P3; distinguish display groups from enforced policy groups | [FORT], [LS-G] |
| C06 | App details: helpers/parents, recent endpoints, package and unit identity | Extend | UI + Bridge | P3; provenance and available process metadata | [LULU], [NL] |
| C07 | Package/Flatpak/container attribution with an honest unknown state | Research | Bridge + Upstream | P3; host/cgroup identity evidence before enforcement | [SW], [NL] |
| C08 | Identify missing/uninstalled executables and offer scoped rule cleanup | Research | UI + Bridge | P3; reliable app inventory, no automatic deletion | [LS-I], [WFC] |

### Rules, explanation and recovery

| ID | Candidate / user outcome | Baseline | Boundary | Priority; dependency | Precedent |
|---|---|---|---|---|---|
| R01 | Keep create/edit/simulate/import/export reachable and capability-honest | E | UI + Bridge | P0; current daemon round-trip verification | [LS-R], [LULU], [FORT-REL] |
| R02 | Multi-rule edit/clone with before/after scope preview | Extend | UI + Bridge | P3; transaction/refusal handling | [LS-R], [WFC] |
| R03 | Named endpoint sets reusable across rules and lists | Research | Bridge + Data + Upstream | P3; confirm v1.8 list operand and size semantics | [LS-G], [OPN] |
| R04 | Tags, notes and policy origin: user, recommended, profile or subscription | Extend | UI + Data | P3; stable metadata and synchronization | [OS-R], [PFS] |
| R05 | Explain “why allowed/blocked?” with matched rule and evaluated scope | Extend | UI + Bridge + Data | P2; preserve event-time rule identity | [LS-I], [LSL-C], [OPN] |
| R06 | Review duplicate, invalid, unused and overly broad rules with simulator evidence | Extend | UI + Bridge + Data | P3; extend existing insights; measured hits, no guessed redundancy | [LS-I], [WFC] |
| R07 | Policy change diff, local revision history and rollback preview | New | UI + Bridge + Data | P3; versioned snapshots, validated restore | [LS-B], [PFS] |
| R08 | Expiry countdown and safe temporary troubleshooting exception with automatic reversion | Extend | UI + Bridge | P2; daemon duration semantics and reconnect proof | [LULU], [TW] |

### Prompts, modes and network profiles

| ID | Candidate / user outcome | Baseline | Boundary | Priority; dependency | Precedent |
|---|---|---|---|---|---|
| P01 | One retry notice per program, with expandable pending attempts | Extend | UI + Bridge | P1; clarity phase 0, keep individual decision scope | [PM-S], [GW] |
| P02 | Show deadline, fallback verdict and whether the app must retry | Extend | UI + Bridge | P2; actual daemon timeout evidence | [PM-S], [WFC] |
| P03 | Explain exactly which app/host/domain/port a decision covers | Extend | UI + Bridge | P2; capability-aware rule preview | [LS-R], [LULU] |
| P04 | Keyboard-safe queued/bulk prompt review with affected-count confirmation | Extend | UI + Bridge | P3; stale/expired prompt handling | [PM-S], [LULU] |
| P05 | Bounded learning with review, explicit finish and restored previous mode | E | UI + Bridge + Data | P0; preserve existing learning/review behavior | [TW], [LULU], [FORT] |
| P06 | Quiet hours/fullscreen alert snooze distinct from filtering pause | Research | UI + Bridge | P3; define pending-decision behavior, never silently weaken policy | [GW], [LS-I] |
| P07 | Profiles with visible enforcement status, manual fallback and transition audit | Extend | UI + Bridge + Data | P3; preserve current capability guards; network/VPN signals | [LS-P], [LULU] |
| P08 | Scheduled profile/rule changes with suspend, clock and timezone semantics | Research | Bridge + Data | P3; explicit profile enforcement and audited scheduler | [NL], [FORT] |

### History and investigation

| ID | Candidate / user outcome | Baseline | Boundary | Priority; dependency | Precedent |
|---|---|---|---|---|---|
| H01 | Opt-in persistent allowed/denied history bounded by age and disk size | New | Data + Bridge | P2; r17 history design and security review | [GW], [PM-F], [NL] |
| H02 | Clear history, retention preview and storage health controls | New | UI + Data | P2; H01, deletion/failure semantics | [PM-S], [NL] |
| H03 | Multi-field AND/OR filters and time-range selection | Extend | UI + Data | P2; H01 indexes; retain current live search | [LS-M], [LSL-C], [OPN] |
| H04 | Saved investigations and export of the exact current selection | New | UI + Data + Bridge | P2; stable filter schema and X01 | [WAZ], [GW] |
| H05 | First/last seen plus new-app/destination/port changes against a baseline | New | UI + Data | P3; H01 coverage, alert thresholds and deduplication | [GW], [LS-M] |
| H06 | Preserve matched-rule ID/class and revision at event time | New | Data + Bridge | P2; stable identifiers and redaction policy | [PFS], [OPN] |
| H07 | Label gaps, sampling, cache overflow and incomplete attribution | Extend | UI + Bridge + Data | P2; count actual loss/lag; never imply full capture | [LSL-A], [OS-X] |
| H08 | Daily/weekly digest of repeated denies and changed patterns | New | UI + Data | P3; H01 and user-selected thresholds | [PM-F], [GW] |

### Context, reputation and traffic

| ID | Candidate / user outcome | Baseline | Boundary | Priority; dependency | Precedent |
|---|---|---|---|---|---|
| D01 | “What is this?” app→destination explanation grounded in observed fields | Extend | UI + Bridge | P1 stretch; clarity plan; offline explanation first | [LS-M], [LULU] |
| D02 | DNS name/answer/alias provenance and local/private/multicast context | Extend | UI + Bridge + Data | P3; distinguish observed, reverse-resolved and inferred names | [PM-S], [LSL-A] |
| D03 | Country/ASN/provider context with source age and uncertainty | Extend | UI + Bridge + Data | P3; optional GeoIP/RDAP sources; context is not threat verdict | [PM-S], [NL] |
| D04 | Optional user-initiated hash/reputation lookup with payload disclosure | Research | UI + Bridge | P3; consent and service capability; do not upload binaries by default | [LULU], [NL] |
| D05 | App binary identity/change review using hashes or available package identity | Research | Bridge + Upstream | P3; supported identity operands and update semantics | [LS-R], [LULU], [GW] |
| D06 | Genuine per-app sent/received counters and top talkers | Research | Upstream + Bridge + Data + UI | Gate; daemon accounting/protocol design; unknown is not zero | [NL], [LS-M] |
| D07 | Traffic timeline/chart that filters the connection view | Research | UI + Data + Upstream | P3; D06 for bytes; count-only charts labeled as attempts | [GW], [LSL-C] |
| D08 | Bandwidth quotas, priorities and rate limits | Research | Upstream + Bridge + UI | Gate; explicit new enforcement scope, outside current release shape | [NL], [FORT] |

### Export, AI analysis and integrations

| ID | Candidate / user outcome | Baseline | Boundary | Priority; dependency | Precedent |
|---|---|---|---|---|---|
| X01 | Manual versioned JSONL export plus summary, dictionary and optional CSV | New | UI + Bridge + Data | P2; owner-approved direction; allowlisted export DTO | [NETIQ], [GW], [OS-X] |
| X02 | Filtered export preview with summary-only default and exact field/redaction display | New | UI + Bridge + Data | P2; X01 security design; no raw policy snapshot shortcut | [LS7], [WAZ] |
| X03 | Portable analysis prompt for Claude, ChatGPT/OpenAI or OpenRouter | New | UI + Data | P2; X01; require evidence references and uncertainty | [LS7] beta precedent; provider-neutral proposal |
| X04 | Useful summaries/chunks with stable within-export pseudonyms and explicit gaps | New | Bridge + Data | P2; X01, sampling/truncation metadata and token-size budget | [GW], [NETIQ] |
| X05 | Validated rule import preview with diff, selective apply and recovery | Extend | UI + Bridge + Data | P3; extend existing import, reject unsupported policy | [PM-A], [LS-G], [WFC] |
| X06 | Authenticated read-only CLI/local API for selected diagnostics | Research | Bridge | P3; authorization and transport review, no new open network listener by default | [PM-A], [LS-CLI], [NETIQ] |
| X07 | Optional syslog/JSONL integration with explicit fields, retries and failure status | Research | Bridge + Data | P3; verify pinned upstream integration, privacy review | [OS-X] |
| X08 | Provider adapters, local-model analysis or scheduled analysis | Research | UI + Bridge + Data | Gate; separately approved routing/keys/cost/retention/consent design | [LS7] beta only |

### Profiles, subscriptions and trustworthy operations

| ID | Candidate / user outcome | Baseline | Boundary | Priority; dependency | Precedent |
|---|---|---|---|---|---|
| O01 | Blocklist freshness, failures, provenance and actual enforcement status | Extend | UI + Bridge + Data | P3; preserve capability-aware existing subscription controls | [LS-G], [PM-S], [FORT] |
| O02 | Subscription update diff; newly imported allows disabled until reviewed | Research | UI + Bridge + Data | P3; signed/trusted-source policy and transactional refresh | [LS-G] |
| O03 | Health page for daemon, transport/auth, database and subscriptions | Extend | UI + Bridge | P2; structured diagnostics and capability snapshot | [OPN], [PFS] |
| O04 | Correlation IDs and redacted support bundle including GUI lag | Extend | UI + Bridge + Data | P2; export allowlist and diagnostic event schema | [OS-X], [PFS] |
| O05 | Detect external rule changes and show provenance/diff | Research | Bridge + Data + Upstream | P3; daemon reload notification and stable revisions | [WFC], [PFS] |
| O06 | UID-aware recommended rules with visible confirmation when account identity moves | Extend | UI + Bridge | P2; runtime account-name resolution and current-rule inspection | [LS-R], [NL] |
| O07 | Multi-node/remote management | Research | UI + Bridge + Upstream | Gate; separate authentication, authorization and remote attack-surface design | [OS-N], [NL] |
| O08 | Inbound per-app prompts, encrypted DNS or LAN threat detection | Research | Upstream + Bridge + UI | Gate; separate product/engine decision; keep each scope distinct | [OS-S], [PM-S], [GW] |

### Desktop quality and platform reliability

| ID | Candidate / user outcome | Baseline | Boundary | Priority; dependency | Precedent |
|---|---|---|---|---|---|
| Q01 | Full keyboard/focus and screen-reader coverage for menus, prompts and bulk actions | Extend | UI | P0; actual GUI interaction proof, not source-string assertions alone | [LS-REL], [WFC] |
| Q02 | Scalable text/high-DPI, non-color verdicts, reduced motion and localization | Research | UI | P3; accessibility inventory and translated-string workflow | [LS-REL], [SW] |
| Q03 | Persistent columns/layout and simple/advanced detail levels | Research | UI + Data | P3; user-tested defaults and settings versioning | [PM-S], [MINI] |
| Q04 | Tray/health/reconnect feedback distinguishes disconnected, paused, enforcing and actual proc/eBPF mode | Extend | UI + Bridge | P1; dual-mode owner requirement, actual state rather than configured intent | [FORT], [TW] |
| Q05 | Restore eBPF process association and validate short-lived process attribution | Extend | Upstream | P1; root/image three-module and kernel checks below | [OS-E], [LSL-A] |
| Q06 | Comparable long-uptime CPU/RSS/attribution measurements for proc and eBPF | New | Upstream + Bridge + Data | P1; controlled workload and current measurement plan | [OS-E], [LSL-A] |
| Q07 | Large-history resource budgets and bounded UI updates | Extend | UI + Bridge + Data | P2; H01 replay/load fixtures and GUI lag counters | [LS-M], [NL] |
| Q08 | Repeatable release proof: pinned-input canary, real GUI gates and artifact/source manifest | Extend | Upstream + UI + Bridge | P0/P1; release cut checklist and install-path verification | [LS-REL], [OS-E]; local release-engineering requirement |

## Dual-mode GUI requirement and what eBPF actually adds

**Owner-required invariant:** the same GUI must support both `/proc` and eBPF
hosts. Prompts, rules, group actions, history and export use the shared bridge
contract and must not require eBPF. `/proc` is a supported backend, not a health
error. Diagnostics must distinguish configured mode, actual active mode and
fallback; unavailable data stays unavailable. The GUI must not write daemon
configuration to change the monitor. These are acceptance requirements, not a
claim that current health diagnostics already satisfy them. r16 restores an
optional backend while keeping `/proc` fully supported.

OpenSnitch's eBPF process monitor associates kernel connection events with
processes; its exec/exit module can help track short-lived programs. The expected
benefit is better attribution and potentially less `/proc` polling, including
fewer unknown-process rows. These are hypotheses to verify on the target kernel.
There is **no measured CPU or memory win yet**. It does not replace OpenSnitch's
firewall engine, grant new rule operands automatically, or create per-app byte
accounting. See the [local groundwork](2026-10-22-ebpf-groundwork.md) and [OS-E].

The host/image must supply `opensnitch.o`, `opensnitch-procs.o` and
`opensnitch-dns.o`; verify root execution, module loading, matching kernel/header
layouts, attribution correctness and the tested `proc` fallback. These modules
are not CO-RE. A log saying “ebpf” alone does not prove all modules loaded; the
process module can be absent while `/proc` monitoring also runs. This is a backend
image/configuration release, separate from rebuilding the GUI Flatpak.

Follow the [r16/r17 umbrella plan](../plans/2026-10-22-r16-platform-upgrade.md):
compare a full day on eBPF with a fresh comparable `proc` run, require lower mean
CPU, RSS slope no worse, correct process association and no daemon errors. Keep
the measured result even if it disproves the performance hypothesis. A new engine
decision stays deferred unless that evidence justifies reopening it.

## Manual AI-ready export: accepted direction and safe boundary

Implement the [existing export plan](../plans/2026-10-22-log-export-for-analysis.md),
not a second incompatible format. A user chooses a time window/app/domain/verdict
selection, previews exactly what leaves, then saves provider-independent JSONL,
summary, dictionary, optional CSV and a reusable analysis prompt. The same files
can be used manually with Claude, ChatGPT/OpenAI, or models through OpenRouter.
Export itself needs no provider account or API key.

Keep `summary-only` as the default: aggregate counts/intervals with approved
pseudonymous keys, not raw connection rows, hostnames, paths or policy snapshots.
More detailed export levels follow the plan's allowlist, per-export consent and
redaction rules. Include stable references within an export, schema/redaction
version, selected window/timezone, source coverage, gaps, truncation and unavailable
fields. Attempts are not sessions; missing byte counts are not zero bytes.

The supplied prompt asks for noisy apps, repeated denies, unfamiliar destinations
and possible rule simplifications, with event/group references and evidence versus
hypothesis separated. Metadata is untrusted input: bound/escape fields, protect CSV
formulas, and tell the model to treat log text as data rather than instructions.
Analysis produces a report. Rule application remains a separate explicit action
with deterministic validation. Uploads, provider credentials, local-model runners
and scheduled analysis are later proposals, not requirements of this accepted MVP.

## Recommended next steps and success evidence

1. **Finish current stabilization:** demonstrate both menus and app/domain/host
   bulk actions in the real GUI; collect passing CI and identify the tested
   commit/artifact. Measure success by zero known action-wiring failures and no
   confusion between source, merged release and installed Flatpak.
2. **Complete r16 clarity phase 0:** friendly names, exception-first groups,
   bounded verdict counts and retry deduplication. In owner walkthroughs, a noisy
   program/destination and its effective action should be identifiable without
   reading raw paths or counting repeated rows manually.
3. **Prove the eBPF host change:** three-module/root/kernel evidence, correct
   short-lived attribution, tested fallback and comparable full-day CPU/RSS.
   Report unknown-process rate and the umbrella plan's explicit performance gates.
4. **Design and review r17 history first:** opt-in storage, retention/disk caps,
   gaps and event-time rule identity. Verify bounded growth and failure behavior;
   distinguish stored events from complete network capture.
5. **Deliver manual AI-ready export:** reproducible fixture-selected exports,
   exact preview, summary-only default, redaction/truncation/CSV tests and stable
   references. Demonstrate manual use of one schema in different AI tools with
   zero bridge uploads and zero policy mutations.
6. **Connect explanation to action:** history row → matched rule → simulator →
   scoped temporary exception. Measure repeated unmatched asks/minute and time
   to explain a deny before and after, with no target invented before baseline.
7. **Expand rule ergonomics after evidence:** bulk preview, provenance, cleanup
   and rollback. Require unsupported daemon capabilities to be refused visibly;
   show actual affected rules and recover the previous policy in validation.
8. **Prioritize later candidates with the owner:** compare profile transitions,
   identity, subscriptions and accessibility against observed pain. Byte counters,
   quotas, inbound prompts, encrypted DNS and remote management each need their
   own scope decision, not accidental inclusion through a competitor checklist.

No arbitrary dates or feature-count promises attach to P2/P3. Preserve the approved
r16 eBPF + GUI clarity and r17 observability/rules split; each implementation gets
its normal plan/review, with host packaging changes separated from feature PRs.

## Primary-source catalog

Sources are direct vendor/project documentation. Several keys group closely
related official pages; beta and adjacent references remain labeled above.

- **[LS-M]** Little Snitch 6 [monitor](https://help.obdev.at/littlesnitch6/lsm-overview) and [connection grouping/filtering](https://help.obdev.at/littlesnitch6/lsm-connection-list).
- **[LS-R]** Little Snitch 6 [rule editor/identity/scope](https://help.obdev.at/littlesnitch6/lsc-edit-rules).
- **[LS-G]** Little Snitch 6 [groups and subscriptions](https://help.obdev.at/littlesnitch6/lsc-rulegroups).
- **[LS-I]** Little Snitch 6 [maintenance insights](https://help.obdev.at/de/littlesnitch6/lsc-insights).
- **[LS-P]** Little Snitch 6 [network profiles](https://help.obdev.at/littlesnitch6/concepts-profiles).
- **[LS-B]** Little Snitch 6 [backup/restore](https://help.obdev.at/littlesnitch6/lsc-backup).
- **[LS-CLI]** Little Snitch 6 [CLI permissions and commands](https://help.obdev.at/littlesnitch6/cmd-overview).
- **[LS-REL]** Little Snitch [stable release notes](https://www.obdev.at/products/littlesnitch/releasenotes.html).
- **[LS7]** Little Snitch 7 [beta release notes, including AI assistant](https://www.obdev.at/products/littlesnitch7-beta/releasenotes.html).
- **[MINI]** Little Snitch Mini [features and tiers](https://www.obdev.at/products/littlesnitch-mini/index.html).
- **[LSL-C]** Little Snitch for Linux [connection UI](https://help.obdev.at/littlesnitch-linux/connections).
- **[LSL-A]** Little Snitch for Linux [architecture/limitations](https://help.obdev.at/littlesnitch-linux/introduction) and [licensing overview](https://www.obdev.at/products/littlesnitch-linux/index.html).
- **[GW]** GlassWire [official feature inventory](https://www.glasswire.com/features/).
- **[NL]** NetLimiter [features](https://netlimiter.com/features) and [traffic priorities](https://netlimiter.com/docs/basic-concepts/priorities).
- **[WFC]** Windows Firewall Control [overview](https://www.binisoft.org/wfc.php) and [official user guide](https://www.binisoft.org/pdf/guides/Malwarebytes-WFC-User-Guide.pdf).
- **[LULU]** LuLu [official guide: profiles, modes, identity, export](https://objective-see.org/products/lulu.html).
- **[NETIQ]** Netiquette [monitor and JSON/CLI export](https://objective-see.org/products/netiquette.html).
- **[OS-R]** OpenSnitch [rules wiki; verify pinned-version support](https://github.com/evilsocket/opensnitch/wiki/Rules).
- **[OS-N]** OpenSnitch [multi-node management](https://github.com/evilsocket/opensnitch/wiki/Nodes).
- **[OS-S]** OpenSnitch [system firewall rules](https://github.com/evilsocket/opensnitch/wiki/System-rules).
- **[OS-X]** OpenSnitch [SIEM integration](https://github.com/evilsocket/opensnitch/wiki/SIEM-integration).
- **[OS-E]** OpenSnitch [v1.8.0 eBPF source/build instructions](https://github.com/evilsocket/opensnitch/tree/v1.8.0/ebpf_prog); pinned local code analysis is in the groundwork linked above.
- **[PM-S]** Portmaster [settings handbook, prompt timeout, profiles/lists/history controls](https://docs.safing.io/portmaster/settings).
- **[PM-F]** Portmaster [package feature/tier overview](https://wiki.safing.io/en/Portmaster/Feature).
- **[PM-A]** Portmaster [API authorization and import/export/validation](https://docs.safing.io/portmaster/api).
- **[SW]** simplewall [official repository/features](https://github.com/henrypp/simplewall).
- **[TW]** TinyWall [official features](https://tinywall.pados.hu/features.php), [FAQ confirming open source](https://tinywall.pados.hu/faq.php), and [current release announcement](https://tinywall.pados.hu/).
- **[FORT]** Fort Firewall [official README](https://github.com/tnodir/fort/blob/master/README.md).
- **[FORT-REL]** Fort Firewall [release history including simulator/timed unlock](https://github.com/tnodir/fort/releases?after=v2.5.1).
- **[OPN]** OPNsense [live logs](https://docs.opnsense.org/manual/logging_firewall.html), [rules](https://docs.opnsense.org/manual/firewall.html), [diagnostics](https://docs.opnsense.org/manual/diagnostics_firewall.html), [aliases](https://docs.opnsense.org/manual/aliases.html).
- **[PFS]** pfSense [logs](https://docs.netgate.com/pfsense/en/latest/monitoring/logs/index.html), [troubleshooting](https://docs.netgate.com/pfsense/en/latest/troubleshooting/firewall.html), [rule metadata](https://docs.netgate.com/pfsense/en/latest/firewall/configure.html), [configuration restore](https://docs.netgate.com/pfsense/en/latest/backup/restore.html).
- **[WAZ]** Wazuh [saved search/reporting](https://documentation.wazuh.com/current/compliance/nist/visualization-and-dashboard.html) and [inventory exports](https://documentation.wazuh.com/current/user-manual/capabilities/system-inventory/generating-system-inventory-reports.html).
