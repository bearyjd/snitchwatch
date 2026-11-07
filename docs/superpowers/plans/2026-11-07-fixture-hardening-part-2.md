# Plan: #322 part 2 — pidfd leftovers + ScannerFixture refactor (follows 2026-11-07-fixture-hardening.md, part 1 / #326)

Date: 2026-11-07. Crate: `scanner-privileged` (test code only; no production change).
Status: DONE 2026-11-07 (see Outcome).

## Where #322 stands (verified by read-only map, 2026-11-07)

PR #326 ("#322 part 1", 8ad1f774) already landed:
- Item 1 (pidfd helper): `fixture_signal::{open_pidfd, kill_verified, kill_marked}` exist; all re-exec tests in `command.rs` use `kill_marked`; `MarkedSleeps::cleanup` signals via pidfd.
- Item 2 (/proc-independent cleanup): `MarkedSleeps::with_pid_file` + test `cleanup_kills_recorded_sleepers_even_when_every_scan_fails`.
- Item 4: `PR_SET_PDEATHSIG` + `getppid` re-check in `fork_tree` children; `assert_ne!(root, me())` in `with_scan`.

Still open:
- A. Non-pidfd kills: `ForkedTree::drop` (`containment.rs` ~967, bare `fixture_signal::kill`); `fixture_signal::killpg` at `command.rs` ~754, ~1141 and `containment.rs` ~1424; `main.rs` `kill_unreaped_group` (~317, raw `killpg`; child is polled unreaped so pid reuse is impossible there — document rather than change, unless review disagrees).
- B. Item 3: `ScannerFixture` refactor — one struct whose `Drop` does all cleanup, replacing `Reaped` (two copies: `containment.rs` ~841, `command.rs` ~1607), `ForkedTree` + `fork_tree_with`, `MarkedSleeps` + `with_scan`, and the seven hand-rolled re-exec tests in `command.rs`.

## Phases (each its own PR, TDD, independent review on the exact head, 6/6 CI)

1. **PR A — pidfd for the leftovers.** Failing-first test per site using a throwaway marked child (never a live signal placeholder). Convert `ForkedTree::drop` and the `killpg` sites to pidfd-verified group/leader kills where identity is checkable; leave `main.rs` with a comment unless review objects.
2. **PR B — extract fixture module.** Move `fixture_signal`, `Reaped`, `ForkedTree`, `MarkedSleeps` out of `containment.rs` (2017 lines) into a `#[cfg(test)]` `test_fixtures/` module with no behaviour change; dedupe the two `Reaped` copies. Pure move, so it is reviewable.
3. **PR C — `ScannerFixture`.** Introduce the struct (Drop = pidfd-verified marker kill, pid-file kill, scan fallback, reap). Port the seven re-exec tests one at a time so cleanup no longer depends on running before the first `assert!`. Keep the old types until the last test is ported, then delete them.

Order matters: B before C (C touches the moved code); A is independent and can go first.

## Safety constraints (from run-5 incidents)
- Run tests only via cargo with a throwaway HOME and CARGO_HOME/RUSTUP_HOME set; never execute `target/debug/deps/*` directly.
- Fixtures signal only through the guarded `fixture_signal` path (refuses pid <= 1, pidfd-verified).
- The words `lifecycle` and `pins` are banned in `.rs` sources (bridge `history::dormant_tests`).
- A MEDIUM+ review finding needs the owner's explicit exception to merge.

## Out of scope
The three load-sensitive timing tests, poller dedupe, lock-test handshake, `just clean-probe-homes`, and the CLAUDE.md `--workspace` wording (listed in #322 comments / STATE.md; separate PRs).

## Open questions for the owner
1. Land as three PRs (A, B, C) as above, or A+B together?
2. `main.rs` `kill_unreaped_group`: comment-only (my recommendation) or convert to pidfd anyway?

## Outcome (2026-11-07)
Landed as #328 (pidfd leftovers, e0347eae), #330 (fixtures moved to `containment::test_fixtures`, a186ccfe), #331 (`ScannerFixture` + 3 ports, 7df601a9), #332 (remaining 4 ports, run-once cleanup, a5032f4d). Issue #322 closed. Each merge: clean independent Opus review on the exact head + 6/6 CI.
- Decisions taken: three-then-four PRs (A, B, C1/C2), `main.rs::kill_unreaped_group` comment-only, no `PIDFD_SIGNAL_PROCESS_GROUP` (CI runner kernels may be older than 6.9).
- Kept on purpose: `Reaped`, `ForkedTree`/`fork_tree(_with)`, `MarkedSleeps` (~15 unit tests in `containment.rs` use them; `ScannerFixture` uses `MarkedSleeps` as its scan engine).
- Left as optional LOW follow-ups (listed in the #322 closing comment): include cleanup problems in the "could not be reaped" panic; comment the run-once flag; `MarkedSleeps` doc; share one pid-file reader.
- Gotchas found: bridge `identity_dormant_tests` fails CI on the token `identify(` in any non-history `.rs` (a test name tripped it); orphan-sleeper helpers must redirect stdio or `.output()` blocks ~29 s.
