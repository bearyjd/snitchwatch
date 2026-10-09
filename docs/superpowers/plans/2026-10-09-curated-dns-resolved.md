# Recommended rules: an opt-in DNS entry for systemd-resolved (owner decision S6)

**Date:** 2026-10-09
**Issue:** #117 (S6). **Owner decision:** option (a), "an opt-in entry on the
Recommended rules page ('DNS only, any address')".
**Baseline:** `origin/main` @ `ac446e2`; branch `feat/curated-dns-resolved`.
**Size:** S. One data entry, one new `Protocol` value, one new destination
kind with a hard allowlist, one fixed Kirigami sentence. No new wire field,
no new capability.
**Builds on:** `2026-10-08-prompt-slot-ux.md` Part D (curated defaults;
S3: opt-in, `/usr` only, host-constrained), `2026-10-08-refused-delete-honesty.md`
(entry statuses), `2026-10-08-packaged-bridge-fetch-rule.md` ("DNS is not
covered").

## Why

On a deny default nothing resolves until the system resolver's upstream
queries are allowed. The upstream server changes with every network (DHCP,
VPN, a hotel's), so no host- or address-constrained rule fits. The bazzite-tower
r10 capture saw `/usr/lib/systemd/systemd-resolved` → the network's DNS
server:53 over udp, and Part D excluded it as owner question S6 because S3 says
"host-constrained". The owner chose to offer it, opt-in.

Without it the other recommended entries cannot work on a deny default:
NetworkManager's connectivity check, `flatpak`→`dl.flathub.org` and the
packaged blocklist fetch all need a name looked up first (the fetch plan says
"DNS is a prerequisite, not a gap in this rule").

## The rule

A curated list rule, in the existing shape minus the destination condition:

| # | type | operand | data | sensitive |
|---|---|---|---|---|
| 0 | simple | `process.path` | `/usr/lib/systemd/systemd-resolved` | yes (exact, case sensitive) |
| 1 | simple | `dest.port` | `53` | no |
| 2 | regexp | `protocol` | `^(tcp\|udp)6?$` | no |

- Name `snitchwatch-default-dns-resolved` (id `dns-resolved`). `allow`,
  `always`, `precedence: false`, description `snitchwatch curated default v1`.
- Operator semantics (`vendor/opensnitch/daemon/rule/operator.go`, v1.8.0,
  read only): a list ANDs its members (`listMatch`). `process.path` simple +
  sensitive is `==` on `con.Process.Path`. `dest.port` simple compares the
  decimal port string. `protocol` regexp is `MatchString`, unanchored, so the
  pattern anchors itself; `con.Protocol` is `tcp`, `udp`, `tcp6`, `udp6`,
  `udplite`, `sctp`, `icmp`... so `^(tcp|udp)6?$` matches exactly the four TCP
  and UDP forms and not `udplite`. The pattern is already lowercase, so the
  daemon's in-place lowercasing (`Compile`) leaves it unchanged and the
  canonical comparison (`canonical.rs`) still reads the daemon's copy as
  unedited. Omitting `dest.*` leaves the destination unconstrained: that is
  the meaning of "any address".
- Matching order (`FindFirstMatch`): iterates enabled rules by name and
  returns at the first `deny`/`reject` or `precedence` rule; a non-precedence
  allow is remembered and loses to any later matching deny. Ours is a
  non-precedence allow, so **any matching deny wins**: a user deny, a
  blocklist deny (`z00-blocklist:`; an IP list blocking a DNS server blocks
  resolved's query to it), a profile deny (`850-profile:`).

### udp and tcp: one entry, a regexp protocol (decided)

DNS needs udp/53 and tcp/53 (truncated answers, large answers, DNSSEC). Options:

1. **Two entries** (`dns-resolved-udp`, `dns-resolved-tcp`). Fits the current
   single-protocol shape with no change, but is two switches for one owner
   decision ("an entry"), lets a user leave DNS half on, and doubles the
   surface of the any-address exception.
2. **One entry, a new protocol value `tcp+udp`** whose condition is the
   regexp `^(tcp|udp)6?$` (chosen). The existing protocol leaf is already
   a regexp (`^tcp6?$`), so this is the same operator type with a wider
   anchored pattern. It stays inside the canonical-check machinery:
   `Protocol::pattern()` builds it, `check_leaves` accepts it, `canonical`
   is unchanged.
3. **A nested list / list operator for the protocol.** The daemon copies list
   members one level deep only (`rule_policy.rs` header), `check_leaves`
   rejects nested lists, and it adds a shape for no gain.

## Data and code changes (kept to the entry list, data and tests)

- `data/curated-defaults-v1.json`: the new entry, **appended** (existing order
  and ids unchanged).
- `curated/mod.rs`:
  - `Protocol::TcpAndUdp` (serde `"tcp+udp"`): pattern `^(tcp|udp)6?$`, label
    `TCP and UDP`.
  - `CuratedEntry.any_address: bool` (`"anyAddress"`, default false). Exactly
    one of `host`, `loopback`, `anyAddress`. `rule()` adds no destination leaf
    for it; `allows()` says "any address".
  - `check_leaves`: a rule with **no destination leaf** is accepted **only**
    as the DNS shape: program exactly `/usr/lib/systemd/systemd-resolved`,
    port exactly `53`, protocol exactly `^(tcp|udp)6?$`. Any other
    destination-less rule is refused, at data load and at the send point
    (`check_curated_rule` is the send-path allowlist). So a later data-file
    edit cannot add a second any-address entry, or widen this one, without a
    reviewed code change. The existing "no destination is never offered"
    test becomes "no destination is offered for one program only".
  - `tcp+udp` is accepted **only** in that destination-less branch. A host or
    loopback entry must stay `tcp` or `udp`: no entry needs the wider pattern
    there, and refusing it keeps the widening to the one reviewed exception.
    Data load also refuses `anyAddress` together with a host or loopback, on
    any other program, and on any other port.
- Nothing in `manager*.rs`, `reconcile.rs`, `store.rs`, `wire.rs`, the
  daemon-command path or the wire protocol changes: they are id-driven and
  read `entry.rule()`/`allows()`. (The sibling worktree `wt-fu120` edits
  `manager*.rs` and `reconcile.rs`; this change avoids them so the merge is
  trivial.)

## Wording (what the user reads)

- Program: `/usr/lib/systemd/systemd-resolved`.
- **Allows** (generated from the entry, so it cannot drift from the rule):
  `/usr/lib/systemd/systemd-resolved may connect to any address on TCP and UDP port 53, over IPv4 and IPv6.`
- **Why** (data file, plain text, 396 of 400 characters):
  "With the firewall set to deny by default, no name can be looked up until
  the system's DNS resolver may reach its DNS server. That server differs on
  every network, so this lets that one program reach any address on port 53.
  It does not make lookups private: whoever runs the server sees them.
  NetworkManager's check, Flathub updates and blocklist downloads need names
  looked up, so they need this."
- **Evidence** (data file): "bazzite-tower r10: /usr/lib/systemd/systemd-resolved
  to the network's DNS server port 53 over udp (not selected in v1: S6). TCP
  port 53 (truncated and large answers) is the standard fallback but was not
  captured; r13 checks both."
- **Kirigami explanation** (fixed text on the page): "Each rule lets one
  program reach one host, or this computer, on one port, and says exactly
  what it allows, except the DNS rule, which lets the system resolver reach
  any address on port 53. Snitchwatch adds none unless you turn it on; Turn
  all on turns on every rule below, including that one, so read each one
  first." plus the existing sentences about rules added earlier and the
  firewall service's last rule list. The old "one place" is no longer true
  for DNS, and the page no longer says it.
- Statuses, the Keep/Remove flow and the "edited by you" text are unchanged
  and apply to this entry like any other.

What it does **not** say it does, and does not do:
- It allows the resolver's traffic only: the rule's program is the exact
  path. An app that queries the local stub (`127.0.0.53:53`) itself is
  attributed to the app, not to resolved, and is not covered (the packaging
  README already says a stub query needs its own allow). Apps on Fedora
  usually go through nss-resolve, which reaches resolved over a Unix socket
  and so makes resolved the sender.
- Other resolver ports are not allowed: DNS over TLS (853), mDNS (5353),
  LLMNR (5355).
- Any address includes private and loopback addresses: if resolved is
  pointed at a hostile server on port 53, the rule does not stop it. That is
  the cost of "any address"; it is why this is opt-in and says so.

## Semantics matched, not changed

- **Off by default.** Nothing is on until the user turns it on
  (`Choices::default()` is empty). A first run with this rule already in the
  firewall (added earlier) reads "In the firewall (added earlier)" with the
  switch on and nothing is sent until the user keeps or turns it off, as for
  every entry.
- **"Turn all on".** The existing behaviour asks for every entry that is not
  already on (Kirigami `setAll`). The DNS entry is therefore turned on by
  "Turn all on", like the other three. This is the existing semantics and is
  not changed here; the page text above now says so. Open question for the
  owner: whether the one any-address entry should be excluded from the bulk
  action.
- **Reserved prefix.** `snitchwatch-default-dns-resolved` is under the
  reserved prefix, so a GUI `AddRule`/`UpdateRule`/`DeleteRule` or import under
  that name is refused (`CURATED_NAME_REFUSED`); only a pure `enabled` toggle
  of the unedited copy is taken (`toggleable`, built on `canonical`). An edited
  copy keeps `CURATED_MANAGED_REASON` and is never deleted by reconcile; it has
  the Remove button.
- **User deletes and edits.** A copy deleted outside Snitchwatch goes to
  `deletedByUser` and is not reinstalled across restarts. An edited copy is
  left alone, even on opt-out. Same code path, id-keyed.
- **Profiles and other prefixes.** The rule is a non-precedence allow, so
  `850-profile:` denies, `z00-blocklist:` denies and any user deny beat it.
  `000-snitchwatch-bridge-fetch` (bridge→443) and this rule match disjoint
  programs. Profile export leaves the `snitchwatch-default-` prefix out.
  The profile path and the curated path refuse each other's names, unchanged.

## Risks

- **A default-allow to any address.** Mitigated by: opt-in; exact
  case-sensitive `/usr` path (never a regexp for the program); port 53; the two
  transports only; `check_curated_rule` pins the whole shape for this program
  only; the text says what it allows; non-precedence, so denies win.
- **Never matches another binary.** `process.path` is a sensitive `simple`
  leaf (`==`); tests prove a different path, a case variant, a prefix or a
  suffix of it, a `/usr/local` copy and an empty path do not match.
- **A user can make resolved's path wrong for them.** If the image ships the
  resolver elsewhere (not `/usr/lib/systemd/systemd-resolved`), the rule is
  inert, never broader. r13 verifies the path on the image.
- **Looks like a duplicate of a user's own DNS rule.** Different name, so
  both coexist; no dedupe.
- **Evidence gap: tcp/53 is not in the r10 capture.** DNS needs both
  transports (truncated and large answers, DNSSEC), so the rule has both; the
  evidence is udp only, stated in the entry's `evidence`, and r13 confirms tcp
  (below).
- **A stale GUI.** An older GUI lists the entry from the bridge's own texts
  (`allows`, `why`) and shows them unchanged; its fixed explanation is the old
  one, and says "one place". Accepted: bridge and GUI ship together.

## Tests (TDD, each RED first)

Bridge, `curated/dns_tests.rs`, with a small mirror of the daemon's operator
semantics (`simple` equality, sensitive or `EqualFold`; `regexp` lowercased
`MatchString`; list ANDed) driven over a table of connections:
- The data file offers `dns-resolved` last, with the exact path, port 53,
  protocol `tcp+udp`, and the exact `allows`, `why` and name.
- The rule matches `{systemd-resolved, any of 1.1.1.1 / 192.168.1.1 / ::1 /
  fe80::1 / 127.0.0.53, 53, tcp|udp|tcp6|udp6}` and **only** that: not a
  different port (5353, 853, 5, 530, 153), protocol (`udplite`, `sctp`,
  `icmp`, `xtcp`, `tcpx`, `tcp66`), path (a prefix, a suffix, a case variant,
  `/usr/local/lib/systemd/systemd-resolved`, `/usr/lib/systemd/systemd-resolve`,
  `/usr/bin/curl`, empty).
- `check_curated_rule`: the DNS rule passes; each widening is refused (a
  regexp or wildcard program, an insensitive program, a different program
  with the same shape, another port, a port range, `tcp+udp` replaced by `.*`
  or by an unanchored pattern, an extra condition, a missing port or
  protocol); any other destination-less entry is refused at data load.
- Off by default: `plan(entries(), no rules, Choices::default())` sends
  nothing for it; on, it plans one `Install` of exactly `entry.rule()`.
- Respect for the user: unedited copy is `toggleable`; an edited copy (port,
  program, an extra leaf) is not, and keeps the reserved-name reason; a copy
  the daemon reports back normalised (`as_reported`) is unedited; a user
  delete is recorded and not reinstalled; an edited copy survives opt-out.
- Reserved prefix: `AddRule`/`UpdateRule` of a rule under the name (or a look-alike widened
  copy) is refused by `validate_user_rule`; `deletable` is false.
- Wire/GUI strings: the summary carries the `allows` and `why` above.

bridge-cli (`tests/curated_defaults.rs`: `turn` and `entry_until` gain id
parameters, the flatpak ones stay as thin wrappers; the mock loader's
`validate_rule_shape` must accept the three-leaf rule): turn the entry on → one
`CHANGE_RULE` of exactly the DNS rule → "installed"; an edited copy is left
alone; a copy deleted outside is not reinstalled after a restart; the entry is
off before any choice.

Kirigami: `RecommendedRulesPage.qml`'s explanation says "any address" and "Turn
all on" (probe in `recommended_rules_qml.rs`), and its rows show a bridge `allows`
as PlainText (existing).

**Mutation checks** (each mutant must turn a named test red; results in the
report): protocol pattern loses `$` / loses `^` / gains `udplite`; program leaf
loses `sensitive`; port 53 → 5353; the destination-less allowlist is dropped (any
program, any port); `check_curated_rule` stops checking the protocol; reconcile
installs an entry nobody chose (on by default); `allows()` text changed; the
entry moves first in the list.

## Gates

`cargo fmt --all --check`; `cargo clippy --all-targets -- -D warnings` and the
Kirigami clippy; `cargo test -j 4 --no-fail-fast`; the Kirigami headless suite
(touched); `just package-check`.

## For the tower r13 gate

1. **The path exists and is the one the daemon reports.** On the Bazzite image,
   `readlink -f /proc/$(pidof systemd-resolved)/exe` is exactly
   `/usr/lib/systemd/systemd-resolved`, and the daemon's connection event for
   resolved reports that `process.path` (not `/lib/...` or a deleted-binary
   suffix).
2. **The rule actually matches resolved's queries**: with a deny default and
   only this entry on, `resolvectl query example.org` succeeds and the daemon
   shows a hit on `snitchwatch-default-dns-resolved` for udp. **TCP needs
   resolved itself to fall back**, so query a name whose answer does not fit
   a UDP reply (a large TXT record, or DNSSEC answers) or use an upstream
   that sets the truncation flag, and confirm a `tcp` or `tcp6` hit on the
   same rule. (`resolvectl --protocol=` chooses DNS versus LLMNR/mDNS, not
   the transport, and `dig +tcp` is attributed to `dig`, not resolved, so
   neither tests this rule.) Check both address families (`udp6`/`tcp6`).
3. **Does not cover what it should not**: a `dig @8.8.8.8` from a shell is
   still prompted/denied (process is `dig`); DoT (853) still denied.
4. **Entry turned off** removes the rule and lookups fail again; the
   daemon's reloaded copy reads "Rule installed" across a daemon restart
   (canonical comparison with a real daemon).
5. **Interplay**: with the packaged `000-snitchwatch-bridge-fetch` rule and
   a blocklist subscription, a list refresh succeeds on a deny default only
   with this entry on.

## Open questions for the owner

1. **Should "Turn all on" include it?** Today it does (all entries). The page
   says so. Excluding any-address entries from the bulk action would be a
   small Kirigami+bridge change.
2. Stub queries from apps (`127.0.0.53`) and DoT/mDNS are not covered, by
   design; a second opt-in entry is possible if r13 shows apps use the stub.

## As implemented (2026-10-09)

Branch `feat/curated-dns-resolved`, local commits only.

- **Shape.** As planned: `anyAddress` entry field, `Protocol::TcpAndUdp`
  (serde names are explicit, `tcp`, `udp`, `tcp+udp`), appended entry
  `dns-resolved`, rule name `snitchwatch-default-dns-resolved`.
  `check_leaves` takes a destination-less rule only for
  `/usr/lib/systemd/systemd-resolved`, port `53`, protocol
  `^(tcp|udp)6?$`; `tcp+udp` is refused for host and this-computer entries.
  Data load refuses `anyAddress` with a host or loopback, for another
  program, port or transport, and the old "no destination at all" case.
- **Spec-change test edits** (not regressions): `curated/tests.rs` pins the
  entry ids and the `allows` strings of the data file, so both gain the
  fourth entry; its "no destination" comment now says "for the DNS resolver
  only".
- **Tests.** `curated/dns_tests.rs` (14 tests; a mirror of the daemon's
  `Operator.Match` in test code evaluates the rule `entries()` builds, with
  the real `regex` crate), two end-to-end tests in
  `bridge-cli/tests/curated_defaults.rs` (`turn_entry` and `entry_until_id`
  take an id; `turn` and `entry_until` stay as the flatpak wrappers), and
  a fixed-text assertion on the page explanation in
  `kirigami/tests/recommended_rules_qml.rs`.
- **Mutation checks.** 26 bridge mutants (protocol pattern anchors and
  members, program sensitivity, port, the destination-less allowlist's
  program/port/transport, the host branch accepting `tcp+udp`, destination
  counting, `allows()` text, the data file's port/protocol/path/order/why,
  reconcile on-by-default, reinstall of a user delete, overwrite of an edit,
  `toggleable`): all killed. One survived the first run (several
  destinations with `anyAddress` and `loopback` on a loopback program, where
  `allows()` would say "any address" for a loopback-only rule) and got a
  test case. Seven of them were rerun against the end-to-end file: all
  killed. Three mutants of the Kirigami explanation (back to "one place", the DNS exception dropped, "including that one" dropped): all killed.
- **What the end-to-end "installed" proves.** The mock daemon's
  responder (`lists.rs` `respond`) runs `validate_rule_shape` on every
  `CHANGE_RULE` and answers `ERROR` for a shape it would not compile, so an
  "installed" DNS entry means that mock accepted the three-condition rule.
  It is a mock of `Compile`, not the real daemon; r13 is the real check.
- **A mistake in the DNS entry disables every entry.** `entries()` returns
  nothing for an invalid data file (existing behaviour: fail safe), so a
  DNS-entry typo shows as every end-to-end test failing, as the port mutant
  did.
- Not updated here, for merge time: `HANDOFF.md` ("Owner decisions pending:
  #117") and the S6 row of the capture table in
  `2026-10-08-prompt-slot-ux.md`.
