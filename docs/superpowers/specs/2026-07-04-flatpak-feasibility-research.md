# Flatpak distribution feasibility research — does Phase 2 actually work?

**Date:** 2026-07-04
**Status:** Research complete. **Finds a real design flaw in `IMPLEMENTATION_PROMPT.md`
Phase 2 as currently scoped — not just a caveat.** See Verdict.
**Audience:** Whoever implements Phase 2 packaging, and the human owner.

## Summary

Phase 2 assumes the Flatpak-sandboxed GUI/bridge reaches opensnitchd (and
each other, depending on final architecture) over **TCP loopback**
(`127.0.0.1:50051` gRPC, `127.0.0.1:3031` WS, per `README.md`). This
research found, with primary-source confirmation, that **TCP loopback
across the Flatpak sandbox boundary is not free** — Flatpak's default
sandbox gets its own **private, isolated network namespace with its own
private loopback device**, not a view onto the host's loopback. Reaching
(or being reached from) anything on the host's `127.0.0.1` requires
`--share=network`, which grants **full, unrestricted internet access**,
not a scoped "loopback only" permission — Flatpak has no partial network
grant. This directly undercuts the audit's original claim that
Flatpak-sandboxing the GUI/bridge is a clean win because "only opensnitchd
needs elevated access" — the sandboxing benefit is real for *capabilities*
(no `CAP_NET_RAW`), but not for *network exposure*, as currently
architected. The fix is concrete and cheap: move whatever IPC crosses the
sandbox boundary onto a **Unix domain socket** instead of TCP, gated by
`--filesystem=xdg-run/<name>` (a real, well-precedented Flatpak permission,
not a workaround) instead of `--share=network`.

## 1. Loopback network access from inside a Flatpak sandbox

**Finding: TCP loopback does NOT cross the sandbox boundary by default —
initial research led me to the wrong conclusion here on the first pass,
worth recording since it's an easy mistake to make.**

The Flatpak/bubblewrap sandbox, absent `--share=network`, creates **"a
private network namespace with only an ipv4 loopback device"** (confirmed
directly from the
[flatpak/flatpak wiki "Sandbox" page](https://github.com/flatpak/flatpak/wiki/Sandbox)).
The ArchWiki's bubblewrap page states this even more explicitly:
**"Unsharing the network namespace prevents an application from accessing
any network, including localhost."** This private loopback device exists
so an app's *own internal* multi-process IPC over `127.0.0.1` still works
inside the sandbox — it is not connected to the host's network stack in
any way. A service bound to the host's `127.0.0.1:50051` is simply
unreachable from a process inside a namespace-isolated sandbox, and the
reverse is equally true: a service bound *inside* such a sandbox is not
reachable from the host, either. Network namespace isolation is
bidirectional.

`--share=network` doesn't add "loopback access" as an incremental grant —
it **shares the host's network namespace entirely**, per
[Flatpak's sandbox permissions docs](https://docs.flatpak.org/en/latest/sandbox-permissions.html):
"if you grant network access then the app will get full network access
... there are no partial access options for network access." Once granted,
the sandboxed process's sockets are directly visible to the host's network
stack (and vice versa) exactly like any other unsandboxed process — which
is also why this single permission is what makes host-loopback reachable
at all, with no way to scope it down to "just 127.0.0.1, not the internet."

**Concrete implication for this repo:** whichever process runs inside the
Flatpak (the GUI, and/or the bridge, depending on final architecture — see
§4/Verdict) cannot reach `127.0.0.1:50051`/`127.0.0.1:3031` on the host
without `--share=network`, and that permission is not scoped to loopback —
it's the same grant a torrent client or web browser would request.

## 2. Reading the Phase 1 auth token file across the sandbox boundary

**Finding: this part works cleanly, via a standard, well-precedented
Flatpak permission — no problem here, unlike §1.**

Flatpak's filesystem sandbox is separate from its network sandbox.
`--filesystem=xdg-run/<name>` grants access to a specific named
subdirectory of `$XDG_RUNTIME_DIR` (i.e. `$XDG_RUNTIME_DIR/<name>`, which
expands to `/run/user/$UID/<name>` in the normal case) — a static,
declarative permission requested in the manifest's `finish-args`, approved
at Flathub review time like any other `finish-args` entry, not something
requiring a runtime interactive grant beyond ordinary install consent. If
the Phase 1 token (and, per the fix below, a Unix socket) live under
`$XDG_RUNTIME_DIR/snitchwatch/`, a single `--filesystem=xdg-run/snitchwatch:ro`
(or without `:ro` if the socket itself needs write access — sockets need
read-write to `connect()`, so drop `:ro` if both token and socket share
the directory) line covers reading the token file cleanly.

**One caveat worth a footnote, not a blocker:** [a Flatpak issue](https://github.com/flatpak/flatpak/issues/4372)
documents that on hosts with a *non-default* `$XDG_RUNTIME_DIR` (not
`/run/user/$UID`), the sandbox can inherit the host's env var value
without correctly remapping it, breaking runtime-dir-relative lookups.
This is an edge case for non-standard systemd configurations, not
something a standard Bazzite install (a normal systemd-managed OS) would
hit — but worth a defensive check (verify `$XDG_RUNTIME_DIR` resolves
consistently) during Phase 2's actual packaging work rather than assumed
away.

## 3. Precedent

**Strong, exact-shape precedent found: `virt-manager`'s official Flathub
manifest.** virt-manager is a sandboxed GUI client that talks to a
privileged host-level daemon (`libvirtd`) it does not bundle — the same
shape as Snitchwatch's GUI/bridge talking to host-level opensnitchd. Its
actual `finish-args` (fetched directly from
[`flathub/org.virt_manager.virt-manager`](https://github.com/flathub/org.virt_manager.virt-manager)):

```yaml
# Access to the UNIX socket for the local libvirtd system instance
- --filesystem=/run/libvirt
# Access to the UNIX socket for the local libvirtd session instance
- --filesystem=xdg-run/libvirt
```

This confirms the `--filesystem=xdg-run/<name>` pattern is real,
production-proven, and Flathub-approved — not a hypothetical workaround.
**Important nuance:** virt-manager reaches libvirtd over a **Unix domain
socket** (`/run/libvirt/libvirt-sock`), via filesystem permissions, not
TCP loopback via network permissions — this is exactly the distinction
that matters for the Verdict below. (virt-manager's manifest *also*
requests `--share=network`, but that's for its separate feature of
connecting to *remote* libvirt hosts over SSH/TCP — unrelated to, and not
required for, its local-socket case.)

**Negative precedent found, and why it doesn't actually apply here:**
[Mullvad VPN is documented as "essentially incompatible" with Flatpak](https://github.com/mullvad/mullvadvpn-app/discussions/8901),
but the cited reasons are missing `CAP_NET_ADMIN`, blocked `/dev/net/tun`
access, and D-Bus restrictions — i.e., Mullvad's problem is that **its own
VPN daemon would need to run inside the sandbox** with kernel-level
network privileges Flatpak specifically withholds. That is a different
shape from Snitchwatch: opensnitchd (the privileged party) is never
proposed to run inside the Flatpak — it stays host-side, exactly per
`AUDIT.md` decision #3. Mullvad's blocker doesn't transfer to this
project's architecture; it's a useful negative example precisely because
it clarifies *why* it doesn't apply (the privileged component is
correctly kept outside the sandbox here).

**No direct precedent found for a Little-Snitch-style firewall shipped as
a Flatpak** — Portmaster (Safing), the closest comparable Linux
application-firewall product, explicitly has no Flatpak or Snap build as
of this research. This is a gap in the field, not a contradiction of
feasibility — it just means there's no existing "firewall GUI as Flatpak"
manifest to copy wholesale; virt-manager's daemon-over-socket pattern is
the best transferable precedent available.

## 4. `cxx-qt`/Kirigami inside the Flatpak KDE runtime

**Finding: the KDE runtime side is mature and well-trodden; the
`cxx-qt`-specific angle is unconfirmed either way (absence of evidence,
not evidence of absence) — treat as low-risk but unverified.**

`org.kde.Platform`/`org.kde.Sdk` are KDE's own maintained Flatpak
runtimes ([`KDE/flatpak-kde-runtime`](https://github.com/KDE/flatpak-kde-runtime)),
already used by a large number of shipped KDE Flatpaks on Flathub,
providing Qt6, KDE Frameworks, Kirigami, and `extra-cmake-modules` —
exactly what a Kirigami app needs, with no cxx-qt-specific gap reported in
any search result. Since `cxx-qt-build` needs CMake alongside Cargo (per
the earlier `2026-07-04-cxx-qt-feasibility-research.md` finding), and
`org.kde.Sdk` already ships CMake + ECM + Qt6 dev packages, the toolchain
side should compose cleanly in principle. The remaining, genuinely
Rust-specific (not cxx-qt-specific) complexity is the standard
Cargo-in-Flatpak vendoring requirement — `flatpak-builder`'s sandboxed
build has no network access mid-build, so Cargo dependencies must be
pre-vendored via `flatpak-cargo-generator.py`
(`flatpak/flatpak-builder-tools`), a solved, general pattern for any
Rust Flatpak, not something specific to this project or to `cxx-qt`. **No
existing Flatpak that combines `cxx-qt` + Kirigami was found during this
research** — this remains a genuine unknown, not a confirmed-safe
combination, and should be a checkpoint during Phase 3b's actual
packaging work, not assumed clean by extrapolation from "KDE Flatpaks
generally work fine."

## Verdict

**Phase 2's distribution model does NOT work cleanly as currently scoped,
and this needs a concrete design change — not just a caveat added to the
existing plan.**

The problem: `IMPLEMENTATION_PROMPT.md` Phase 2 (and the underlying
`README.md`/design-spec architecture) assumes TCP loopback (gRPC on
`127.0.0.1:50051`, WS on `127.0.0.1:3031`) as the transport for whatever
IPC crosses the Flatpak sandbox boundary. Per §1, that forces
`--share=network` on the Flatpak manifest — the same all-or-nothing
full-internet-access grant a torrent client needs. This is a real
regression against the audit's own stated rationale for Flatpak-sandboxing
Component A in the first place ("the GUI and bridge can be
Flatpak-sandboxed" as a security win) — a Flatpak that must request full
network access to talk to its own local daemon has given up most of the
network-exposure containment the sandbox was supposed to provide, even
though it still correctly avoids raw-socket/netfilter *capabilities*.

**Concrete required change:** move whichever IPC actually crosses the
sandbox boundary onto a **Unix domain socket** under
`$XDG_RUNTIME_DIR/snitchwatch/`, exposed via
`--filesystem=xdg-run/snitchwatch` — the exact same permission mechanism
already needed for the Phase 1 token file (§2), so this can be a single
unified grant covering both the token and the socket, not two separate
asks. This is not a hypothetical fix; it's the same mechanism
`virt-manager`'s Flathub-approved manifest already uses for its structurally
identical daemon-over-socket case (§3).

**This surfaced a second issue while researching it: Phase 1/Phase 2's
own architecture isn't settled on which IPC actually crosses the
boundary, and the answer changes what needs fixing:**

- If the bridge runs **host-side as its own systemd user service**
  (per `IMPLEMENTATION_PROMPT.md` Phase 2's fail-open resolution — bridge
  decoupled from the GUI window, autostarted independently), then only the
  **GUI-to-bridge WS connection** crosses the sandbox boundary. This is
  entirely within this project's control to fix: `axum`/`tokio` support
  serving over a `UnixListener` as cleanly as a `TcpListener`, so
  converting `snitchwatch-bridge`'s WS server to a Unix socket is a
  contained, low-risk change.
- If instead the bridge runs **in-process inside the sandboxed GUI**
  (as sketched in this session's own
  `docs/superpowers/plans/2026-07-04-kirigami-shell-rewrite.md`, which
  proposed the Kirigami shell link `snitchwatch-bridge` as a library and
  bypass the WS layer entirely for the native shell), then the boundary
  crossing becomes the **reverse direction**: host-side opensnitchd
  dialing *into* the bridge's gRPC server, which would now be bound inside
  the sandbox. Fixing that requires opensnitchd's own gRPC client to dial
  a Unix socket path instead of a TCP address (via its `Server.Address`
  config) — **this is NOT confirmed to be something opensnitchd supports**
  and is outside this project's control to fix if it doesn't. **This is a
  real, unresolved tension between the Kirigami rewrite plan's proposed
  in-process architecture and Phase 2's already-decided fail-open
  resolution — the two documents currently assume different things about
  where the bridge process lives, and that has to be reconciled before
  Phase 3b implementation touches this, not discovered mid-build.**

**Recommended concrete changes to the two documents:**
1. `IMPLEMENTATION_PROMPT.md` Phase 2 should explicitly specify Unix-domain-socket
   transport (not TCP loopback) for whatever crosses the Flatpak boundary,
   sharing the `--filesystem=xdg-run/snitchwatch` grant with the Phase 1
   token.
2. Before Phase 3b implementation, resolve whether the bridge is
   host-side (matching Phase 2's fail-open decision, and the easier fix —
   convert the WS server to UDS, fully within this project's control) or
   in-process with the Kirigami GUI (per the rewrite plan's current
   sketch, and the harder fix — depends on unverified opensnitchd UDS
   support). **My recommendation, stated plainly: keep the bridge
   host-side as a systemd user service, matching the already-decided
   fail-open resolution, and update the Kirigami rewrite plan's Task 13
   to have the shell connect to the bridge over a WS-over-Unix-socket
   client instead of linking the bridge in-process.** This avoids betting
   the sandboxing fix on an unverified opensnitchd capability.

## Sources

- [Flatpak — Sandbox Permissions documentation](https://docs.flatpak.org/en/latest/sandbox-permissions.html)
- [flatpak/flatpak wiki — Sandbox](https://github.com/flatpak/flatpak/wiki/Sandbox)
- [ArchWiki — Bubblewrap](https://wiki.archlinux.org/title/Bubblewrap)
- [GNOME blog — The flatpak security model, part 2 (Alexander Larsson)](https://blogs.gnome.org/alexl/2017/01/20/the-flatpak-security-model-part-2-who-needs-sandboxing-anyway/)
- [flathub/org.virt_manager.virt-manager](https://github.com/flathub/org.virt_manager.virt-manager) —
  manifest `finish-args` fetched directly from
  `org.virt_manager.virt-manager.yaml`
- [mullvad/mullvadvpn-app discussion #8901 — Flatpak support](https://github.com/mullvad/mullvadvpn-app/discussions/8901)
- [Safing Portmaster — API/settings docs](https://docs.safing.io/portmaster/api) and
  [Portmaster GitHub](https://github.com/safing/portmaster) (no Flatpak build found)
- [flatpak/flatpak issue #4372 — `$XDG_RUNTIME_DIR` not remapped in sandbox](https://github.com/flatpak/flatpak/issues/4372)
- [KDE/flatpak-kde-runtime](https://github.com/KDE/flatpak-kde-runtime)
- This session's `docs/superpowers/specs/2026-07-04-cxx-qt-feasibility-research.md`
  and `docs/superpowers/plans/2026-07-04-kirigami-shell-rewrite.md`, cross-referenced
  for the bridge-location tension noted in the Verdict.
