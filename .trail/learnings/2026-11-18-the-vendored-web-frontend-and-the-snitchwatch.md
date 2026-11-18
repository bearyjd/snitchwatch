---
title: "The vendored web frontend and the bridge's static routes are gone; the bridge serves only /stream"
author: Snitchwatch
date: 2026-11-18
branch: chore/379-remove-web-frontend
cost: { minutes: 183, tokens: 712580 }
id: l-d1413a94
scope: repo
status: active
src: agent
touches:
  - crates/snitchwatch-bridge/src/ws_server.rs
  - packaging/release/bridge_artifact.py
---

Retired in #379 PR 2. web/ could only run behind Tauri's token-injecting TCP proxy (removed in PR 1), so it had no consumer. The bridge now serves only /stream; / and /assets/* are 404 (test only_stream_is_served). Kept on purpose: legacy 'remember' wire compat, --no-default-features in RELEASE_PACKAGE_ARGS (a no-op now, but removing it changes BUILD_COMMAND and the MANIFEST), and check_no_web_ui plus the pack_layout.rs marker test as a guard against re-vendoring GPL-2.0-only code. The app icon moved to packaging/icons/; packaging_shape now asserts every Flatpak install source exists, because the icon path was the one shipped dependency on web/. just doctor was removed since its only check was web_smoke.
