# System bridge integration (overlay, pending a new release)

This overlay is the privileged-host deployment of Snitchwatch, and the only
deny-safe one: the image path from r15 on (owner decision 2026-10-16, #35).
It is separate from the existing `snitchwatch-bridge.service` **user** unit
and does not change that release contract. Do not enable both: the legacy
unit uses TCP `127.0.0.1:50051`, which any local process can bind first, so
it is not deny-safe; the system bridge is socket-activated only.

## Trust boundary

`opensnitchd` runs as root and connects only to
`/run/snitchwatch/opensnitchd.sock` (root:root, `0600`). systemd creates that
socket and its root-owned, non-writable parent; the unprivileged
`snitchwatch` bridge receives an already-open descriptor and cannot replace
the pathname. There is no production TCP fallback.

The GUI receives a separate socket, `/run/snitchwatch/bridge.sock`
(root:`snitchwatch-ui`, `0660`). Administrators opt a desktop account in with
`usermod -aG snitchwatch-ui <user>` and the user must start a new login
session. Membership grants authority to make firewall decisions through the
GUI; it is deliberately not a per-user isolation boundary.

The temporary token in `/run/snitchwatch-auth/token` is group-readable only
for compatibility with existing GUI clients. It is not an additional factor:
any member that can connect to the GUI socket can read it. The socket's group
mode is the authorization boundary; a future protocol revision can remove the
token handshake in system mode.

## Unattended requests

The reviewed follow-up tracks authenticated external WebSocket sessions,
registered after token validation and a successful authentication
acknowledgement. Internal broadcast subscribers do not establish GUI presence.
An Ask without an authenticated session returns gRPC `Unavailable` before
creating a pending row, so OpenSnitch applies its own configured default
action. This holds while filtering is paused too: a pause only auto-allows
while at least one GUI is authenticated. A pause always lasts 5 minutes, 30
minutes or 1 hour and then ends on its own. The bridge also clears it when
the last authenticated session ends, and it ignores a pause request whose
sender's session generation has ended: each request is stamped with the
generation its WebSocket session authenticated under, so a pause still queued
from a GUI that just left doesn't apply even if another GUI authenticates
first. The bridge logs the uid of each session that changes the pause. The
bridge does not duplicate policy or translate reject into deny.

When the last client disconnects, existing pending requests are canceled even
if a new client immediately reconnects. RPC cancellation removes its pending
row and broadcasts removal; late verdicts cannot create a rule or history
entry. An authenticated but silent GUI
still uses the daemon's existing RPC deadline.

These runtime changes are reviewed uncommitted work on base `2690109`; they
are not in the existing published release. Disposable-VM no-GUI and
last-external-client checks, a fresh headless boot, and synthetic protected-IPC
RPC cancellation passed with `DefaultAction: allow` and enforcing SELinux.
Conditional runtime and authorization checks passed on the exact new artifacts;
default KDE startup
also exited with `QWidget: Cannot create a QWidget without QApplication`
(the GUI uses `QGuiApplication`). The reviewed bundle rendered with guest-only
Wayland, software Qt Quick, Basic controls and generic platform-theme overrides;
the exact sole GUI's disconnect then completed real daemon/curl fallback within
76 ms of kill initiation and returned pending to zero within 75 ms. Those
conditional tests do not establish a default startup fix. This startup defect
and OpenSnitch 1.8.0's observed `nfq_close` shutdown crash remain unresolved
rollout gates.

In that conditional environment, independently reviewed rendered Allow
completed the exact daemon/curl request in 521 ms with a matching allowed row
and zero pending; the observer sent no verdict. The same GUI also survived
bridge restart/token change and authenticated again. Those results are separate
from the helper-only token-rotation timing.

The reviewed GUI binary also rejected a nonmember's token access in a bounded
probe. Both tested accounts had read-only IPC/auth mounts, with denied mutation
attempts. Resolved, pending and interrupted daemon-stop samples were captured;
the pending sample retained both queue watchdog warnings and an unsuccessful
`nfq_close` message despite exit zero. The final resolved stop after actual GUI
Allow also retained one queue watchdog warning, the `nfq_close` message and an
nftables netlink `operation not permitted` error despite no live Ask.
These results do not resolve the known
daemon shutdown race. The retained KDE 6.9 runtime also produced an end-of-life
warning at guest installation; a supported SDK/runtime needs build and runtime
validation before release.


## Stage into an image

Use `packaging/system/stage.sh`, not a live installer. It validates a
previously verified SHA-256, checks that the selected binary advertises
`SNITCHWATCH_SYSTEM_BRIDGE=1`, then writes only beneath the supplied staging
root:

```bash
packaging/system/stage.sh /path/to/image-root \
  /path/to/verified/snitchwatch-bridge-cli <verified-sha256>
```

The staged overlay supplies sysusers, tmpfiles, one system service, two
socket units, and the one packaged opensnitchd rule.

Both shipped opensnitchd configs set `"ProcMonitorMethod": "ebpf"`. The
bazzite-tower image overwrites it to `"proc"` after `stage.sh` runs: eBPF
failed to load on the owner's laptop kernel before (snitchwatch#6), so it
stays off until there is evidence it loads on kernel 7.2. The BTF
requirement is explained in
`docs/superpowers/specs/2026-07-14-daemon-kernel-diagnostics-design.md`.

`/etc/opensnitchd/rules/000-snitchwatch-bridge-fetch.json` lets this
account's bridge download blocklists over HTTPS (TCP 443) under
`DefaultAction: deny`. Denies and blocklists still win; see
[`../../packaging/README.md`](../../packaging/README.md#the-packaged-fetch-rule).
After the first boot, confirm that
`sudo grep 'Error compiling list rule' /var/log/opensnitchd.log` (the shipped config sets `LogFile: /var/log/opensnitchd.log`, so the journal won't show it) finds nothing for it. Enable the two `.socket` units in the image/preset; do not
enable the service directly.

The overlay also stages the daemon's config and its drop-in, as one artifact
with the socket they dial (PR #166 H1):

- `/etc/opensnitchd/default-config.json` (source:
  `packaging/system/opensnitchd-default-config.json`): `DefaultAction: deny`
  and `Server.Address: unix:opensnitchd.sock`. It replaces the bluebuild
  recipe's own per-user TCP config, so copy the staged tree into the
  image after the recipe's `files` module.
- `/usr/lib/systemd/system/opensnitch.service.d/snitchwatch-system-bridge.conf`
  (source: `packaging/system/opensnitch-snitchwatch-system-bridge.conf`), so
  the relative address resolves inside the protected runtime directory, after
  systemd has created the listener:

```ini
[Unit]
Wants=snitchwatch-system-bridge-grpc.socket
After=snitchwatch-system-bridge-grpc.socket

[Service]
WorkingDirectory=-/run/snitchwatch
```

Whether the absolute `unix:///run/snitchwatch/opensnitchd.sock` URI works
with OpenSnitch 1.8.0 is **unverified**. Commit 2690109 recorded that it
didn't; a PR #166 review reads `config_utils.go` as stripping only `unix:`
and gRPC's passthrough dialing the absolute path, as upstream's own default
`unix:///tmp/osui.sock` does. On a test VM, try the absolute address first;
if the daemon connects through it, switch the config to it, so the address
no longer depends on the drop-in's working directory. Either way, verify the
daemon connects before rolling the image out.

### Fail closed, and console recovery

`Wants=` (not `Requires=`) and the `-` on `WorkingDirectory` are deliberate
(owner decision 2026-10-16): if the socket or `/run/snitchwatch` is missing,
`opensnitch.service` still starts, has no UI to dial, and applies
`DefaultAction: deny`. Nothing is let through, and nothing outside the rules
gets out either, so the desktop may have no network until the bridge is back.
The bridge can't report this itself: without its socket it isn't running,
so neither the GUI nor its diagnostics can see it. From a console (a VT
login works without network), in this order:

```bash
# 1. Look.
systemctl status snitchwatch-system-bridge-grpc.socket snitchwatch-system-bridge.service
ls -ld /run/snitchwatch /run/snitchwatch-auth   # root root 0711; snitchwatch snitchwatch-ui 2750
ls -l /run/snitchwatch/                         # opensnitchd.sock: root root, srw-------
journalctl -b -u snitchwatch-system-bridge.service -u snitchwatch-system-bridge-grpc.socket

# 2. Recreate the runtime directories (the overlay's
#    /usr/lib/tmpfiles.d/snitchwatch.conf); a no-op if they exist.
sudo systemd-tmpfiles --create snitchwatch.conf

# 3. Clear the failed state and start both sockets.
sudo systemctl reset-failed snitchwatch-system-bridge.service \
  snitchwatch-system-bridge-grpc.socket snitchwatch-system-bridge-gui.socket
sudo systemctl start snitchwatch-system-bridge-grpc.socket snitchwatch-system-bridge-gui.socket
ls -l /run/snitchwatch/opensnitchd.sock         # must exist now

# 4. Restart the daemon. Required, not optional: its working directory is
#    fixed when it starts, so a daemon that started while /run/snitchwatch
#    was missing stays in / and keeps dialing /opensnitchd.sock until it is
#    restarted, even with the directory and socket back.
sudo systemctl restart opensnitch.service
```

The bridge service has `StartLimitIntervalSec=0` (PR #166 L1): a crash loop
keeps retrying every 2 s (`RestartSec`) instead of hitting systemd's start
limit and leaving the bridge and its sockets failed until someone runs the
`reset-failed` above. A bridge that crashes on every start still needs the
journal and a fix; under `deny` the network stays closed meanwhile.

### Before rebooting into the image: the `/etc` merge

An image update keeps a locally edited `/etc` file: ostree's three-way merge
leaves an `/etc/opensnitchd/default-config.json` changed on the host (for
example by the old per-user setup, or by the daemon saving a CHANGE_CONFIG)
as it was, on `127.0.0.1:50051`, instead of taking the image's. Before
rebooting into the new deployment:

```bash
sudo ostree admin config-diff | grep opensnitchd
jq .Server.Address /etc/opensnitchd/default-config.json
```

If `config-diff` lists `M etc/opensnitchd/default-config.json` (locally
modified), the merge will keep it. Undo the local change before rebooting:

```bash
sudo cp /usr/etc/opensnitchd/default-config.json /etc/opensnitchd/default-config.json
sudo ostree admin config-diff | grep opensnitchd   # now lists nothing
```

`/usr/etc` holds the booted deployment's pristine copy; with `/etc` equal to
it again, the merge at reboot (rpm-ostree finalizes staged deployments at
shutdown) takes the new image's file. After the reboot,
`jq .Server.Address /etc/opensnitchd/default-config.json` must print
`"unix:opensnitchd.sock"`. If it doesn't (the change was made after the
check, or deployments weren't staged), repeat the `cp` from the new
deployment's `/usr/etc` from a console and `sudo systemctl restart
opensnitch.service`. Never leave `127.0.0.1:50051` with `deny`: that is the
squattable transport (#35).

## GUI profile, migration, and verification

Existing releases continue to use the primary
`packaging/flatpak/org.snitchwatch.Snitchwatch.yml` profile and its per-user
`xdg-run/snitchwatch` bridge. For the system deployment, build/install the
alternative `packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml` profile;
it has the same app-id and therefore replaces rather than coinstalls with the
legacy profile. It grants read-only `/run/snitchwatch` and
`/run/snitchwatch-auth`, and sets `SNITCHWATCH_SYSTEM_BRIDGE=1` for the GUI.

The reviewed manifests declare checksum-pinned protoc 29.3 and the Rust SDK
extension's mold linker, with compiler and debug tooling removed before export.
Both profiles retain their existing runtime permissions. See
[`../../packaging/flatpak/README.md`](../../packaging/flatpak/README.md) for
pinned Cargo-source generation and clean build preparation. An isolated x86_64
system-profile source build passed with Qt 6.9.3 and Rust 1.89.0 from reviewed
snapshot SHA-256 `851f3a5647c373fe88477b0dddb63b5bc980084611f1d0b934f5ae82dd50cf07`.
That snapshot includes the reviewed uncommitted runtime/tool changes; it does
not establish release publication or immutable image installation. Guest
installation reported the retained KDE 6.9 runtime as end-of-life; rebuild and
repeat release validation with a supported SDK/runtime before publication.

For a native GUI launch instead, select the same profile explicitly:

```bash
SNITCHWATCH_SYSTEM_BRIDGE=1 snitchwatch-kirigami
```

Its process still needs host `snitchwatch-ui` group membership to connect.
Before relying on this setup, test the actual Flatpak on the target image: a
group member must connect and a non-member must receive `EACCES`; the sandbox
must not be able to create or replace either socket path. Do not add
`--share=network` as a workaround.

This is an overlay until a bridge release packages these files and the daemon
configuration migration together. The published user-service release remains
unchanged for existing installations.

Broad recommended DNS uses the authenticated ticket and durable consent protocol
documented in [curated broad opt-in](../bridge/curated-broad-opt-in.md).
