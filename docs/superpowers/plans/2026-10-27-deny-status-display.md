# Deny-status display (option B)

Status: approved in the overnight plan (`2026-10-27-overnight-autonomous-run.md`,
item 5). GUI-only (`crates/snitchwatch-kirigami`): no bridge, wire, daemon or
image change, and no new Flatpak permission. Contract: bazzite-tower #138.

## Contract (bazzite-tower #138)
- `/run/snitchwatch/deny-status.json`, root-owned 0644 regular file in a
  `root:root 0711` directory, written atomically by the tower's root helper.
- Fields: `schema` (1), `profile`, `defaultAction`, `optIn`, `pending`,
  `expiresAt` (ISO UTC or null), `canEnable`, `blockers` (strings), `expired`,
  `updatedAt` (ISO UTC, write time), `bootId`.
- Consumer rules: a missing file is unknown; a `bootId` other than this boot's
  (`/proc/sys/kernel/random/boot_id`) is unknown; `pending: false` or
  `expired: true` ends the trial; compute expiry from `expiresAt` against the
  clock rather than trusting `expired`. Advisory display data only.
- Commands: `ujust snitchwatch-deny on [minutes]` (1-60, default 10),
  `confirm`, `off`, `status`.
- Flatpak: `packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml` already
  has `--filesystem=/run/snitchwatch:ro`. The per-user manifest doesn't, so the
  display reads "unknown" there, which is honest.

## States shown
| State | When |
|---|---|
| Unknown | file missing, not a regular file, a symlink, over 16 KiB, unreadable, not JSON, any field missing (`expiresAt` and `bootId` may be null, not absent) or mistyped, `schema` other than 1, no current boot id, `bootId` null or not this boot's, `updatedAt` more than 2 min after this clock, or a pending trial whose deadline the helper couldn't have written (more than 60 min plus 2 min slack after `updatedAt`; `expired: false` but the deadline not after `updatedAt`; `expired: true` but the deadline more than 2 min after `updatedAt`) |
| Allow | `defaultAction: allow`, no trial; blockers listed when `canEnable` is false |
| Deny trial | `pending: true`, `expired: false`, now before `expiresAt`: "reverts in M min S s (at HH:MM)" |
| Revert overdue | `pending: true` and now at or past `expiresAt`, or `expired: true` while still pending (the helper republished after a failed revert; `updatedAt` is then after `expiresAt`) |
| Deny confirmed | `defaultAction: deny`, `optIn: true`, no trial |
| Deny, not confirmed | `defaultAction: deny` without `optIn` and no trial |
| Other | any other `defaultAction` value |

Every known state carries "as of HH:MM" from `updatedAt`. Unknown fields are
ignored. Blocker text is daemon/config content, shown as plain text.

Checked against the helper source (bazzite-tower #138,
`bazzite-tower-snitchwatch-deny`): every time is `strftime("%Y-%m-%dT%H:%M:%SZ")`
in UTC (whole seconds, "Z", no offset or fraction), so the parser accepts only
that. `expired` is computed at write time as now > `expiresAt` while pending.
A successful revert rewrites the snapshot with `pending: false`; a failed one
leaves the trial pending, and a later `publish-status` (the boot service or a
manual run) rewrites it with `expired: true` and an `updatedAt` after the
deadline.

## UI
A small section on the Daemon Health page, re-read once a second only while
it and its window are shown, injected from `main.qml` (so tests
that load the page never read `/run`): the state line, the countdown or
overdue line, "as of", the blockers, and the four `ujust` commands, each with
a Copy button. No action buttons, no privilege, no process spawning.

## Tests (fixture files only, paths injected)
Pure unit tests per state, including a stale `bootId`, a missing file,
garbage, an oversize file, a symlink, a directory, an unknown schema, unknown
fields, a trial countdown, and an overdue revert. A QML probe loads the
section with fixture paths and checks the text and that only copy buttons
exist. The real helper and the real `/run` path are never touched.

## Not verified here
Reading the file from inside the Flatpak on a real boot: that waits for the
next image that ships #138.
