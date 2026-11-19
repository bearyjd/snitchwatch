---
title: The onboarding page no longer offers to start the daemon because the GUI cannot fix an unreachable bridge
author: Snitchwatch
date: 2026-11-19
branch: fix/384-onboarding-no-service-control
cost: { minutes: 257, tokens: 2031670 }
id: l-5c8eebda
scope: repo
status: active
src: agent
touches:
  - crates/snitchwatch-kirigami/qml/OnboardingPage.qml
  - crates/snitchwatch-kirigami/src/wizard.rs
---

Issue #384: the wizard's Start daemon button ran systemctl --system start opensnitch.service, which does not start the socket-activated bridge and cannot run in the shipped Flatpak (no systemd or flatpak-spawn access). The only real failure state is unreachableRetrying (no authenticated bridge connection); its causes (missing socket, not in snitchwatch-ui, crash loop) need host root. So the action was removed, not retargeted, and the page now says so and points at docs/packaging/system-bridge-integration.md. UnitMissing/UnitInactive had no producer and were deleted; the which dependency went with them. Guard: crates/snitchwatch-kirigami/tests/onboarding_no_service_control.rs.
