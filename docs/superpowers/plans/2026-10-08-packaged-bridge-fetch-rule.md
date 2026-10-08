# Packaged allow rule for the system bridge's blocklist downloads

**Date:** 2026-10-08
**Baseline:** `main` @ `0f9fcae`.
**Owner decision (2026-10-08):** ship **one** narrow opensnitchd allow rule
so the system bridge's own blocklist downloads work on a deny-by-default
system. It allows only `/usr/bin/snitchwatch-bridge-cli` running as the
`snitchwatch` account to open HTTPS connections (TCP, port 443).
**Closes:** the first item of "Risks and open questions" in
`2026-10-07-blocklist-enforcement.md`.
**Size:** S. One branch: one JSON file, the stager, one policy guard, tests
and docs.

## Citation convention

`vendor:` means opensnitch v1.8.0 (`vendor/opensnitch`). No Go toolchain is
available here, so the daemon's behaviour below comes from reading the
vendored source. The Rust tests act as proxies for it; they do not run the
daemon.

## The rule

`packaging/bluebuild/files/system/etc/opensnitchd/rules/000-snitchwatch-bridge-fetch.json`
(mode 0644, root):

- `list` operator with exactly four ANDed members:
  - `process.path` `simple`, `sensitive: true`, `/usr/bin/snitchwatch-bridge-cli`;
  - `user.name` `simple`, `snitchwatch`;
  - `dest.port` `simple`, `443`;
  - `protocol` `regexp`, `^tcp6?$`.
- `action: allow`, `duration: always`, `enabled: true`,
  `precedence: false`, `nolog: false` (the fetches stay in the log).
- `created`/`updated` are RFC3339, so `Serialize` doesn't warn on every
  Subscribe (`vendor:daemon/rule/rule.go`).

### Why each part

- **Protocol.** `parseDirection` names IPv6 TCP `tcp6`
  (`vendor:daemon/conman/connection.go`, `"tcp" + protoType`). A `simple`
  `tcp` would miss every IPv6 fetch. List members can only be ANDed, so one
  anchored regexp covers both. UDP 443 (QUIC) is not allowed.
- **Precedence false (owner rule).** `FindFirstMatch`
  (`vendor:daemon/rule/loader.go`) returns at the first matching
  deny/reject, or at a precedence rule. A non-precedence allow is only kept
  as the match while scanning goes on. So any user deny or blocklist rule
  (`z00-blocklist:…`) that matches the list's host still blocks the fetch.
  The refresh then fails and the list's status shows the fetch error, as it
  does today.
- **Name and sort order.** `sortRules` evaluates enabled rules in
  `sort.Strings` order. With `precedence: false` this rule's place in that
  order changes nothing: a matching deny anywhere still wins, and a later
  matching allow is still an allow. The `000-` prefix lists it next to
  upstream's own `000-allow-localhost` rules. It passes
  `rule_name::validate_rule_name`. The file name is `name + ".json"`, as
  the loader assumes when deleting (`deleteRule`, `deleteRuleFromDisk`).
- **Reserved prefix `000-snitchwatch-`** (added after the #89 import/export
  security review).
  - `rule_name::PACKAGED_RULE_NAME_PREFIX` sits next to `z00-blocklist:`
    and `900-blocklist:`. No GUI action or import may add, change, rename
    or delete a rule under it.
  - Otherwise an imported file could swap the allow for a deny, which stops
    list downloads. It could also make the rule `until restart`, and the
    daemon then deletes its file.
  - The refusals:
    - `rule_notification` refuses Add, Update, a rename in either direction,
      and Delete.
    - `DaemonCommands::send` refuses any command.
    - The Rules page lists the rule read-only with fixed text, "Built into
      Snitchwatch: lets its background service download blocklists.", and
      `deletable: false`.
  - The prefix lies outside the bands the bridge manages and purges
    (`z00-`/`900-blocklist:`, `850-profile:`, `snitchwatch-default-`).
- **Ports.** Only 443 is covered. A list URL with another port
  (`https://host:8443/…`) still needs a user allow.
- **DNS is not covered.** `GuardedResolver` uses `getaddrinfo`
  (`tokio::net::lookup_host`). On Fedora/Bazzite that goes through
  nss-resolve over a Unix socket. The upstream queries are
  systemd-resolved's own traffic, as for every other app on a
  deny-by-default system. Not verified live.

## The user condition: `user.name`, not a fixed `user.id`

`Compile` resolves a `simple` `user.name` to a uid with `user.Lookup`
(`vendor:daemon/rule/operator.go`). On failure `loadRule` returns the error
before storing the rule, and `Load` logs a warning and goes on to the next
file. So **a missing account means the rule is skipped**, never broadened,
and the daemon keeps running with the rest of its rules.

- **Boot order.** `systemd-sysusers.service` has `DefaultDependencies=no`
  and `Before=sysinit.target`. The RPM's `opensnitch.service` has default
  dependencies, so it starts after `sysinit.target`. On any boot where
  sysusers creates the account, it exists before the daemon compiles its
  rules.
- **No account, no bridge.** If sysusers never creates `snitchwatch`,
  `User=snitchwatch` stops the system bridge from starting. A skipped rule
  then has nothing to allow.
- **Lookup source.** The daemon links cgo (`netfilter`, `dns/ebpfhook`), and
  the RPM spec builds without `osusergo`. `user.Lookup` therefore goes
  through NSS, which covers `/etc/passwd` and nss-altfiles' `/usr/lib/passwd`.
- **Live install window.** Live reload re-reads a rule only on a write. If
  the rule file lands before the account exists, restart
  `opensnitch.service` after the account is created.
- **Why not a fixed uid.** A uid in `sysusers.d` is only a suggestion:
  systemd-sysusers allocates another one when it is taken ("Suggested user
  ID %u for %s already used"). An existing `snitchwatch` account keeps its
  uid too. A `user.id` rule could then silently name a different account.
  `user.name` always means the account that actually exists.
- **The user condition is load-bearing.** The release tarball installs the
  legacy *user* bridge at the same `/usr/bin/snitchwatch-bridge-cli`. Without
  this condition the rule would allow every desktop user's bridge as well.

### Consequence for the GUI (new guard)

`Compile` overwrites the leaf's `Data` with the uid, and `Serialize` reports
it that way. So the GUI sees `user.name = <uid>`.

- Before this change, a GUI toggle sent that back.
  - Disabling it, the daemon doesn't compile disabled rules, so it saved
    `user.name: "<uid>"` over the shipped file.
  - Re-enabling it, `user.Lookup("<uid>")` failed and the rule stayed dead.
  - On rpm-ostree that local `/etc` change also survives image updates.
- **Guard:** `rule_policy::validate_leaf` refuses a `simple` `user.name`
  whose data is all digits.
  - Such a rule is listed read-only for its conditions
    (`SHAPE_READ_ONLY_REASON`) and stays deletable.
  - This applies to every daemon `user.name` rule, including stock-UI ones:
    once enabled, the daemon reports all of them with a uid.
  - Nothing the bridge builds uses `user.name`.
  - The packaged rule is refused earlier, by its reserved name. The guard
    covers every other `user.name` rule.

## Shipping

- **bluebuild:** the `files` module already copies `files/system/` to `/`.
  This file is the single source of truth.
  - Today's image runs the legacy user bridge and has no `snitchwatch`
    account. There the daemon logs an unknown-user warning and the rule is
    inert, which is harmless.
- **System-bridge overlay:** `packaging/system/stage.sh` installs the same
  file to `$DEST/etc/opensnitchd/rules/` at 0644.
- **rpm-ostree layering doc:** that path installs the user bridge, which
  this rule deliberately doesn't match. A short note says to install the
  rule only alongside the system bridge.
- **Release tarball:** unchanged. It carries the user bridge and its user
  unit, not the system account.
- **Flatpak:** ships no daemon rules.

## Bridge and Rules page

- **Blocklist reconcile and orphan purge.** These only ever select names
  under the blocklist prefixes (`is_reserved_blocklist_name`, which
  excludes the packaged prefix). They also require a rule the bridge made
  (`made_by_bridge`). And every `BlocklistCommand` delete refuses any other
  name.
- **#44 "applies to every app".** This needs the interactive-verdict
  description and no `process.path` anywhere. The rule has neither, so it
  is never flagged.

## Tests

- `tests/packaging_shape.rs` (run by `just package-check`, which also
  `json.load`s the file):
  - exact shape and file name;
  - the name passes, sits under the packaged prefix, and isn't in a
    blocklist band;
  - it is read-only with fixed text and not deletable;
  - the operator passes `validate_operator` on disk and is refused once
    compiled;
  - a small evaluator that mirrors `operator.go` matches only the system
    bridge's TCP/TCP6 443. Each condition has a connection that differs
    only there and must not match.
- Bridge unit tests:
  - `rule_wire`: the disk and compiled forms go through `rule_to_wire`.
    Both get the fixed read-only text and `deletable: false`.
    `notification_for_effect` refuses the row when a GUI echoes it back;
  - `rule_policy`: the numeric `user.name` guard, and packaged rules being
    read-only and not deletable;
  - `rule_name`: what the prefix does and doesn't reserve;
  - `rule_notification`: Add, Update, both renames and Delete are refused;
  - `send_policy_tests`: `DaemonCommands::send` refuses change and delete;
  - `daemon_sink_tests`: the orphan purge leaves the rule alone.
- Kirigami `rules/all_apps.rs`: the compiled rule goes through
  `RulesCache` → `snapshot_wire` → `SetRules` → `RulesStore`, in both
  forms. It is not flagged, is user-sourced, is read-only with the fixed
  text, is not deletable, and gets no toggle.
- Stager test: the rule is staged at 0644 with the canonical bytes.
- **Mutation check:**
  - drop each condition;
  - change `^tcp6?$` to `tcp`;
  - set `precedence` to true;
  - set the path's `sensitive` to false;
  - remove the guard arm;
  - remove each reserved-name refusal.

  Each mutation must fail a test.

## Verification on a real host (runbook)

After staging the overlay and booting, these show the rule loaded and
compiled:

- `sudo grep 'Error compiling list rule' /var/log/opensnitchd.log` (the shipped config sets `LogFile: /var/log/opensnitchd.log`, so the journal won't show it) finds nothing
  for `000-snitchwatch-bridge-fetch`;
- the Rules page lists the rule.

Then subscribe to a list with no GUI attached, and confirm the refresh
succeeds.
