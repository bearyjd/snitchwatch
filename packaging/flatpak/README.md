# GUI Flatpak builds

Both profiles build the Kirigami GUI from source with KDE SDK 6.11 and its
`org.freedesktop.Sdk.Extension.rust-stable` extension. The system profile
uses read-only `/run/snitchwatch` and `/run/snitchwatch-auth` mounts; the
primary profile retains the existing per-user transport. They share an
app-id, so select one profile for each deployment.

The October 5 validation retained KDE 6.9 and reported it as end-of-life.
Both manifests now intentionally select supported stable KDE 6.11 with its
25.08 Rust extension. Record exact SDK/runtime/extension commits and repeat
clean build and default KDE startup validation before publication. Historical
6.9 conditional GUI results remain separate from those new release gates.

The declared compiler inputs currently support **x86_64 only**. The manifests
download protoc 29.3 from the official protobuf release and verify its
SHA256. They install it under `/app/libexec/snitchwatch-build-tools` for
tonic's build script, then remove that directory before export. No protobuf
compiler or Rust compiler belongs in the exported runtime.

The Rust extension supplies mold. GCC needs an `ld.mold` name, so the
manifests create a symlink inside the GUI's temporary build directory and
append `-B` and `-fuse-ld=mold` to the inherited SDK Rust flags. They preserve
the SDK's existing optimization and hardening flags. An unexpected
`CARGO_ENCODED_RUSTFLAGS` causes a build failure because it would override
those flags. This uses GCC's supported linker selection rather than an
external compiler adapter. The pinned SDK must contain GCC 12.1 or newer
and the Rust extension's mold executable. See [GCC link options](https://gcc.gnu.org/onlinedocs/gcc/Link-Options.html),
[SDK mold usage](https://github.com/rui314/mold#how-to-use),
and [CXX-Qt linking](https://kdab.github.io/cxx-qt/book/internals/build-system.html).

## Generate and verify Cargo inputs

Run from the repository root with Python 3.11+, `aiohttp` and `tomlkit`
available in the build-preparation environment. The upstream generator's
dependency requirements are declared in its pinned script; no dependency
installation happens during the app build.

`cargo-generator-pin.json` records the exact upstream revision and script
SHA256. Download that exact URL to a preparation directory, then run:

```sh
python3 packaging/flatpak/generate-cargo-sources.py \
  --generator /path/to/preparation/flatpak-cargo-generator.py
```

The wrapper checks the generator checksum before executing it, invokes it
on the current `Cargo.lock`, and checks every generated crate URL and
checksum plus Cargo source replacement configuration against that lockfile.
It writes `generated-cargo-sources.json` and
`generated-cargo-sources.provenance.json`. Regenerate whenever the lockfile
changes, and retain both outputs with build evidence. The verifier currently
accepts crates.io registry dependencies and local workspace crates; a new
Git dependency requires an explicit verifier update.

## Build and retain evidence

Initialize the pinned `vendor/opensnitch` submodule before preparing source;
the proto crate reads it during compilation. Use a fresh source export and
target directory. Do not include developer `.cargo/config.toml`, `target/`,
cached GUI executables, or external compiler adapters in that export.

After preparing the generated sources, build without installing or launching
the application:

```sh
flatpak-builder --user --disable-cache --keep-build-dirs \
  --state-dir=/path/to/fresh-builder-state \
  --repo=/path/to/fresh-export-repo /path/to/fresh-app-dir \
  packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml
```

For the per-user profile select `org.snitchwatch.Snitchwatch.yml` instead.
Cargo uses `--locked --offline`; source downloads occur before the build.
The build sandbox has no network grant. Install SDK/runtime dependencies in
an isolated Flatpak installation before building and retain their exact
commits; neither manifest pins a mutable runtime branch to a commit itself.

Record the source commit and patch/snapshot hash, OpenSnitch gitlink,
lockfile and generated source hashes, exact SDK/runtime/extension commits,
tool versions, build log, exported metadata, executable and bundle hashes.
Check the freshly linked build executable's `.comment` section for mold; the
standard export strip can remove it. Verify hardening on the exported executable
and inspect final permissions. A finish/export of an existing GUI binary does not
establish a clean source build. Build/export alone also does not establish
guest runtime acceptance or production rollout readiness.
