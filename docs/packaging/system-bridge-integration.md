# System bridge integration (overlay, pending a new release)

This overlay is the privileged-host deployment of Snitchwatch. It is separate
from the existing `snitchwatch-bridge.service` **user** unit and does not
change that release contract. Do not enable both: the legacy unit uses TCP
`127.0.0.1:50051`; the system bridge is socket-activated only.

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
while at least one GUI is authenticated. The bridge clears it when the last
authenticated session ends and ignores a pause request that arrives with no
GUI attached, so a pause doesn't carry over to the next GUI. One race remains:
a pause still queued from a GUI that just left can apply if another GUI
authenticates first (stamping each pause with its sender's session
generation would close it). The bridge does not duplicate policy or translate reject into deny.

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

The staged overlay supplies sysusers, tmpfiles, one system service, and two
socket units. Enable the two `.socket` units in the image/preset; do not
enable the service directly. For OpenSnitch 1.8.0, configure
`Server.Address: unix:opensnitchd.sock` and give `opensnitch.service` this
drop-in so the relative socket address resolves inside the protected runtime
directory, after systemd has created the listener:

```ini
# /etc/systemd/system/opensnitch.service.d/snitchwatch-system-bridge.conf
[Unit]
Requires=snitchwatch-system-bridge-grpc.socket
After=snitchwatch-system-bridge-grpc.socket

[Service]
WorkingDirectory=/run/snitchwatch
```

The absolute `unix:///run/snitchwatch/opensnitchd.sock` URI does not work with
OpenSnitch 1.8.0's address parsing. Verify the daemon connects through the
relative address and working directory before changing the default action to
deny.

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
