# Snitchwatch packaging (Phase 2)

Everything needed to distribute Component A (the GUI + bridge + daemon) on
Bazzite / Universal Blue.

## Architecture at a glance

Three processes, three trust levels:

| Process                     | Where it runs                    | Packaged as |
| --------------------------- | --------------------------------- | ----------- |
| `opensnitchd`               | Host, privileged (root)          | Baked into the bluebuild image (the DIY rpm-ostree path is retired) |
| `snitchwatch-bridge-cli`    | Host, unprivileged system service (`snitchwatch` account) | system units in `system/` (socket-activated) |
| `snitchwatch-kirigami` GUI  | Flatpak sandbox, unprivileged    | Flatpak (`flatpak/org.snitchwatch.Snitchwatch.system.yml`) |

The Flatpak packages `snitchwatch-kirigami` (Qt6/QML + Kirigami), not
`snitchwatch-tauri` — Kirigami is the settled GUI stack going forward (see
this repo's `CLAUDE.md` "Settled architecture decisions" #4) and has reached
feature parity including its Task 7 safety verification. `snitchwatch-tauri`
and `web/` remain in the repo but are intentionally not what ships here —
kept until this Flatpak's first real packaged release ships (owner
decision, 2026-07-11); see
[`../docs/superpowers/plans/2026-07-04-kirigami-shell-rewrite.md`](../docs/superpowers/plans/2026-07-04-kirigami-shell-rewrite.md)'s
status note for detail.

**The system bridge is the only shipped flavour** (#35): there is no per-user
bridge unit and no per-user Flatpak profile. The GUI reaches the host-side
bridge over the **Unix domain socket** `/run/snitchwatch/bridge.sock`, granted
read-only to the sandbox (`/run/snitchwatch`, `/run/snitchwatch-auth`). There is deliberately **no
`--share=network`** on the Flatpak: a Flatpak's private network namespace
cannot reach host loopback anyway, and that permission would grant full
internet access rather than scoped loopback. See
[`../docs/superpowers/specs/2026-07-04-flatpak-feasibility-research.md`](../docs/superpowers/specs/2026-07-04-flatpak-feasibility-research.md).

## The install path

- **Batteries-included** — a signed custom Bazzite image with `opensnitchd`
  baked in and enabled from first boot: [`bluebuild/recipe.yml`](bluebuild/recipe.yml),
  plus the system overlay staged by [`system/stage.sh`](system/stage.sh).
- The earlier **Lightweight / DIY** rpm-ostree layering path (per-user bridge on
  TCP) is retired: [`../docs/packaging/rpm-ostree-layering.md`](../docs/packaging/rpm-ostree-layering.md).

The system overlay's daemon config
([`system/opensnitchd-default-config.json`](system/opensnitchd-default-config.json)):
`DefaultAction: allow` (owner decision 2026-10-25; `deny` is an operator
opt-in) and `Server.Address: unix:opensnitchd.sock`, the
system bridge's root-only socket (#35). Only `system/stage.sh` installs it,
together with the `opensnitch.service` drop-in the relative address relies
on and the socket it dials: one artifact (PR #166 H1); see
[`../docs/packaging/system-bridge-integration.md`](../docs/packaging/system-bridge-integration.md).
The recipe's own copy
([`bluebuild/files/system/etc/opensnitchd/default-config.json`](bluebuild/files/system/etc/opensnitchd/default-config.json))
is identical to the overlay's: `unix:opensnitchd.sock` with
`DefaultAction: allow`. Without the overlay the daemon (cwd `/`) can only dial
`/opensnitchd.sock`, which only root can create, so it finds no UI and applies
`allow`; no TCP port is left to squat (#35).

## The packaged fetch rule

Under a `DefaultAction: deny` opt-in with no GUI attached, the daemon would
deny the system bridge's own blocklist downloads. Snitchwatch therefore ships exactly
one opensnitchd rule (owner decision, 2026-10-08):
[`bluebuild/files/system/etc/opensnitchd/rules/000-snitchwatch-bridge-fetch.json`](bluebuild/files/system/etc/opensnitchd/rules/000-snitchwatch-bridge-fetch.json).

- **What it allows.** It ANDs four conditions:
  - `process.path` is exactly `/usr/bin/snitchwatch-bridge-cli`;
  - `user.name` is `snitchwatch`;
  - `dest.port` is `443`;
  - `protocol` matches `^tcp6?$`.

  Nothing else is allowed: no other port, no UDP/QUIC, and no desktop
  user's bridge.
- **Denies still win.** It is an `allow` with `precedence: false`. A user
  deny or a subscribed blocklist that matches a list's host still blocks
  that fetch, and the list's status shows the error.
- **If the account is missing, the rule is skipped.** The daemon resolves
  `user.name` when it loads the rule. Without the `snitchwatch` account it
  logs `Error compiling list rule` and skips the rule; it never broadens
  it.
  - A host without the overlay has no such account, so the rule is inert
    there for this reason.
  - With the system-bridge overlay, sysusers creates the account before
    `opensnitch.service` starts.
  - On a live host, restart `opensnitch.service` once after the account
    exists.
- **Reserved and read-only.** The name prefix `000-snitchwatch-` is
  reserved: no GUI action or rule import may change, rename or delete a
  rule under it. The Rules page lists it read-only as "Built into
  Snitchwatch: lets its background service download blocklists."
- **Not covered:** list URLs on ports other than 443, and DNS. DNS is a
  prerequisite, not a gap in this rule: with no GUI attached, a list
  refresh only works once the system resolver's own upstream queries
  (e.g. `systemd-resolved`) are allowed. OpenSnitch intercepts loopback
  too (its "allow localhost connections" system rule is off by default),
  so a query to `127.0.0.53:53` needs allowing as well.
- **Unit name:** the Fedora RPM's unit is `opensnitch.service`, with no
  `opensnitchd.service` alias (issue #92). `opensnitchd` is the daemon's
  binary, not its unit.
- **Where it ships:**
  - the bluebuild image (`files` module);
  - the system-bridge overlay (`system/stage.sh` installs the same file
    to `/etc/opensnitchd/rules/`, mode 0644).

  The release tarball and the Flatpak don't carry it: the tarball ships
  only the binary, and the rule matches the system bridge's account.

Details and the daemon-source reasoning:
[`../docs/superpowers/plans/2026-10-08-packaged-bridge-fetch-rule.md`](../docs/superpowers/plans/2026-10-08-packaged-bridge-fetch-rule.md).

## Files

```
packaging/
├── bluebuild/
│   ├── recipe.yml                                  # batteries-included image recipe
│   └── files/system/etc/opensnitchd/
│       ├── default-config.json                     # daemon config (unix:opensnitchd.sock, allow)
│       └── rules/000-snitchwatch-bridge-fetch.json # the one shipped allow rule (canonical)
├── flatpak/
│   ├── org.snitchwatch.Snitchwatch.system.yml      # GUI-only Flatpak manifest (no --share=network)
│   ├── org.snitchwatch.Snitchwatch.desktop
│   └── org.snitchwatch.Snitchwatch.metainfo.xml
├── endpoint-events/                                # opt-in: share connection events with Vigie (not installed)
└── system/                                         # system-bridge overlay (units, sockets, daemon config)
```

## Build (needs tooling absent from CI)

None of these can be built in the CI sandbox — they need a real Bazzite host
plus `bluebuild` / `flatpak-builder`. The files are authored as complete,
correct artifacts and their syntax is validated in CI (YAML/JSON parse +
shape tests).

```bash
# Batteries-included image (needs the bluebuild CLI + podman/buildah):
bluebuild build packaging/bluebuild/recipe.yml

# GUI Flatpak (prepare pinned Cargo inputs as documented in flatpak/README.md):
python3 packaging/flatpak/generate-cargo-sources.py \
  --generator /path/to/preparation/flatpak-cargo-generator.py
flatpak-builder --user --disable-cache \
  build-dir packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml
```

See [`flatpak/README.md`](flatpak/README.md) for the pinned generator,
declared build-only protoc/mold inputs, clean build procedure and evidence
requirements. The build command above exports locally and does not install
or launch the GUI.

## Pre-built bridge tarball (image-baked bridge)

`packaging/release/` builds `snitchwatch-bridge-cli` (binary and manifest only,
no unit) into a deterministic, sha256-pinned
release tarball inside the digest-pinned Fedora 45 image in
`release/pins.env`. `.github/workflows/release.yml` runs it on `v*` tags and
creates a draft GitHub release. It lets an image bake the bridge in
(`/usr/bin`); the system overlay (`system/stage.sh`) supplies its units. Consumer
contract: [`../docs/packaging/bridge-release-artifact.md`](../docs/packaging/bridge-release-artifact.md).

```bash
just release-bridge-repro                       # build + reproducibility check → dist/
just release-verify dist/snitchwatch-bridge-<version>-x86_64.tar.gz <sha256>  # SHA256= from the build
```

The DIY rpm-ostree-layering path is retired; see
[`../docs/packaging/rpm-ostree-layering.md`](../docs/packaging/rpm-ostree-layering.md).

For the full manual verification runbook covering the items above that
need a real Bazzite host (bluebuild image build, Flatpak build, live
opensnitchd dial-in, the closed-window fail-open fix, and the tray-state
transitions added 2026-07-12), see
[`../docs/packaging/phase2-manual-verification-runbook.md`](../docs/packaging/phase2-manual-verification-runbook.md).

## System-bridge overlay (the shipped flavour)

[`system/`](system/) is a separate, socket-activated system deployment for
the dedicated `snitchwatch` account, and the only flavour that ships (#35). See
[`../docs/packaging/system-bridge-integration.md`](../docs/packaging/system-bridge-integration.md)
for the trust boundary, deterministic image staging, desktop-group enrollment,
and Flatpak migration checks. `flatpak/org.snitchwatch.Snitchwatch.system.yml`
is the only Flatpak profile.
