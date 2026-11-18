---
title: The Tauri shell is retired; Kirigami is the only shell; the dev TCP flag stays because the tests use DevTcp
author: Snitchwatch
date: 2026-11-18
branch: chore/379-remove-tauri
cost: { minutes: 17, tokens: 40724 }
id: l-c5fee96d
scope: repo
status: active
src: agent
touches:
  - ".github/workflows/ci.yml"
  - ".github/workflows/release.yml"
  - justfile
  - packaging/release/audit_gate.py
  - packaging/release/bridge_artifact.py
  - Cargo.lock
  - Cargo.toml
  - crates/snitchwatch-bridge/tests/packaging_shape/main.rs
---

Removed crates/snitchwatch-tauri and tests/tauri_smoke (#379 PR 1) because Kirigami is the only GUI shell and runs no bridge of its own.
SNITCHWATCH_DEV_TCP stays because about 37 test files use GrpcListen::DevTcp and the spike needs it; moving them to connect_unix is a separate migration.
Cargo.lock was regenerated with plain cargo check, not cargo update, because update would bump surviving packages. Added lines in the diff are only dependency names losing their version qualifier because a duplicate crate version vanished; parsing both lockfiles showed no surviving package changed version or source.
The CI apt trim was safe because cargo tree -e normal,build showed no webkit2gtk, gtk, soup or javascriptcore crate.
The quick-xml audit ignores stay because quick-xml 0.37.5 via tauri-winrt-notification under notify-rust still sits in the Kirigami lock.
