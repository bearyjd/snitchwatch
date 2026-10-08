# Snitchwatch feature roadmap — competitive analysis

**Date:** 2026-10-07
**Status:** Research complete; roadmap proposed, not yet owner-approved.
Phase ordering and the non-goals below are recommendations — the settled
architecture decisions in `CLAUDE.md` are inputs, not things this doc
re-opens.
**Scope:** Component A (Snitchwatch: bridge + Kirigami GUI on top of
`opensnitchd` v1.8.0). Component B (the scanner) is out of scope.
**Inputs:** web research on 15 comparable products across macOS, Windows
and Linux (the 12 most relevant are summarized in §1), an inventory of
what `main` @ `4e25e2a` (code-identical to `9fdb336`) and draft PR #39 @
`5c2b44a` actually do, and the upstream `opensnitchd` v1.8.0 protocol/
operands. Sources are listed at the end; anything only secondhand is
marked *(unconfirmed)* in prose and `?` in the matrix.

## Summary

1. **Fix what already looks shipped before adding anything new.** The
   inventory found UI that is visible but not wired to the daemon:
   blocklist subscriptions and profiles never produce opensnitchd rules
   (no-op rule sinks, in-memory stores, refresh loop never started); the
   Rules page shows only rules created from prompts this bridge session;
   and the prompt's "This host" / "Any host on this domain" scopes emit
   rules with no `process.path`, so an allow answered for one app applies
   to every process. Phase 0 makes the UI tell the truth.
2. **Snitchwatch's defensible position is clear and not occupied:** the
   only open-source, *prompt-before-connect*, native-KDE, Flatpak/immutable-
   OS-first per-app firewall on Linux. Little Snitch for Linux (free, 2026)
   has no prompts, a closed daemon and a web UI; the stock OpenSnitch UI is
   Python/Qt with Plasma 6 tray bugs, no first-run flow, and Flatpak
   packaging as its most-upvoted open request; Portmaster is DNS-centric
   with paid tiers.
3. **Most parity features need no daemon changes.** opensnitchd v1.8.0
   already supports rich operands (cmdline, parent, env, user, hashes,
   networks, interfaces, list files), arbitrary durations, precedence,
   global rule hit/miss counters, and a recent-events list that names the
   matched rule. Rule editing, rule groups, insights, import/export,
   modes, persistent history and checksum flows are all UI + bridge work.
4. **A few headline competitor features need upstream daemon changes**
   (byte counters/quotas, cgroup/Flatpak/container identity, inbound
   prompts) — those go last, as upstream contributions, not forks
   (settled decision 2).

## 1. Landscape

| Product | Platform | Interaction model | License / price | Status (Oct 2026) |
|---|---|---|---|---|
| Little Snitch 6 | macOS | Hold-and-ask prompt | Closed, $59 | 6.5, 2026-09 |
| **Little Snitch for Linux** | Linux (eBPF, kernel ≥ 6.12) | **No prompts** — its rules docs describe only allow/deny rules plus a default action for unmatched connections (no ask action); rules made after the fact from a connections list; web UI/PWA | Free; eBPF + UI GPLv2, daemon closed | 1.1, 2026-07 |
| LuLu | macOS | Hold-and-ask prompt | GPLv3, free | 4.5.1, 2026-08 |
| OpenSnitch stock UI | Linux | Hold-and-ask prompt | GPLv3, free | 1.8.0, 2025-12; master active |
| Safing Portmaster | Linux, Windows | Default action Permit/Ask/Block; Ask has 60 s timeout | GPLv3; paid Plus/Pro tiers | 2.2.3, 2026-08 |
| GlassWire | Windows | "Ask to Connect" + monitoring-first | Closed; free + Premium | 3.11, 2026-10 |
| simplewall / WFC / Fort / TinyWall | Windows | Mostly **after-block** alerts or no prompts | Mixed (GPL / closed free) | All maintained (TinyWall in maintenance mode) |
| NetLimiter | Windows | Ask rules + bandwidth control | Closed, subscription | 5.3, 2026-09 |
| picosnitch | Linux | Monitor only (no blocking) | GPLv3 | 2.2.1, 2026-07 |

Takeaways that shape this roadmap:

- **Prompt-before-connect is rare and valued.** Of the Linux options only
  OpenSnitch (whose UI Snitchwatch replaces) and Portmaster's Ask mode do
  it; most Windows tools alert only after the block, and users complain
  the connection fails and must be retried. Keep prompts first-class.
- **Learning modes and good defaults are how competitors avoid prompt
  fatigue** (Little Snitch Silent Allow → Alert, LuLu "allow Apple/
  installed apps", WFC "allow signed", Fort auto-learn). OpenSnitch's
  first run is a popup storm. This is Snitchwatch's biggest UX gap after
  Phase 0.
- **Rule organization is the most common power-user complaint**
  (OpenSnitch #1078 rule groups; Little Snitch's rule groups and Insights
  are praised).
- **Visibility sells monitoring tools** (GlassWire's timeline, Little
  Snitch's traffic chart and map, Portmaster's history) — but per-app byte
  counts need daemon support opensnitchd lacks (#1081).

## 2. Feature matrix

Legend: ● full · ◐ partial · ○ none · ? unconfirmed. Snitchwatch column:
**shipped** (merged on `main` and reachable in the Kirigami GUI — the GUI
itself is not yet released as a package, see Phase 1), **shipped ⚠**
(visible but broken/not enforced), **bridge** (implemented in the bridge,
no Kirigami UI), **#39** (only on draft PR #39), **—** (absent).

| # | Feature | LS6 mac | LS Linux | LuLu | OpenSnitch UI | Portmaster | GlassWire | Fort | Snitchwatch |
|---|---|---|---|---|---|---|---|---|---|
| 1 | Prompt before connection completes | ● | ○ | ● | ● | ● (Ask) | ● | ○ (after-block) | shipped |
| 2 | Prompt scope: app + host/domain/port | ● | n/a | ◐ | ● | ? | ○ | n/a | **shipped ⚠** host/domain not app-bound; no port scope |
| 3 | Temporary / timed rules | ● | ? | ● | ● | ? | ○ | ◐ | shipped (once/5 min/until restart/forever) |
| 4 | Identity beyond path (signature/hash/parent/cmdline) | ● | ◐ parent | ● | ● | ◐ fingerprints | ◐ | ◐ parent | — (path only) |
| 5 | Manual rule editor (create/edit) | ● | ● | ● | ● | ● | ◐ | ● | bridge (no UI) |
| 6 | Shows all existing daemon rules | ● | ● | ● | ● | ● | ● | ● | **shipped ⚠** session-created only |
| 7 | Rule groups / organization | ● | ? | ○ | ○ (#1078) | ◐ | ○ | ● | — |
| 8 | Rule insights (hits, unused, redundant, simulator) | ● | ● matching-rules view | ○ | ○ (#1617) | ? | ○ | ● simulator | ◐ simulator |
| 9 | Import / export / backup | ● | ? | ● | ◐ | ○ (#388) | ? | ● | — |
| 10 | Modes: silent-allow (learning) / silent-deny / lockdown | ● | ○ | ◐ | ○ (#527) | ◐ | ● lockdown | ● | ◐ pause (auto-allow, never expires) |
| 11 | Network-location profiles, auto-switch | ● | ? | ○ | ○ | ○ | ◐ | ◐ | **shipped ⚠** not enforced, in-memory |
| 12 | Blocklist subscriptions, auto-update | ● | ● | ◐ | ◐ (master only) | ● | ○ | ● | **shipped ⚠** not enforced, no refresh |
| 13 | Live connection list + search | ● | ● | ○ | ● | ● | ● | ● | shipped |
| 14 | Persistent history | ● | ● | ○ | ● SQLite | ● | ● | ● | — (in-memory, 10k rows) |
| 15 | Per-app traffic volume / charts | ● | ● | ○ | ○ (#1081) | ● | ● | ● | ◐ daemon aggregate tiles; per-connection bytes hardcoded 0 |
| 16 | Map / country grouping | ● | ? | ○ | ◐ GeoIP | ● country rules | ● | ○ | ◐ Geo page (needs local DB) |
| 17 | Explain app/domain, reputation lookup | ● | ? | ◐ VirusTotal | ◐ VT plugin | ? | ● VT | ○ | ◐ rDNS + opt-in RDAP |
| 18 | Binary-changed / new-app alerts | ● (6.5) | ? | ○ | ◐ checksum buttons | ? | ● | ○ | — |
| 19 | Tamper protection / admin gating | ◐ | ? | ? | ◐ TLS nodes | ? | ◐ | ● password | ◐ token + socket perms; group gate in #39 |
| 20 | Encrypted DNS / DNS filtering | ● | ○ | ○ | ○ | ● | ○ | ○ | — (non-goal, §4) |
| 21 | Bandwidth limits / quotas | ○ | ○ | ○ | ○ | ○ | ○ | ◐ | — (non-goal, §4) |
| 22 | Inbound connection control | ● | ● direction | ? | ○ (#116) | ● | ● | ● | — (needs daemon) |
| 23 | CLI / scripting | ● | ? | ○ | ○ (#1512) | ◐ API | ○ | ● | — |
| 24 | Multi-machine management | ◐ MDM | ◐ remote web UI | ○ | ● multi-node | ○ | ● Premium | ○ | — (deferred) |
| 25 | Container / Flatpak-aware identity | n/a | ? | n/a | ○ (#1547, #1116) | ◐ Snap/AppImage | n/a | n/a | — (needs daemon for enforcement) |
| 26 | First-run learning / curated defaults | ● | ○ (#57) | ● | ○ popup storm | ● | ? | ● auto-learn | ◐ wizard (probes a unit that is not packaged) |
| 27 | Native desktop app + tray | ● | ○ web UI | ● | ◐ (Plasma 6 tray #1667) | ● | ● | ● | shipped (Kirigami) |
| 28 | Flatpak / immutable-OS packaging | n/a | ? | n/a | ○ (#548, top request) | ? | n/a | n/a | #39 (Flatpak + bootc overlay) |

Windows-only comparators not in the table: NetLimiter is the reference
for bandwidth limits/quotas/priorities; WFC for tamper protection
("Secure Rules", password lock) and signed-app learning; simplewall/
TinyWall for minimal default-deny with timed allows; GlassWire for
network-threat alerts (ARP spoofing, new devices, evil twin).

## 3. Phased roadmap

Each item carries a **tier** — **UI** (Kirigami only), **BR** (bridge/
protocol, testable with `MockOpensnitchd`), **DM** (needs opensnitchd
changes → upstream contribution) — and a rough **size** (S ≤ 1 PR-day,
M a few days, L a week+).

### Sequencing constraint: draft PR #39

PR #39's head `5c2b44a` is pinned by bazzite-tower's image candidate;
moving it means re-pinning and re-collecting VM evidence. #39 changes
`crates/snitchwatch-bridge-cli/src/lib.rs`, and in the bridge crate
`grpc_server.rs` (which holds every caller of `verdict_to_rule` /
`scope_degradation`), `ws_server.rs`, `auth.rs`, `cache/connections.rs`,
new `client_presence.rs`, and `translator/upstream.rs` (mostly tests); in
Kirigami, `wizard.rs`, `wizard_controller.rs`, `bridge_runtime.rs` and
`qml/OnboardingPage.qml`. Work that touches those files — or changes a
contract their code relies on — should land **after #39 merges**. Work
confined to files #39 does not touch (`translator/verdict.rs` internals,
`blocklists/`, `profiles/`, `PendingDecisionSheet.qml`,
`BlocklistsPage.qml`, `ProfilesPage.qml` and most other QML pages) has no
file overlap with #39 and can land on `main` now — though CI on #39's
merge ref will re-run against it, so check #39's tests don't assert the
old behavior. Anything that must be *wired* in `bridge-cli/src/lib.rs`
(rule sinks, persistent stores) still waits.

### Phase 0 — Make the shipped UI true (now)

Severities below are from a read-only security review of `main` @
`9fdb336` (code-identical to `4e25e2a`) against opensnitchd v1.8.0's actual rule evaluation
(`daemon/rule/loader.go`, `rule.go`), run 2026-10-07.

| ID | Item | Tier | Size | Notes |
|---|---|---|---|---|
| P0.1 | **Bind prompt scopes to the app** (**HIGH**). "This host" / "Any host on this domain" must emit a `list` operator = `process.path` AND the host/`dest.ip`/domain-regex operator; refuse (don't degrade to a process-agnostic rule) when `process_path` is empty; keep an explicit, clearly-labelled "any app" option; put the process in the rule name. | BR + UI | S | Today `verdict.rs:320-326, 476-484` emit host-only rules, so any timed/forever allow answered for one app lets every process reach that host with no prompt — and `dst_host` is attacker-influenced per the module's own threat model. Inline "this time" answers are unaffected (never persisted). A host-only allow cannot override an existing per-app deny (daemon keeps scanning after a non-precedence allow), but a host-only *deny* blocks every app. **Ships in:** the published v0.1.1 tarball (built from `9fdb336`) and #39's pinned head `5c2b44a` (the bazzite-tower image's source) — a fix on `main` protects neither until a new bridge release and a #39 re-pin, which is the #39 owner's call (same decision as P0.5). **Split:** emitting the app-bound `list` operator when `process_path` is known is internal to `verdict.rs` (no file overlap with #39; #39's added tests don't assert verdict rule shapes — checked) and can land on `main` now with `verdict_to_rule`'s signature unchanged; *refusing* when `process_path` is empty changes the contract its callers in `grpc_server.rs` rely on → after #39. Needs a migration note for existing host-only rules. |
| P0.2 | **Show every daemon rule**, not just session-created ones: consume `ClientConfig.rules` on `Subscribe` and keep the cache in sync. | BR | S–M | Prerequisite for any rule editor. Touches `grpc_server.rs` → after #39. |
| P0.3 | **Blocklists: be honest now, then enforce** (**HIGH** while unlabelled). Immediately label `BlocklistsPage.qml` "Preview — not enforced". Then enforce through opensnitchd's own `lists.domains` / `lists.nets` operators (one rule per subscription pointing at a bridge-written list directory), persist subscriptions, start the refresh loop, and report sink failures as "not enforced" instead of "ok". | UI then BR | S, then M | Production builds both managers with in-memory stores and no-op sinks (`bridge-cli/src/lib.rs:173-182`; same on #39); status is set "ok" before the sink runs. Today, since nothing is enforced, blocklisted hosts get through silently whenever an app already has an "any host" allow — the browser case blocklists exist for. Once wired, a blocklist deny beats any non-precedence allow (`loader.go:497-515`). List directory must be readable by root `opensnitchd` and writable only by the bridge account (system mode: `snitchwatch`). **Open design question:** opensnitchd lets a matching deny beat any non-precedence allow, which contradicts the 2026-04-10 spec's "user rules always win" — decide before wiring. Wiring → after #39. |
| P0.4 | **Profiles: be honest now, enforce later** (**HIGH** while unlabelled). Immediately label the page "Preview — not enforced" (or hide it); later persist profiles and give them a real rule sink. | UI then BR | S, then M | Same no-op sink/in-memory store as P0.3; profiles are lost on restart. Enforcement design should reuse P0.3's per-rule-set approach. |
| P0.5 | **Pause filtering** (**MEDIUM** on `main`, **HIGH** with #39): add an automatic expiry (`FilterPauseExpired` is never sent), broadcast pause as its own state, and make the tray resync honor it — three paths reset the tray to Idle while the pause stays on (`daemon_watchdog.rs:58-62`, `grpc_server.rs:345`, `cache/connections.rs:81-99`). | BR + UI | S–M | Touches `grpc_server.rs` and `cache/connections.rs` → after #39, except the #39-internal fix that follows. **#39 interaction (flag to #39's owner):** #39's "no authenticated GUI → `Unavailable`" check sits *after* the paused early-return, so a paused system bridge keeps auto-allowing with no GUI attached, and any `snitchwatch-ui` member can pause the whole machine past logout. Fix in #39 itself (check GUI presence before the pause branch; log who paused) — that moves its pinned head, so it's the bazzite-tower owner's call. |
| P0.6 | **Remove dead/misleading UI:** never-set countdown and zeroed per-connection sparkline/byte counts (UI-only; files #39 doesn't touch → now); stuck pending rows after the daemon's ask timeout (`cache/connections.rs` → after #39). | UI + BR | S each | Hide what cannot be true yet rather than fake it. The onboarding probe of non-existent `snitchwatch-opensnitchd.service` exists on `main` (and the Tauri shell) only — #39 already replaces it with authenticated-bridge state, so don't fix it separately. |
| P0.7 | **Housekeeping:** close issue #34 (fixed by #38; source guard `production_client_runtime_cannot_take_over_service_resources`), file issues for P0.1–P0.6, decide #17 (document the upstream `isAsking` starvation gap, evilsocket/opensnitch#1644, as known). | — | S | Outward-facing; needs owner OK. |

### Phase 1 — Ship the system bridge (in progress elsewhere)

Owned by the bazzite-tower session; listed here only for ordering. PR #39
rollout gates (doc: bazzite-tower `docs/research/snitchwatch-system-
bridge.md`, "Consumer rollout gate"), issue #35 closure on deployment,
GUI Flatpak release, then the separate deny-by-default policy decision.
After the GUI package ships: retire `snitchwatch-tauri` + `web/` (settled
decision 4; also removes the GPL-2.0-only vendored UI), then enable
Renovate (PR #33) — both churn `Cargo.lock`, so not before #39 merges.

### Phase 2 — Rule power and prompt-fatigue parity (UI + bridge only)

| ID | Item | Tier | Size | Competitor reference |
|---|---|---|---|---|
| P2.1 | **Rule editor**: create/edit with an operand builder — process path/cmdline/parent/user/hash, dest host/domain-regex/IP/network/port, protocol, interface; any duration (Go duration strings); precedence; nolog. | UI | M–L | LS6, OpenSnitch, Fort. Bridge `AddRule`/`UpdateRule` already exist. Depends on P0.2. |
| P2.2 | **Richer prompt scopes**: port and app+port, user, "this app via parent", Deny vs Reject with plain-language help. | UI + BR | M | OpenSnitch scope presets; OpenSnitch #1519 Deny/Reject confusion. |
| P2.3 | **Operating modes**: Alert, Silent-allow-and-log (learning, with a review queue to turn observations into rules), Silent-deny (unattended), Lockdown/panic (high-precedence deny-all), timed pause. | BR + UI | M | LS6 modes, GlassWire Lockdown, TinyWall block-all, OpenSnitch #527 kill-switch. |
| P2.4 | **First-run learning + curated defaults**: starter rule set for Fedora/Bazzite/KDE system services (NetworkManager, rpm-ostree, flatpak, PackageKit, time sync, KDE services), optional N-minute learning period, then switch to Alert. | UI + data | M | LS6 factory rule groups, LuLu/WFC "allow signed/installed", Fort auto-learn; LS-Linux #57 asks for this. Biggest prompt-fatigue win. |
| P2.5 | **Rule groups**: name-prefix/description metadata, group enable/disable, bulk actions. | UI + BR | M | LS6 rule groups; OpenSnitch's top organization request (#1078). |
| P2.6 | **Rule insights**: per-rule hit counts, unused and shadowed/redundant rule detection, extend the simulator to all operands. | BR + UI | M | LS6 Insights, Fort Filter Simulator, OpenSnitch #1617. The daemon only reports global `rule_hits`/`rule_misses`; per-rule counts must be derived from `Statistics.events` (each names the matched rule), which is capped (`Stats.MaxEvents`, 250 by default) — lossy until P3.1 history accumulates them. |
| P2.7 | **Import / export / backup** (versioned JSON, schema-validated, dry-run diff on import). | BR + UI | S–M | LS6 CLI export, LuLu, WFC; Portmaster #388 shows users notice its absence. |

### Phase 3 — Visibility

| ID | Item | Tier | Size | Competitor reference |
|---|---|---|---|---|
| P3.1 | **Persistent history** (SQLite in the bridge's state directory, retention setting, search/filter, CSV export). | BR + UI | M | OpenSnitch, Portmaster, LS6, GlassWire. |
| P3.2 | **Stats breakdowns** from the daemon's `by_host/executable/port/uid` maps and `dns_responses` (currently dropped). | BR + UI | S–M | OpenSnitch stats tabs. |
| P3.3 | **Process details**: args, cwd, env, process tree, checksums (daemon sends them; `ConnectionRow` drops them). | BR + UI | S | LuLu process hierarchy, OpenSnitch process dialog. |
| P3.4 | **Per-app traffic volume**: contribute connection/process byte counters upstream (#1081). The daemon's `pid-monitor` task (`TASK_START`) is not a substitute: in v1.8.0 it reads only `/proc/<pid>/io` (`rchar`/`wchar`, which include file I/O) and never populates its network counters. Charts only after real data exists. | DM | L | GlassWire timeline, LS6 traffic chart, LS-Linux diagram, Portmaster, Fort. Fills the per-connection byte columns still hardcoded to 0 (the Traffic tab itself was fixed to show daemon aggregates by #19 / PR #27). |
| P3.5 | **"What is this?" assistant**: offline knowledge base for common Linux/KDE/Flatpak processes and well-known domains, plus opt-in online lookups (existing RDAP; VirusTotal-by-hash opt-in). | UI + data | M | LS6 Internet Access Policy/research assistant, LuLu/GlassWire VirusTotal. |
| P3.6 | **Alerts**: per-rule Notify action, "first network activity of an app" alert, optional daily/weekly digest. | BR + UI | S–M | LS6 Notify rules, GlassWire first-activity, Portmaster weekly report. |
| P3.7 | **Geo**: ship or fetch a redistributable GeoIP DB (DB-IP Lite or MaxMind with key), country/ASN grouping. | UI + packaging | S–M | LS6 group-by-country, Portmaster country rules. |

### Phase 4 — Integrity and Linux-native identity

| ID | Item | Tier | Size | Competitor reference |
|---|---|---|---|---|
| P4.1 | **App-changed flow**: turn on `Rules.EnableChecksums` via `CHANGE_CONFIG`, add hash operands to rules, and prompt "this app's binary changed — update rule?" | BR + UI | M | LS6 6.5 integrity monitoring, GlassWire app-info change, picosnitch, OpenSnitch checksum buttons. Design needed for rpm-ostree/Flatpak update churn so legitimate OS updates don't prompt for every binary. |
| P4.2 | **Flatpak/container attribution (display)**: derive Flatpak app ID, systemd unit, toolbox/podman container from `/proc/<pid>/cgroup` and show it in prompts and lists. | BR | M | No Linux competitor does this well; most Bazzite desktop apps are Flatpaks. Research spike first (pid races, mount-namespace paths). |
| P4.3 | **Flatpak/container identity in rules (enforcement)**: cgroup/app-ID operands. | DM | L | OpenSnitch #1116, #1547 — upstream contribution. |
| P4.4 | **Tamper resistance**: polkit-gated destructive actions (disable interception, delete all rules, change default action), audit log of rule changes, alert when rules change outside Snitchwatch. | BR + UI | M | WFC Secure Rules/password lock, LS6 restricted editing. |
| P4.5 | **Inbound connection prompts/rules**. | DM | L | LS6, Portmaster, Fort; OpenSnitch #116 (deferred "v2" in the 2026-04-10 design spec). |

### Phase 5 — Power users and fleets

| ID | Item | Tier | Size | Notes |
|---|---|---|---|---|
| P5.1 | **`snitchwatch` CLI** over the authenticated WS protocol: list/add/remove rules, temporary rules for scripts, export/import, mode switch. | BR | M | LS6 CLI, Fort CLI; OpenSnitch #208/#1512. |
| P5.2 | **Blocklist presets and formats**: curated presets (Hagezi, StevenBlack, OISD), IP/CIDR lists via `lists.ips`/`lists.nets`. | BR + UI | S–M | LS-Linux presets, Fort zones. Depends on P0.3. |
| P5.3 | **Multi-node management** (one GUI, several daemons). | BR + UI | L | OpenSnitch multi-node; LAN/remote mode is deferred in the 2026-04-10 spec; needs an auth design beyond the local socket. |
| P5.4 | **Upstream contributions**: byte counters (#1081), cgroup operands, inbound prompts, serialized asks (opensnitch#1644, root cause of issue #17). | DM | L | Keeps Snitchwatch riding opensnitchd (settled decision 2) instead of forking it. |

## 4. Non-goals (recommended)

- **Own interception engine** (eBPF/nfqueue) — settled decision 2. Little
  Snitch for Linux's eBPF approach also shows the trade-off: its vendor
  calls it "privacy, not security" and documents bypass under load.
- **DNS resolver / DoH / DNS filtering** — Portmaster's and LS6's
  territory and a large surface; on Fedora/Bazzite, systemd-resolved
  already provides DoT. Revisit only if users ask.
- **Bandwidth limiting / quotas** — NetLimiter territory; opensnitchd has
  no shaping (its `quota` operand is commented out). Visibility (P3.4) is
  in scope; control is not.
- **VPN / onion routing** (Portmaster SPN) and **network-threat
  detection** (GlassWire ARP/evil-twin/new-device alerts) — different
  products; the latter fits Component B's scanner better, if anywhere.

## 5. Dependencies

- P0.2 (full rule list) → P2.1 editor, P2.5 groups, P2.6 insights, P2.7
  export.
- P0.3 (list-file enforcement) → P0.4 profile enforcement, P5.2 presets.
- P2.3 modes + P2.4 learning share a "silent-allow-and-log" mechanism;
  build it once.
- P3.1 history → P3.6 digests and better P2.6 "unused rule" detection.
- P3.4 real traffic data → any charts; no charts on fake data.
- P4.1 checksums need the rpm-ostree/Flatpak update-churn design first.
- Anything touching #39's files → after #39 merges (see §3 constraint).

## 6. Suggested next five tasks

1. P0.1 app-bound prompt scopes, first half (security; `verdict.rs`
   internals only, signature unchanged — can land on `main` now). It only
   protects users once it reaches a bridge release and #39's head, so
   pair it with the #39-owner decision in item 2.
2. P0.7 housekeeping: close #34, file issues for the P0 findings, and
   flag P0.1 (host-only rules in the pinned head) and P0.5 (pause before
   GUI-presence check) to #39's owner.
3. "Honest UI" pass on QML #39 doesn't touch: the "Preview — not
   enforced" labels from P0.3/P0.4 (`BlocklistsPage.qml`,
   `ProfilesPage.qml`) and the UI-only half of P0.6 (hide the dead
   countdown and zeroed per-connection byte counts).
4. Design note for P0.3 list-directory enforcement under both the user
   service and #39's system mode (ownership/permissions/SELinux labels),
   ready to implement when #39 merges.
5. P2.4 curated-defaults spike: capture the first-boot connection set on a
   fresh Bazzite image to seed the starter rule set. Uses the same VM
   tooling as bazzite-tower's rollout-gate work — wait until that work is
   done rather than share its VM or skew its timing measurements.

## Sources

Product research (fetched 2026-10-07):

- Little Snitch 6: <https://obdev.at/products/littlesnitch/releasenotes6.html>,
  <https://help.obdev.at/littlesnitch6/concepts-rules>,
  <https://help.obdev.at/littlesnitch6/concepts-opmodes>,
  <https://help.obdev.at/littlesnitch6/concepts-profiles>,
  <https://help.obdev.at/littlesnitch6/concepts-blocklists>,
  <https://help.obdev.at/littlesnitch6/lsc-insights>,
  <https://help.obdev.at/littlesnitch6/cmd-overview>
- Little Snitch for Linux: <https://obdev.at/products/littlesnitch-linux>,
  <https://obdev.at/blog/little-snitch-for-linux/>,
  <https://help.obdev.at/littlesnitch-linux/rules>,
  <https://help.obdev.at/littlesnitch-linux/blocklists>,
  <https://github.com/obdev/littlesnitch-linux>,
  <https://discuss.privacyguides.net/t/little-snitch-for-linux/36986>
- LuLu: <https://objective-see.org/products/lulu.html>
- OpenSnitch: <https://github.com/evilsocket/opensnitch/releases>,
  <https://github.com/evilsocket/opensnitch/wiki/Rules>,
  <https://github.com/evilsocket/opensnitch/blob/v1.8.0/proto/ui.proto>,
  <https://github.com/evilsocket/opensnitch/blob/v1.8.0/daemon/rule/operator.go>,
  <https://github.com/evilsocket/opensnitch/issues> (#116, #208, #527,
  #548, #1078, #1081, #1116, #1512, #1519, #1547, #1617, #1644, #1667)
- Portmaster: <https://github.com/safing/portmaster>,
  <https://docs.safing.io/portmaster/settings>, <https://safing.io/pricing/>
- GlassWire: <https://www.glasswire.com/features/>,
  <https://www.glasswire.com/changes/>
- Windows Firewall Control:
  <https://www.binisoft.org/pdf/guides/Malwarebytes-WFC-User-Guide.pdf>
- Fort Firewall: <https://github.com/tnodir/fort/wiki/Fort-Firewall-User-Guide>,
  <https://github.com/tnodir/fort/wiki/Rules>
- simplewall: <https://github.com/henrypp/simplewall>
- TinyWall: <https://tinywall.pados.hu/faq.php>
- NetLimiter: <https://netlimiter.com/docs/basic-concepts/rules>,
  <https://netlimiter.com/docs/basic-concepts/quota-rule>
- picosnitch: <https://github.com/elesiuta/picosnitch>

Internal inventory: `crates/snitchwatch-bridge/src/translator/verdict.rs`
(`verdict_to_rule`, `build_operator_checked`),
`crates/snitchwatch-bridge/src/blocklists/mod.rs` (`NoopRuleSink`,
`spawn_refresh_loop` test-only caller),
`crates/snitchwatch-bridge/src/profiles/mod.rs` (`NoopProfileRuleSink`),
`crates/snitchwatch-bridge/src/grpc_server.rs` (Subscribe handling),
`crates/snitchwatch-kirigami/src/wizard.rs` (onboarding unit probe),
`vendor/opensnitch/proto/ui.proto`, `vendor/opensnitch/daemon/rule/operator.go`.
