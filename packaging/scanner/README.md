# Deep Scan host integration

The Flatpak contains only the GUI. Install these host artifacts together: the
one-shot privileged scanner, the unprivileged session-bus broker, its activation
file, the polkit policy, and the private scanner state directory. Installing
`pkexec` inside the Flatpak cannot provide host access.

Build the host binaries from the merged, reviewed scanner/broker implementation,
then stage an image/package. Update the matching GUI and host artifacts together:

```sh
cargo build --release -p scanner-privileged -p scanner-broker
bash packaging/scanner/stage.sh /tmp/snitchwatch-scanner-package \
  target/release/snitchwatch-scanner-privileged VERIFIED_SCANNER_SHA256 \
  target/release/snitchwatch-scanner-broker VERIFIED_BROKER_SHA256
```

Use SHA256 values verified against the intended build artifacts. The staging
script refuses `/` and never runs supplied executables. Install the staged tree
through the host's image/package mechanism, with all files owned by root and
directories writable only by root. It is not a Flatpak install. On an immutable
Bazzite host this means adding the staged tree to the image (after the existing
bridge overlay) or a host RPM, then deploying that image/package. Run
`systemd-tmpfiles --create snitchwatch-scanner.conf` during host provisioning.
The host must provide `/usr/bin/pkexec`, polkit and a desktop authentication
agent. Scanner check tools (`rpm`, `rpm-ostree`, `modinfo`, `chkrootkit`) remain
host dependencies; unavailable checks are explicitly skipped in the report.

Rebuild/update the GUI Flatpak with the matching manifest: both profiles grant
only `--talk-name=org.snitchwatch.Scanner`. No arbitrary host execution, host
filesystem, system-bus or network grant is added. Restart the GUI after updating.
From a host terminal a manual scan remains available with
`pkexec /usr/libexec/snitchwatch-scanner-privileged --json`.

The host session service exposes `org.snitchwatch.Scanner1.RunDeepScan()` at
`/org/snitchwatch/Scanner`. It takes no arguments and invokes only the fixed
scanner path with `--json` through `/usr/bin/pkexec`; it accepts no caller paths,
environment, shell fragments or database locations. Polkit retains active-session
administrator authentication with cached authorization (`auth_admin_keep`);
inactive and other sessions are denied. The broker returns typed status plus
JSON/error text, limits reports to 4 MiB and stderr to 64 KiB, rejects overlapping requests,
waits at most 10 minutes (including authentication), and exits after 30 idle
seconds. The root scanner has a separate 9-minute process-group watchdog because
an unprivileged broker cannot kill an already elevated process. There is no
persistent root service. Elevated `--db` overrides are rejected, including during
cached authorization; unprivileged development scans retain the override.

The scanner reserves stdout for the JSON report and sends diagnostics to stderr.
Before its watchdog kills the scan group, it attempts one nonblocking, atomic
timeout marker on the broker's stderr pipe. The broker reports a watchdog timeout
only when that exact marker accompanies SIGKILL. If the pipe is full or unavailable,
the watchdog still kills the group; the broker reports a generic failure because
the termination cause is unknown. Unmarked SIGKILL, including possible OOM kills,
is never assumed to be a timeout.

The broker is a new privilege-adjacent interface: review it and the host package
before deployment. Automated tests use fake executables and a private D-Bus
session; they do not request real authorization or run a root scan. Live desktop
authentication and the final host installation need a deployment smoke test.

Official references: [Flatpak permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html),
[D-Bus service activation](https://dbus.freedesktop.org/doc/dbus-specification.html#message-bus-starting-services),
and [pkexec security and policy arguments](https://www.freedesktop.org/software/polkit/docs/latest/pkexec.1.html).
