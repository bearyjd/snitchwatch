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
