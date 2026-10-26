---
title: Rebuilding the SYSTEM overlay and GUI after a merge
author: Snitchwatch
date: 2026-10-26
branch: main
cost: { minutes: 31, tokens: 335446 }
touches:
  - ".agent_native/agent_roadmap.md"
  - IMPLEMENTATION_PROMPT.md
  - README.md
  - crates/snitchwatch-kirigami/qml/RuleSimulatorSheet.qml
  - docs/packaging/bridge-release-artifact.md
  - packaging/README.md
  - packaging/bluebuild/recipe.yml
  - packaging/system/snitchwatch-system-bridge.service
  - crates/snitchwatch-bridge/src/ws_messages.rs
  - crates/snitchwatch-bridge/tests/system_package_contract.rs
---

## Decided
After a packaging change merges, rebuild from the merged main SHA by copying the reviewed one-shot wrappers (`dist/<build>/rebuild-local.py`, `dist/<build>/bridge/rebuild-bridge-local.py`) into a fresh `dist/<new>/` and changing only the location check and the bundle name. They refuse existing outputs. Run them on the host via `host-spawn` (the agent shell is a toolbox container: no rpm-ostree, and no host /run or /etc).
## Tried and ruled out
Assuming the bridge ELF stays byte-identical when only doc comments and `#[cfg(test)]` change: it doesn't. Doc-comment edits change the crate hash, so LLVM `.llvm.N` and mangled `h<16hex>` suffixes differ (the size moved by 552 bytes). Verified by diffing `strings` output with those suffixes filtered: no other differences. The binary also does not embed the git SHA.
## Watch out
The SYSTEM overlay tarball carries `etc/opensnitchd/default-config.json`. Any DefaultAction or config change means the old overlay is superseded. Check the config inside the tarball before handing it to the tower.
