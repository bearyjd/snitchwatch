# Agent-Native Roadmap — Snitchwatch

Audit date: 2026-07-07. This repo is **original work**, not a vendored
upstream clone: `vendor/opensnitch` is a pinned git submodule (v1.8.0,
read-only reference for protocol/config shape) and `Cargo.toml` shows the
actual product is a 10-crate Rust workspace ("Snitchwatch") that bridges
opensnitchd's gRPC protocol to a Little-Snitch-style WebSocket UI, plus two
unrelated scanner crates (`scanner-core`, `scanner-privileged`) for a
separate "Bazzite security scanner" component. It is pre-alpha, already has
~3,000+ lines of Rust tests, an in-process mock-daemon test harness, and a
mature `docs/superpowers/{specs,plans}/` paper trail of prior design
decisions. No `.github/` CI and no `CLAUDE.md` existed before this audit.

Items are ranked by **Human-Attention-Saved per Unit of Effort**. Top 5 are
immediately actionable with no further human input needed.

## Top 5 — do these first

1. **Write `CLAUDE.md`** (done as part of this audit — see repo root). Cost:
   one read-through. Payoff: every future agent session stops re-deriving
   the default-members/Qt6 quirk, the plan-doc convention, and the three
   already-settled architecture decisions (GUI stack, socket transport,
   fail-open→deny) that are otherwise buried across `AUDIT.md`/`HANDOFF.md`/
   `IMPLEMENTATION_PROMPT.md`. Highest ratio in this list — pure transcription
   of decisions already made, zero new judgment required.

2. **DONE — Add a CI workflow (`.github/workflows/ci.yml`)** running
   `cargo check`/`cargo clippy --all-targets -- -D warnings`/`cargo test`,
   scoped to `default-members` (no Qt6 needed) on `ubuntu-latest`, with the
   Tauri Linux build deps (`libwebkit2gtk-4.1-dev` etc.) installed since
   `snitchwatch-tauri` is a default member. Deliberately does **not** shell
   out to `just check`/`just test` verbatim, because those pass `--workspace`
   and would pull in `kirigami-spike`/`snitchwatch-kirigami`, which need
   Qt6/KF6 dev packages not provisioned on the runner and whose CI behavior
   is unverified (see item 6) — this is called out in a comment at the top
   of the workflow. YAML syntax validated locally
   (`python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"`)
   and the underlying commands (`cargo check --locked`,
   `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked`)
   were run locally and pass; the workflow itself has not been run through
   actual GitHub Actions from this environment.

3. **DONE — Document (and script) the one-time Playwright setup** for
   `tests/web_smoke` and `tests/tauri_smoke`. Added a "Playwright smoke
   suites (one-time setup)" section to `README.md` pointing at
   `just web-smoke-install` / `just tauri-smoke-install`, plus a new
   `just doctor` target that checks for `tests/web_smoke/node_modules` and
   `tests/tauri_smoke/node_modules` and prints a fix hint (exit 1) if
   missing. Verified locally: `just doctor` correctly reported
   `tests/web_smoke/node_modules` present and `tests/tauri_smoke/node_modules`
   missing in this environment (exit code 1 with the install hint).

4. **DONE — Note the `vendor/opensnitch` submodule's expected dirtiness.**
   The one-sentence note in CLAUDE.md was already in place. Additionally
   excluded the stray `.omc/` path via
   `.git/modules/vendor/opensnitch/info/exclude` — a local-only, uncommitted
   git config file (never pushed/shared), so this doesn't touch the tracked
   `vendor/opensnitch` working tree or its own `.gitignore` (still out of
   scope, per the original note). Verified: `git status --short` no longer
   lists `vendor/opensnitch` at all in this checkout.

5. **Point future agents at the existing test-double pattern
   (`SystemInspector`/`MockInspector` in `scanner-core/src/testkit.rs`,
   `SystemFacts`/fake in `scanner-privileged/tests/end_to_end.rs`, and
   `MockOpensnitchd` in `tests/mock_opensnitchd`) as the mandatory
   reproduction path for anything touching real system state or a real
   daemon.** This already exists and is well-built — the gap is discoverability,
   not missing infrastructure. Effort: a short "Reproduction paths" section
   in CLAUDE.md (done). Payoff: stops a future agent from reaching for `sudo`,
   a real opensnitchd, or a live Bazzite host to reproduce a scanner/bridge
   bug when a deterministic in-process double already does the job in
   milliseconds.

## Further out (real effort, defer until above lands)

6. **QML/Kirigami test flakiness risk is unverified at scale.** The
   `snitchwatch-kirigami` crate's tests require system Qt6 + Kirigami dev
   packages and `QT_QPA_PLATFORM=offscreen`; they were not run as part of
   this audit (no long builds, and Qt6 dev packages are not confirmed
   present in this environment). Before leaning on an agent to iterate on
   Kirigami UI bugs autonomously, someone should verify these tests actually
   pass headless in the target CI/agent sandbox image, not just that they
   compile. Effort: medium (needs a Qt6-provisioned environment). Payoff:
   large if Kirigami work picks up, since Phase 3's whole rewrite lives here.

7. **No raw daemon-traffic recording/replay fixture yet.** The bridge's
   reproduction path (mock daemon + WS client) is script-driven and
   excellent for *known* protocol sequences, but there's no captured-pcap or
   recorded-gRPC-transcript fixture for reproducing a bug reported against a
   *specific* real opensnitchd version's quirky behavior. Worth a follow-up
   if/when a real bug report references daemon-specific behavior the mock
   doesn't model. Effort: medium-high (needs a real daemon to capture
   against). Payoff: currently speculative — no such bug report exists yet.

8. **Component B (security scanner) baseline classification and privileged
   checks are real design surfaces, not chokepoints to codify** — they're
   already resolved in `docs/superpowers/specs/2026-07-04-scanner-*.md` and
   implemented with the testkit pattern from item 5. No action needed beyond
   linking those docs from CLAUDE.md (done).

## What's already good (don't touch)

- The mock-daemon (`tests/mock_opensnitchd`) + `bridge_protocol_test.rs`
  round-trip test is a genuinely strong reproduction harness — new bridge
  bugs should almost always get a new scripted scenario here before a fix.
- `scanner-core::testkit::MockInspector` / `scanner-privileged`'s
  `SystemFacts` fake are the correct abstraction boundary for testing
  host-dependent code without root or a real Bazzite box.
- The `docs/superpowers/plans/2026-MM-DD-<slug>.md` convention is already
  consistently followed and gives an agent a ready-made template for
  documenting new work before implementing it.
