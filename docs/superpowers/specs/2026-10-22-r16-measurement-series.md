# r16 measurement series (design note, DRAFT)

Status: design only, no code. Purpose: give r16's eBPF decision a before/after
number. Today there is one trustworthy reading: on `proc`, opensnitchd averaged
~20.5% of one core over 12.6 h on the r14 laptop, RSS 122 MB at 6 min -> 222 MB
at 12.6 h (tower, 2026-10-22). One-off samples are not enough: the earlier 40-70%
figure and the 2-12% early figure both came from different moments of the same
daemon.

## What to measure
Per process, every 60 s, for opensnitchd, snitchwatch-system-bridge and the
Kirigami GUI:
- CPU: delta of (utime + stime) from `/proc/<pid>/stat` over the wall-clock
  interval, as percent of one core. No `top` parsing.
- RSS (`VmRSS` in `/proc/<pid>/status`), thread count, open fd count.
System context per sample: 1/5/15-minute load average, number of running
processes with `ProcMonitorMethod`-relevant churn (fork rate from
`/proc/stat` `processes` counter delta). Workload proxy: unmatched asks per
minute, counted from the bridge journal ("nobody answered" lines; r15 adds
path+destination) so two runs can be compared at similar load.

Counters only. No connection data, hostnames or paths leave the host in this
series, so it needs no redaction.

## How it runs
- A small dependency-free shell (or Python `-I`) loop that finds PIDs with
  `systemctl show -p MainPID --value <unit>`; the GUI under flatpak/bwrap is
  found with `flatpak ps` or its sandbox PID and may be sampled only partially.
- Permissions: CPU and RSS come from world-readable `/proc/<pid>/stat` and
  `status`, but the open-fd count (`/proc/<pid>/fd`) and the bridge journal need
  root (opensnitchd and the system bridge run as root) or membership of
  `systemd-journal`/`adm`. Run the collector as root (a system timer or sudo by
  the owner), or in user mode drop the fd count and the journal-derived asks/min
  and record only CPU, RSS, threads, load and fork rate.
- Appends one NDJSON line per process per minute to `--out-dir`, mode 0600,
  directory 0700 (same private-file rules as other state files). Default: when
  run as root, `/var/lib/snitchwatch/measure/<run-id>.ndjson`; in user mode,
  `$XDG_STATE_HOME/snitchwatch/measure/` (fallback `~/.local/state/...`).
- Runs as a root systemd timer/unit (needed for fd counts and the journal) or as a
  foreground script the owner starts with sudo; the tower or owner runs it on the
  laptop. Nothing in the firewall path changes.

## Runs and comparison
- Run A: `ProcMonitorMethod: proc` (today). Run B: `ebpf`, same host and image.
  >= 24 h each, including a normal workday, same GUI state (open/closed).
- Record per run: kernel, image digest, opensnitchd build, ProcMonitorMethod,
  whether the GUI was attached, start/end.
- Report: median and p95 CPU, mean CPU, RSS at start and end and the slope
  (MiB/h), peak RSS, fork-rate and asks/min medians for context.

## r16 acceptance gate (matches the plan)
Ship ebpf only if run B shows lower mean CPU than a FRESH run A on the same
host under comparable conditions (the ~20.5% of a core figure is a sanity
reference only, since readings vary by day) and an RSS slope no worse than run A, with no daemon errors or degraded
state in the log. Fallback to `proc` stays tested.

## Open questions
1. Who runs it: owner, or the tower session with the owner's OK (it reads only
   /proc and systemd state, but it runs on the laptop)?
2. Is there a way to compare at equal workload, or do we accept day-to-day
   variance and rely on 24 h means plus the fork-rate/asks context?
3. Where do the NDJSON files live long term (they feed r17's history store)?
