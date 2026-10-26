# Sharing this computer's connection events with Vigie

Status: design for owner review, 2026-10-26. Nothing built. Companion doc in the
Vigie repo: `docs/research/snitchwatch-endpoint-events-2026-10-26.md`.

## Why

Vigie watches the whole LAN from the router. Snitchwatch, through `opensnitchd`,
guards this one computer. Each sees something the other can't:

- The router sees every packet that leaves this computer, but not which program
  sent it or what the firewall decided.
- OpenSnitch knows the program and the decision, but it can only report what it
  intercepts. If something gets around it (a VM or container on its own network,
  traffic before the daemon starts, a stopped or crashed daemon, malware that
  disables it), the Snitchwatch GUI shows nothing at all.

**Main benefit: the guard is checked from outside.** When the router sees this
computer talk and OpenSnitch reported nothing for that connection, or reported
nothing at all for a while, Vigie can say so. Neither tool can do that alone.
Per-app attribution in Vigie (which program made the 3 GB upload at 2 am) is
the second benefit.

## Constraint from the owner

"Via some open piece": use an existing, documented interface, and stay close
to each project's original intent. So:

- No Snitchwatch code in the data path. The bridge, GUI, socket, token and trust
  model don't change, and the Flatpak keeps having no network access.
- The open piece is OpenSnitch's own event logger (`Server.Loggers` in the
  daemon config), the same one upstream documents for SIEM integration
  (roadmap X07), plus a standard log shipper.
- Snitchwatch stays the only enforcer on this computer. Vigie never decides
  anything here.

## Data path

```
opensnitchd --(remote logger, JSON lines, TCP 127.0.0.1:514)--> rsyslog --(allowlist, TLS)--> Vigie input
```

(Revised 2026-10-26 after reading the logger code. The first draft used the
local `syslog` logger with JSON, but that logger only writes RFC 5424 or CSV,
`log/loggers/syslog.go:35-38`.)

1. **Daemon to a loopback listener.** Use a `remote` logger with
   `Format: json`, `Protocol: tcp`, `Server: 127.0.0.1:514`. It is the only
   logger with JSON output (`log/loggers/remote.go:85-91`) and writes one
   object per line (`json.go:77-79`, `remote.go:208-214`).
   - **Port.** 514 is below `net.ipv4.ip_unprivileged_port_start` (1024 on this
     host), so an unprivileged local process can't bind it first and receive
     the stream (the #35 squatting problem).
   - **Filtering.** The daemon accepts its own connections without filtering
     (`main.go:414`).
   - **Never across the LAN.** The `remote` logger is plain TCP or UDP only
     (`remote.go:165-172`).
2. **rsyslog to Vigie.** rsyslog is a standard Fedora package, not in the
   image today. It listens only on 127.0.0.1:514 with a ruleset that:
   - takes the raw line as the message (no syslog header is sent);
   - parses the JSON (`mmjsonparse`);
   - renders only the allowlisted fields;
   - forwards them over TLS with a pinned server certificate (`omfwd` with the
     gnutls or openssl stream driver).
   The other two shippers considered are ruled out:
   - systemd-journal-upload can't drop fields;
   - Fluent Bit isn't in Fedora's default repos.
3. **Vigie.** It needs a TLS syslog input (RFC 5425 framing). It stores these
   events as a new source next to its router collector, with the same
   provenance rules, and joins them to conntrack.

Why not CSV or RFC 5424 through the local logger:
- They interpolate values without escaping (`formats/csv.go:24-38`,
  `formats/formats.go:45-61`).
- A program controls its own path and arguments, so it could forge or shift
  fields. JSON comes from `json.Marshal`, so it's escaped.

## What the daemon actually emits (v1.8.0, verified in source)

- **Which connections.** `onPacket` calls `stats.OnConnectionEvent` for every
  intercepted connection after the verdict (`main.go:428`), including ones
  decided by the default action: there `Action` and `Rule` are the string
  `"<nil>"` (`statistics/stats.go:122-135`).
- **Which connections are not reported:**
  - DNS responses (`main.go:401`);
  - connections the daemon couldn't parse, which get the default action
    unlogged (`main.go:408-411`);
  - the daemon's own traffic (`:414`);
  - rules marked `nolog` (`:421`);
  - connections not intercepted when `InterceptUnknown` is false (the
    comment at `:425`).
- **Format.** One JSON object per event, `{Event, Rule, Action, Type}`, where
  `Event` is the whole `protocol.Connection` (`log/formats/json.go:22-80`):
  - protocol, src/dst IP and port, `dst_host`;
  - uid, pid, `process_path`, cwd;
  - **`process_args`, `process_env`**, checksums and the process tree.
- **It's lossy under load.** If the logger queue stays full for more than 1 ms,
  the event is dropped, and repeated hits reload the loggers
  (`log/loggers/logger.go:167-199`).

## Privacy: the allowlist is mandatory

The raw event includes each process's environment and arguments. These can
hold tokens, passwords and file paths, so they must never leave this computer.
The shipper forwards only:

- time, protocol, src IP/port, dst IP/port, `dst_host`;
- `process_path`, uid;
- `Action`, `Rule`, `Type`.

It drops `process_env`, `process_args`, `process_cwd`, `process_tree` and
checksums (checksums can be revisited later for D05 binary identity).

Other requirements:

- Opt-in, off by default.
- LAN-only, over TLS with a pinned server key, the same pattern Vigie's
  collector already uses.
- Vigie applies its own retention.

A test in the packaging contract asserts the shipped shipper config drops
those fields.

## What Vigie does with it (Vigie repo work)

1. **Device page, per app.** For this computer, show per-program connection
   counts, allowed and denied counts, and top destinations, labelled with
   their source ("reported by OpenSnitch on this computer").
2. **Incident: traffic OpenSnitch didn't report.** A router conntrack flow
   from this computer has no matching endpoint event within a tolerance. The
   match is on protocol, source port, destination IP and port, and time. It
   must say "likely", and suppress known-unreported classes: DNS, the
   daemon's own traffic, `nolog` rules, and traffic that came in from outside.
   Event loss under load also explains a missing match, so the incident needs
   a rate or minimum count, not a single flow.
3. **Incident: endpoint guard went quiet.** The router sees traffic from this
   computer, but no endpoint events have arrived for N minutes. Cause: the
   daemon, the logger or the shipper stopped.
4. **Explained baselines.** Vigie's upload-baseline incidents for this
   computer can name the program.

## What Snitchwatch does

- Package an opt-in recipe: a `Loggers` entry for
  `/etc/opensnitchd/default-config.json` and a shipper config with the field
  allowlist. These are documentation and packaging files, not code. Note that
  the daemon config is root-only (0600) and the `/etc` three-way merge keeps
  local edits, so enabling it is an operator step like the deny opt-in.
- Optional, later: one Health-page line, "Sharing connection events with your
  network monitor: on". It would need the bridge to read the daemon config's
  `Loggers`, so it's out of scope until the rest works.

## Also needed

- rsyslog packages in the image (`rsyslog`, `rsyslog-mmjsonparse`, and a TLS
  driver package). That's a tower/image change, separate from the Snitchwatch
  packaging PR. Until then, it's an operator `rpm-ostree install`.
- Under the deny opt-in, rsyslog's outbound TLS to Vigie needs an allow rule
  for `/usr/sbin/rsyslogd` to that one host and port. The packaged default
  (`allow`) needs none.
- If rsyslog isn't running, the daemon drops events (`remote.go:200-206`
  writes only while connected). Vigie's "guard went quiet" incident covers
  this case.

## Open questions

1. Vigie's input: a TLS syslog listener (RFC 5425), and how it maps into its
   provenance model.
2. Whether the image should ship rsyslog, or leave it as an operator step.
3. How to identify this computer in Vigie: a stable host identity in the
   shipper config, joined to Vigie's device inventory.
4. Matching tolerance and how loss is shown (measure on real traffic before
   picking numbers).

## Not in scope

- Vigie sending anything back to Snitchwatch (for example LAN device names).
  That needs Snitchwatch to call Vigie's private API, and is a separate
  decision.
- Any enforcement by Vigie on this computer.
- Raw packet or payload capture.
