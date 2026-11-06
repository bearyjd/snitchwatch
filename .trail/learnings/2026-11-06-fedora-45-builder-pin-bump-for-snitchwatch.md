---
title: Fedora 45 builder pin bump for the bridge release (pins.env)
author: Snitchwatch
date: 2026-11-06
branch: chore/pins-env-fedora45
touches:
  - packaging/release/pins.env
---

## Decided
- Bumped packaging/release/pins.env to fedora:45@sha256:ea0998f4... (amd64 manifest, same as the tower's), rust 1.99.1, protoc 33.5, and the "Fedora 44" mentions in docs, justfile, release.yml and the two release tests. Work is on branch chore/pins-env-fedora45 in a worktree off origin/main, because local main was 120 commits behind.
- ui.pb.go and ui_grpc.pb.go are not in this repo or in vendor/opensnitch. The tower's daemon stage generates them, so nothing to re-pin here.

## Tried and ruled out
- `git submodule update` in a new worktree fails unless run with `-c protocol.file.allow=always`.
- No outcome yet for `just release-bridge-repro` under Fedora 45; the `fc44` NEVRA fixtures in release_artifact/main.rs are not checked yet.

## Watch out
- Do not cut, promote or publish r16. The builder change alters the reproducible binary hash, so the r16 candidate needs a rebuild and re-review.
- After merge, the tower re-pins with ci/repin-snitchwatch.py in its own session, so send it the merged SHA.
