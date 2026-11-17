---
title: TCP gRPC is dev-only behind an env flag; the per-user bridge falls back to a Unix socket and the per-user unit is dropped
author: Snitchwatch
date: 2026-11-17
branch: feat/35-tcp-dev-only
cost: { minutes: 21, tokens: 27439 }
id: l-ee26dca9
scope: repo
status: active
src: agent
touches:
  - docs/superpowers/plans/2026-11-16-35-tcp-dev-only.md
  - crates/snitchwatch-bridge-cli/src/config.rs
---

## Decided
Owner answers 2026-11-16 for #35: without the dev flag the bridge uses a per-user Unix socket (it does not refuse to start). The per-user systemd unit is dropped and the system bridge is the only shipped flavour. The bluebuild recipe config uses the root-only unix address. The Tauri and Kirigami dev flow and the spike crate are gated too. Tauri refuses to start without the flag, because its webview proxy hands the WS token to any local process. The release tarball ships the binary and manifest only. The DIY layering path is retired and the per-user Flatpak profile is dropped.
The per-user socket has no packaged consumer: opensnitchd has one system-wide Server.Address and cannot dial a per-uid path. It is a dev/test default that never listens on TCP.
A stray SNITCHWATCH_GRPC_BIND without the flag is a hard error, so an old unit cannot silently fall back to the Unix default while its daemon dials TCP.

## Tried and ruled out
Refusing to start without the flag: leaves the per-user flavour unusable. A new UnixUser transport variant: no behaviour would differ. Putting the per-user socket under the Flatpak-granted snitchwatch directory: a sandboxed same-uid GUI could pose as the daemon, so it goes in a sibling snitchwatch-grpc directory. The /tmp fallback for XDG_RUNTIME_DIR: it is the squatting problem again.

## Watch out
The earlier count of 29 test files using TCP was really 46 files (7 production or packaging). just run-spike was already broken: it passes a URL the spike rejects. Kirigami never embeds a bridge, only its tests are affected, and they are outside default-members.
In the code: docs/superpowers/plans/2026-11-16-35-tcp-dev-only.md, flag SNITCHWATCH_DEV_TCP=1, crates/snitchwatch-bridge-cli/src/config.rs.
