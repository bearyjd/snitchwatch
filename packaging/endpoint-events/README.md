# Sharing connection events with Vigie (opt-in)

Off by default. Nothing here is installed by the image, the system overlay or
any staging script. Enabling it is an operator step, like the `deny` opt-in.
Design: [`docs/superpowers/specs/2026-10-26-vigie-endpoint-events.md`](../../docs/superpowers/specs/2026-10-26-vigie-endpoint-events.md)
(#231). Vigie side: bearyjd/vigie-network#21.

```
opensnitchd --(remote logger, JSON lines, TCP 127.0.0.1:514)--> rsyslog --(allowlist, TLS)--> Vigie
```

No Snitchwatch code is in the data path. The bridge, GUI, socket, token and
Flatpak sandbox are unchanged. Vigie never decides anything on this computer.

## Files

| File | Goes to | What it is |
| ---- | ------- | ---------- |
| [`opensnitchd-logger.json`](opensnitchd-logger.json) | merged into `Server.Loggers` of `/etc/opensnitchd/default-config.json` | the daemon's `remote` logger: JSON, TCP, `127.0.0.1:514` |
| [`snitchwatch-endpoint-events.conf`](snitchwatch-endpoint-events.conf) | `/etc/rsyslog.d/` | loopback listener, field allowlist, TLS forward to Vigie |
| [`verify.sh`](verify.sh) | nowhere | `just endpoint-events-verify`: the real-rsyslog proof below |

## What leaves this computer, and what doesn't

The daemon's event is the whole connection, including each process's
environment and arguments, which can hold tokens and passwords. The drop-in's
template forwards **only**:

- `time` (when rsyslog received the event), `host_id` (yours, see below);
- `protocol`, `src_ip`, `src_port`, `dst_ip`, `dst_port`, `dst_host`;
- `process_path`, `user_id`;
- `action`, `rule`, `type` (the daemon's `Action`, `Rule`, `Type`; rsyslog
  lowercases template output names).

It never forwards `process_env`, `process_args`, `process_cwd`,
`process_tree` or `process_checksums`, nor `process_id`. Only connection events
(`Type` 0) that parse as JSON are forwarded; anything else on the listener is
dropped. The `packaging_shape` tests pin this, and `verify.sh` proves it
against real rsyslog with a fake token in the environment and arguments.

Each event goes out as one JSON object per RFC 5425 (octet-counted) frame over
TLS, with no syslog header. The server must present a certificate signed by
the pinned CA and carrying the configured name (`x509/name`); there is no
fallback to plain TCP.

## Why port 514 on loopback

- **Loopback only.** The daemon's `remote` logger is plain TCP or UDP, with no
  TLS (`vendor/opensnitch/daemon/log/loggers/remote.go:165-172`), so it must
  never cross the LAN. rsyslog listens on `127.0.0.1` only and does the TLS.
- **Privileged port.** 514 is below `net.ipv4.ip_unprivileged_port_start`
  (1024 by default), so an unprivileged local process can't bind it first and
  receive the stream. That is the #35 squatting problem a port like 50051 has.
  Only root (rsyslog runs as root on Fedora) can bind it.
- **SELinux.** On Fedora 44's targeted policy TCP 514 is labelled
  `rsh_port_t`, not `syslogd_port_t`, but the policy explicitly allows it:
  `sesearch -A -s syslogd_t -t rsh_port_t -c tcp_socket` prints
  `allow syslogd_t rsh_port_t:tcp_socket { name_bind name_connect };`
  (checked in the pinned Fedora 44 image, 2026-10-26). No relabel is needed.
  That is a policy query, not a run under enforcing mode on Bazzite; step 3
  checks the real host.

## Event loss is expected

None of this is guaranteed delivery, and Vigie must treat gaps as "likely",
never as proof:

- The daemon drops an event if its logger queue stays full for 1 ms, and only
  writes while it's connected to rsyslog (`log/loggers/logger.go:167-199`,
  `remote.go:200-206`).
- rsyslog keeps up to 10,000 events in memory while Vigie is unreachable and
  drops new ones once that's full; it never blocks the listener.
- There is deliberately **no disk-assisted queue**: it would write the whole
  raw event, environment and arguments included, to `/var/lib/rsyslog`.
  Events queued when rsyslog stops are lost.

## Enabling it

Do this as root, from a checkout of this repository (the merge reads
`packaging/endpoint-events/opensnitchd-logger.json`).

1. **Install rsyslog.** It isn't in the image. These are the Fedora 44 package
   names (checked with `dnf repoquery` in the pinned Fedora 44 image,
   2026-10-26):

   ```bash
   sudo rpm-ostree install rsyslog rsyslog-mmjsonparse rsyslog-gnutls
   systemctl reboot   # or: sudo rpm-ostree apply-live
   ```

   **Side effect.** Fedora's stock `/etc/rsyslog.conf` reads the journal
   (`imjournal`) and writes `/var/log/messages`, `/var/log/secure` and others.
   Enabling rsyslog starts copying the whole journal to disk. The drop-in's own
   events don't go there (their ruleset ends in `stop`), but the rest of the
   journal does. If you don't want that, trim `/etc/rsyslog.conf` to
   `include(file="/etc/rsyslog.d/*.conf")` plus a `global(workDirectory=...)`.

2. **Place Vigie's CA and the drop-in.** Put the CA that signed Vigie's TLS
   certificate at `/etc/pki/snitchwatch-endpoint-events/vigie-ca.pem` (0644).
   Copy the drop-in to `/etc/rsyslog.d/` and fill the four placeholders:

   | Placeholder | Value |
   | ----------- | ----- |
   | `@VIGIE_ADDRESS@` | Vigie's IP address (an IP, so rsyslog needs no DNS under `deny`) |
   | `@VIGIE_PORT@` | Vigie's TLS syslog port, e.g. `6514` |
   | `@VIGIE_CERT_NAME@` | the name in Vigie's certificate (checked by `x509/name`) |
   | `@HOST_ID@` | this computer's stable id in Vigie's device inventory |

   ```bash
   sudo install -D -m 0644 packaging/endpoint-events/snitchwatch-endpoint-events.conf \
     /etc/rsyslog.d/snitchwatch-endpoint-events.conf
   sudoedit /etc/rsyslog.d/snitchwatch-endpoint-events.conf   # fill @...@
   sudo grep -n '@[A-Z_]*@' /etc/rsyslog.d/snitchwatch-endpoint-events.conf   # must print nothing
   sudo rsyslogd -N1                                                          # must report no errors
   ```

3. **Start rsyslog**, before the daemon starts logging to it:

   ```bash
   sudo systemctl enable --now rsyslog.service
   sudo ss -ltnp 'sport = :514'   # rsyslogd on 127.0.0.1:514 only
   ```

   If nothing listens, look for a `bind` error in `journalctl -u rsyslog` and an
   SELinux denial in `sudo ausearch -m avc -ts recent`. Fedora's policy allows
   rsyslog to bind TCP 514 (see "Why port 514" above), so a denial means this
   host's policy differs; stop and report it rather than relabelling the port.

4. **Add the logger to the daemon config.** The daemon keeps this file
   root-only (0600), so build and check the replacement as root, swap it in only
   if `jq` succeeded, and restart only then. Running it twice is harmless: an
   existing `remote` entry for `127.0.0.1:514` is replaced, not duplicated. The
   first run keeps the original as `default-config.json.pre-endpoint-events`.

   <!-- verify.sh: enable -->
   ```bash
   sudo sh -c 'set -e; f=/etc/opensnitchd/default-config.json
     src=packaging/endpoint-events/opensnitchd-logger.json
     jq -e --slurpfile l "$src" ".Server.Loggers = ([.Server.Loggers // [] | .[] | select(.Name != \"remote\" or .Server != \"127.0.0.1:514\")] + \$l)" "$f" > "$f.new"
     chmod 600 "$f.new"; [ -e "$f.pre-endpoint-events" ] || cp -p "$f" "$f.pre-endpoint-events"
     mv "$f.new" "$f"' \
     && sudo systemctl restart opensnitch.service
   ```

   Check:

   ```bash
   sudo jq .Server.Loggers /etc/opensnitchd/default-config.json
   sudo ss -tnp 'dport = :514'        # opensnitchd connected to 127.0.0.1:514
   sudo ss -tnp 'dport = :6514'       # rsyslogd connected to Vigie (your port)
   ```

   The Fedora unit is `opensnitch.service`; `opensnitchd` is the binary
   (issue #92).

5. **The `/etc` merge.** This edit makes `default-config.json` locally modified,
   so image updates keep it, logger included (see "Before rebooting into the
   image" in [`docs/packaging/system-bridge-integration.md`](../../docs/packaging/system-bridge-integration.md)).
   If you reset the file to the image's copy there, run step 4 again.

### Under the `deny` opt-in

The packaged default (`allow`) needs no rule. If you opted into `deny`, rsyslog's
outbound TLS to Vigie needs one. This is a template, not a shipped file: fill in
Vigie's IP and port, save it as
`/etc/opensnitchd/rules/endpoint-events-rsyslog-to-vigie.json` (0644) and
restart `opensnitch.service`. Don't use the reserved `000-snitchwatch-` prefix.
The daemon matches the binary's real path. On Fedora 44 that is
`/usr/bin/rsyslogd` (`/usr/sbin` was merged into `/usr/bin`; `verify.sh`
prints `command -v rsyslogd` for the pinned image). Check yours with
`readlink -f "$(command -v rsyslogd)"`.

```json
{
  "name": "endpoint-events-rsyslog-to-vigie",
  "description": "Operator opt-in: rsyslog forwards allowlisted connection events to Vigie over TLS.",
  "action": "allow",
  "duration": "always",
  "enabled": true,
  "precedence": false,
  "nolog": false,
  "operator": {
    "operand": "list", "type": "list", "data": "", "sensitive": false,
    "list": [
      {"operand": "process.path", "type": "simple", "data": "/usr/bin/rsyslogd", "sensitive": true, "list": []},
      {"operand": "dest.ip", "type": "simple", "data": "VIGIE_ADDRESS", "sensitive": false, "list": []},
      {"operand": "dest.port", "type": "simple", "data": "VIGIE_PORT", "sensitive": false, "list": []},
      {"operand": "protocol", "type": "simple", "data": "tcp", "sensitive": false, "list": []}
    ]
  }
}
```

The daemon's own connection to `127.0.0.1:514` needs no rule: it accepts its own
traffic before any rule is checked (`vendor/opensnitch/daemon/main.go:414`).

## The way back

Remove the logger first, so the daemon stops sending, then rsyslog. As in step
4, the edit is built and checked as root and swapped in only on success:

<!-- verify.sh: way-back -->
```bash
sudo sh -c 'set -e; f=/etc/opensnitchd/default-config.json
  jq -e ".Server.Loggers = [.Server.Loggers // [] | .[] | select(.Name != \"remote\" or .Server != \"127.0.0.1:514\")] | if .Server.Loggers == [] then del(.Server.Loggers) else . end" "$f" > "$f.new"
  chmod 600 "$f.new"; mv "$f.new" "$f"' \
  && sudo systemctl restart opensnitch.service
```

Then:

```bash
sudo systemctl disable --now rsyslog.service
sudo rm /etc/rsyslog.d/snitchwatch-endpoint-events.conf
sudo rm /etc/pki/snitchwatch-endpoint-events/vigie-ca.pem
sudo rmdir /etc/pki/snitchwatch-endpoint-events
sudo rm -f /etc/opensnitchd/rules/endpoint-events-rsyslog-to-vigie.json   # if you added it
sudo rpm-ostree uninstall rsyslog rsyslog-mmjsonparse rsyslog-gnutls       # then reboot
```

`default-config.json.pre-endpoint-events` is your
pre-opt-in copy; delete it once you're satisfied.

## Verification

```bash
just endpoint-events-verify
```

Runs [`verify.sh`](verify.sh) in the digest-pinned Fedora 44 image
(`packaging/release/pins.env`) with rootless podman: `rsyslogd -N1` on the
drop-in under Fedora's stock config and a minimal one; an end-to-end run
against a local TLS receiver asserting the allowlisted fields arrive and a fake
token in `process_env`/`process_args` doesn't; a receiver from another CA that
must get nothing; and the merge and way-back snippets above (marked
`verify.sh:`) against a copy of the packaged daemon config. It needs the
Fedora mirrors, so it isn't part of `just package-check`.

Not verified anywhere yet: a live `opensnitchd` dialing the listener, rsyslog
binding 514 under enforcing SELinux on a real Bazzite host (step 3), and Vigie's
receiving side.
