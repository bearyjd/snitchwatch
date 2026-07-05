# Snitchwatch packaging (Phase 2)

Everything needed to distribute Component A (the GUI + bridge + daemon) on
Bazzite / Universal Blue.

## Architecture at a glance

Three processes, three trust levels:

| Process                  | Where it runs                    | Packaged as |
| ------------------------ | -------------------------------- | ----------- |
| `opensnitchd`            | Host, privileged (root)          | Baked into a bluebuild image **or** rpm-ostree layered |
| `snitchwatch-bridge-cli` | Host, unprivileged (`--user`)    | systemd user unit (`systemd/snitchwatch-bridge.service`) |
| `snitchwatch-tauri` GUI  | Flatpak sandbox, unprivileged    | Flatpak (`flatpak/org.snitchwatch.Snitchwatch.yml`) |

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

## Files

```
packaging/
├── bluebuild/
│   ├── recipe.yml                                  # batteries-included image recipe
│   └── files/system/etc/opensnitchd/default-config.json  # fail-closed daemon config (canonical)
├── flatpak/
│   ├── org.snitchwatch.Snitchwatch.yml             # GUI-only Flatpak manifest (no --share=network)
│   ├── org.snitchwatch.Snitchwatch.desktop
│   └── org.snitchwatch.Snitchwatch.metainfo.xml
└── systemd/
    └── snitchwatch-bridge.service                  # host-side bridge, systemd --user
```

## Build (needs tooling absent from CI)

None of these can be built in the CI sandbox — they need a real Bazzite host
plus `bluebuild` / `flatpak-builder`. The files are authored as complete,
correct artifacts and their syntax is validated in CI (YAML/JSON parse +
`systemd-analyze verify` where available).

```bash
# Batteries-included image (needs the bluebuild CLI + podman/buildah):
bluebuild build packaging/bluebuild/recipe.yml

# GUI Flatpak (needs flatpak-builder + the GNOME 46 runtime):
python3 flatpak-cargo-generator.py Cargo.lock \
  -o packaging/flatpak/generated-cargo-sources.json
flatpak run org.flatpak.Builder --user --install --force-clean \
  build-dir packaging/flatpak/org.snitchwatch.Snitchwatch.yml
```

For the lightweight path's step-by-step (including installing the bridge user
service and end-to-end verification), follow
[`../docs/packaging/rpm-ostree-layering.md`](../docs/packaging/rpm-ostree-layering.md).
