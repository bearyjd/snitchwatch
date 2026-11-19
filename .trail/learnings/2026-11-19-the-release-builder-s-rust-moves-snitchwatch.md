---
title: "The release builder's Rust moves under the digest-pinned image, so the pin check trips and SW_RUST_VERSION must be bumped"
author: Snitchwatch
date: 2026-11-19
branch: chore/pins-rust-1.100.0
cost: { minutes: 326, tokens: 2255153 }
id: l-74ccf47f
scope: repo
status: active
replaces: l-3b8873
src: agent
touches:
  - README.md
  - docs/ENV.md
  - packaging/release/pins.env
  - docs/superpowers/plans/2026-11-18-followups-384-385-386.md
  - HANDOFF.md
  - ".github/workflows/ci.yml"
  - crates/snitchwatch-bridge/src/ws_server.rs
  - crates/snitchwatch-bridge/src/ws_server/tests.rs
  - crates/snitchwatch-bridge/tests/packaging_shape/main.rs
  - crates/snitchwatch-bridge/tests/release_artifact/pack_layout.rs
---

## Decided
The builder image is digest-pinned but its rust comes from Fedora's live updates repo, so a Fedora rust update fails the build on purpose. Bump SW_RUST_VERSION (now 1.100.0) and the two doc mentions, rebuild from a clean clone, run release-bridge-repro and release-verify, then send the tower the merged SHA (it re-pins with its own tool).

## Tried and ruled out
Bumping the image digest: not needed, only the rust RPM moved. Treating the failure as a bug in the change being tested: it is the drift check working.

## Watch out
A new toolchain changes the reproducible binary hash, so a release candidate needs a rebuild and re-review. protoc 33.5 had not drifted at this bump; check it if the next failure names it.
In the code: packaging/release/pins.env, packaging/release/build-bridge.sh (the check), docs/ENV.md.
