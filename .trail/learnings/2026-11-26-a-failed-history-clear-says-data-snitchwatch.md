---
title: "A failed history clear says data is left, and recovery carries on past a linked leftover"
author: Snitchwatch
date: 2026-11-26
branch: fix/history-441-control-tombstone
id: l-05a31a45
scope: repo
status: active
src: agent
---

## Decided
- A clear failing after the rename writes the rows accepted after the preview into a new store, resumes recording if it was on, and reports data left, because a plain refusal reads as "nothing changed".
- The preview counts a leftover tombstone and confirm rechecks it, so no row is unlinked that the user's confirmation did not cover. A tombstone that cannot be scanned gets a retry token only when no store sits beside it.
- A message never claims recording is off unless that path turned it off.
## Tried and ruled out
- Issuing an unchecked token beside a live store: after a double failure it could unlink carried rows nobody counted.
- Letting the new linked-tombstone error propagate from startup recovery: it skipped the hot-journal rollback.
## Watch out
- A failure between unlinking the tombstone and inserting the carried rows loses those rows, so that path needs its own handling and never the generic undo.
- A link count read before an unlink can miss a hard link added in between by the same user.
In the code: history/control_clear.rs, control_preview.rs (scan_tombstone), control_files.rs (recover, FilesError::Linked); issue #441.
