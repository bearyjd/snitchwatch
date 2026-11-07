# eBPF load test for the tower session (runbook, notes only)

Date: 2026-11-07. Base: `origin/main` a5032f4d. Companion to
`docs/superpowers/specs/2026-10-22-ebpf-groundwork.md` (read it first; it holds the
evidence and the daemon source citations). This file is the *procedure*. Nothing here
changes a config, image, workflow or release. Every claim not read from a repo file is
marked **UNVERIFIED**; the test exists to settle those.

## 0. Question and answer shape

**Question:** do OpenSnitch v1.8.0's prebuilt eBPF modules load and attribute
connections correctly on the tower's kernel (7.2.x Bazzite), when `opensnitchd` runs as
root with `ProcMonitorMethod: ebpf`?

**This is not a Snitchwatch code task.** `opensnitchd` loads the modules; the bridge never
touches eBPF (CLAUDE.md settled decision #2). The deliverable is evidence for a packaging
decision, plus a go/no-go for flipping the tower's `proc` pin.

Outcomes, in order of cost:

| Result | Meaning | Next |
|---|---|---|
| A. all 3 modules load and attribution is right | prebuilt RPM modules work on this kernel | ship the `.o` files in the image, flip the tower checks together with our config (section 6) |
| B. load fails (verifier / struct layout) | non-CO-RE modules mismatch this kernel | try the in-image build (section 5); needs `kernel-devel` for the exact kernel |
| C. loads but attribution is wrong | silent wrong-process attribution, worse than failing | do NOT ship; keep `proc`; this is the case that could justify a CO-RE port upstream |
| D. daemon falls back to `proc` | main module failed; same as today | read the log line for the reason |

## 1. Safety rules (read before running anything)

- **Run it in the tower's VM gate, not on a real host.** The daemon installs nfqueue
  rules; a bad run can cut network access. Never on the owner's laptop without the owner
  present (the groundwork note says this step "touches the firewall").
- Use `DefaultAction: allow` (the packaged default, owner decision 2026-10-25). Do not
  test with `deny`.
- Do not run `snitchwatch-bridge-cli` outside an isolated `XDG_RUNTIME_DIR`; it replaces a
  running bridge's socket and token. This test does not need the bridge at all: use the
  daemon's own log.
- Never edit `vendor/opensnitch` (read-only submodule).
- Snapshot or discard the VM afterwards; do not bake anything from the test into an image.

## 2. Preconditions (check, record each as pass/fail)

1. Kernel: `uname -r` (record it; the groundwork note names `7.2.0-ogc6.1.fc44`).
2. Running as root in the VM (the 2026-07-31 "ebpf fails" finding was a rootless
   container artifact, issue #6; as root ebpf loaded on 6.19).
3. debugfs/tracefs mounted: `mount | grep -E 'debugfs|tracefs'`. **UNVERIFIED** on this
   kernel; the README needs it, plus FTRACE_SYSCALLS for the procs module.
4. Kernel options (record, do not assume): `zcat /proc/config.gz | grep -E
   'CONFIG_(BPF|BPF_SYSCALL|BPF_EVENTS|KPROBES|KPROBE_EVENTS|CGROUP_BPF|FTRACE_SYSCALLS)='`.
   If `/proc/config.gz` is absent, use `/boot/config-$(uname -r)`.
5. Note: `/sys/kernel/btf/vmlinux` is NOT the precondition (the modules are non-CO-RE);
   record whether it exists anyway for the 2026-07-14 diagnostics spec.

## 3. Procedure A: prebuilt modules from the v1.8.0 RPM

1. Fetch the RPM named in `packaging/bluebuild/recipe.yml`
   (`opensnitch-1.8.0-1.x86_64.rpm`, from the upstream GitHub release). Record its sha256.
2. List its contents without installing: `rpm -qlp opensnitch-1.8.0-1.x86_64.rpm | grep -i
   -E 'ebpf|\.o$'`. **UNVERIFIED** that it ships `opensnitch.o`, `opensnitch-procs.o`,
   `opensnitch-dns.o`; if any are missing, that alone decides outcome B/D.
3. Extract to a scratch dir (`rpm2cpio ... | cpio -idmv`), then copy the three `.o` files to
   `/usr/lib/opensnitchd/ebpf/`. Search order is `/usr/local/lib/opensnitchd/ebpf`, then
   `/usr/lib/opensnitchd/ebpf`, then the deprecated `/etc/opensnitchd`
   (`daemon/core/ebpf.go:14-17`): make sure no stale `.o` sits in `/etc/opensnitchd`, and
   leave `Ebpf.ModulesPath` unset (setting it restricts the search to that path).
4. Config: use the shipped system overlay config (it already sets
   `"ProcMonitorMethod": "ebpf"` and `DefaultAction: allow`); confirm with
   `grep -E 'ProcMonitorMethod|DefaultAction' /etc/opensnitchd/default-config.json`.
5. Start the daemon as root in the VM, foreground or via its unit, and capture the full log
   (`/var/log/opensnitchd.log` or `journalctl -u opensnitchd`). Keep it for the report.
6. Read the log for exactly these (strings from the groundwork note, source
   `procmon/monitor/init.go`, `core/ebpf.go`):
   - pass: three `[eBPF] module loaded` lines **and** `Process monitor method ebpf`;
   - main module failed: `[eBPF] module specs error` / `module collection error` /
     `unable to load eBPF module ... Your kernel version might not be compatible`, then
     `Process monitor method /proc` (the daemon did not exit; fallback worked);
   - only `opensnitch-procs.o` missing: still `ebpf`, with the proc monitors started
     alongside (`init.go:101-108`): easy to misread, so read both lines;
   - `opensnitch-dns.o` failing only warns (not a failure of the main path).
   That single error message covers a missing file, a verifier failure and a permissions
   failure alike, so also capture `dmesg | tail -50` right after a failed load for the
   verifier text.

A clean load is **necessary, not sufficient** (procedure B).

## 4. Procedure B: attribution check (only if section 3 passed)

Non-CO-RE modules built against other headers can load yet read wrong offsets.

1. Generate known traffic from known, distinct processes, e.g. `curl -s https://example.org`,
   `python3 -c` with a socket connect, and a long-lived `nc` connection, each under its own
   binary path.
2. For each, find the daemon's attributed process (path and pid) in its log or connection
   stats and compare with the real one (`ss -tnp`, `/proc/<pid>/exe`).
3. Repeat across a process that execs after connecting, a short-lived process (connects
   then exits fast), and traffic from a child of a shell, since exec/exit tracking is what
   `opensnitch-procs.o` adds.
4. Record, per case: expected process, attributed process, match yes/no. Any mismatch is
   outcome C. Also record connections attributed to an unknown/empty process under
   `ebpf` that `proc` mode resolves.

## 5. Procedure C: in-image build (only if section 3 fails to load)

The upstream build is not CO-RE and needs kernel sources (`ebpf_prog/README`):
1. `kernel-devel` matching the running kernel exactly. **UNVERIFIED** that a package for
   `7.2.0-ogc6.1.fc44` exists (custom kernel). If it does not, stop here: outcome B is
   final for the prebuilt route and the next step is an upstream/CO-RE decision, not more
   attempts.
2. clang + llvm (version **UNVERIFIED**), then
   `make KERNEL_DIR=/usr/src/kernels/$(uname -r)/` in `ebpf_prog/` (clang -> LLVM IR ->
   `llc -march=bpf`; `llvm-strip -g` is a separate step, README line 20).
3. Install the three `.o` under `/usr/lib/opensnitchd/ebpf/` and repeat sections 3 and 4.
Build in a scratch copy of `vendor/opensnitch/ebpf_prog` (do not write into the submodule).

## 6. If it passes: what flips together (do not do it from this runbook)

Per the groundwork note, step 3: the tower's stage/verify checks hard-require `proc`.
Those checks and our config must change in one step, keeping the tested `proc` fallback.
Related repo facts to reconcile at that point: `packaging/bluebuild/recipe.yml:60-69` and
`docs/packaging/rpm-ostree-layering.md:44-51` already say ebpf loads under root;
`docs/packaging/system-bridge-integration.md:102-106` already carries the 2026-10-22
correction that the issue-#6 failure was a rootless-container artifact (it is correct as
written; the premise that ebpf ever failed for a kernel reason is unproven).
Then measure CPU/RSS under ebpf with the r16 measurement series
(`docs/superpowers/specs/2026-10-22-r16-measurement-series.md`): no ebpf CPU numbers
exist anywhere today (the 40-70% and 20.5% figures were `proc`).

## 7. Report back (paste into the snitchwatch session)

```
kernel: <uname -r>            run as root: yes/no        VM/host: <which>
RPM sha256: <...>             modules present in RPM: opensnitch.o y/n, -procs.o y/n, -dns.o y/n
config: ProcMonitorMethod=<..> DefaultAction=<..> ModulesPath=<unset/..>
kernel opts: <BPF.. FTRACE_SYSCALLS y/n>   debugfs/tracefs mounted: y/n   BTF present: y/n
log: modules loaded <n>/3     "Process monitor method": ebpf | /proc
errors (verbatim, <=10 lines): <...>      dmesg verifier text (<=10 lines): <...>
attribution: <table of cases, section 4>
outcome: A | B | C | D        kernel-devel for this kernel available: y/n/not tried
```

Do not paste full logs into chat; keep them as files and send paths/short excerpts.

## 8. Rollback

The test only copies `.o` files and starts a daemon in a throwaway VM: discard the VM.
If it was run anywhere persistent, remove `/usr/lib/opensnitchd/ebpf/*.o`, restore the
original config, stop the daemon, and confirm connectivity.

## 9. Not covered here

CO-RE port of the modules (upstream work, only justified by outcome B-final or C);
CPU/RSS comparison (measurement series); GUI changes (none needed).
