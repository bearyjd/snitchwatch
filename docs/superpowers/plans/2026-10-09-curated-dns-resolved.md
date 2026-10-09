# Recommended rules: an opt-in DNS entry for systemd-resolved (owner decision S6)

**Date:** 2026-10-09 (design fixed after the PR #121 security review the same day)
**Issue:** #117 (S6). **Owner decision:** option (a), "an opt-in entry on the
Recommended rules page ('DNS only, any address')". The review below narrows
the rule (it names its sender) and the bulk action (it is never turned on in
bulk); the destination stays "any address".
**Baseline:** `origin/main` @ `ac446e2`, then merged with `ddb6b96` (#122);
branch `feat/curated-dns-resolved`.
**Size:** S–M. One data entry, one new `Protocol` value, one new destination
kind with a hard allowlist including a sender pin, one additive wire flag
(`broad`), one fixed Kirigami sentence and a store rule. No new capability.
**Builds on:** `2026-10-08-prompt-slot-ux.md` Part D (curated defaults;
S3: opt-in, `/usr` only, host-constrained), `2026-10-08-refused-delete-honesty.md`
(entry statuses), `2026-10-08-packaged-bridge-fetch-rule.md` ("DNS is not
covered"; the user condition is load-bearing).

## Why

On a deny default nothing resolves until the system resolver's upstream
queries are allowed. The upstream server changes with every network (DHCP,
VPN, a hotel's), so no host- or address-constrained rule fits. The bazzite-tower
r10 capture saw `/usr/lib/systemd/systemd-resolved` → the network's DNS
server:53 over udp, and Part D excluded it as owner question S6 because S3 says
"host-constrained". The owner chose to offer it, opt-in.

Without it, the NetworkManager and Flathub entries and the packaged blocklist
fetch cannot work on a deny default: they need a name looked up first (the
fetch plan says "DNS is a prerequisite, not a gap in this rule"). `chronyc`
talks to this computer and needs no lookup.

## The rule

A curated list rule, in the existing shape with no destination condition and a
sender pin:

| # | type | operand | data | sensitive |
|---|---|---|---|---|
| 0 | simple | `process.path` | `/usr/lib/systemd/systemd-resolved` | yes (exact, case sensitive) |
| 1 | simple | `user.id` | `193` | no |
| 2 | simple | `dest.port` | `53` | no |
| 3 | regexp | `protocol` | `^(tcp\|udp)6?$` | no |

- Name `snitchwatch-default-dns-resolved` (id `dns-resolved`). `allow`,
  `always`, `precedence: false`, description `snitchwatch curated default v1`.
- **Why the sender is pinned (security review H1, HIGH).** A path alone does
  not name the sender. `/usr/lib/systemd/systemd-resolved` is mode 0755 and
  honours `LD_PRELOAD`, so any unprivileged user can run it with a preloaded
  library and get a process whose `process.path` is exactly the rule's path:
  an any-address tcp/udp:53 channel for command and control or exfiltration
  (user and mount namespaces and host-network containers reach the same
  place). The three older entries share the weakness but are bounded to one
  host and port; this one removes the bound, so it must name its sender.
  `user.id` is compared with `con.Entry.UserId`, the socket's host uid (the
  eBPF process uid, or the nfqueue packet uid read in the initial user
  namespace), which a user namespace cannot fake; only root can run a process
  as 193, and root can do anything anyway.
- **`user.id`, not `user.name` (the fetch rule's choice) and why the
  precedent's reasoning doesn't carry over.** The packaged fetch rule is a
  file the daemon loads and never rewrites. This rule is installed with
  `CHANGE_RULE`, and `Loader.Replace` compiles the rule **then saves it**
  (`loader.go`): `Compile` overwrites a `simple` `user.name` leaf's data with
  the uid, so the saved file would say `"193"`. After a daemon restart,
  `user.Lookup("193")` fails ("user.name Operand error") and the rule is
  silently skipped, which the bridge would read as "deleted outside" and never
  reinstall. `user.id` is not rewritten, so what the daemon reports is what was
  sent and `canonical.rs` needs no new normalisation. The fetch plan's
  worry about a fixed uid (sysusers may allocate another) is real, so a wrong
  uid fails closed: the rule matches nothing, DNS stays blocked and r13 below
  catches it. Fedora fixes `systemd-resolve` at 193: on the Fedora 44 dev
  host, `/usr/lib/sysusers.d/systemd-resolve.conf` has
  `u systemd-resolve 193 "systemd Resolver"`, `getent passwd systemd-resolve`
  gives 193, and `/usr/lib/systemd/systemd-resolved` is mode 0755 at the
  rule's path (not yet seen on the Bazzite image; r13).
- Operator semantics (`vendor/opensnitch/daemon/rule/operator.go`, v1.8.0,
  read only): a list ANDs its members (`listMatch`). `process.path` simple +
  sensitive is `==` on `con.Process.Path`, after `CleanPath` strips a
  ` (deleted)` suffix. `user.id` simple is `EqualFold` on the decimal uid.
  `dest.port` simple compares the decimal port string. `protocol` regexp is
  `MatchString`, unanchored, so the pattern anchors itself; `con.Protocol` is
  `tcp`, `udp`, `tcp6`, `udp6`, `udplite`, `sctp`, `icmp`..., so
  `^(tcp|udp)6?$` matches exactly the four TCP and UDP forms. The pattern is
  already lowercase, so `Compile`'s in-place lowercasing leaves it unchanged.
  Omitting `dest.*` leaves the destination unconstrained: "any address".
- Matching order (`FindFirstMatch`): iterates enabled rules by name and
  returns at the first `deny`/`reject` or `precedence` rule; a non-precedence
  allow is remembered and loses to any later matching deny. Ours is a
  non-precedence allow, so **any matching deny wins**: a user deny, a
  blocklist deny (`z00-blocklist:`; an IP list blocking a DNS server blocks
  resolved's query to it), a profile deny (`850-profile:`).

### udp and tcp: one entry, a regexp protocol (decided)

DNS needs udp/53 and tcp/53 (truncated answers, large answers, DNSSEC). Options:

1. **Two entries** (`dns-resolved-udp`, `dns-resolved-tcp`). Fits the current
   single-protocol shape, but is two switches for one owner decision, lets a
   user leave DNS half on, and doubles the surface of the any-address
   exception.
2. **One entry, a new protocol value `tcp+udp`** with the regexp
   `^(tcp|udp)6?$` (chosen): the same operator type with a wider anchored
   pattern, inside the canonical-check machinery (`Protocol::pattern()` builds
   it, the allowlist accepts it, `canonical` is unchanged).
3. **A nested list for the protocol.** The daemon copies list members one
   level deep only, the allowlist rejects nested lists, and it adds a shape
   for no gain.

## Data and code changes

- `data/curated-defaults-v1.json`: the new entry, **appended** (existing order
  and ids unchanged).
- `curated/mod.rs`:
  - `Protocol::TcpAndUdp` (`rename_all = "lowercase"` plus one explicit
    `"tcp+udp"`): pattern `^(tcp|udp)6?$`, label `TCP and UDP`.
  - `CuratedEntry.any_address` (`"anyAddress"`, default false). Exactly one of
    `host`, `loopback`, `anyAddress`. `rule()` adds no destination leaf and
    pins `user.id` instead; `allows()` says "any address ... but only while it
    runs as user ID 193 (the systemd-resolve account)"; `broad()` is the flag
    the GUIs read.
  - The allowlist, split into `check_program`, `check_destination`,
    `check_port`, `check_protocol`: a rule with **no `dest.host`/`dest.ip`
    leaf** is accepted **only** as the DNS shape, program exactly
    `/usr/lib/systemd/systemd-resolved`, then `simple user.id 193` (not
    sensitive) **right after the path**, port exactly `53`, protocol exactly
    `^(tcp|udp)6?$`. A destination-less rule without the exact pin, or for
    any other user, program, port or transport, is refused at data load and
    at the send point (`check_curated_rule`). `tcp+udp` is refused for host and
    loopback entries; a user pin in any other place or shape is refused.
- `curated/wire.rs` + `manager_state.rs` (one line each): the summary gains
  `broad: bool` (`serde(default, skip_serializing_if)`), derived from the
  entry. It is **additive**: absent means false, so an older bridge's summary
  parses and an older GUI ignores it. (This supersedes the first draft's "no
  new wire field".)
- Kirigami `CuratedStore::request_all(on)`: "Turn all on" never asks for a
  `broad` entry in either branch (the `e.on != on` branch and the adopt of a
  rule already in the firewall); "Turn all off" does include it (the safe
  direction). A single row's switch and Keep still work.
- `bridge-cli/tests/curated_defaults.rs` is **untouched** (PR #122 edited its
  helpers); the DNS tests live in `tests/curated_dns.rs` with their own small
  copy of the few helpers they need. Sharing one helper module is a later
  cleanup once nobody else is editing the file.

## Wording (what the user reads)

- Program: `/usr/lib/systemd/systemd-resolved`.
- **Allows** (generated from the entry, so it cannot drift from the rule):
  `/usr/lib/systemd/systemd-resolved may connect to any address on TCP and UDP port 53, over IPv4 and IPv6, but only while it runs as user ID 193 (the systemd-resolve account).`
- **Why** (data file, plain text, 394 of 400 characters): "A deny default
  blocks every lookup until the system resolver may reach its DNS server. That
  server differs on every network, so this lets it reach any address on port
  53. Lookups are not private: whoever runs the server sees them, and any
  program can send data out inside lookups, since every app's lookups use this
  rule. Flathub updates, blocklist downloads and NetworkManager's check need
  it."
- **Evidence** (data file): "bazzite-tower r10: /usr/lib/systemd/systemd-resolved
  to the network's DNS server port 53 over udp (left out of v1: owner question
  S6, decided opt-in). TCP port 53 (truncated and large answers) was not
  captured, nor was the sender's user ID 193 (Fedora's systemd-resolve
  account); r13 checks both."
- **Kirigami explanation** (fixed text on the page): "Each rule lets one
  program reach one host, or this computer, on one port, and says exactly what
  it allows, except the DNS rule, which lets the system resolver reach any
  address on port 53. Snitchwatch adds none unless you turn it on. The "Turn
  all on" button skips the DNS rule, which you turn on by itself; "Turn all
  off" turns it off too. A rule already in the firewall (added earlier) stays
  as it is until you keep it or turn it off. What each says is as of the
  firewall service's last rule list."
- Statuses, the Keep/Remove flow and the "edited by you" text are unchanged
  and apply to this entry like any other.

What it does **not** do:
- It allows the resolver's traffic only. An app that queries the local stub
  (`127.0.0.53:53`) itself is attributed to the app, not to resolved, and is
  not covered (the packaging README already says a stub query needs its own
  allow). Apps on Fedora usually go through nss-resolve, which reaches
  resolved over a Unix socket and so makes resolved the sender.
- Other resolver ports are not allowed: DNS over TLS (853), mDNS (5353),
  LLMNR (5355).
- It does not stop data leaving **inside** lookups: with it on, any program's
  lookup reaches resolved, which sends it to any DNS server (tunnelling).
  Every app's lookups go through this rule. The row says so. It is the cost of
  "any address" and why it is opt-in and not part of "Turn all on".
- A hostile DNS server on port 53 is reachable. Same cost.

## Semantics matched, not changed

- **Off by default.** Nothing is on until the user turns it on
  (`Choices::default()` is empty). A first run with this rule already in the
  firewall (added earlier) reads "In the firewall (added earlier)" and nothing
  is sent until the user keeps or turns it off, as for every entry.
- **"Turn all on" (changed on purpose, M1).** The other entries keep their
  bulk semantics. The DNS entry is the one that "allows more than one named
  place", so the bridge marks it `broad` and the GUI never turns it on or
  adopts it in bulk; it is a deliberate single action. The page says so.
- **Reserved prefix.** `snitchwatch-default-dns-resolved` is under the
  reserved prefix, so a GUI `AddRule`/`UpdateRule`/`DeleteRule` or import under
  that name is refused (`CURATED_NAME_REFUSED`); only a pure `enabled` toggle
  of the unedited copy is taken (`toggleable`, built on `canonical`). An edited
  copy (including one that lost the pin) keeps `CURATED_MANAGED_REASON`, is
  never deleted by reconcile, and has the Remove button.
- **User deletes and edits.** A copy deleted outside Snitchwatch goes to
  `deletedByUser` and is not reinstalled across restarts. An edited copy is
  left alone, even on opt-out. Same code path, id-keyed.
- **Profiles and other prefixes.** The rule is a non-precedence allow, so
  `850-profile:` denies, `z00-blocklist:` denies and any user deny beat it.
  `000-snitchwatch-bridge-fetch` (bridge→443, account `snitchwatch`) and this
  rule match disjoint programs. Profile export leaves the
  `snitchwatch-default-` prefix out. The profile path and the curated path
  refuse each other's names, unchanged.

## Risks

- **A default-allow to any address.** Mitigated by: opt-in and never in bulk;
  exact case-sensitive `/usr` path (never a regexp for the program); the
  resolver's own account by user ID; port 53; the two transports only;
  `check_curated_rule` pins the whole shape for this program only; the text
  says what it allows and what it can't stop; non-precedence, so denies win.
- **Another binary, or the same binary run by another user, doesn't match.**
  The path is a sensitive `simple` leaf (`==`); tests prove a different path, a
  case variant, a prefix or suffix, a `/usr/local` copy and an empty path do
  not match, and that the **same path, port and protocol from another uid**
  (a user's `LD_PRELOAD`ed copy, root, 192, 194, 1930, nobody) does not.
  The mirror also models `CleanPath`: a binary replaced on disk
  (` (deleted)`) **does** match, which only root can cause under `/usr`.
- **A wrong uid fails closed.** If the image's `systemd-resolve` isn't 193, the
  rule matches nothing: DNS stays blocked while the entry reads "Rule
  installed". r13 verifies the uid. (A dynamic lookup was rejected: the data
  file's rule must be static for the canonical comparison.)
- **A path that is wrong for the image is inert, never broader.** r13 verifies
  `/usr/lib/systemd/systemd-resolved`.
- **A data-file mistake disables every entry.** `entries()` returns nothing for
  an invalid file (fail safe, existing).
- **Evidence gap: tcp/53 and the uid are not in the r10 capture.** Stated in
  the entry's `evidence`; r13 confirms both.
- **A stale GUI.** An older GUI lists the entry from the bridge's own texts and
  ignores `broad`, so its "Turn all on" still includes the entry, and its fixed
  explanation is the old one. Accepted: bridge and GUI ship together.

## Tests (TDD, each RED first)

Bridge, `curated/dns_tests.rs` (rule shape, matching, allowlist, data load)
and `dns_policy_tests.rs` (reconcile, reserved name, `broad`), with a mirror of
the daemon's operator semantics (`simple` equality, sensitive or `EqualFold`;
`regexp` lowercased `MatchString`; list ANDed; `CleanPath`; `user.id` against
the connection's uid; an unknown operand or type panics) evaluated over
`entries()`' own rule:
- The data file offers `dns-resolved` last, with the exact path, uid pin,
  port 53, protocol `tcp+udp`, and the exact `allows`, `why` (at most 400
  characters) and name; the rule's four leaves are exactly the table above.
- It matches resolver traffic from uid 193 to six addresses over six protocol
  spellings, and **only** that: not 34 near misses, including the same binary
  from uid 1000, 0, 192, 194, 1930, 19 and 65534, other paths, ports (5353,
  853, 530, 153...) and protocols (`udplite`, `sctp`, `tcpx`, `tcp66`...).
- `check_curated_rule`: the DNS rule passes; each widening is refused (the
  program as a regexp, insensitive or another program; the pin missing,
  duplicated, moved, another uid, `user.name` even with the number, regexp,
  sensitive; another port or a range; the protocol widened or unanchored;
  an extra, missing or reordered condition); a host entry can't use the
  both-transports pattern or borrow the pin; data load refuses `anyAddress`
  for any other program, port or transport, with a host or loopback, or
  without the flag.
- Off by default; turned on, one `Install` of exactly `entry.rule()`; a
  first run with the rule already in the firewall changes nothing; a user
  delete stays deleted; an edit (port, program, protocol, **the pin changed,
  the pin removed, the pin as `user.name`**, precedence) is left alone, even
  on opt-out; the daemon's report of the rule is unedited.
- Reserved prefix: `validate_user_rule` refuses the name; read-only and
  non-deletable; an unpinned copy can't be toggled.
- `wire.rs`: `broad` is sent only when true, round-trips, and an older summary
  without the field parses. Only the DNS entry is `broad`.

bridge-cli, `tests/curated_dns.rs`, on the mock daemon (whose responder runs
`validate_rule_shape`, so "installed" proves the four-leaf rule compiles
there): listed off with the bridge's words and `broad`; turn on → one
`CHANGE_RULE` of exactly the four leaves → installed; flatpak not broad and
not installed; off → one `DELETE_RULE`; an edited copy and an unpinned copy
are left alone; a copy deleted outside is not reinstalled after a restart.

Kirigami: `CuratedStore::request_all` (both branches, "off" includes it, nothing
to ask when only broad entries are left, single turn-on and Keep still work);
`RecommendedRulesPage.qml`'s explanation (the DNS exception, the quoted button
names, no "one place"), and the probe sends a broad entry through the real
model: "Turn all on" asks only for the others, "Turn all off" for the rest.

**Mutation checks** (results in the report): the protocol pattern's anchors and
members; program sensitivity; port; the allowlist's program, port, transport;
the host branch accepting `tcp+udp`; the pin dropped from the rule, made
`user.name`, a wrong uid, sensitive; the pin optional or unchecked in the
allowlist, its uid, operand and sensitivity unchecked; `allows()` dropping the
sender; the data file's port, protocol, path, order and `why`; reconcile
on by default, reinstalling a user delete, overwriting an edit; `toggleable`;
`broad` always or never, dropped from the summary; the store including,
adopting or asking for a broad entry, or leaving it out of "Turn all off".

## Gates

`cargo fmt --all --check`; `cargo clippy --all-targets -- -D warnings` and the
Kirigami clippy; `cargo test -j 4 --no-fail-fast`; the Kirigami headless suite;
`just package-check`.

## For the tower r13 gate

1. **The path exists and is the one the daemon reports.** On the Bazzite image,
   `readlink -f /proc/$(pidof systemd-resolved)/exe` is exactly
   `/usr/lib/systemd/systemd-resolved`, and the daemon's connection event for
   resolved reports that `process.path` (not `/lib/...`).
2. **The sender is uid 193 and the events say so.** `getent passwd
   systemd-resolve` gives 193, and **resolved's udp and tcp events both carry
   uid 193** in the daemon's connection log; confirm the rule matches them.
   (A different uid means the rule matches nothing: change `DNS_USER_ID`.)
3. **The rule matches resolved's queries**: with a deny default and only this
   entry on, `resolvectl query example.org` succeeds and the daemon shows a hit
   on `snitchwatch-default-dns-resolved` for udp. **TCP needs resolved itself
   to fall back**, so query a name whose answer does not fit a UDP reply (a
   large TXT record, or DNSSEC answers) or use an upstream that sets the
   truncation flag, and confirm a `tcp` or `tcp6` hit on the same rule.
   (`resolvectl --protocol=` chooses DNS versus LLMNR/mDNS, not the transport,
   and `dig +tcp` is attributed to `dig`, so neither tests this rule.) Check
   both address families (`udp6`/`tcp6`).
4. **The sender pin works**: as an ordinary user,
   `LD_PRELOAD=/usr/lib64/libfoo.so /usr/lib/systemd/systemd-resolved ...` (or
   a copy that sends one UDP packet to port 53) is **not** allowed by this
   rule; the daemon prompts for it.
5. **Does not cover what it should not**: a `dig @8.8.8.8` from a shell is
   still prompted/denied (process is `dig`); DoT (853) still denied.
6. **Entry turned off** removes the rule and lookups fail again; after a
   daemon restart the rule still loads (the saved file has `user.id`, not a
   number under `user.name`) and reads "Rule installed".
7. **Interplay**: with the packaged `000-snitchwatch-bridge-fetch` rule and a
   blocklist subscription, a list refresh succeeds on a deny default only with
   this entry on.
8. **"Turn all on"** on the Recommended page leaves this entry off, and a copy
   already in the firewall is not adopted by it; its own switch turns it on.

## Open questions for the owner

1. Stub queries from apps (`127.0.0.53`) and DoT/mDNS are not covered, by
   design; a second opt-in entry is possible if r13 shows apps use the stub.
2. The three older entries pin the path only. Pinning their senders too
   (`NetworkManager` runs as root, so root; `flatpak`, `chronyc` run as the
   user) is a separate decision with a different cost; not done here.

## As implemented (2026-10-09)

Branch `feat/curated-dns-resolved`, local commits only, merged with
`origin/main` (`ddb6b96`, #122) without conflicts.

- **First round** (`95a7532`..`6c173f3`, `1f26540`): the entry, the allowlist, the
  tests, the Kirigami sentence. The first design pinned the path only, and
  "Turn all on" included the entry.
- **Fix round after review** (H1 in `874494d`; M1 and the sibling test file in
  `2e692b9`): the pin, the `broad` flag, the store rule and the cleanups.
  Code-review items done: the file over 800 lines (the DNS tests moved to
  `curated_dns.rs`, leaving `curated_defaults.rs` identical to main), the
  `check_curated_rule` doc and the tests.rs comment and test name, `check_leaves` split,
  `Protocol` serde simplified (`rename_all` plus one `rename`), the real-name
  assertion, the explanation's wording, the evidence text matching the data
  file, `HANDOFF.md` and the prompt-slot plan's S6 row.
- **Spec-change test edits** (not regressions): `curated/tests.rs` pins the
  entry ids and the `allows` strings of the data file, so both gain the
  fourth entry (with the pin sentence).
- **Equivalent mutants** (cannot change behaviour, found by running them):
  the entry's own destination-count check is redundant with the allowlist
  (a host or loopback together with `anyAddress` builds a rule the allowlist
  refuses); `user.name` accepted as the pin is stopped by the editor
  policy's digits-only `user.name` refusal; the choice of "named destination"
  detection by peeking for `dest.*` or for the port yields the same refusals.
  They stay as defence in depth.
- **What the end-to-end "installed" proves.** The mock daemon's responder
  (`lists.rs` `respond`) runs `validate_rule_shape` on every `CHANGE_RULE`
  and answers `ERROR` for a shape it would not compile, so an "installed"
  DNS entry means that mock accepted the four-condition rule. It is a mock of
  `Compile`, not the real daemon; r13 is the real check.
