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
enable the service directly. `opensnitchd` must be configured with
`Server.Address: unix:///run/snitchwatch/opensnitchd.sock` before the default
action is changed to deny.

## GUI profile, migration, and verification

Existing releases continue to use the primary
`packaging/flatpak/org.snitchwatch.Snitchwatch.yml` profile and its per-user
`xdg-run/snitchwatch` bridge. For the system deployment, build/install the
alternative `packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml` profile;
it has the same app-id and therefore replaces rather than coinstalls with the
legacy profile. It grants read-only `/run/snitchwatch` and
`/run/snitchwatch-auth`, and sets `SNITCHWATCH_SYSTEM_BRIDGE=1` for the GUI.

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
