# Overnight run 7 (2026-11-16): ready the repo for r16.3

Owner asked (2026-11-16) for an autonomous overnight plan. Standing rules apply: TDD, PR, independent review
(+ `/codex` adversarial), merge pinned to the head SHA on 6/6 CI, never push to main directly. Hard limits:
repo-side only. No sudo, no firewall, no tower image or release workflow, nothing filed upstream, no `rm -rf`,
no tag, no worktree removal of the older `wt-*`/`w1-w4` (needs the owner). Run tests under a throwaway HOME
(`CARGO_HOME`/`RUSTUP_HOME` real). Stop and write STATE.md if context nears 250k.

## Decisions this run carries out (owner, 2026-11-15)
r16.3 = master daemon `a1353848` + eBPF (plan `2026-11-08-ebpf-r16-3.md`); #35 keep the TCP gRPC fallback behind
a dev-only flag; #356 accept the 3 s grace and prepare the upstream field request; load test = A for v1.8.0 only.

## Work items, in order

1. **#363 close-out** (plan `2026-11-15-kirigami-probe-load-flakes.md`). Branch `fix/363-drawer-map` (commit
   `c948d3bd`: drawer wall-clock waits + 120 s watchdog, map focus-return gates, Tab trails). The full 15-round
   load hunt (`scratchpad/hunt.sh`, log `hunt4.log`) is running. Remaining failures (answer_surfaces, connections,
   blocklists: wrong focus / extra key events) get a per-case gate on the effect, or a product bug issue if the
   control is really unreachable. Repeat hunt until 0/165. Then PR, `/codex`, merge on green, close #363 with
   the hunt numbers. Done when: a full hunt is 0 failures and #363 is closed.
2. **#35: TCP gRPC fallback behind a dev-only flag.** Explore first (read-only): where the bridge binds
   127.0.0.1:50051, who enables it, what the packaged config and tests rely on. Then plan doc, TDD, security-reviewer
   + codex (security-sensitive: a local process that binds the port first controls root opensnitchd). Done when:
   the default build does not listen on TCP, the dev flag enables it, docs and tests agree, issue #35 updated. If the
   explore shows the packaged default already is the Unix socket, make the TCP path opt-in and say so; if it needs a
   design choice not covered by the owner's decision, stop that item and record the question.
3. **#356 upstream request draft.** A tracked draft in `docs/superpowers/research/` for a per-process id field in
   the opensnitchd Subscribe `ClientConfig` (not filed; the owner reads drafts first). Update #356 with the decision.
4. **Close-out docs.** `HANDOFF.md` section 2026-11-16, `.omc/STATE.md` (<= 60 lines), a trail learning per
   non-obvious finding, tower SHA batch (every SHA merged since `a2ae174a`, to include `4b167b1a` and this run's).
   Send the batch to the tower session if a send tool is available; otherwise leave it first in STATE.md.

## Not in this run (owner or tower)
Tower brief items (kernel-devel, clang/llvm major, a real `build-modules.sh` run, matched daemon build, flip of the
three `proc` checks, VM load test on the master pair with the owner present), one real pkexec scan before any image,
old-worktree removal, branch cleanup (~130 remote branches), GUI release cut.

## Morning report
`HANDOFF.md` top section plus a numbered list: merged PRs with SHAs, what is blocked and on whom, decisions needed.
