---
title: "Layered packages are matched by their parsed name, and only a name-version-release shape is split"
author: Snitchwatch
date: 2026-11-20
branch: chore/drift-plan-and-bundle
cost: { minutes: 1, tokens: 1283 }
id: l-074686e0
scope: repo
status: active
src: agent
touches:
  - crates/scanner-core/src/deployment.rs
---

## Decided
A layered-package entry is split into name/version/release only when its last two hyphen segments both start with a digit. Anything else is a bare name and is matched whole.

## Tried and ruled out
- Prefix matching: layered `python3-foo` makes `python3` look layered.
- Always dropping the last two hyphen segments: bare names like `libfoo-2-devel` or `xorg-x11-server` reduce to `libfoo` / `xorg`, the same false-positive class as prefix matching.

## Watch out
The format `rpm-ostree status --json` uses for layered entries (bare name vs versioned) is not confirmed on a live host. Check it on a real host before relying on this for file_drift.

In the code: crates/scanner-core/src/deployment.rs (`has_layered_package`, `nevra_name`), tests `layered_match_uses_nevra_name_not_prefix`, `hyphenated_bare_names_are_not_truncated`.
