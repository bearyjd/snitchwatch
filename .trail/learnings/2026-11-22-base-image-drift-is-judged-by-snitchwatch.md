---
title: "Base-image drift is judged by comparing each flagged file's OSTree object checksum, not by fsck or a tree diff"
author: Snitchwatch
date: 2026-11-22
branch: docs/handoff-2026-11-21
cost: { minutes: 147, tokens: 1784799 }
id: l-0af9617e
scope: repo
status: active
src: agent
touches:
  - crates/snitchwatch-bridge/src/history/caps_tests.rs
  - crates/snitchwatch-bridge/src/history/durable.rs
  - crates/snitchwatch-bridge/src/history/identity_dormant_tests.rs
  - crates/snitchwatch-bridge/src/history/record.rs
  - crates/snitchwatch-bridge/src/history/rule_identity.rs
  - crates/snitchwatch-bridge/src/history/rule_identity_fuzz_tests.rs
  - crates/snitchwatch-bridge/src/history/rule_identity_tests.rs
  - crates/snitchwatch-bridge/src/history/snapshots.rs
  - crates/snitchwatch-bridge/src/history/snapshots_tests.rs
  - crates/snitchwatch-bridge/src/history/writer.rs
---

## Decided
For rpm-flagged base files, expected = the checksum OSTree records for that path in the booted deployment's commit, actual = the checksum OSTree computes for the live file. Equal means the change is part of the image and expected. Files and symlinks use the first checksum field, directories the second (the directory-metadata one).

## Tried and ruled out
- `ostree fsck`: stops at the first corrupt object, so it cannot list them, reports checksums not paths, and a full pass over a real image takes many minutes. On a healthy stock host it still fails on one legitimate file whose extended attributes differ.
- `ostree diff` of the commit against `/usr` compares the wrong subtree; against the deployment directory it is slow and noisy.
- A cached full-hash baseline: it means an in-process walk of the whole tree in the privileged tier and reimplements what OSTree already tracks.

## Watch out
- `rpm -Va` on an ostree image reports thousands of lines by design: `/var` and `/boot` are not part of the image, and the image build changes files after rpm installs them. Neither is tampering.
- `ostree checksum --ignore-xattrs` does not equal the recorded value; extended attributes are part of the checksum.
- Reading a root-only file as an unprivileged user fails the checksum; do not read that as a mismatch.

In the code: crates/scanner-privileged/src/checks/file_drift.rs and base_tree.rs, plan docs/superpowers/plans/2026-11-21-file-drift-base-tree-primitive.md.
