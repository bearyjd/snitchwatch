# Sharing connection events with Vigie (opt-in)

Off by default. Enabling it is an operator step, like the `deny` opt-in. The
Snitchwatch system image built from the tower's accepted source `f5a18d28` or
later (the first such image is
`ghcr.io/bearyjd/bazzite-tower@sha256:148de52b6590ddb28fc2d7ed71035e7859aa350d7d1e21fa7ad6545d4999b1df`,
published 2026-10-28; check `rpm -q rsyslog` and the `/usr/share` directory on
yours) ships the rsyslog packages with every unit **disabled**, and an inert copy of
this directory's rsyslog config, nftables table and two units in
`/usr/share/snitchwatch/endpoint-events/`. Nothing listens on 514 or 6514, the
daemon config has no logger entry, and `opensnitchd-logger.json` is **not**
shipped: take that one from this repository. Images from before that source
ship none of this.
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
| [`snitchwatch-endpoint-events.conf`](snitchwatch-endpoint-events.conf) | `/etc/snitchwatch-endpoint-events/rsyslog.conf` | the whole config of a separate rsyslog instance: loopback listener, field validation and allowlist, mutual-TLS forward to Vigie |
| [`snitchwatch-endpoint-events.service`](snitchwatch-endpoint-events.service) | `/etc/systemd/system/` | runs that instance; bound to the table's unit, refuses an unclean config |
| [`snitchwatch-endpoint-events.nft`](snitchwatch-endpoint-events.nft) | `/etc/nftables/` | its own nftables table: only root may connect to `127.0.0.1:514` |
| [`snitchwatch-endpoint-events-nft.service`](snitchwatch-endpoint-events-nft.service) | `/etc/systemd/system/` | oneshot that loads (and atomically reloads) that table |
| [`sample-event.json`](sample-event.json) | nowhere | one forwarded event exactly as the proof's receiver got it (the hand-off sample for Vigie) |
| [`verify.sh`](verify.sh) | nowhere | `just endpoint-events-verify`: the real-rsyslog proof below |

## What leaves this computer, and what doesn't

The daemon's event is the whole connection, including each process's
environment and arguments, which can hold tokens and passwords. The instance's
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
  ends up in the debug log. Never enable it on a host with this recipe.
- **Validation drops, it doesn't trim.** A field over its cap drops the whole
  event (see "Sizes" below), so a very long path or rule name means that
  connection is missing in Vigie, not shortened.

## Provenance: what Vigie can trust

Events are **"reported by this host"**, nothing more:

- **Only root can feed the forward.** rsyslog's listener is on loopback, and
  binding 514 needs root. A separate nftables table
  ([`snitchwatch-endpoint-events.nft`](snitchwatch-endpoint-events.nft))
  rejects any connection to `127.0.0.1:514` (or `[::1]:514`) from a non-root
  socket, whether or not the daemon is up. It also rejects packets for those
  addresses arriving from any interface other than `lo`: with `route_localnet=1`
  on a bridge (netavark sets it on the bridges it manages) a container could
  otherwise route them in. Without the table, any local user could connect and
  inject daemon-shaped events that rsyslog would forward under this host's
  certificate. The listener's unit is bound to the table's (step 3), so the
  listener is never up without it.
- **Every forwarded field is checked** in the ruleset, and an event that fails
  any check is dropped:
  - `type` is `0`;
  - `protocol` is one the daemon emits: `tcp`, `udp`, `udplite`, `sctp` or
    `icmp`, each optionally with `6`;
  - IPs are dotted-quad IPv4 (octets up to 255) or IPv6 characters with a `:`;
  - ports are 0-65535, `user_id` a uint32;
  - `dst_host` is up to 253 hostname characters;
  - `process_path` is an absolute path (at most 768 bytes), empty, the daemon's
    `Kernel connection` marker, or the process name (`comm`, up to 15 bytes, or 45 once Go has replaced invalid UTF-8 bytes with U+FFFD) it
    reports when the executable can't be resolved
    (`vendor/opensnitch/daemon/procmon/details.go:258-275`). A value without a
    leading `/` is therefore a marker or a process name, never a path;
  - `action` is `allow`, `deny`, `reject` or `<nil>`;
  - `rule` is at most 128 bytes.

  These are **shape and size checks, not a schema**: an IPv6 address is only
  checked for its characters, a rule name only for its length. Vigie must
  validate what it stores.
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
config's `omfwd` action:

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

   **Where the files come from.** The commands below say
   `packaging/endpoint-events/...`, a path in a checkout of this repository.
   On an image that ships them, use the same file names from
   `/usr/share/snitchwatch/endpoint-events/` instead (root-owned, 0644, in no
   unit search path, so nothing runs until you copy a file to the `/etc`
   locations below). `opensnitchd-logger.json` always comes from a checkout.

1. **Install rsyslog.** Skip this step on an image that already ships the
   packages (check with `rpm -q rsyslog rsyslog-mmjsonparse rsyslog-gnutls`).
   Otherwise these are the Fedora 44 package names (checked with
   `dnf repoquery` in the pinned Fedora 44 image, 2026-10-26):

   ```bash
   sudo rpm-ostree install rsyslog rsyslog-mmjsonparse rsyslog-gnutls
   systemctl reboot   # or: sudo rpm-ostree apply-live
   ```

   This recipe runs its **own** rsyslog instance
   (`snitchwatch-endpoint-events.service`) with its own config, and never
   touches the host's `rsyslog.service` or `/etc/rsyslog.d`. You don't need to
   enable the host's rsyslog; if you do, it carries on as before and never sees
   these events.

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

   **The instance's config.** Copy it to `/etc/snitchwatch-endpoint-events/`
   and fill the three placeholders:

   | Placeholder | Value |
   | ----------- | ----- |
   | `@VIGIE_ADDRESS@` | Vigie's IP address (an IP, so rsyslog needs no DNS under `deny`) |
   | `@VIGIE_CERT_NAME@` | the name in Vigie's certificate (checked by `x509/name`) |
   | `@HOST_ID@` | a label for this computer, at most 64 characters; informational only (Vigie identifies the host by its client certificate) |

   First the instance's work directory (rsyslog's `-N1` needs it to exist)
   and the SELinux labels. The directory is under `/var/lib/rsyslog` so it
   gets rsyslog's own label (`syslogd_var_lib_t`); `restorecon` applies it,
   and the certificates' `cert_t`, to anything you created by hand:

   <!-- verify.sh: prepare -->
   ```bash
   sudo install -d -m 0700 /var/lib/rsyslog/snitchwatch-endpoint-events
   sudo install -D -m 0644 packaging/endpoint-events/snitchwatch-endpoint-events.conf \
     /etc/snitchwatch-endpoint-events/rsyslog.conf
   sudo restorecon -RFv /var/lib/rsyslog/snitchwatch-endpoint-events \
     /etc/pki/snitchwatch-endpoint-events /etc/snitchwatch-endpoint-events
   ```

   Fill the placeholders (`sudoedit /etc/snitchwatch-endpoint-events/rsyslog.conf`),
   then check:

   <!-- verify.sh: check -->
   ```bash
   c=/etc/snitchwatch-endpoint-events/rsyslog.conf
   sudo grep -n '@[A-Z_]*@' "$c" && echo "fill these placeholders first" && false
   sudo rsyslogd -N1 -f "$c"              # must report no errors
   ```

   **Vigie's port is 6514** (syslog over TLS), set in the config's `omfwd`
   action. Under enforcing SELinux, rsyslog may connect out only to ports
   labelled for syslog, and 6514 already is (`syslog_tls_port_t`), so the
   recipe needs no SELinux change. **Another port is outside this recipe.**
   It's an operator decision: you'd edit `port=` in the config and label that
   port for syslog yourself (`semanage port`, see Fedora's SELinux
   documentation), checking first that nothing else owns it, and you own
   adding and later removing that mapping. Without the label the listener runs
   but nothing reaches Vigie (`ausearch -m avc` shows the `name_connect`
   denial). Nothing in this recipe runs `semanage`.

3. **Install and start the table and the listener**, before the daemon starts
   logging to it.

   <!-- verify.sh: start -->
   ```bash
   sudo install -D -m 0644 packaging/endpoint-events/snitchwatch-endpoint-events.nft \
     /etc/nftables/snitchwatch-endpoint-events.nft
   sudo install -m 0644 packaging/endpoint-events/snitchwatch-endpoint-events-nft.service \
     packaging/endpoint-events/snitchwatch-endpoint-events.service /etc/systemd/system/
   sudo restorecon -Fv /etc/nftables/snitchwatch-endpoint-events.nft \
     /etc/systemd/system/snitchwatch-endpoint-events-nft.service \
     /etc/systemd/system/snitchwatch-endpoint-events.service
   sudo systemctl daemon-reload
   sudo systemctl enable --now snitchwatch-endpoint-events-nft.service \
     snitchwatch-endpoint-events.service
   sudo nft list table inet snitchwatch_endpoint_events   # two output and two input rejects
   sudo ss -ltnp 'sport = :514'   # one rsyslogd, on 127.0.0.1:514 only
   ```

   Then, as yourself (not root), this must print `refused: good`:

   ```bash
   bash -c 'exec 3<>/dev/tcp/127.0.0.1/514' && echo "OPEN TO USERS: stop" || echo "refused: good"
   ```

   How the two units keep the listener safe:
   - **It needs the table.** The listener unit `BindsTo=` and is `After=` the
     table's unit, so it starts only once the table has loaded successfully. If
     the load fails, the listener never starts. Whenever the table's unit stops
     or fails, the listener is stopped first, and `PartOf=` restarts it after a
     restart of the table's unit. To pick up an edited `.nft` file, use
     `systemctl reload snitchwatch-endpoint-events-nft.service`: the file
     replaces the table in one transaction and never deletes it.
   - **It refuses a broken config.** The unit runs `rsyslogd -N1` before every
     start, and the config sets `abortOnUncleanConfig`, so a config error (now
     or after any later edit) means no listener at all. That matters: rsyslog
     with a missing or broken ruleset still opens a listener and routes it to
     its default ruleset (seen in the proof). This instance's default ruleset has
     no actions either, so nothing in it can reach the journal or `/var/log`.

   **If nothing listens on 514**, don't go on to step 4:
   - `systemctl status snitchwatch-endpoint-events.service` and
     `journalctl -u snitchwatch-endpoint-events.service` show the `-N1` error,
     a failed dependency (the table) or a `bind` error;
   - `sudo rsyslogd -N1 -f /etc/snitchwatch-endpoint-events/rsyslog.conf`
     repeats the config check;
   - `sudo ausearch -m avc -ts recent` shows an SELinux denial. Fedora's policy
     allows rsyslog to bind TCP 514 (see "Why port 514" above), so a denial
     means this host's policy differs; stop and report it rather than
     relabelling the port. A denial on
     `/var/lib/rsyslog/snitchwatch-endpoint-events` or the certificates means
     the `restorecon` in step 2 was skipped; rerun it.

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
      {"operand": "dest.port", "type": "simple", "data": "6514", "sensitive": false, "list": []},
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

Then everything else the recipe added: the units and table, the instance's
config and work directory, the certificates, and the deny-mode rule if you
added it. (A port label you added yourself for a non-default port is yours to
remove.)

<!-- verify.sh: rollback -->
```bash
# The listener first; stopping the table's unit would stop it anyway.
sudo systemctl disable --now snitchwatch-endpoint-events.service
sudo systemctl disable --now snitchwatch-endpoint-events-nft.service   # deletes the table
sudo rm -f /etc/systemd/system/snitchwatch-endpoint-events.service \
  /etc/systemd/system/snitchwatch-endpoint-events-nft.service \
  /etc/nftables/snitchwatch-endpoint-events.nft \
  /etc/snitchwatch-endpoint-events/rsyslog.conf
sudo systemctl daemon-reload
sudo rmdir /etc/snitchwatch-endpoint-events
sudo find /var/lib/rsyslog/snitchwatch-endpoint-events -delete
sudo rm -f /etc/pki/snitchwatch-endpoint-events/{vigie-ca.pem,host-cert.pem,host-key.pem,host.csr}
sudo rmdir /etc/pki/snitchwatch-endpoint-events
sudo rm -f /etc/opensnitchd/rules/endpoint-events-rsyslog-to-vigie.json   # if you added it
```

Finally, only if you layered the packages yourself in step 1 (an image that
ships them has nothing to uninstall), and nothing else uses them:

```bash
sudo rpm-ostree uninstall rsyslog rsyslog-mmjsonparse rsyslog-gnutls   # then reboot
```

`default-config.json.pre-endpoint-events` is your
pre-opt-in copy; delete it once you're satisfied.

## Verification

```bash
just endpoint-events-verify
```

Runs [`verify.sh`](verify.sh) with rootless podman. It installs the packages
once into a container from the digest-pinned Fedora 44 image
(`packaging/release/pins.env`), commits that to a throwaway local image, runs
two phases on it and removes it. `NET_ADMIN` and `SYS_ADMIN` apply only inside
the container's own namespaces, so nothing touches the host's firewall or `/etc`.

**Phase 1, a plain container:**

- **nftables.** `nft -c`, a load and a reload (still exactly four rejects).
  uid 65534's connect to `127.0.0.1:514` is refused and root's accepted. As a
  best effort, a peer network namespace routing to `127.0.0.1` over a
  `route_localnet` veth is refused by the input rule, with a positive control
  (accepted without it). If the container can't build that path, the run
  prints `UNREPRODUCED` instead.
- **Fresh install.** The README's own `prepare` and `check` snippets run first,
  in order, with nothing pre-created, so a step that depends on a later one
  fails here.
- **Broken config.** `rsyslogd -N1` passes on the filled config. A config with
  its ruleset name misspelt fails `-N1`, and `rsyslogd` refuses to start with
  it (`abortOnUncleanConfig`) without ever opening port 514.
- **End to end,** with Fedora's stock host rsyslog running alongside, to a
  local mutual-TLS receiver. Exactly the three valid events arrive: the sample
  event, plus the daemon's `Kernel connection` and `comm` path values. The
  first equals [`sample-event.json`](sample-event.json) in values and types.
  Malformed, oversize and out-of-range events are dropped. The fake token in
  env/args/cwd/tree/checksums is in no frame and no file under `/var/log`,
  `/var/lib` or `/var/spool`, and a read error fails the check.
- **Frame bound.** A worst-case event arrives in one frame under 8 KiB. A path
  and a rule exactly at their caps, ending in a two-byte character, arrive
  intact as strict UTF-8; one byte over is dropped.
- **TLS refusals,** each followed by a positive control that proves the
  receiver could record. Nothing is delivered to a server from another CA or
  with the wrong name. Nothing is accepted from a client certificate of
  another CA, or a sender with no certificate or no key.
- **README snippets.** The merge and way-back snippets above (marked
  `verify.sh:`) run against a copy of the packaged daemon config.  A
  `semanage` stub records any call: none of the README's snippets may make
  one.

**Phase 2, the same image booted with systemd,** installed by running the
README's `prepare`, `check` and `start` snippets verbatim on a fresh system
(`sudo` and `restorecon` stubbed; SELinux is off in the container):

- a broken table file means the listener never starts;
- starting the listener brings the table up first;
- three restarts of the table's unit, with a poller watching throughout, show
  no moment with a listener and no table, and the listener comes back each
  time;
- a reload keeps the table and the listener's PID;
- stopping the table's unit stops the listener;
- a broken config means no listener;
- nothing sent to the listener reaches the live journal or `/var`;
- the complete way back (logger, units, table, config, work directory,
  certificates) leaves nothing the recipe added, and nothing called
  `semanage`.

It needs the Fedora mirrors, so it isn't part of `just package-check`. With
slow mirrors, `SW_VERIFY_DNF_CACHE=<volume> just endpoint-events-verify` keeps
dnf's cache in that podman volume between runs.

Not verified anywhere yet: a live `opensnitchd` dialing the listener; the
instance under enforcing SELinux on a real Bazzite host (step 3), including its
state directory; the nftables table alongside a running firewalld and
netavark; and Vigie's receiving side.
