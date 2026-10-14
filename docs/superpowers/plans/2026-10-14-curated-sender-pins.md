# Curated defaults: sender pins (#120 36/41) and follow-ups #155-#158

**Date:** 2026-10-14
**Source:** issue #120 owner decision (2026-10-14) on items 36/41; issues
#155 (#120 item 46), #156 (47), #157 (48), #158 (49).
**Baseline:** `main` @ `4ead981`, branch `fix/round2-curated-pins`.
**Size:** M/L. Bridge: `data/curated-defaults-v1.json`, `curated/mod.rs`,
`curated/canonical.rs`, `curated/reconcile.rs`, `curated/store.rs`,
`curated/manager*.rs`, `daemon_commands/curated_command.rs`; bridge-cli:
`lib.rs` (lookup wiring), `tests/curated_support`; Kirigami: the
Recommended page (#158).

Kept as they are: the #128 consent tickets, #132's PassKey/flush, #130/#149
refusal handling, item 12's undo, the DNS rule's exact shape (its broad
grant fingerprint hashes it: any change would re-prompt every user who
consented).

## Sender pins (#120 36/41)

A path alone doesn't name a sender: `CleanPath` falls back to argv[0] when
the exe link is unreadable, and any user can run a `/usr` binary with
`LD_PRELOAD`. Owner decision: pin the fixed-account entries by `user.id`
(not `user.name`: the daemon's `Compile` rewrites a `user.name` leaf to the
uid and saves it that way, so the reloaded rule fails, #121 review).

| entry | sender |
|---|---|
| `networkmanager-connectivity-check` | uid 0 |
| `tailscale-control`, `tailscale-logs` | uid 0 |
| `chronyd-ntp` | the `chrony` account, looked up at run time |
| `avahi-mdns` | the `avahi` account, looked up at run time |
| `chronyc-local` | path only: loopback port 323, run by any user on purpose (`chronyc tracking`) |
| `flatpak-flathub` | path only: run by the desktop user, whose uid sandboxed apps share |
| `dns-resolved` | unchanged: uid 193, confirmed against `systemd-resolve` (item 38) |

**Unconfirmed:** the accounts are Fedora's packaging names (`chrony`,
`avahi`; NetworkManager and tailscaled run as root). The tower still has to
confirm what these services run as on the Bazzite image. They live in one
place, each entry's `sender` in the data file.

- Data: an optional `sender`, `{"userId": N}` or `{"account": "name"}`.
  The DNS entry keeps its built-in pin and may not carry one.
- Rule shape: path, destination, `user.id`, port, protocol. That is the
  DNS rule's order already, so its leaves don't move. The allowlist
  (`check_curated_rule`) accepts an optional `user.id` leaf after a named or
  multicast destination: `simple`, case-insensitive, a canonical decimal
  uid. It stays required (193) for any address.
- A run-time account's uid isn't known to the pure code. `rule_as(uid)`
  builds the rule. `rule()` leaves such an entry's uid empty, which the send
  check refuses, so that form can never be sent. "Unedited" for such an
  entry means "this rule pinned to the uid the copy itself names"
  (`rule_matching`). The manager compares that uid with the lookup: equal
  means current, different means stale (below).

### Lookup (#155, #157)

- One blocking task looks up every account at once (`systemd-resolve`,
  `chrony`, `avahi`). Only one is in flight at a time: a pass waits up to
  5 s for a task it started. While a task is still running (a hung NSS), a
  pass reuses the last result without waiting and starts no new task. The
  task stores its result itself, and a result that differs from the last
  bumps an epoch and wakes the worker. The epoch is part of `PassKey`, so a
  late answer re-plans and an unchanged one doesn't loop.
- Per entry the result is three-way:
  - **unknown** (no answer, a timeout, no such account): the entry is
    withheld. Nothing is installed, upgraded or re-enabled for it, and a copy
    already there is left as it is (never deleted on a lookup).
  - **a definite mismatch** (#155a): `systemd-resolve` isn't 193, or an
    installed copy's uid isn't the account's. Withheld the same way and
    shown Unavailable, with a fixed "stale" reason that says how to remove
    it (turn it off). Not auto-deleted: owner rule from item 38, kept.
  - **confirmed**.
- Skipped while inert. The per-user bridge and every in-process GUI bridge
  install nothing, so they never read the host's accounts.
- #157: `CuratedDefaults::new` starts with no lookup at all, so every
  account reads as unknown (fail closed, no host read). Only `run_system`
  (production) sets the host's database. Setting a lookup bumps the epoch,
  so the first pass after it re-plans. `curated_support` and bridge-cli's
  `cfg(test)` fake gain `chrony`/`avahi`. No `RunOptions` field, so the ~15
  files that build one stay untouched.

### Upgrade of an installed unpinned copy

Opensnitchd's `replaceUserRule` replaces a rule by name in place, and keeps
the old one if the new one fails to compile. A `CHANGE_RULE` of the same
name therefore swaps the rule with no window where it is missing.

- `is_legacy(entry, rule)`: the copy is exactly the entry's rule as v1
  shipped it, without the sender leaf. It still counts as Snitchwatch's
  own: not EditedByYou, deleted when turned off, toggleable off on the
  Rules page. It is never used to install.
- Reconcile, for a legacy copy:
  - entry on: a new `Upgrade` action. Status Installed/InstalledButOff, as
    the copy is.
  - turned off: deleted, as now.
  - undecided (first run): left as it is (re-review M1), until the user
    turns it on (upgrade) or off (delete).
- `Upgrade` is applied only while the account is confirmed. The command is
  the pinned rule with the copy's `enabled` and `created`. No consent
  prompt: it only narrows a rule the user already chose.
- A refused or failed upgrade keeps the entry on with its old copy. It is
  recorded as a failure (no retry on the same daemon stream) and shown as a
  problem. #130's turn-off-and-delete is not used here: the old rule never
  left memory, and the user's choice is unchanged.
- Choices: a legacy copy isn't recorded (`valid_copy` only knows the pinned
  form). A file from the previous version drops the old record on load, as
  for any changed entry; `installed_ids` keeps the id.

## #156: reenable's files-left guard

Test: accept a DNS keep/re-enable intent, then set the persisted
`choices.files_left` note before the pass, and assert that no re-enable is
sent. Then mutate the guard in a private copy to show the test catches it.

## #158: Recommended page

- On but Unavailable (an unconfirmed or stale account): the switch stays on,
  and the page shows the entry's problem text rather than reading as a
  failure or as off.
- `install_refusals`: cleared by a Remove request (`ask_removal`) and when
  the daemon stream changes (each note keeps its generation).
- A QML probe step for each.

## Tests (written first, seen failing)

- mod/policy: the pinned shapes; the allowlist with and without the uid
  leaf; an account entry's empty-uid form refused; DNS rule bytes
  unchanged.
- reconcile: a legacy copy is upgraded when on, deleted when off, left
  alone when undecided; a legacy copy is never EditedByYou.
- manager:
  - an enabled legacy copy gets one CHANGE_RULE and no DELETE_RULE;
  - InstalledButOff stays off;
  - a refused upgrade leaves the entry on;
  - an unknown chrony lookup neither rewrites nor deletes, and off still
    deletes;
  - single-flight under a hung lookup;
  - a late result re-plans;
  - a DNS mismatch shows stale and doesn't delete;
  - a new manager reads no host account;
  - `install_refusals` cleared.

## Revision (2026-10-14, tower facts on the r13 image)

The tower read these from `ps` and `/proc` on r13:

| service | runs as | uid |
|---|---|---|
| NetworkManager | root | 0 |
| tailscaled | root | 0 |
| chronyd | `chrony` | 994 (dynamic) |
| avahi-daemon | `avahi` | 70 (static) |
| systemd-resolved | `systemd-resolve` | **990** (dynamic) |

`sysusers.d` declares 193 for `systemd-resolve`, but the image's
`/etc/passwd` has 990. The #121 pin to 193 therefore never matched on the
tower, and since #149 the DNS entry has shown Unavailable there.

### The DNS entry is pinned by account name

- DNS becomes an ordinary `{"account": "systemd-resolve"}` entry, and
  `DNS_USER_ID` is gone. The DNS entry is available when the account
  resolves, whatever uid it resolves to.
- The allowlist still requires a `user.id` leaf for any address, but takes
  any canonical uid. The data check requires an account sender on an
  any-address entry, so no path-only or uid-0 any-address entry can exist.
- DNS has no legacy form. A copy pinned to 193 is an unedited copy at a uid
  that has moved (below). A path-only DNS copy (item 40) is never counted
  as Snitchwatch's.
- Grant fingerprint: still `canonical(entry.rule())`. For an account entry
  that rule has an empty uid, so a grant binds to "the resolver's account",
  not to a uid. Every existing DNS grant (hashed with 193) drops once on
  load: users who consented re-consent once. `BROAD_CONSENT` is unchanged,
  for GUI/bridge version skew (item 37).

### A uid that moved (reshapes #155a)

A copy has moved when a confirmed lookup returns a different uid than the
uid the copy names. It is never detected on an unconfirmed lookup.

What happens next depends on one switch, `ON_UID_CHANGE` in
`curated/manager_resolver.rs`. It is a placeholder pending the owner's
decision:

- **A, `Rewrite` (current placeholder).** The upgrade path replaces the
  copy in place at the new uid, keeping `enabled` and `created`. There is
  no new consent: it is the same account, and changing passwd needs root.
  - For DNS, the rewrite goes through the guarded broad send, with #128's
    checks (live grant, `broad_revision`, saved, persistent storage).
  - With no live grant (after the fingerprint change, the common case),
    nothing is sent. The entry shows a fixed stale reason that points to
    Review/Keep, and a Keep ticket then allows exactly one rewrite.
- **B, `TurnOff`.** The grant is revoked and the choice turned off, with a
  note. The next pass deletes the copy. Under a deny default, DNS then
  stays blocked until the user consents again.
- Both: an undecided copy at a moved uid is left alone with the reason
  attached (M1). A refused rewrite isn't retried on the same stream.
- A moved copy is never turned on: its queued re-enables are dropped, and
  `send_managed_toggle` refuses turn-on. Turning off stays allowed. The old
  uid may now belong to another account.

### Paths

`ReadPath` is `readlink(/proc/<pid>/exe)`, which the kernel resolves. A
service started as `/usr/sbin/chronyd` (or tailscaled, NetworkManager,
avahi-daemon) therefore reaches the daemon as `/usr/bin/...`, because
`/usr/sbin` links to `bin`. The data paths already use `/usr/bin`.

The only way a `/usr/sbin` path can appear is `CleanPath`'s argv[0]
fallback (item 41). Such a path matches no curated rule, so it fails
closed.

### Owner decision (2026-10-14): A, as real behaviour

The `ON_UID_CHANGE` switch and option B are removed. When a confirmed
lookup finds a service account at a different uid than its copy names, the
copy is re-pinned to the new uid of the same account, with no new consent,
and the entry shows a short fixed note.

- An unconfirmed lookup (None, a timeout) never rewrites and never deletes.
  A None between two confirmed answers changes nothing.
- The rewrite is atomic: one `CHANGE_RULE` of the same name, which
  `replaceUserRule` swaps in memory. The rule is never missing, and both
  uids are allowed for at most that one command.
- DNS keeps its grant across the move. Grants bind to the account, so a
  grant still matches after a uid change. A grant saved before this change
  (hashed with the old fixed uid) is carried over on load when it matches
  the copy recorded as installed for the entry: that is the grant for "this
  account at the uid it was installed with". It is then re-stamped with the
  account form. Without any grant, an enabled DNS copy isn't rewritten, and
  the entry asks the user to confirm.
- A refused rewrite takes #130/#149's refused-install path: the entry is
  turned off, its grant revoked, the name tracked as maybe applied, and a
  note shown. The next pass deletes the copy, because the old uid may now
  be another account's. A rewrite that fails without a refusal isn't
  retried on the same stream. Either way, at most one rewrite is sent per
  confirmed change.
- A copy from before the sender pin (legacy upgrade) keeps its own refusal
  rule: the entry stays on with the old copy, because that copy is the
  status quo the user chose.

## PR #161 security review, round 1

- **M1.** An account's uid is accepted only when it is a system uid,
  `1..=SYS_UID_MAX`. `SYS_UID_MAX` is 999, Fedora's `login.defs`, which
  leaves out root, `nobody` and every person's uid.
  - It must also pass a reverse lookup. `getpwuid_r(uid)` must name the
    same account, checked in the same single-flight task. A mismatch or a
    failure counts as unconfirmed.
  - The allowlist refuses `user.id` 0 for any address.
  - Production passes both lookups (`set_account_lookups`). Tests' forward
    fakes derive the reverse from the account list.
- **L1.** `Guard::Replace` never turns on a copy that is off now.
- **L2.** A Rules-page turn-on is refused while the entry is withheld, as
  well as when it has moved. Turning off is allowed.
- **#157's `run_system` wiring** is caught by
  `run_system_sets_the_hosts_account_lookup`. The test reads the source,
  because `run_system` needs socket activation.

### Surviving mutants, and why they are equivalent

- **(a) `track_maybe_applied` for a refused rewrite.** A refused rewrite
  turns the entry off. The next pass deletes the name whatever the cache
  shows, because a disabled entry's copy is listed and unedited at either
  uid. A rewrite that did apply unlisted is at the account's own new uid,
  so it is harmless until that delete.
- **(b) The unknown-account filter in `withheld()`, against the
  transient-None test only.** It is one of three layers:
  - `withhold()` drops `Upgrade`/`Install` for a withheld entry;
  - `upgrade()` returns early for a withheld entry;
  - the send check refuses an empty `user.id`.

  The earlier `an_unknown_account_leaves_an_old_copy_as_it_is` kills the
  same mutant.
