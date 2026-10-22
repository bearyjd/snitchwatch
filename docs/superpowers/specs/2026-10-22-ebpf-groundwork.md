# eBPF process monitoring: read-only groundwork for r16

Status: research note, no code. Evidence is from the vendored OpenSnitch tree
(`vendor/opensnitch`, v1.8.0, b404c4c) and this repo's docs, read 2026-10-22.
Items marked UNVERIFIED were not tested.

## What the daemon loads
- Three modules, by file name: `opensnitch.o` (connection tracking, required;
  `daemon/procmon/ebpf/ebpf.go:132`), `opensnitch-procs.o` (exec/exit events;
  `procmon/ebpf/events.go:96`), `opensnitch-dns.o` (DNS hook;
  `daemon/dns/ebpfhook.go:118`).
- Search path: `/usr/local/lib/opensnitchd/ebpf`, then `/usr/lib/opensnitchd/ebpf`
  (`daemon/core/ebpf.go:14-17`). If `Ebpf.ModulesPath` is set, only that path is
  searched. Our shipped configs set no `ModulesPath`.

## How they are built (upstream)
- `ebpf_prog/Makefile`: clang -> LLVM IR -> `llc -march=bpf`, then `llvm-strip -g`;
  output `.o` files land in `ebpf_prog/`. No vmlinux.h, no bpftool step.
- NOT CO-RE. The sources include kernel headers (`net/sock.h`, `linux/sched.h`)
  and compile struct layouts from them; `BPF_CORE_READ` is unused. The README says
  "Compilation requires getting kernel sources for now" and gives the Fedora
  recipe: install kernel-devel, clang, llvm, then
  `make KERNEL_DIR=/usr/src/kernels/$(uname -r)/`.
- So `/sys/kernel/btf/vmlinux` is not the real precondition (the 2026-07-14
  kernel-diagnostics spec treats it as one); matching kernel headers are. Needed
  kernel options per the README: BPF, BPF_SYSCALL, BPF_EVENTS, KPROBES,
  KPROBE_EVENTS, CGROUP_BPF, and FTRACE_SYSCALLS for `opensnitch-procs.o`; debugfs
  mounted. The module version stamp is the wildcard 0xFFFFFFFE, so it does not
  block loading by itself.

## What happens when it fails
- `core.LoadEbpfModule` tries each path; on failure it logs `[eBPF] module specs
  error` or `[eBPF] module collection error` and finally returns "unable to load
  eBPF module ... Your kernel version might not be compatible". That one message
  covers a missing file, a verifier failure and a permissions failure alike.
- The daemon does NOT exit: `procmon/monitor/init.go` stops the eBPF state, records
  the error and falls back to `ProcMonitorMethod: proc` ("Process monitor method
  /proc"). `opensnitch-dns.o` failing only warns. So enabling ebpf degrades to
  today's behaviour rather than breaking the firewall (the tower's note that the
  daemon "degrades badly" should be measured, not assumed).

## What this repo already says (and where it conflicts)
- `packaging/bluebuild/recipe.yml:60-69` and `docs/packaging/rpm-ostree-layering.md
  :44-51`: the 2026-07-31 "ebpf fails on kernel 6.19" finding (issue #6) was a
  rootless-container permission artifact; as root, ebpf loaded fine on 6.19.
- `docs/packaging/system-bridge-integration.md:102-106` still says the tower
  overwrites the config to `proc` because "eBPF failed to load on the owner's
  laptop kernel before (snitchwatch#6)". That contradicts the recipe and is stale.
  The premise that ebpf ever failed for a kernel reason is unproven.
- The tower's image does not install the opensnitch RPM; it builds a patched daemon
  from Go source and installs no module files (tower, 2026-10-22). So today ebpf
  could not load on that image regardless of the kernel. Whether the upstream
  RPM's prebuilt `.o` files exist and load on kernel 7.2 is UNVERIFIED.

## Recipe (UNTESTED)
1. Cheapest test first: take the v1.8.0 RPM's prebuilt modules, put them under
   `/usr/lib/opensnitchd/ebpf/` on a test image, run the daemon as ROOT with
   `ProcMonitorMethod: ebpf`, read `/var/log/opensnitchd.log` for `[eBPF] module
   loaded` x3 and "Process monitor method ebpf". Non-CO-RE modules built against
   an older kernel's headers may mismatch 7.2 struct layouts (verifier error or
   wrong offsets), so a clean load is necessary but not sufficient: also check
   that connections resolve to the right process.
2. If those do not work, build in the image: `kernel-devel` for the image kernel
   (UNVERIFIED that a package matching `7.2.0-ogc6.1.fc44` exists for this custom
   kernel), clang + llvm (version UNVERIFIED), then the Makefile above; install
   under `/usr`, baked at build time.
3. Flip the tower's stage/verify checks (they hard-require `proc`) together with
   our config; keep the tested `proc` fallback.

## Risks and unknowns
- Struct-layout drift between the headers used to build and the running kernel.
- kernel-devel for the custom Bazzite kernel; clang/llvm version compatibility.
- debugfs/tracefs and FTRACE_SYSCALLS on the Bazzite kernel: unconfirmed.
- No CPU numbers for ebpf exist anywhere; the 40-70% and 20.5% figures were `proc`.

## First task for r16
Not a kernel investigation: run the daemon as root on the laptop with ebpf and the
RPM's modules and capture the full log (see the measurement-series note for how
to compare CPU/RSS after). It needs the owner (touches the firewall).
