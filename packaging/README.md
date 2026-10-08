# Snitchwatch packaging (Phase 2)

Everything needed to distribute Component A (the GUI + bridge + daemon) on
Bazzite / Universal Blue.

## Architecture at a glance

Three processes, three trust levels:

| Process                     | Where it runs                    | Packaged as |
| --------------------------- | --------------------------------- | ----------- |
| `opensnitchd`               | Host, privileged (root)          | Baked into a bluebuild image **or** rpm-ostree layered |
| `snitchwatch-bridge-cli`    | Host, unprivileged (`--user`)    | systemd user unit (`systemd/snitchwatch-bridge.service`) |
| `snitchwatch-kirigami` GUI  | Flatpak sandbox, unprivileged    | Flatpak (`flatpak/org.snitchwatch.Snitchwatch.yml`) |

The Flatpak packages `snitchwatch-kirigami` (Qt6/QML + Kirigami), not
`snitchwatch-tauri` — Kirigami is the settled GUI stack going forward (see
this repo's `CLAUDE.md` "Settled architecture decisions" #4) and has reached
feature parity including its Task 7 safety verification. `snitchwatch-tauri`
and `web/` remain in the repo but are intentionally not what ships here —
kept until this Flatpak's first real packaged release ships (owner
decision, 2026-07-11); see
[`../docs/superpowers/plans/2026-07-04-kirigami-shell-rewrite.md`](../docs/superpowers/plans/2026-07-04-kirigami-shell-rewrite.md)'s
status note for detail.

The GUI reaches the host-side bridge over a **Unix domain socket** under
`$XDG_RUNTIME_DIR/snitchwatch/`, granted to the sandbox via
`--filesystem=xdg-run/snitchwatch`. There is deliberately **no
`--share=network`** on the Flatpak: a Flatpak's private network namespace
cannot reach host loopback anyway, and that permission would grant full
internet access rather than scoped loopback. See
[`../docs/superpowers/specs/2026-07-04-flatpak-feasibility-research.md`](../docs/superpowers/specs/2026-07-04-flatpak-feasibility-research.md).

## The two install paths

- **Batteries-included** — a signed custom Bazzite image with `opensnitchd`
  baked in and enabled from first boot: [`bluebuild/recipe.yml`](bluebuild/recipe.yml).
- **Lightweight / DIY** — layer `opensnitchd` onto stock Bazzite with
  `rpm-ostree`: [`../docs/packaging/rpm-ostree-layering.md`](../docs/packaging/rpm-ostree-layering.md).

Both ship the same fail-**closed** daemon config
([`bluebuild/files/system/etc/opensnitchd/default-config.json`](bluebuild/files/system/etc/opensnitchd/default-config.json)):
`DefaultAction: deny` and `Server.Address: 127.0.0.1:50051`.

## The packaged fetch rule

With `DefaultAction: deny` and no GUI attached, the daemon would deny the
system bridge's own blocklist downloads. Snitchwatch therefore ships exactly
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
  - On today's bluebuild image, which runs the per-user bridge, the rule
    is inert for this reason.
  - With the system-bridge overlay, sysusers creates the account before
    `opensnitch.service` starts.
  - On a live host, restart `opensnitch.service` once after the account
    exists.
- **The Rules page shows it read-only.** It is listed like any daemon rule
  but can't be toggled: the daemon reports `user.name` as the uid, and
  sending that back would break the rule. It can still be deleted.
- **Not covered:** DNS, and list URLs on other ports.
- **Where it ships:**
  - the bluebuild image (`files` module);
  - the system-bridge overlay (`system/stage.sh` installs the same file
    to `/etc/opensnitchd/rules/`, mode 0644).

  The release tarball and the Flatpak don't carry it: the tarball ships
  the per-user bridge, which the rule deliberately doesn't match.

Details and the daemon-source reasoning:
[`../docs/superpowers/plans/2026-10-08-packaged-bridge-fetch-rule.md`](../docs/superpowers/plans/2026-10-08-packaged-bridge-fetch-rule.md).

## Files

```
packaging/
├── bluebuild/
│   ├── recipe.yml                                  # batteries-included image recipe
│   └── files/system/etc/opensnitchd/
│       ├── default-config.json                     # fail-closed daemon config (canonical)
│       └── rules/000-snitchwatch-bridge-fetch.json # the one shipped allow rule (canonical)
├── flatpak/
│   ├── org.snitchwatch.Snitchwatch.yml             # GUI-only Flatpak manifest (no --share=network)
│   ├── org.snitchwatch.Snitchwatch.system.yml      # alternative system-bridge profile (same app-id)
│   ├── org.snitchwatch.Snitchwatch.desktop
│   └── org.snitchwatch.Snitchwatch.metainfo.xml
├── systemd/
│   └── snitchwatch-bridge.service                  # legacy host-side user bridge
└── system/                                         # pending system-bridge overlay
```

## Build (needs tooling absent from CI)

None of these can be built in the CI sandbox — they need a real Bazzite host
plus `bluebuild` / `flatpak-builder`. The files are authored as complete,
correct artifacts and their syntax is validated in CI (YAML/JSON parse +
`systemd-analyze verify` where available).

```bash
# Batteries-included image (needs the bluebuild CLI + podman/buildah):
bluebuild build packaging/bluebuild/recipe.yml

# GUI Flatpak (prepare pinned Cargo inputs as documented in flatpak/README.md):
python3 packaging/flatpak/generate-cargo-sources.py \
  --generator /path/to/preparation/flatpak-cargo-generator.py
flatpak-builder --user --disable-cache \
  build-dir packaging/flatpak/org.snitchwatch.Snitchwatch.yml
```

See [`flatpak/README.md`](flatpak/README.md) for the pinned generator,
declared build-only protoc/mold inputs, clean build procedure and evidence
requirements. The build command above exports locally and does not install
or launch the GUI.

## Pre-built bridge tarball (image-baked bridge)

`packaging/release/` builds `snitchwatch-bridge-cli` plus
`systemd/snitchwatch-bridge.service` into a deterministic, sha256-pinned
release tarball inside the digest-pinned Fedora 44 image in
`release/pins.env`. `.github/workflows/release.yml` runs it on `v*` tags and
creates a draft GitHub release. It lets an image bake the bridge in
(`/usr/bin` + `/usr/lib/systemd/user`, enabled with
`systemctl --global enable`) instead of installing it per user. Consumer
contract: [`../docs/packaging/bridge-release-artifact.md`](../docs/packaging/bridge-release-artifact.md).

```bash
just release-bridge-repro                       # build + reproducibility check → dist/
just release-verify dist/snitchwatch-bridge-<version>-x86_64.tar.gz <sha256>  # SHA256= from the build
```

For the lightweight path's step-by-step (including installing the bridge user
service and end-to-end verification), follow
[`../docs/packaging/rpm-ostree-layering.md`](../docs/packaging/rpm-ostree-layering.md).

For the full manual verification runbook covering the items above that
need a real Bazzite host (bluebuild image build, Flatpak build, live
opensnitchd dial-in, the closed-window fail-open fix, and the tray-state
transitions added 2026-07-12), see
[`../docs/packaging/phase2-manual-verification-runbook.md`](../docs/packaging/phase2-manual-verification-runbook.md).

## System-bridge overlay (pending a new release)

[`system/`](system/) is a separate, socket-activated system deployment for
the dedicated `snitchwatch` account. It is intentionally not substituted for
the legacy user-service assets above until a bridge release carries the new
binary contract. See
[`../docs/packaging/system-bridge-integration.md`](../docs/packaging/system-bridge-integration.md)
for the trust boundary, deterministic image staging, desktop-group enrollment,
and Flatpak migration checks. Use `flatpak/org.snitchwatch.Snitchwatch.system.yml`
only for that system deployment; it deliberately shares the primary profile's
app-id and is not coinstallable with it.
