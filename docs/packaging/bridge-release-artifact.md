# Bridge release artifact (image-baked install)

The Snitchwatch bridge (`snitchwatch-bridge-cli` plus its systemd **user**
unit) is published as a pre-built, sha256-pinned tarball for immutable images
(bazzite-tower, Fedora 44 bootc) to install at image build time: fetch a
release asset by exact version, check a pinned sha256, extract — no package
scriptlets. This is the consumer contract. Design and decisions:
[`../superpowers/plans/2026-10-03-bridge-release-artifact.md`](../superpowers/plans/2026-10-03-bridge-release-artifact.md).

## Release assets

```
https://github.com/bearyjd/snitchwatch/releases/download/v${VERSION}/snitchwatch-bridge-${VERSION}-x86_64.tar.gz
https://github.com/bearyjd/snitchwatch/releases/download/v${VERSION}/snitchwatch-bridge-${VERSION}-x86_64.tar.gz.sha256
```

`.sha256` is one `sha256sum`-format line: `<64 hex>  <tarball basename>`.
Releases are created as **drafts** by `.github/workflows/release.yml`; the URLs
resolve only after the maintainer publishes the draft.

## Tarball layout

One top-level directory, `snitchwatch-bridge-${VERSION}-x86_64/`. Every member
is owned `0:0` (`root:root`), has mtime = the commit's `SOURCE_DATE_EPOCH`,
and directories are `0755`.

| Path (relative to the top dir) | Mode | Installs to |
|---|---|---|
| `usr/bin/snitchwatch-bridge-cli` | 0755 | `/usr/bin/snitchwatch-bridge-cli` |
| `usr/lib/systemd/user/snitchwatch-bridge.service` | 0644 | `/usr/lib/systemd/user/snitchwatch-bridge.service` |
| `usr/share/snitchwatch-bridge/MANIFEST.json` | 0644 | `/usr/share/snitchwatch-bridge/MANIFEST.json` |
| `usr/share/snitchwatch-bridge/SHA256SUMS` | 0644 | `/usr/share/snitchwatch-bridge/SHA256SUMS` |
| `usr/share/licenses/snitchwatch-bridge/*` | 0644 | `/usr/share/licenses/snitchwatch-bridge/` |

The license directory holds `LICENSE` (Snitchwatch's own code,
GPL-3.0-or-later), `LICENSE.opensnitch` (OpenSnitch's GPL-3.0 text — the gRPC
bindings are generated from its `ui.proto`) and `THIRD-PARTY-LICENSES.md`
(SPDX expression plus the verbatim license/notice files of every third-party
crate compiled into the binary — exactly the `cargo tree -p
snitchwatch-bridge-cli --no-default-features` set).

`SHA256SUMS` lists every file except itself as `<sha256>  <path>` with paths
relative to the top dir, so it checks both before install
(`cd <top> && sha256sum -c usr/share/snitchwatch-bridge/SHA256SUMS`) and after
(`cd / && sha256sum -c /usr/share/snitchwatch-bridge/SHA256SUMS`).

**Trust model.** Only the sha256 **you pin** in the image repo authenticates
the artifact — and pin it only after the build-provenance attestation checks
out with the strict form (any workflow/ref in the repo passes a bare `--repo`):

```bash
gh attestation verify "snitchwatch-bridge-${VERSION}-x86_64.tar.gz" \
  --repo bearyjd/snitchwatch \
  --signer-workflow bearyjd/snitchwatch/.github/workflows/release.yml \
  --source-ref "refs/tags/v${VERSION}" \
  --deny-self-hosted-runners
```

`SHA256SUMS`, `MANIFEST.json` and the `.sha256` release asset are integrity
checks, **not authentication**: anyone able to replace the tarball can
regenerate all three. Never fetch the `.sha256` at image-build time and
compare against it — compare against the hash pinned in your repo.

## The binary and the unit

- `snitchwatch-bridge-cli` is built with `--no-default-features`, i.e. **without
  the embedded web UI**: the vendored `web/` frontend is Objective Development's
  GPL-2.0-only code and can't share an executable with the GPL-3.0/Apache-2.0
  inputs (plan decision I). The release bridge therefore answers only the
  token-gated `/stream` WebSocket on its socket; `/`, `/assets/*` and every other
  path are 404 (the Kirigami GUI never uses them). `verify` rejects a binary that
  contains the web UI. It is otherwise self-contained (rustls + bundled CA roots,
  bundled SQLite, no OpenSSL). Its only `NEEDED` libraries are
  `libgcc_s.so.1`, `libm.so.6`, `libc.so.6`, `ld-linux-x86-64.so.2`; it needs
  glibc ≥ 2.34. It is built in
  `registry.fedoraproject.org/fedora:44@sha256:…` (see `packaging/release/pins.env`).
- `--version` prints exactly `snitchwatch-bridge-cli ${VERSION}`; `--help`
  prints usage. Both exit 0 before any I/O (no socket, token, or bind).
  **Without a flag the binary starts a bridge** — never run it bare in an
  image build or smoke test.
- `snitchwatch-bridge.service` is a user unit: `ExecStart=/usr/bin/snitchwatch-bridge-cli`,
  binds gRPC on `127.0.0.1:50051` (the address the shipped opensnitchd config
  dials), WS socket + token under `$XDG_RUNTIME_DIR/snitchwatch/`,
  `WantedBy=default.target`, and `ConditionUser=!@system` so a globally-enabled
  unit never starts for system accounts (e.g. a display-manager greeter).

## MANIFEST.json (schema_version 1)

Sorted keys, 2-space indent. Contents independent of the checkout path.

| Key | Meaning |
|---|---|
| `name`, `version`, `arch` | `snitchwatch-bridge`, the crate version, `x86_64` |
| `source.git_commit` / `git_tag` / `dirty` / `source_date_epoch` | What was built. Releases have `git_tag` = `v${VERSION}` and `dirty: false`. |
| `build.builder_image` | Digest-pinned Fedora 44 image |
| `build.rustc_vv`, `cargo_version`, `protoc_version`, `rpms{}` | Toolchain as built (exact Fedora NEVRAs for rust, cargo, gcc, glibc, glibc-devel, libgcc, binutils, llvm-libs, zlib-ng-compat, python3, protobuf-compiler) |
| `build.rpm_qa`, `build.rpm_qa_sha256` | Every installed RPM (sorted NEVRAs) and its digest — the image digest pins only the base layer, `dnf` pulls live updates |
| `build.command`, `rustflags`, `cargo_lock_sha256` | `cargo build --release --locked -p snitchwatch-bridge-cli --no-default-features`, the path-remap template, the lockfile hash |
| `upstream.opensnitch_commit` / `opensnitch_tag` / `ui_proto_sha256` | The vendored OpenSnitch protocol the bridge speaks (v1.8.0) |
| `runtime.needed`, `runtime.glibc_min` | Dynamic dependencies (verified at build time) |
| `install.*` | The install paths and unit name above |
| `files[]` | `{path, mode, size, sha256}` for every file except `MANIFEST.json`/`SHA256SUMS` |

## Installing in an image build

```bash
SW_VERSION=0.1.0
SW_SHA256=<pinned sha256 of the tarball>
SW_TARBALL="snitchwatch-bridge-${SW_VERSION}-x86_64.tar.gz"

curl -fsSLo "/tmp/${SW_TARBALL}" \
  "https://github.com/bearyjd/snitchwatch/releases/download/v${SW_VERSION}/${SW_TARBALL}"
echo "${SW_SHA256}  /tmp/${SW_TARBALL}" | sha256sum -c -

mkdir -p /tmp/sw && tar -xzf "/tmp/${SW_TARBALL}" -C /tmp/sw
top="/tmp/sw/snitchwatch-bridge-${SW_VERSION}-x86_64"
(cd "$top" && sha256sum -c usr/share/snitchwatch-bridge/SHA256SUMS)

install -Dm0755 "$top/usr/bin/snitchwatch-bridge-cli" /usr/bin/snitchwatch-bridge-cli
install -Dm0644 "$top/usr/lib/systemd/user/snitchwatch-bridge.service" \
  /usr/lib/systemd/user/snitchwatch-bridge.service
install -Dm0644 -t /usr/share/snitchwatch-bridge "$top"/usr/share/snitchwatch-bridge/*
install -Dm0644 -t /usr/share/licenses/snitchwatch-bridge "$top"/usr/share/licenses/snitchwatch-bridge/*
systemctl --global enable snitchwatch-bridge.service
```

`systemctl --global enable` creates
`/etc/systemd/user/default.target.wants/snitchwatch-bridge.service →
/usr/lib/systemd/user/snitchwatch-bridge.service`. A user can opt out with
`systemctl --user mask snitchwatch-bridge.service`.

## What an image smoke test should assert

```bash
test "$(stat -c '%a %u:%g' /usr/bin/snitchwatch-bridge-cli)" = "755 0:0"
test "$(stat -c '%a' /usr/lib/systemd/user/snitchwatch-bridge.service)" = 644
test "$(systemctl --global is-enabled snitchwatch-bridge.service)" = enabled
test "$(readlink /etc/systemd/user/default.target.wants/snitchwatch-bridge.service)" \
  = /usr/lib/systemd/user/snitchwatch-bridge.service
(cd / && sha256sum -c --quiet /usr/share/snitchwatch-bridge/SHA256SUMS)
test "$(jq -r .version /usr/share/snitchwatch-bridge/MANIFEST.json)" = "$SW_VERSION"
test "$(jq -r .source.dirty /usr/share/snitchwatch-bridge/MANIFEST.json)" = false
# protocol pairing: the bridge's vendored OpenSnitch == the installed daemon
test "$(jq -r .upstream.opensnitch_tag /usr/share/snitchwatch-bridge/MANIFEST.json)" = v1.8.0
# ...and assert the daemon you pinned is that same version (however it was
# installed — an extracted RPM may not be in the rpm database).
! ldd /usr/bin/snitchwatch-bridge-cli | grep -q 'not found'
test "$(/usr/bin/snitchwatch-bridge-cli --version)" = "snitchwatch-bridge-cli $SW_VERSION"   # flag only, never bare
XDG_RUNTIME_DIR="$(mktemp -d)" systemd-analyze verify --user \
  /usr/lib/systemd/user/snitchwatch-bridge.service
grep -qx 'ConditionUser=!@system' /usr/lib/systemd/user/snitchwatch-bridge.service
# the unit's gRPC bind must be what the daemon dials, and the daemon fails closed
grep -q 'SNITCHWATCH_GRPC_BIND=127.0.0.1:50051' /usr/lib/systemd/user/snitchwatch-bridge.service
jq -e '.Server.Address == "127.0.0.1:50051" and .DefaultAction == "deny"' \
  /etc/opensnitchd/default-config.json
test ! -e /usr/bin/opensnitch-ui   # upstream GUI would fight the bridge for the daemon
```

## Cutting a release (maintainer)

1. Make sure the repo-root `LICENSE` exists (decision G) and the workspace
   version in `Cargo.toml` is the one you want to ship.
2. On the commit to release: `SW_RELEASE_TAG=vX.Y.Z just release-bridge-repro`,
   then `just release-verify dist/snitchwatch-bridge-X.Y.Z-x86_64.tar.gz <sha256>`
   with the `SHA256=` line it printed.
3. `git tag vX.Y.Z && git push origin vX.Y.Z`. The `Release` workflow rebuilds
   in the same pinned container, rebuilds again from a second checkout path
   and requires identical bytes, re-checks the install, runs the RustSec gate
   (`audit_gate.py`: fails only on advisories in crates the release binary
   actually compiles), then (in the `release` environment) refuses if the tag
   moved or a release already exists, attests provenance, and creates a
   **draft** release.
4. Compare the draft's sha256 with step 2's. They match only if Fedora 44
   shipped no toolchain update in between (`MANIFEST.json` → `build.rpm_qa`
   shows exactly what each build used). Run the strict attestation check
   above, publish the draft, and pin that sha256 in the image.

A `workflow_dispatch` run of `Release` builds and verifies without publishing.

## Local commands

```bash
just release-bridge            # build into dist/ (ALLOW_DIRTY=1 for an uncommitted tree)
just release-bridge-repro      # build twice from different paths; require identical bytes
just release-verify <tarball> <sha256>  # host-side hash check first, then verify + install + unit checks in a throwaway Fedora 44 container
just package-check             # includes the packer/shape tests and a real unit verify
```

All three `release-*` recipes run the same `packaging/release/*.sh` scripts as
CI, inside a fresh container from the pinned image. `release-verify` executes
the tarball's binary, so it checks the sha256 you pass (your trust anchor:
your own build's `SHA256=` line, or an attestation-verified download) on the
host before any container starts, and mounts only temp copies of
`packaging/release/` and the tarball.
Don't run `systemd-analyze verify --user` on the shipped unit on a dev machine
that has its own `~/.config/systemd/user/snitchwatch-bridge.service` — that
copy (and its drop-ins) shadows the file under test; use `just package-check`
or `just release-verify` instead.

## Maintaining the pins

`packaging/release/pins.env` is the single source of truth: builder image
digest, expected rustc and protoc versions, and the vendored OpenSnitch
commit/tag. Builds fail loudly when Fedora 44 ships a new rust or protoc
("bump pins.env"). Bump the image digest in `pins.env` **and** in
`release.yml`'s `container.image` together —
`crates/snitchwatch-bridge/tests/release_shape.rs` fails if they differ.
Byte-reproducibility is claimed only inside the pinned builder image with the
toolchain recorded in the MANIFEST (Fedora's zlib-ng, gcc and glibc-devel all
affect the bytes).

## Known limitations

- **The Kirigami GUI's in-process bridge collides with this service**
  ([#34](https://github.com/bearyjd/snitchwatch/issues/34)): both use
  `127.0.0.1:50051` and `$XDG_RUNTIME_DIR/snitchwatch/`. Don't ship the GUI
  next to the enabled unit until #34 is resolved.
- Concurrent logins of several real users race for `127.0.0.1:50051`; only
  the first user's bridge binds it.
- A per-user `~/.config/systemd/user/snitchwatch-bridge.service` (e.g. from the
  rpm-ostree-layering dev path) overrides the image's unit for that user —
  remove it to run the image-baked binary.
- x86_64 only.
