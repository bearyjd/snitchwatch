# Plan: sha256-pinned release artifact for the Snitchwatch bridge

## Summary

Phase B step 1 of the bazzite-tower plan (OpenSnitch daemon + Snitchwatch GUI +
deny-by-default) needs the bridge as a **pre-built, sha256-pinned binary
artifact** that an immutable bootc image (Fedora 44 base) can consume at image
build time, the same way bazzite-tower already consumes the opensnitch v1.8.0
release RPM: download a release asset by exact version, check a pinned sha256,
extract without running scriptlets.

This plan adds a tag-driven release pipeline that builds
`snitchwatch-bridge-cli` inside a digest-pinned Fedora 44 container, packs it
with the systemd **user** unit and a MANIFEST into a deterministic tarball,
proves the build reproducible, verifies the result (hashes, `ldd`, flags,
`systemd-analyze verify --user`, `systemctl --global enable`), and publishes a
**draft** GitHub release with a build-provenance attestation.

No daemon config, `DefaultAction`, or live `opensnitchd` is touched. The only
binary-visible change is an additive `--help`/`--version` early exit (see
"Approved decisions").

## Approved decisions (owner CONFIRM, 2026-10-03)

| ID | Decision |
|----|----------|
| A | Add `--help`/`-h` and `--version`/`-V` to `snitchwatch-bridge-cli` as an early exit **before any I/O** (no tracing init, no socket, no token, no bind). Any other argv is still ignored, exactly as today; the no-argument path is unchanged. |
| B | Add `ConditionUser=!@system` to `packaging/systemd/snitchwatch-bridge.service` so a globally-enabled unit never starts for system accounts (e.g. a display-manager greeter). |
| C (A1) | Fix `just package-check`'s unit verify: verify a **renamed** temp copy of the unit (avoids `~/.config` drop-in shadowing) with `ExecStart` pointed at a freshly built binary, and fail on any non-zero exit or any diagnostic about that unit. "Skipped" is printed only when `systemd-analyze` is genuinely absent. |
| D | Toolchain = Fedora 44 RPMs (`rust`, `cargo`, `gcc`, `protobuf-compiler`), versions **asserted** against `packaging/release/pins.env`; exact NEVRAs recorded in the MANIFEST. Drift fails the build loudly ("bump pins.env"). No `rust-toolchain.toml`. |
| E | Integrity = sha256 pinned by the consumer; provenance = GitHub artifact attestation (`actions/attest-build-provenance`). cosign deferred. |
| F | First tag `v0.1.0` (= workspace version). Filename has no `fc44`; the builder image is recorded in the MANIFEST. |
| G | **Resolved 2026-10-03: GPL-3.0-or-later** for Snitchwatch's own code (repo-root `LICENSE`, Cargo `license` fields, Flatpak metainfo; consistency asserted in `packaging_shape.rs`). GPL-2.0-only couldn't combine with the GPL-3.0 `ui.proto`-generated code, Apache-2.0-only crates (`prost`, `ring`) or LGPL-3.0 Qt. A *release-mode* build still refuses without `LICENSE`. |
| I | **Resolved 2026-10-03: feature-gate the web UI out of release builds.** The vendored `web/` UI is upstream **GPL-2.0-only** (`web/VENDORED.md` wrongly said or-later; corrected), which can't share an executable with GPL-3.0 / Apache-2.0-only code. New `web-ui` Cargo feature on `snitchwatch-bridge` (passed through by `snitchwatch-bridge-cli`), on by default; the release build uses `--no-default-features`, so `/`, `/assets/*` and the SPA fallback are 404 there. `verify` rejects a binary containing the web UI's copyright line. |
| H1 | **Security (pre-existing), filed as #35:** whoever binds `127.0.0.1:50051` first controls root opensnitchd (incl. persistent `CHANGE_CONFIG`). Not fixed here (needs gRPC over a per-user Unix socket — runtime + daemon config). Blocks bazzite-tower's deny-by-default claim, not this artifact. |
| L1 | **Resolved:** `cargo update -p h2 -p rustls -p rustls-webpki` (RUSTSEC-2026-0258/-0285/-0098/-0099/-0104) and a release gate (`audit-bridge` job + `audit_gate.py`) that fails only on advisories in the crates the release build compiles. |
| M5 | **Keep D for v0.1.0:** no frozen builder image yet; the full `rpm -qa` list and its digest are recorded in the MANIFEST instead. |
| H | File a GitHub issue for the Kirigami in-process-bridge vs. bridge-service collision. Not fixed here (runtime behavior). |

## Findings that shaped this plan (2026-10-03 exploration)

- `.github/workflows/ci.yml` has 4 jobs; none releases. No tags, no releases, no
  root `LICENSE`. Every job checks out `submodules: true`; `vendor/opensnitch`
  is pinned at `b404c4c6316760fa7bc415509d3f8d747f7dc9cc` (v1.8.0) and
  `snitchwatch-proto/build.rs` needs it (compiles `ui.proto` via `protoc`).
- `just package-check`'s unit verify has been **failing silently** in CI
  (run 31074945123: "Command /usr/bin/snitchwatch-bridge-cli is not
  executable" followed by "systemd-analyze not available — skipped"); locally
  it passes only because a dev copy + `override.conf` in
  `~/.config/systemd/user/` shadows the unit under test.
- `snitchwatch-bridge-cli` ignores argv: `--help` starts a full bridge, and a
  second bridge instance replaces the first one's `token` and `bridge.sock`
  *before* failing its gRPC bind (reproduced with two isolated instances).
- The binary is self-contained (rust-embed `web/`, rustls + ring +
  webpki-roots, bundled SQLite, pure-Rust zbus). `ldd`: `linux-vdso`,
  `libgcc_s.so.1`, `libm.so.6`, `libc.so.6`, `ld-linux-x86-64.so.2`. Highest
  `GLIBC_` symbol version: 2.34.
- Fedora 44 (`registry.fedoraproject.org/fedora:44@sha256:e1656bc110fc33e855f7a02e4420c21066428bc7a0277160d40ee125ef8a1c65`):
  glibc 2.43, rust/cargo 1.98.1-1.fc44, protobuf-compiler 3.19.6-20.fc44,
  systemd 259.9. Inside it, with the binary at `/usr/bin`,
  `XDG_RUNTIME_DIR=… systemd-analyze verify --user` exits 0 (it errors without
  `XDG_RUNTIME_DIR`), and `systemctl --global enable` creates
  `/etc/systemd/user/default.target.wants/snitchwatch-bridge.service →
  /usr/lib/systemd/user/snitchwatch-bridge.service`.
- rust-embed 8.11.0 embeds each `web/` file's mtime/ctime unless
  `SOURCE_DATE_EPOCH` is set (`rust-embed-utils/src/lib.rs:103-129`).

## Consumer contract (what bazzite-tower pins)

- Release URL pattern:
  `https://github.com/bearyjd/snitchwatch/releases/download/v${VERSION}/snitchwatch-bridge-${VERSION}-x86_64.tar.gz`
  plus `…tar.gz.sha256` (`sha256sum` format: `<64 hex>  <basename>\n`).
- Tarball: single top dir `snitchwatch-bridge-${VERSION}-x86_64/`, every
  member uid/gid 0 (`root`/`root`), mtime = `SOURCE_DATE_EPOCH`, dirs 0755:

  | Member (relative to top dir) | Mode |
  |---|---|
  | `usr/bin/snitchwatch-bridge-cli` | 0755 |
  | `usr/lib/systemd/user/snitchwatch-bridge.service` | 0644 |
  | `usr/share/snitchwatch-bridge/MANIFEST.json` | 0644 |
  | `usr/share/snitchwatch-bridge/SHA256SUMS` | 0644 |
  | `usr/share/licenses/snitchwatch-bridge/*` | 0644 |

- `SHA256SUMS` lines are `<sha256>  <path relative to top dir>` for every
  file except itself (including `MANIFEST.json`), sorted by path, so both
  `cd <top> && sha256sum -c usr/share/snitchwatch-bridge/SHA256SUMS` and,
  after install, `cd / && sha256sum -c /usr/share/snitchwatch-bridge/SHA256SUMS`
  work.
- Unit name: `snitchwatch-bridge.service` (user unit), enabled at image build
  with `systemctl --global enable snitchwatch-bridge.service`.

### MANIFEST.json (schema_version 1)

Keys are emitted sorted, 2-space indent, trailing newline. Nothing in it may
depend on the absolute checkout path (the repro check builds from two paths).

```json
{
  "schema_version": 1,
  "name": "snitchwatch-bridge",
  "version": "0.1.0",
  "arch": "x86_64",
  "source": {
    "git_commit": "<40 hex>",
    "git_tag": "v0.1.0 or null",
    "dirty": false,
    "source_date_epoch": 0
  },
  "build": {
    "builder_image": "registry.fedoraproject.org/fedora:44@sha256:…",
    "command": "cargo build --release --locked -p snitchwatch-bridge-cli",
    "rustflags": "--remap-path-prefix=<src>=/snitchwatch … (placeholders, not real paths)",
    "rustc_vv": "<verbatim `rustc -vV`>",
    "cargo_version": "<verbatim `cargo -V`>",
    "protoc_version": "libprotoc 3.19.6",
    "rpms": {"rust": "…", "cargo": "…", "gcc": "…", "glibc": "…", "protobuf-compiler": "…"},
    "cargo_lock_sha256": "<hex>"
  },
  "upstream": {
    "opensnitch_commit": "b404c4c6316760fa7bc415509d3f8d747f7dc9cc",
    "opensnitch_tag": "v1.8.0",
    "ui_proto_sha256": "<hex of vendor/opensnitch/proto/ui.proto>"
  },
  "runtime": {
    "needed": ["ld-linux-x86-64.so.2", "libc.so.6", "libgcc_s.so.1", "libm.so.6"],
    "glibc_min": "2.34"
  },
  "install": {
    "binary": "/usr/bin/snitchwatch-bridge-cli",
    "unit": "/usr/lib/systemd/user/snitchwatch-bridge.service",
    "unit_name": "snitchwatch-bridge.service",
    "unit_scope": "user"
  },
  "files": [
    {"path": "usr/bin/snitchwatch-bridge-cli", "mode": "0755", "size": 0, "sha256": "<hex>"}
  ]
}
```

`files[]` lists every tarball file except `MANIFEST.json` and `SHA256SUMS`,
sorted by path. `build`/`upstream`/`runtime` come from a `--meta` JSON file
(see `bridge_artifact.py buildinfo`); `install` is constant.

## Files to change / add

| File | Change |
|---|---|
| `crates/snitchwatch-bridge-cli/src/cli.rs` (new) | Pure argv classification + usage/version text. |
| `crates/snitchwatch-bridge-cli/src/lib.rs` | `pub mod cli;` |
| `crates/snitchwatch-bridge-cli/src/main.rs` | Early exit on `--help`/`--version` before tracing init. |
| `crates/snitchwatch-bridge-cli/tests/cli_flags.rs` (new) | Spawn the real binary: flags exit 0 with no I/O; no-arg still starts. |
| `packaging/systemd/snitchwatch-bridge.service` | `ConditionUser=!@system` in `[Unit]` (+ comment). |
| `crates/snitchwatch-bridge/tests/packaging_shape.rs` | Unit contract assertions for the image-baked path. |
| `packaging/release/pins.env` (new) | Single source of truth for pins. |
| `packaging/release/install-deps.sh` (new) | The one dnf dependency list (container only). |
| `packaging/release/build-bridge.sh` (new) | Preflight, build, stage, pack, self-verify. Runs in the F44 container. |
| `packaging/release/bridge_artifact.py` (new) | `version` / `buildinfo` / `pack` / `verify` subcommands (stdlib only). |
| `packaging/release/repro-check.sh` (new) | Second build from a copy at another path; byte-compare tarballs. |
| `packaging/release/check-install.sh` (new) | Throwaway-container-only: install to `/`, `sha256sum -c`, unit verify, `--global enable`. |
| `packaging/release/verify-unit.sh` (new) | `just package-check`'s real unit verify (decision C). |
| `crates/snitchwatch-bridge/tests/release_artifact.rs` (new) | Drives `bridge_artifact.py` against a fixture stage. |
| `crates/snitchwatch-bridge/tests/release_shape.rs` (new) | Shape test for `release.yml` + `pins.env` + build script. |
| `.github/workflows/release.yml` (new) | Tag-driven build → verify → repro → attest → draft release. |
| `justfile` | `release-bridge`, `release-bridge-repro`, `release-verify`; `package-check` uses `verify-unit.sh`. |
| `.gitignore` | `/dist/` |
| `docs/packaging/bridge-release-artifact.md` (new) | Consumer contract + how to cut a release. |
| `packaging/README.md`, `README.md`, `HANDOFF.md` | Point at the new artifact; record status, risks. |

## Interfaces (executors implement exactly these)

### `packaging/release/pins.env`

POSIX `KEY=value` lines, no quoting needed, sourced by bash:

```
SW_BUILDER_IMAGE=registry.fedoraproject.org/fedora:44@sha256:e1656bc110fc33e855f7a02e4420c21066428bc7a0277160d40ee125ef8a1c65
SW_RUST_VERSION=1.98.1
SW_PROTOC_VERSION=3.19.6
SW_ARCH=x86_64
SW_OPENSNITCH_COMMIT=b404c4c6316760fa7bc415509d3f8d747f7dc9cc
SW_OPENSNITCH_TAG=v1.8.0
```

### `build-bridge.sh` (container only)

Environment in:

- `SRC_DIR` (default: repo root derived from the script path) — may be a
  **read-only** mount; the script never writes into it.
- `OUT_DIR` (default `$SRC_DIR/dist` — only when not read-only; the just
  recipe passes `/out`).
- `CARGO_TARGET_DIR` (default `/tmp/sw-target`), `CARGO_HOME` (default
  `${HOME}/.cargo`).
- `SW_GIT_SHA` (optional; else `git rev-parse HEAD`).
- `SW_RELEASE_TAG` (optional; when non-empty → **release mode**: must equal
  `v$VERSION`, tree must be clean, repo-root `LICENSE` must exist).
- `ALLOW_DIRTY=1` (dev mode only; recorded as `"dirty": true`).
- `SW_INSTALL_DEPS=1` → run `install-deps.sh` first.

Git is always invoked with `safe.directory=*` (via `GIT_CONFIG_COUNT` env) and
`GIT_OPTIONAL_LOCKS=0` (read-only mount, CI container uid mismatch).

Steps: arch == `SW_ARCH`; version via `bridge_artifact.py version`; preflight
(rustc/protoc versions vs pins; gitlink `HEAD:vendor/opensnitch` and the
checked-out submodule HEAD both == `SW_OPENSNITCH_COMMIT`; dirty check via
`git status --porcelain --ignore-submodules=untracked`; release-mode
checks); `SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)`;
`RUSTFLAGS="--remap-path-prefix=$SRC_DIR=/snitchwatch --remap-path-prefix=$CARGO_HOME=/cargo --remap-path-prefix=$CARGO_TARGET_DIR=/target"`;
`cargo build --release --locked -p snitchwatch-bridge-cli`; stage under a temp
dir (binary 0755, unit 0644, license files 0644: repo-root `LICENSE` →
`LICENSE` if present, `vendor/opensnitch/LICENSE` → `LICENSE.opensnitch`,
`web/VENDORED.md` → `VENDORED-web.md`); `buildinfo` → meta JSON; `pack`;
`verify --check-ldd --run-flags`. Prints the tarball path and sha256 last.

### `bridge_artifact.py` (Python ≥ 3.11, stdlib only)

- `version --repo DIR` → prints the `snitchwatch-bridge-cli` version
  (crate `version.workspace = true` → `[workspace.package] version`, via
  `tomllib`).
- `buildinfo --repo DIR --binary PATH --builder-image REF --rustflags-template STR`
  → prints the meta JSON (`build`, `upstream`, `runtime` objects). Runs
  `rustc -vV`, `cargo -V`, `protoc --version`, `rpm -q --qf …` for the 5 RPMs,
  `readelf -d` (NEEDED, sorted), `objdump -T` (max `GLIBC_x.y`), hashes
  `Cargo.lock` and `vendor/opensnitch/proto/ui.proto`, reads the submodule
  commit with git.
- `pack --stage DIR --name snitchwatch-bridge --version V --arch A
  --git-commit SHA [--git-tag T] [--dirty] --source-date-epoch N --meta FILE
  --out DIR`
  - `--stage` is the *top-dir contents* (contains `usr/…`).
  - Refuses: `--git-tag` given and != `v$V`; any file outside the allowed
    layout; missing binary or unit; symlinks/special files; `--git-commit`
    not 40 hex.
  - Normalises modes (0755 binary, 0644 everything else, 0755 dirs) — never
    trusts the stage's umask. Does **not** mutate the stage.
  - Writes `<name>-<V>-<A>.tar.gz` (USTAR, sorted members incl. dirs,
    uid/gid 0, uname/gname `root`, mtime N, gzip mtime 0, no filename in the
    gzip header) and `<…>.tar.gz.sha256`.
- `verify --tarball PATH [--expect-version V] [--check-ldd] [--run-flags]`
  - `.sha256` sibling must exist and match.
  - Safe extraction to a temp dir: only regular files + dirs, no absolute
    paths, no `..`, every member under the single top dir
    `snitchwatch-bridge-<V>-<A>/`, uid/gid 0.
  - Exact layout + modes; MANIFEST schema, `files[]` ↔ extracted bytes
    (sha256, size, mode) one-to-one; `SHA256SUMS` re-checked; member mtimes ==
    `source.source_date_epoch`.
  - `--check-ldd`: `ldd` has no `not found`; `readelf -d` NEEDED ⊆
    `{libgcc_s.so.1, libm.so.6, libc.so.6, ld-linux-x86-64.so.2}` and equals
    `runtime.needed`.
  - `--run-flags`: with `XDG_RUNTIME_DIR` and `SNITCHWATCH_WS_SOCKET` pointed
    into a fresh short temp dir and a 5s timeout: `--version` stdout ==
    `snitchwatch-bridge-cli <V>\n`, `--help` exits 0 and mentions
    `SNITCHWATCH_GRPC_BIND`; the temp dir is still empty afterwards.
  - Exit 0 on success; non-zero with one `verify: FAIL: <reason>` line per
    failure.

### `check-install.sh <tarball>`

Refuses to run unless `SW_THROWAWAY_CONTAINER=1` **and** a container marker
(`/run/.containerenv` or `/.dockerenv`) exists — it writes to `/usr`. Installs
each file with `install -D -m <mode from MANIFEST>` to `/`, then:
`cd / && sha256sum -c /usr/share/snitchwatch-bridge/SHA256SUMS`;
`XDG_RUNTIME_DIR=$(mktemp -d) systemd-analyze verify --user
/usr/lib/systemd/user/snitchwatch-bridge.service` (non-zero or any output line
naming the unit = fail); `systemctl --global enable snitchwatch-bridge.service`;
`systemctl --global is-enabled` == `enabled`; the `default.target.wants`
symlink target == `/usr/lib/systemd/user/snitchwatch-bridge.service`;
`/usr/bin/snitchwatch-bridge-cli --version`.

### `repro-check.sh <first-tarball>`

Copies `SRC_DIR` (including `.git`, excluding `target/` and `dist/`) to a
different absolute path, runs `build-bridge.sh` there with fresh
`CARGO_TARGET_DIR`/`OUT_DIR` and the same `SW_*` env, and fails unless the
two tarballs are byte-identical (on mismatch, lists the members whose
sha256 differ).

### `release.yml`

- `on: push: tags: ['v*']` and `workflow_dispatch` (build/verify only).
- Top-level `permissions: contents: read`.
- Job `build-bridge`: `ubuntu-latest`, `container: image: <SW_BUILDER_IMAGE>`;
  `dnf install git` first (checkout needs it for submodules);
  `actions/checkout` with `submodules: true`, `persist-credentials: false`;
  `install-deps.sh`; `build-bridge.sh` with `SW_GIT_SHA=${{ github.sha }}` and
  `SW_RELEASE_TAG` = the tag on tag pushes; `repro-check.sh`;
  `check-install.sh` with `SW_THROWAWAY_CONTAINER=1`; upload `dist/` artifact.
- Job `publish`: `if: github.ref_type == 'tag'`, `needs: build-bridge`, the
  **only** job with `contents: write` (+ `id-token: write`,
  `attestations: write`); download artifact, `sha256sum -c`,
  `actions/attest-build-provenance`, `gh release create "$TAG" … --draft
  --verify-tag`.
- Every `uses:` pinned to a full 40-hex commit SHA with a `# vX.Y.Z` comment.

## Amendments after review (2026-10-03)

Code review, security review and a Codex review changed these interfaces;
where they conflict with the sections above, this list wins.

- Build command: `cargo build --release --locked -p snitchwatch-bridge-cli --no-default-features` (decision I).
- `verify` gains `--expect-sha256 HEX` (checked first; required by `--check-ldd`/`--run-flags`) and `--extract-to DIR`. It reads the tarball into memory with size caps, checks every header before reading contents, and requires the uncompressed tar to equal the canonical re-serialization of its own contents (no appended members, pax headers, extra gzip members). Deep MANIFEST schema/type checks; `git_tag` implies `dirty: false`; rejects an embedded web UI.
- `check-install.sh <tarball> <expected-sha256>` installs from `verify --extract-to`'s output with a path/mode allowlist (no second `tar -x`).
- `bridge_artifact.py thirdparty --repo DIR --out FILE` writes `THIRD-PARTY-LICENSES.md` for exactly the crates compiled into the release build (`cargo tree … --no-default-features` ∩ `cargo metadata`); `VENDORED-web.md` is no longer shipped.
- MANIFEST `build` adds `rpm_qa` / `rpm_qa_sha256` and more `rpms` entries (binutils, glibc-devel, libgcc, llvm-libs, zlib-ng-compat, python3).
- Git-ignored files under build inputs count as dirty; tool-override env vars are unset; in release mode an existing local tag must resolve to HEAD.
- `repro-check.sh` rebuilds with a fresh `CARGO_HOME` and `HOME` too.
- `release.yml`: `audit-bridge` job (RustSec gate, L1); `publish` needs both jobs, runs in `environment: release`, refuses a moved tag or an existing release/draft, prints the strict `gh attestation verify` form; job timeouts.
- `just release-verify <tarball> <sha256>`: host-side hash check first, mounts only temp copies.
- `release.yml` peels the pushed ref to a commit (`git rev-parse "$GITHUB_SHA^{commit}"`) for `SW_GIT_SHA` and exports it as `build-bridge.outputs.commit`; publish's tag guard compares against that, so annotated/signed tags work (Codex review, P2).
- `snitchwatch-bridge-cli` classifies argv before the tokio runtime exists; closed stdout on `--help`/`--version` exits 0.

## NOT building

- The Kirigami artifact (report only; blocked on decision H's collision and on
  settled decision #3 — GUI ships as a Flatpak).
- An RPM (no scriptlets are wanted; ownership benefits don't justify a spec yet).
- cosign signing; aarch64; a frozen builder image.
- Any change to `opensnitchd` config, `DefaultAction`, or bridge runtime
  behavior beyond decision A.

## Testing strategy

- `cli_flags.rs`: `--help`/`-h`/`--version`/`-V` exit 0 within 5s, expected
  stdout, temp `SNITCHWATCH_WS_SOCKET` dir stays empty; no-arg control run
  prints `GRPC_LISTEN_ADDR=` (bridge still starts) then is killed.
- `cli.rs` unit tests for argv classification (help wins over version; other
  args → none).
- `packaging_shape.rs`: exact `ExecStart=/usr/bin/snitchwatch-bridge-cli`, no
  `%h`/`~`/`.local` outside comments, `[Install]` `WantedBy=default.target`,
  `ConditionUser=!@system` in `[Unit]`.
- `release_artifact.rs` (fixture stage, no container): double pack
  byte-identical; exact `tar -tvz --numeric-owner` layout/modes/owners/mtime;
  MANIFEST `files[]` matches `sha256sum` of extracted files; `sha256sum -c`
  of `SHA256SUMS` passes; `.sha256` format; verify passes; verify fails after
  repacking with a modified binary + regenerated `.sha256`; pack refuses
  tag/version mismatch and unexpected files.
- `release_shape.rs`: workflow trigger, container digest == `pins.env`,
  `submodules: true`, permissions scoping, `--draft`, SHA-pinned `uses:`,
  build command uses `--release --locked -p snitchwatch-bridge-cli`.
- Container-level (local `just release-bridge-repro` + `just release-verify`,
  and in `release.yml`): real build, repro byte-compare, `ldd`, flags,
  `systemd-analyze verify --user`, `systemctl --global enable`.

## Validation commands

```bash
cargo fmt --all -- --check
just check            # cargo check + clippy -D warnings (workspace)
just test             # workspace tests
just package-check    # now really verifies the unit
just release-bridge-repro
just release-verify dist/snitchwatch-bridge-0.1.0-x86_64.tar.gz
```

## Acceptance criteria

- Reproducible: two container builds from different checkout paths produce a
  byte-identical tarball.
- MANIFEST hashes match the files; `SHA256SUMS` validates.
- `just package-check` green (and genuinely verifying), clippy `-D warnings`
  and tests green.
- No change to daemon config or bridge runtime behavior beyond decision A.

## Risks

- **Kirigami in-process bridge collides with the enabled user service**
  (decision H issue). Blocks shipping the GUI next to the enabled unit, not
  this artifact.
- **Toolchain drift** (decision D) fails release builds after Fedora updates
  until `pins.env` is bumped. A pinned image digest can also be garbage
  collected upstream → bump.
- **Gzip bytes depend on the zlib implementation** (Fedora uses zlib-ng), so
  byte-reproduction is only claimed inside the pinned builder image.
- **Multi-user / greeter**: `ConditionUser=!@system` covers system accounts;
  concurrent real users still race for `127.0.0.1:50051` (pre-existing).
- **License (decision G)** blocks the first tagged release by design.
