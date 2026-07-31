# Lightweight install: rpm-ostree layering on stock Bazzite

This is the **lightweight / DIY** path for running Snitchwatch on an
unmodified Bazzite (or any Universal Blue / immutable Fedora) host. It layers
the OpenSnitch daemon onto the stock image with `rpm-ostree` instead of baking
it into a custom image.

If you would rather not touch your base image at all and prefer a
ready-to-boot, signed image with the daemon already baked in, use the
**batteries-included** path instead — see [`../../packaging/bluebuild/recipe.yml`](../../packaging/bluebuild/recipe.yml).
Neither path is "unsupported"; they are two supported shapes of the same
install, trading image immutability for convenience.

Both paths end in the same place: a host-side `opensnitchd` that fails
**closed** (`DefaultAction: deny`) and dials the Snitchwatch bridge's gRPC
server at `127.0.0.1:50051`, plus the GUI as a Flatpak and the bridge as a
systemd `--user` service.

---

## 1. Layer the OpenSnitch daemon

**Corrected 2026-07-31 (see issue #7):** opensnitch is *not* packaged in
Fedora's repos or Bazzite's COPR set — `rpm-ostree install opensnitch`
fails with "No match for argument". Layer the upstream GitHub release RPM
instead (v1.8.0 matches this repo's vendored submodule pin):

```bash
curl -LO https://github.com/evilsocket/opensnitch/releases/download/v1.8.0/opensnitch-1.8.0-1.x86_64.rpm
rpm-ostree install ./opensnitch-1.8.0-1.x86_64.rpm
systemctl reboot
```

Known caveat on kernels ≥ ~6.19: the RPM's bundled eBPF module
(`opensnitch.o`) may fail to load (see issue #6). If
`/var/log/opensnitchd.log` shows `unable to load eBPF module`, set
`"ProcMonitorMethod": "proc"` in `/etc/opensnitchd/default-config.json`.

After the reboot, confirm it layered:

```bash
rpm-ostree status --json | jq '.deployments[0]."requested-packages"'
# ... should list "opensnitch"
```

> The `opensnitch` package ships only the daemon we want. Do **not** install
> `opensnitch-ui` — Snitchwatch replaces the upstream GUI, and running both
> against the same daemon will fight over the UI gRPC channel. See the
> "coexistence" note at the bottom.

---

## 2. Ship the fail-closed daemon config

Stock OpenSnitch ships `DefaultAction: allow`, which means that **whenever no
UI is connected, the daemon silently allows all traffic** — the exact
fail-open behavior a firewall product should not have. Snitchwatch overrides
this with a fail-**closed** config that also points the daemon at the
bridge's default gRPC bind.

Copy the config shipped in this repo into place:

```bash
sudo install -Dm644 \
  packaging/bluebuild/files/system/etc/opensnitchd/default-config.json \
  /etc/opensnitchd/default-config.json
```

The two fields that matter, relative to upstream's defaults:

| Field            | Upstream default        | Snitchwatch value     | Why |
| ---------------- | ----------------------- | --------------------- | --- |
| `DefaultAction`  | `allow`                 | `deny`                | Fail **closed** when the decision channel (bridge/GUI) is down. |
| `Server.Address` | `unix:///tmp/osui.sock` | `127.0.0.1:50051`     | Dial the Snitchwatch bridge's default gRPC bind. |

> `/etc` is writable on an rpm-ostree system (it is part of the writable
> `/etc` overlay, not the read-only `/usr` tree), so this `install` needs no
> `usroverlay` and survives upgrades.

Restart the daemon so it picks up the new config:

```bash
sudo systemctl enable --now opensnitchd.service
sudo systemctl restart opensnitchd.service
```

Verify:

```bash
systemctl is-active opensnitchd.service      # -> active
grep -E '"DefaultAction"|"Address"' /etc/opensnitchd/default-config.json
```

---

## 3. Install the bridge as a systemd --user service

The bridge translates the daemon's gRPC protocol to the GUI's WebSocket
protocol. It runs **host-side** (not in the Flatpak sandbox) and as its own
`--user` service, deliberately decoupled from the GUI window — see the
fail-open stance in the packaging plan. Closing the GUI window must not take
the AskRule decision channel down.

Install the bridge binary and unit:

```bash
# Build the host-side bridge binary from this repo:
cargo build --release -p snitchwatch-bridge-cli
install -Dm755 target/release/snitchwatch-bridge-cli ~/.local/bin/snitchwatch-bridge-cli

# Install the user unit:
install -Dm644 packaging/systemd/snitchwatch-bridge.service \
  ~/.config/systemd/user/snitchwatch-bridge.service

# The shipped unit's ExecStart points at /usr/bin (the image-baked location).
# For this ~/.local/bin install, override it:
systemctl --user edit snitchwatch-bridge.service
#   [Service]
#   ExecStart=
#   ExecStart=%h/.local/bin/snitchwatch-bridge-cli

systemctl --user daemon-reload
systemctl --user enable --now snitchwatch-bridge.service
systemctl --user status snitchwatch-bridge.service      # -> active (running)
```

The bridge now listens on:

- gRPC `127.0.0.1:50051` (opensnitchd dials in here),
- a WS Unix domain socket at `$XDG_RUNTIME_DIR/snitchwatch/bridge.sock`,
- with a handshake token at `$XDG_RUNTIME_DIR/snitchwatch/token`.

---

## 4. Install the GUI Flatpak

```bash
# From a locally built bundle (see packaging/README.md for the build step):
flatpak install --user --bundle build/org.snitchwatch.Snitchwatch.flatpak

flatpak run org.snitchwatch.Snitchwatch
```

The Flatpak is granted `--filesystem=xdg-run/snitchwatch`, so the sandboxed
GUI can reach the bridge's Unix socket + token in step 3 — no network
permission needed or granted.

---

## 5. End-to-end verification

Confirm the whole chain, including the fail-open fix:

```bash
# Daemon up and fail-closed:
systemctl is-active opensnitchd.service
grep '"DefaultAction": "deny"' /etc/opensnitchd/default-config.json

# Bridge up independently of the GUI window:
systemctl --user is-active snitchwatch-bridge.service

# AskRule still round-trips with the GUI window CLOSED — poke the bridge WS
# directly with a raw client (token first, per the Unix-socket handshake):
TOKEN=$(cat "$XDG_RUNTIME_DIR/snitchwatch/token")
{ printf '%s\n' "$TOKEN"; cat; } \
  | websocat ws-c:unix:"$XDG_RUNTIME_DIR/snitchwatch/bridge.sock":/stream
```

Closing the GUI window must NOT stop `snitchwatch-bridge.service`; the daemon
therefore keeps its interactive AskRule channel and only ever falls back to
`DefaultAction: deny` on a genuine bridge outage.

---

## Coexistence with upstream opensnitch-ui

If the stock `opensnitch-ui` is also installed and autostarting, it will
compete with the Snitchwatch bridge for the daemon's UI gRPC channel. Detect
and disable it:

```bash
# Is the upstream GUI layered / present?
rpm -q opensnitch-ui 2>/dev/null && echo "opensnitch-ui present — disable its autostart"

# Disable its autostart (per-user):
rm -f ~/.config/autostart/opensnitch_ui.desktop
```

The bridge's `ui.proto` is vendored and may drift from a differently-versioned
upstream `opensnitch-ui`; running exactly one UI client against the daemon
avoids both the channel contention and any protocol-skew surprises.
