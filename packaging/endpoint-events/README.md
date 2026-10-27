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
| [`snitchwatch-endpoint-events.conf`](snitchwatch-endpoint-events.conf) | `/etc/rsyslog.d/` | loopback listener, field validation and allowlist, mutual-TLS forward to Vigie |
| [`snitchwatch-endpoint-events.nft`](snitchwatch-endpoint-events.nft) | `/etc/nftables/` | its own nftables table: only root may connect to `127.0.0.1:514` |
| [`snitchwatch-endpoint-events-nft.service`](snitchwatch-endpoint-events-nft.service) | `/etc/systemd/system/` | oneshot that loads that table before rsyslog and the daemon start |
| [`sample-event.json`](sample-event.json) | nowhere | one forwarded event exactly as the proof's receiver got it (the hand-off sample for Vigie) |
| [`verify.sh`](verify.sh) | nowhere | `just endpoint-events-verify`: the real-rsyslog proof below |

## What leaves this computer, and what doesn't

The daemon's event is the whole connection, including each process's
environment and arguments, which can hold tokens and passwords. The drop-in's
template forwards **only**:

- `time`: when rsyslog received the event from the daemon (rsyslog's
  `timegenerated`, RFC 3339 with microseconds and offset). The daemon's JSON
  carries no timestamp of its own. Vigie also records its own receive time;
- `host_id`: informational only. Vigie identifies this computer by its
  client certificate, never by this field;
- `protocol`, `src_ip`, `src_port`, `dst_ip`, `dst_port`, `dst_host`;
- `process_path`, `user_id`;
- `action`, `rule`, `type` (the daemon's `Action`, `Rule`, `Type`; rsyslog
  lowercases template output names).

It never forwards `process_env`, `process_args`, `process_cwd`,
`process_tree` or `process_checksums`, nor `process_id`. Only connection events
(`Type` 0) that parse as JSON and whose every forwarded field passes
validation are forwarded; anything else on the listener is dropped. The
`packaging_shape` tests pin this, and `verify.sh` proves it against real
rsyslog with a fake token in the environment and arguments.

That guarantee covers **this forwarding path only**:

- **Other daemon loggers get everything.** Any other entry already in
  `Server.Loggers` receives the full event, environment and arguments included.
  Check before enabling (step 4) and remove any you don't recognise.
- **No rsyslog debug output.** Running rsyslog with `-d`, `-dn` or
  `RSYSLOG_DEBUG`/`RSYSLOG_DEBUGLOG` prints raw messages, so the whole event
  ends up in the debug log. Never enable it on a host with this drop-in.
- **Validation drops, it doesn't trim.** A field over its cap drops the whole
  event (see "Sizes" below), so a very long path or rule name means that
  connection is missing in Vigie, not shortened.

## Provenance: what Vigie can trust

Events are **"reported by this host"**, nothing more:

- **Only root can feed the forward.** rsyslog's listener is on loopback, and
  binding 514 needs root. A separate nftables table
  ([`snitchwatch-endpoint-events.nft`](snitchwatch-endpoint-events.nft))
  rejects any connection to `127.0.0.1:514` (or `[::1]:514`) from a non-root
  socket, whether or not the daemon is up. Without it, any local user could
  connect and inject daemon-shaped events that rsyslog would forward under
  this host's certificate.
- **Every forwarded field is validated** in the ruleset: type `0`, protocol,
  IPs, ports (0-65535), `dst_host`, `process_path` (absolute or empty, at most
  768 bytes), `user_id`, `action` (`allow`, `deny`, `reject` or `<nil>`) and
  `rule` (at most 128 bytes). Anything else is dropped.
- **Root can still forge.** Root on this host (or anything running as root)
  can connect and send whatever it likes. The client certificate proves which
  host sent an event, not that `opensnitchd` saw it.

The table is its own `inet snitchwatch_endpoint_events` table, not firewalld
policy: firewalld (nftables backend) and opensnitchd each manage only their own
tables (`vendor/opensnitch/daemon/firewall/nftables/tables.go:66-79`), and a
`firewall-cmd --reload` leaves it alone. Not verified on a live Bazzite host
with firewalld running.

## TLS and framing settings (exact)

The forward is mutual TLS; there is no fallback to plain TCP. From the
drop-in's `omfwd` action:

| Setting | Value | Meaning |
| ------- | ----- | ------- |
| `protocol` | `tcp` | |
| `tcp_framing` | `octet-counted` | RFC 5425 framing: `<length> <payload>` |
| `StreamDriver` | `gtls` | GnuTLS (`rsyslog-gnutls`) |
| `StreamDriverMode` | `1` | TLS required |
| `StreamDriverAuthMode` | `x509/name` | Vigie's certificate must chain to the pinned CA **and** name `@VIGIE_CERT_NAME@` (SAN or CN) |
| `StreamDriverPermittedPeers` | `@VIGIE_CERT_NAME@` | |
| `streamDriver.CAFile` | `/etc/pki/snitchwatch-endpoint-events/vigie-ca.pem` | the pinned CA (Vigie's) |
| `streamDriver.CertFile` | `/etc/pki/snitchwatch-endpoint-events/host-cert.pem` | this computer's client certificate |
| `streamDriver.KeyFile` | `/etc/pki/snitchwatch-endpoint-events/host-key.pem` | its key, root-only 0600 |

TLS versions and ciphers are GnuTLS's system defaults (Fedora's crypto policy).

Each frame's payload is one JSON object (no syslog header, no trailing
newline), as in [`sample-event.json`](sample-event.json). Sizes:

- **Out: under 8 KiB per frame.** Vigie drops larger frames. The ruleset drops
  any event whose `process_path` exceeds 768 bytes, `rule` 128 bytes or
  `dst_host` 253 ASCII characters; the other fields are short by pattern.
  Even if every byte of the path and rule needs six-byte `\u00XX` escaping,
  the frame stays under 8 KiB, given a `@HOST_ID@` of at most 64 characters.
  Nothing is truncated, so a frame is always valid UTF-8 JSON.
- **In: 64k** (`global(maxMessageSize="64k")`, rsyslog-wide). The daemon's raw
  line includes the whole environment and arguments and often exceeds
  rsyslog's 8k default; a cut line would fail to parse and be dropped. Raw
  lines over 64k are still dropped.

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

2. **Certificates.** Vigie owns registration, so follow its instructions for
   how a host certificate is issued and registered; this is the generic shape.
   Create this computer's key and a certificate signing request on the host,
   so the key never leaves it:

   ```bash
   sudo install -d -m 0755 /etc/pki/snitchwatch-endpoint-events
   sudo openssl req -newkey rsa:3072 -nodes -subj "/CN=HOST_NAME" \
     -addext "subjectAltName=DNS:HOST_NAME" \
     -keyout /etc/pki/snitchwatch-endpoint-events/host-key.pem \
     -out /etc/pki/snitchwatch-endpoint-events/host.csr
   sudo chmod 600 /etc/pki/snitchwatch-endpoint-events/host-key.pem
   ```

   - Have Vigie sign the CSR (or issue a certificate for this host's key) and
     register the host: its certificate's SAN, or its fingerprint
     (`sudo openssl x509 -in /etc/pki/snitchwatch-endpoint-events/host-cert.pem
     -noout -fingerprint -sha256`), joined to Vigie's device inventory. That
     registration, not `host_id`, is what identifies this computer to Vigie.
   - Install the signed certificate as
     `/etc/pki/snitchwatch-endpoint-events/host-cert.pem` (0644), and the CA
     that signed Vigie's server certificate as
     `/etc/pki/snitchwatch-endpoint-events/vigie-ca.pem` (0644).
   - A self-signed host certificate works too, if Vigie pins its fingerprint.

   **The drop-in.** Copy it to `/etc/rsyslog.d/` and fill the four placeholders:

   | Placeholder | Value |
   | ----------- | ----- |
   | `@VIGIE_ADDRESS@` | Vigie's IP address (an IP, so rsyslog needs no DNS under `deny`) |
   | `@VIGIE_PORT@` | Vigie's TLS syslog port, e.g. `6514` |
   | `@VIGIE_CERT_NAME@` | the name in Vigie's certificate (checked by `x509/name`) |
   | `@HOST_ID@` | a label for this computer, at most 64 characters; informational only (Vigie identifies the host by its client certificate) |

   ```bash
   sudo install -D -m 0644 packaging/endpoint-events/snitchwatch-endpoint-events.conf \
     /etc/rsyslog.d/snitchwatch-endpoint-events.conf
   sudoedit /etc/rsyslog.d/snitchwatch-endpoint-events.conf   # fill @...@
   sudo grep -n '@[A-Z_]*@' /etc/rsyslog.d/snitchwatch-endpoint-events.conf   # must print nothing
   sudo rsyslogd -N1                                                          # must report no errors
   ```

3. **Restrict the listener to root, then restart rsyslog**, before the daemon
   starts logging to it. `restart`, not `enable --now`: a running rsyslog
   (for example one started for the journal) doesn't reread its config until
   restarted, so the drop-in wouldn't load.

   ```bash
   sudo install -D -m 0644 packaging/endpoint-events/snitchwatch-endpoint-events.nft \
     /etc/nftables/snitchwatch-endpoint-events.nft
   sudo install -m 0644 packaging/endpoint-events/snitchwatch-endpoint-events-nft.service \
     /etc/systemd/system/snitchwatch-endpoint-events-nft.service
   sudo systemctl daemon-reload
   sudo systemctl enable --now snitchwatch-endpoint-events-nft.service
   sudo nft list table inet snitchwatch_endpoint_events   # the two reject rules

   sudo rsyslogd -N1 \
     && sudo systemctl enable rsyslog.service \
     && sudo systemctl restart rsyslog.service
   sudo ss -ltnp 'sport = :514'   # rsyslogd on 127.0.0.1:514 only
   bash -c 'exec 3<>/dev/tcp/127.0.0.1/514' && echo "OPEN TO USERS: stop" || echo "refused: good"
   ```

   The last line runs as you, not root, and must print `refused: good`.

   **Never restart rsyslog over a failing `-N1`.** If the drop-in fails to
   parse, rsyslog still opens the listener but routes it to its *default*
   ruleset, so raw events, environment included, would go to the journal and
   `/var/log` (seen in the proof: `ruleset 'snitchwatch_endpoint_events' for
   port 514 not found - using default ruleset instead`). Step 4 enables the
   logger only after the listener and the drop-in are confirmed.

   **If nothing listens on 514**, don't go on to step 4:
   - `sudo rsyslogd -N1` reports the config error, if any;
   - `systemctl status rsyslog.service` must show it restarted after you placed
     the drop-in (check the start time);
   - `journalctl -u rsyslog` shows a `bind` error, and
     `sudo ausearch -m avc -ts recent` an SELinux denial. Fedora's policy allows
     rsyslog to bind TCP 514 (see "Why port 514" above), so a denial means this
     host's policy differs; stop and report it rather than relabelling the port.

4. **Add the logger to the daemon config.** First check what's already there:
   every other entry in `Server.Loggers` gets the full event, environment
   included.

   ```bash
   sudo jq .Server.Loggers /etc/opensnitchd/default-config.json   # null, [] or only ours
   ```

   The daemon keeps this file root-only (0600), so build and check the replacement as root, swap it in only
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
sudo systemctl disable --now snitchwatch-endpoint-events-nft.service   # deletes the table
sudo rm /etc/systemd/system/snitchwatch-endpoint-events-nft.service \
  /etc/nftables/snitchwatch-endpoint-events.nft
sudo systemctl daemon-reload
sudo rm -f /etc/pki/snitchwatch-endpoint-events/{vigie-ca.pem,host-cert.pem,host-key.pem,host.csr}
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
(`packaging/release/pins.env`) with rootless podman. `--cap-add=NET_ADMIN` is
scoped to the container's own network namespace, so the table loads there and
never on the host. It checks:

- **nftables.** `nft -c` and a load (twice, to show a reload replaces the
  table); uid 65534 can't connect to `127.0.0.1:514`, root can. The `::1` rule
  is loaded but not exercised.
- **Config checks.** `rsyslogd -N1` under Fedora's stock config and a minimal
  one.
- **End to end under the stock `rsyslog.conf`,** with a disk-only, slowed main
  queue, to a local mutual-TLS receiver. Only the valid connection event
  arrives, and it equals [`sample-event.json`](sample-event.json) in values
  and types. Malformed and oversize events are dropped, and the fake token in
  env/args/cwd/tree/checksums is in no frame and no file under `/var/log`,
  `/var/lib/rsyslog` or `/var/spool`.
- **Frame bound.** A worst-case event arrives in one frame under 8 KiB. A
  path and a rule exactly at their caps, ending in a two-byte character,
  arrive intact as strict UTF-8; one byte over is dropped.
- **TLS refusals.** Nothing is delivered to a server from another CA or from
  the pinned CA with the wrong name. Nothing is accepted from a client
  certificate of another CA, or a sender with no certificate or no key.
- **README snippets.** The merge and way-back snippets above (marked
  `verify.sh:`) run against a copy of the packaged daemon config.

It needs the Fedora mirrors, so it isn't part of `just package-check`. With
slow mirrors, `SW_VERIFY_DNF_CACHE=<volume> just endpoint-events-verify` keeps
dnf's cache in that podman volume between runs.

Not verified anywhere yet: a live `opensnitchd` dialing the listener, rsyslog
binding 514 under enforcing SELinux on a real Bazzite host (step 3), the
nftables table alongside a running firewalld, and Vigie's receiving side.
