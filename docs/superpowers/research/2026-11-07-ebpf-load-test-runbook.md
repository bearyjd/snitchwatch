# eBPF load test for the tower session (runbook, notes only)

Date: 2026-11-07. Base: `origin/main` a5032f4d. Companion to
`docs/superpowers/specs/2026-10-22-ebpf-groundwork.md` (read it first; it holds the
evidence and the daemon source citations). This file is the *procedure*. Nothing here
changes a config, image, workflow or release. Every claim not read from a repo file is
marked **UNVERIFIED**; the test exists to settle those. Daemon citations are
`vendor/opensnitch/daemon/...` at v1.8.0 (b404c4c). 2026-11-08: the submodule is now pinned to
upstream master `a1353848` (v1.9.0); every line cite below was re-checked there and is unchanged
except `ebpf_prog/Makefile:8` (now `:14`). Log strings are as at v1.8.0: re-check on the pinned
commit before relying on one. Section 5a builds the daemon and modules from the pin.

## 0. Question and answer shape

**Question:** do OpenSnitch v1.8.0's prebuilt eBPF modules load, attach and attribute
connections correctly on the tower image's kernel, when `opensnitchd` runs as root with
`ProcMonitorMethod: ebpf`?

**This is not a Snitchwatch code task.** `opensnitchd` loads the modules; the bridge never
touches eBPF (CLAUDE.md settled decision #2). The deliverable is evidence for a packaging
decision, plus a go/no-go for flipping the tower's `proc` pin.

Outcomes. Tell B from D by the **logged error type**, not by the fallback line: every
main-module failure ends in the `/proc` fallback (`procmon/monitor/init.go:113-133`), so
a B always also looks like a D.

| Result | How to recognise it | Meaning / next |
|---|---|---|
| A | `opensnitch.o` and `opensnitch-procs.o` load, no attach errors, `Process monitor method ebpf`, attribution matches (section 4); the dns module is noted separately, it does not decide A/B | prebuilt modules work here; ship the `.o` files in the tower image and flip the tower checks together with our config (section 6) |
| B | `module specs error` / `module collection error` (the latter carries the verifier text) / `opening kprobe` / `opening kretprobe` / `[eBPF events] <tracepoint>: ...` (exec/exit tracepoint attach, `events.go:113-146`; the daemon still prints `Process monitor method ebpf`, so a run with these is NOT an A) / `Error loading opensnitch.o (collection)` (`procmon/ebpf/ebpf.go:141`) / `eBPF module opensnitch.o malformed, bpfmap[..] nil` (about `ebpf.go:204`); load errors then fall back to `/proc`, but attach errors (`opening kprobe/kretprobe`) are only warnings and the daemon stays on ebpf. The same rule applies to `opensnitch-procs.o`: a specs/collection error for it, or an `Assign` error (`opensnitch-procs.o not available: <assign err>`, `events.go:102-103`, a map mismatch like the main module's), is B (main module fine, procs rejected or mismatched) | modules do not fit this kernel; try the in-image build (section 5) |
| C | loads and attaches, but attribution is wrong | silent wrong-process attribution, worse than failing: do NOT ship; keep `proc`; this is the case that could justify a CO-RE port upstream |
| D | `might not be compatible` that names `opensnitch.o` or `opensnitch-procs.o` with **no** preceding `module specs error` / `module collection error` line (a missing `.o` is skipped at debug level and ends in that same message, `core/ebpf.go:41-43,59-62`), or a `[eBPF] unable to remove memlock` warning, or `opensnitch-procs.o not available` caused by a missing file, a tracefs mount failure or a perf-map failure (`initPerfMap`, `events.go:108`; **UNVERIFIED** that a perf-map failure is never a kernel matter) and not by a specs/collection/`Assign` error. Precedence: a memlock warning plus any load failure is D first; a memlock warning followed by a clean load is recorded, not D | environment problem (missing files, privileges, rlimit), not a kernel verdict: check the `ls` of all three directories, fix the install and rerun |

## 1. Safety rules (read before running anything)

- **Run it in a VM, not on a real host.** The daemon installs nfqueue rules; a bad run can
  cut network access. Never on the owner's laptop without the owner present (the
  groundwork note says this step "touches the firewall").
- **The VM must boot the kernel under test.** A VM brings its own kernel: record
  `uname -r` and confirm it equals the target image's kernel. If it does not, the result
  says nothing about the tower image and must be reported as invalid.
- Use `DefaultAction: allow` (the packaged default, owner decision 2026-10-25). Do not
  test with `deny`.
- Do not run `snitchwatch-bridge-cli` outside an isolated `XDG_RUNTIME_DIR`; it replaces a
  running bridge's socket and token. This test does not use the bridge: the evidence is the
  daemon's own log.
- Never edit `vendor/opensnitch` (read-only submodule); do any build in a scratch copy.
- Discard the VM afterwards; do not bake anything from the test into an image.

## 2. Preconditions (check, record each as pass/fail)

1. Kernel: `uname -r` (record it; the groundwork note names `7.2.0-ogc6.1.fc44`). It must
   equal the target image kernel (section 1).
2. Running as root (the 2026-07-31 "ebpf fails" finding was a rootless-container artifact,
   issue #6; as root ebpf loaded on 6.19).
3. debugfs/tracefs: `mount | grep -E 'debugfs|tracefs'`. The upstream README says debugfs
   must be mounted but also that the daemon tries to mount it itself since v1.6.0
   (`ebpf_prog/README`); record what you see. **UNVERIFIED** on this kernel.
4. Kernel options (record, do not assume): `zcat /proc/config.gz | grep -E
   'CONFIG_(BPF|BPF_SYSCALL|BPF_EVENTS|KPROBES|KPROBE_EVENTS|CGROUP_BPF|FTRACE_SYSCALLS)='`.
   If `/proc/config.gz` is absent try `/usr/lib/modules/$(uname -r)/config`, then
   `/boot/config-$(uname -r)` (on ostree systems `/boot` may not carry it).
5. `/sys/kernel/btf/vmlinux` is NOT the precondition (the modules are non-CO-RE); record
   whether it exists anyway for the 2026-07-14 diagnostics spec.
6. Is the VM an rpm-ostree image (read-only `/usr`)? **UNVERIFIED** what the tower's VM gate
   image is; `touch /usr/lib/x` tells you. It decides where section 3 step 3 may write.
7. The tower image builds a patched daemon from Go source (groundwork note lines 55-56): record
   `opensnitchd -version` and the patch set. Whether the v1.8.0 `.o` files match that build's
   map assignments is **UNVERIFIED**; an `Error loading opensnitch.o (collection)` from a
   mismatch is outcome B.

## 3. Procedure A: prebuilt modules from the v1.8.0 RPM

1. Fetch the RPM named in `packaging/bluebuild/recipe.yml:58`
   (`opensnitch-1.8.0-1.x86_64.rpm`, from the upstream GitHub release). Record its sha256.
2. List its contents without installing: `rpm -qlp opensnitch-1.8.0-1.x86_64.rpm | grep -E
   'ebpf|\.o$'`. The vendored spec installs `opensnitch.o`, `opensnitch-procs.o` and
   `opensnitch-dns.o` to `/usr/lib/opensnitchd/ebpf/`
   (`vendor/opensnitch/utils/packaging/daemon/rpm/opensnitch.spec:68-70,122-124`);
   **UNVERIFIED** only that the actual v1.8.0 release file matches. A missing module is
   outcome D (install problem), not B.
3. Put the three `.o` files in place:
   - On a bluebuild-image VM the RPM is already installed (`recipe.yml:58`) and the files
     are already under `/usr/lib/opensnitchd/ebpf/`: skip the copy, confirm they exist.
   - On the tower image (no RPM, no module files; groundwork note lines 55-57) extract with
     `rpm2cpio ... | cpio -idmv` in a scratch dir and copy them to
     `/usr/local/lib/opensnitchd/ebpf/` (writable on ostree; the daemon searches it
     first). Do not write to `/usr/lib` on an atomic image.
   - The daemon searches `/usr/local/lib/opensnitchd/ebpf`, then `/usr/lib/opensnitchd/ebpf`,
     then the deprecated `/etc/opensnitchd` (`daemon/core/ebpf.go:14-19`), and a failed load
     `continue`s to the next path (`ebpf.go:48,53`). **Check all three directories for
     stale `.o` files** (`ls -l` each), because a stale copy earlier in the order loads
     instead of the test copy and a later one can mask a failure. Leave `Ebpf.ModulesPath`
     unset (setting it restricts the search to that path).
4. Config: **set `"ProcMonitorMethod": "ebpf"` explicitly and `"LogLevel": -1`** (TRACE; 0 is
   DEBUG, 1 INFO, `log/log.go:36-46`) in the test config. The tower image overwrites `ebpf` to `proc` after `stage.sh`
   (`docs/packaging/system-bridge-integration.md:102-105`), so a `proc` result from
   `grep -E 'ProcMonitorMethod|LogLevel|DefaultAction' /etc/opensnitchd/default-config.json`
   means the test is not valid yet. The `module loaded` and `Process monitor method` lines are Info (level 1 or lower is enough); the `trying to load` lines need 0 or lower and section 4 needs -1. Both shipped configs set
   `"LogLevel": 2` (IMPORTANT, `log/log.go:38-42`), which drops the info-level lines this
   test relies on, and the level from the config is applied before the CLI log flags, so a
   `-debug` flag does not substitute for it. Put the `LogLevel` used in the section 7 report.
5. Start the daemon as root and capture the full log. Main file: `/var/log/opensnitchd.log`
   (the `LogFile` setting). The systemd unit is `opensnitch.service` (not `opensnitchd`;
   `recipe.yml:85-93`; **UNVERIFIED** on the tower image, which builds its daemon from source): `journalctl -u opensnitch` only helps if `LogFile` is unset.
6. Read the log for these, at `LogLevel: -1` (sources: `core/ebpf.go`,
   `procmon/monitor/init.go`, `procmon/ebpf/ebpf.go`, `dns/ebpfhook.go`):
   - **pass:** `[eBPF] module loaded: <path>` for `opensnitch.o` and `opensnitch-procs.o` (`ebpf.go:56`) and
     `Process monitor method ebpf` (`init.go:96`), and **none** of the failure lines below. A
     third line for `opensnitch-dns.o` is reported separately (dns does not decide A/B; it
     loads through the same `LoadEbpfModule`, `dns/ebpfhook.go:118`, so a missing dns `.o` prints
     the same `might not be compatible` text: read which module the message names);
   - **main module failed (B or D):** `[eBPF] module specs error` or `[eBPF] module
     collection error: <text>`, ending in `unable to load eBPF module (...). Your kernel
     version (<ver>) might not be compatible.` (`ebpf.go:60`; grep `might not be
     compatible`, the string has a version in the middle), then `Process monitor method
     /proc` (`init.go:132`);
   - **loaded but did not attach (B):** `opening kprobe: ...` / `opening kretprobe: ...`
     (`procmon/ebpf/ebpf.go:147-187`) and, for the procs module, `[eBPF events] sys_enter_execve:`
     (also `sys_exit_execve`, `sys_enter_execveat`, `sys_exit_execveat`, `sched_process_exit`) and
     `[eBPF events] Some tracepoints not loaded` (`events.go:113-146`). These are warnings, the
     daemon stays on ebpf; count them as failures even if `module loaded` printed, because exec
     tracking (section 4 step 3) depends on those tracepoints;
   - **`[eBPF] unable to remove memlock`** (`procmon/ebpf/ebpf.go:126-128`, runs before any
     load): an environment problem (privileges / rlimit), outcome D when any load failure follows it;
     a memlock warning followed by a clean load is recorded in the report, not D;
   - **`opensnitch-procs.o` problem:** `Process monitor method ebpf` *and*
     `opensnitch-procs.o not available: ...` (`init.go:105-106`): still on ebpf with the proc
     monitors alongside, so read both lines. Classify by the error text: specs/collection or `Assign`
     error: B; a missing file, a tracefs mount failure or a perf-map failure: D;
   - **dns module:** `opensnitch-dns.o` failing only warns (`EBPF-DNS: Unable to attach
     ebpf listener`); note it, it does not decide A/B.
   The verifier output is part of the `module collection error` text (cilium/ebpf returns it
   there). Full verifier logging is requested only when the level is **exactly 0** (DEBUG,
   `if log.GetLogLevel() == log.DEBUG`, `ebpf.go:31-33`), so at the -1 used here the error text is
   shorter and **UNVERIFIED** as to how much verifier detail it carries. If the result is B,
   rerun once at `LogLevel: 0` to get the full verifier text; that error can be hundreds of
   lines, so keep the whole log as a file and quote at most the first and last lines. **UNVERIFIED** that `dmesg` shows anything useful; record
   `dmesg | tail -50` anyway, but treat the log line as the primary evidence.

A clean load is **necessary, not sufficient** (section 4).

## 4. Procedure B: attribution check (only if section 3 passed)

Non-CO-RE modules built against other headers can load yet read wrong offsets.

1. The observable: with `DefaultAction: allow` and no UI connected, an unmatched connection
   takes `applyDefaultAction`, which logs `con.String()` **only at TRACE** (`main.go:431-432`;
   `conman/connection.go:315-319` formats `path (pid) -> host:port (proto uid)`). So set
   `"LogLevel": -1`; at 0 nothing attributed is printed. It gives path **and** pid; compare both
   with `ss -tnp` and `/proc/<pid>/exe` on your side. No bridge or GUI is needed.
   **UNVERIFIED** that this is readable at volume; if it is not, stop and report that instead of
   improvising a client. (A temporary catch-all allow rule would instead log at debug level on
   match; if you use one, remove it in section 8.)
2. Generate known traffic from known, distinct binaries, e.g. `curl -s https://example.org`,
   `python3 -c` with a socket connect, and a long-lived `nc` connection, each under its own
   binary path.
3. Repeat for the cases where exec tracking matters. The expected answers below are the
   obvious ones, **UNVERIFIED** as ebpf behaviour (no repo source states them):
   - a process that connects and then execs: expect the binary that made the connection;
   - a short-lived process that connects and exits at once: expect its own path, not empty;
   - a child of a shell (`sh -c 'curl ...'`): expect `curl`, not `sh`.
4. **Run every case twice, once with `ProcMonitorMethod: proc` (baseline) and once with `ebpf`.**
   Record per case: expected path, `proc` result, `ebpf` result. Only a mismatch that appears
   under `ebpf` and not under `proc` is outcome C; a mismatch in both is daemon-wide behaviour
   to report separately. Also record connections attributed to an empty/unknown process under
   `ebpf` that `proc` mode resolves. Unattributed shapes (`conman/connection.go:312-315`): process missing, `srcport:srcip (uid:N)
   ->(proto)-> host:port`; entry missing, `srcport:srcip ->(proto)-> host:port` (no uid); a nil
   connection prints `Applying DefaultAction (...) on <nil>` (`main.go:431-432`), grep for that.

## 5. Procedure C: in-image build (only if section 3 failed as B)

The upstream build is not CO-RE and needs kernel sources (`ebpf_prog/README`):
1. Work in a **scratch copy** of `vendor/opensnitch/ebpf_prog`; never build inside the
   submodule. On an atomic VM, `kernel-devel`/clang/llvm need layering or a toolbox container;
   build against the **host** kernel's headers, not the container's.
2. `kernel-devel` matching the running kernel exactly. **UNVERIFIED** that a package for the
   recorded `uname -r` exists (custom kernel). If it does not, the README's other route is
   building against a kernel source tree (`ebpf_prog/README:3-21`); if neither is available,
   outcome B is final for the prebuilt route and the next step is an upstream/CO-RE
   decision, not more attempts.
3. clang + llvm (version **UNVERIFIED**), then
   `make KERNEL_DIR=/usr/src/kernels/$(uname -r)/ KERNEL_HEADERS=/usr/src/kernels/$(uname -r)/` in the
   scratch copy (the Makefile's `KERNEL_HEADERS` default is a Debian path, `ebpf_prog/Makefile:14`;
   the README passes both, line 18) (clang -> LLVM IR ->
   `llc -march=bpf`; `llvm-strip -g` is a separate step, README line 20).
4. Caveat: a `bpfmap nil` / `(collection)` / `Assign` mismatch on the tower's patched daemon is not
   necessarily a kernel problem, and rebuilding v1.8.0's `ebpf_prog` may not fix it; report it as is.
5. Install the three `.o` under `/usr/local/lib/opensnitchd/ebpf/` and repeat sections 3
   (steps 4-6) and 4.

## 5a. Procedure D: daemon + modules from the a1353848 pin (matched pair)

Use this instead of section 3's RPM when testing the pinned master (it carries `614537c9`, the
kernel 6.19 build fix). Daemon and `.o` files must come from the **same** commit.
1. Scratch clone, never the submodule: `git clone https://github.com/bearyjd/opensnitch.git -b
   snitchwatch/stable os-pin && git -C os-pin checkout a1353848ba1b660320e90cefea782c3fba272c00`
   (or `git -C vendor/opensnitch archive a1353848 | tar -x -C os-pin`). Then require `test "$(git -C os-pin rev-parse HEAD)" = a1353848ba1b660320e90cefea782c3fba272c00` (stop if it differs); the archive variant never touches the fork.
2. Daemon: Go **>= 1.25** (`daemon/go.mod:3` says `go 1.25.0`; **UNVERIFIED** that the tower's
   builder has it). `cd os-pin/daemon && go build -o opensnitchd .` (the Makefile's `go get` step
   needs network; **UNVERIFIED** offline). `./opensnitchd -version` must print `1.9.0`
   (`daemon/core/version.go:6`).
3. Modules: as section 5 steps 1-3 in `os-pin/ebpf_prog`, adding `KERNEL_VER=$(uname -r)`: the
   pinned Makefile adds the 6.19 flags only when `KERNEL_VER >= 6.19` (`ebpf_prog/Makefile:17,57-59`).
4. Install the daemon binary where the image expects it and the three `.o` under
   `/usr/local/lib/opensnitchd/ebpf/`, record both sha256s with the commit, then run sections 3
   (steps 4-6) and 4. Tower work only: nothing in this repo builds either artefact.

## 6. If it passes: what flips together (do not do it from this runbook)

For the **tower image** the three `.o` files must be shipped (the bluebuild image already
gets them from the RPM) and the tower's stage/verify checks, which hard-require `proc`,
must change in the same step as our config, keeping the tested `proc` fallback (groundwork
note, step 3). Repo facts to reconcile then: `packaging/bluebuild/recipe.yml:60-69` and
`docs/packaging/rpm-ostree-layering.md:46-53` already say ebpf loads under root;
`docs/packaging/system-bridge-integration.md:102-110` carries the 2026-10-22 correction
that the issue-#6 failure was a rootless-container artifact (the groundwork note, written
the same day, calls the surrounding text stale; the correction was added in that doc
afterwards, so the doc is correct as it stands; the premise that ebpf ever failed for a
kernel reason is unproven).
Then measure CPU/RSS under ebpf with the r16 measurement series
(`docs/superpowers/specs/2026-10-22-r16-measurement-series.md`): no ebpf CPU numbers exist
anywhere today (the 40-70% and 20.5% figures were `proc`).

## 7. Report back (paste into the snitchwatch session)

```
kernel: <uname -r>  == target image kernel: yes/no   run as root: yes/no   VM image: <ostree y/n>
RPM sha256: <...>   modules in RPM: opensnitch.o y/n, -procs.o y/n, -dns.o y/n
stale .o in /usr/local/lib, /usr/lib, /etc/opensnitchd: <none / paths>
opensnitchd -version: <..>   patch set: <none/..>
config: ProcMonitorMethod=<..> LogLevel=<..> DefaultAction=<..> ModulesPath=<unset/..>
kernel opts: <BPF.. FTRACE_SYSCALLS y/n>   debugfs/tracefs: y/n   BTF present: y/n
log: "module loaded" main y/n, procs y/n (decide A/B)   dns y/n (noted only)   "Process monitor method": ebpf | /proc
"opensnitch-procs.o not available" line: <none / verbatim>   "EBPF-DNS" warning: <none / verbatim>
attach errors (opening kprobe/kretprobe, [eBPF events] tracepoints): <none / verbatim>   memlock warning: y/n
load errors (specs/collection error: file path + first/last lines only): <...>   dmesg (<=10 lines): <...>
attribution: <table of cases, section 4>
outcome: A | B | C | D        kernel-devel for this kernel available: y/n/not tried
```

Do not paste full logs into chat; keep them as files and send paths/short excerpts.

## 8. Rollback

The test only places `.o` files and starts a daemon in a throwaway VM: discard the VM.
If it was run anywhere persistent, remove **only** the copies you placed under
`/usr/local/lib/opensnitchd/ebpf/` (never `rm` under `/usr/lib/opensnitchd/ebpf/`, those
belong to the RPM), restore the original config, stop the daemon, and confirm connectivity.

## 9. Not covered here

CO-RE port of the modules (upstream work, only justified by outcome B-final or C);
CPU/RSS comparison (measurement series); GUI changes (none needed).
