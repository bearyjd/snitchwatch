default:
    @just --list

# Checks for one-time setup steps an agent/dev is likely to hit cold
# (currently: Playwright browsers for the two smoke suites). Exits non-zero
# with a fix hint if something's missing; does not modify anything.
doctor:
    #!/usr/bin/env bash
    set -euo pipefail
    missing=0
    if [ ! -d tests/web_smoke/node_modules ]; then
        echo "MISSING: tests/web_smoke/node_modules — run 'just web-smoke-install' before 'just web-smoke'"
        missing=1
    fi
    if [ ! -d tests/tauri_smoke/node_modules ]; then
        echo "MISSING: tests/tauri_smoke/node_modules — run 'just tauri-smoke-install' before 'just tauri-smoke'"
        missing=1
    fi
    if [ "$missing" -eq 0 ]; then
        echo "doctor: all one-time setup steps look done"
    fi
    exit "$missing"

build:
    cargo build --workspace

test:
    cargo test --workspace

test-bridge:
    cargo test -p snitchwatch-bridge

check:
    cargo check --workspace
    cargo clippy --workspace --all-targets -- -D warnings

fmt:
    cargo fmt --all

regen-proto:
    cargo build -p snitchwatch-proto

run-bridge:
    RUST_LOG=info cargo run -p snitchwatch-bridge-cli

run-spike endpoint="http://127.0.0.1:50051":
    RUST_LOG=info cargo run -p snitchwatch-spike -- {{endpoint}}

# Re-run the idempotent rebrand pass over the vendored web/ tree.
web-rebrand:
    ./web/rebrand.sh
    @git diff --stat web/

# Run the Playwright smoke tests against a freshly built bridge.
web-smoke:
    cd tests/web_smoke && npx playwright test

# Install the Playwright Firefox channel into tests/web_smoke/node_modules.
web-smoke-install:
    cd tests/web_smoke && npm install && npx playwright install firefox

# Run the Tauri shell in dev mode (live bridge + native window). Kept
# alongside kirigami-dev until Tauri/web/ are retired (see the
# kirigami-shell-rewrite plan's status note) — Kirigami is the shell that
# actually ships.
tauri-dev:
    cargo run -p snitchwatch-tauri

# Build a release Tauri bundle (deb/rpm/appimage as configured in tauri.conf.json)
tauri-build:
    cargo build -p snitchwatch-tauri --release

# Run the Kirigami shell in dev mode (live bridge + native Qt6/QML window).
# Requires system Qt6 + KDE Frameworks 6 (Kirigami) dev packages — see
# CLAUDE.md's "kirigami-spike and snitchwatch-kirigami are excluded from
# default-members" note. This is the shell that actually ships (see
# packaging/README.md).
kirigami-dev:
    cargo run -p snitchwatch-kirigami

# Build a release Kirigami shell binary.
kirigami-build:
    cargo build -p snitchwatch-kirigami --release

# Qt-level input-routing tests for the Kirigami shell, via qmltestrunner.
#
# These synthesise REAL mouse events (QtTest `mouseClick()`), which the
# cxx-qt test harness cannot do — cxx-qt-lib exposes no way to post a
# QMouseEvent, so `cargo test` can only call QML functions directly.
#
# They test structural mirrors of the delegates, NOT the real pages:
# qmltestrunner cannot load `com.snitchwatch.shell` because cxx-qt links
# those types statically into each binary rather than shipping a QML plugin.
# `tests/qml_source_guards.rs` covers the real files.
#
# Needs qt6-qtdeclarative (qmltestrunner-qt6 + the QtTest QML module), which
# the Kirigami build already requires.
qml-test:
    QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic QT_FORCE_STDERR_LOGGING=1 \
        qmltestrunner-qt6 -input crates/snitchwatch-kirigami/tests/qml

# Playwright smoke test for the Tauri shell (requires `npm install` in tests/tauri_smoke first)
tauri-smoke:
    cd tests/tauri_smoke && npx playwright test

# One-time install of the Playwright deps
tauri-smoke-install:
    cd tests/tauri_smoke && npm install && npx playwright install firefox

# Run only the blocklist test suite. Offline: lists come from fixture
# fetchers and local TLS servers, never the network (the bridge fetches only
# https from non-local addresses, so there is no plain-HTTP fixture server).
test-blocklists:
    cargo test -p snitchwatch-bridge blocklists -- --nocapture
    cargo test -p snitchwatch-bridge --test blocklists_e2e -- --nocapture

# Validate the packaging artifacts without a Bazzite host. Parses the
# YAML/JSON, really verifies the systemd unit (verify-unit.sh: a renamed copy
# pointed at a freshly built binary, so a ~/.config drop-in can't shadow it
# and a failure can't be mistaken for "systemd-analyze missing"), and runs the
# Rust packaging/release shape tests plus the release-tarball packer tests, and
# shellchecks the release scripts (skipped when shellcheck isn't installed).
package-check:
    python3 -c "import yaml,sys; yaml.safe_load(open('packaging/bluebuild/recipe.yml')); yaml.safe_load(open('packaging/flatpak/org.snitchwatch.Snitchwatch.yml')); yaml.safe_load(open('.github/workflows/release.yml')); print('YAML ok')"
    python3 -c "import json; json.load(open('packaging/bluebuild/files/system/etc/opensnitchd/default-config.json')); print('JSON ok')"
    bash packaging/release/verify-unit.sh
    if command -v shellcheck >/dev/null 2>&1; then shellcheck packaging/release/*.sh; else echo "shellcheck not installed — skipped"; fi
    cargo test -p snitchwatch-bridge --test packaging_shape --test release_shape --test release_artifact
    # The release tarball's feature set (no embedded web UI, decision I) —
    # nothing else builds it, so lint and test it here.
    cargo clippy -p snitchwatch-bridge --no-default-features --all-targets --locked -- -D warnings
    cargo clippy -p snitchwatch-bridge-cli --no-default-features --locked -- -D warnings
    cargo test -p snitchwatch-bridge --no-default-features --lib --locked ws_server

# --- Bridge release tarball (docs/packaging/bridge-release-artifact.md) ---
# These run packaging/release/*.sh inside a fresh container from the
# digest-pinned Fedora 44 builder image in packaging/release/pins.env — the
# same scripts .github/workflows/release.yml runs. The checkout is mounted
# read-only and output lands in ./dist (release-verify is the exception: it
# never mounts the checkout, only temp copies; see its comment).
# `label=disable` instead of a `:Z` mount so podman never relabels the
# checkout. Crates are cached in the `snitchwatch-release-cargo` podman volume;
# the target dir is always fresh.
#
# Env passed through: SW_RELEASE_TAG=vX.Y.Z reproduces a release build
# (clean tree + repo-root LICENSE required); ALLOW_DIRTY=1 builds an
# uncommitted tree (recorded as "dirty" in the MANIFEST).

# Build dist/snitchwatch-bridge-<version>-x86_64.tar.gz{,.sha256}.
release-bridge:
    #!/usr/bin/env bash
    set -euo pipefail
    . packaging/release/pins.env
    mkdir -p dist
    podman run --rm --security-opt label=disable \
        -v "$PWD":/src:ro -v "$PWD/dist":/out -v snitchwatch-release-cargo:/cargo-home \
        -e SRC_DIR=/src -e OUT_DIR=/out -e CARGO_HOME=/cargo-home -e SW_INSTALL_DEPS=1 \
        -e SW_RELEASE_TAG="${SW_RELEASE_TAG:-}" -e ALLOW_DIRTY="${ALLOW_DIRTY:-}" \
        "$SW_BUILDER_IMAGE" /src/packaging/release/build-bridge.sh

# Build, then rebuild from a copy at another path and require a
# byte-identical tarball (the release workflow's reproducibility gate).
release-bridge-repro:
    #!/usr/bin/env bash
    set -euo pipefail
    . packaging/release/pins.env
    mkdir -p dist
    podman run --rm --security-opt label=disable \
        -v "$PWD":/src:ro -v "$PWD/dist":/out -v snitchwatch-release-cargo:/cargo-home \
        -e SRC_DIR=/src -e OUT_DIR=/out -e CARGO_HOME=/cargo-home -e SW_INSTALL_DEPS=1 \
        -e SW_RELEASE_TAG="${SW_RELEASE_TAG:-}" -e ALLOW_DIRTY="${ALLOW_DIRTY:-}" \
        "$SW_BUILDER_IMAGE" bash -c 'set -euo pipefail
            out="$(/src/packaging/release/build-bridge.sh | tee /dev/stderr)"
            /src/packaging/release/repro-check.sh "$(printf "%s\n" "$out" | sed -n "s/^TARBALL=//p")"'

# What runs in the container: MANIFEST/SHA256SUMS/layout checks, ldd,
# --help/--version, then an install to / + `systemd-analyze verify --user` +
# `systemctl --global enable`.
#
# The sha256 argument is the TRUST ANCHOR: take it from your own build output
# (`SHA256=` at the end of `just release-bridge`) or from a download whose
# `gh attestation verify` (see the release notes) you have already run. The
# tarball's own `.sha256`, MANIFEST.json and SHA256SUMS prove integrity only —
# an attacker who can replace the tarball can replace those too.
#
# Fails closed, on the HOST, before any container runs: the tarball must hash
# to <sha256>. The tarball and its `.sha256` are then copied into a private
# temp dir under dist/ (which is what the container sees, read-only, so nothing
# can swap the file after it was hashed and the container can't see other
# files in dist/), together with a copy of packaging/release/ — the checkout
# itself is never mounted. The container re-checks the hash against the same
# copy. A relative <tarball> is resolved against the directory `just` was
# invoked from; the arguments reach the script as positional parameters, never
# interpolated into shell text.

# Verify a tarball against a known sha256 in a throwaway Fedora 44 container.
[positional-arguments]
release-verify tarball sha256:
    #!/usr/bin/env bash
    set -euo pipefail
    . packaging/release/pins.env
    tarball="$1" sha256="$2"
    invoked_from={{ quote(invocation_directory()) }}
    if ! [[ "$sha256" =~ ^[0-9a-f]{64}$ ]]; then
        echo "release-verify: FAIL: sha256 must be 64 lowercase hex characters, got '$sha256'" >&2
        exit 1
    fi
    src="$(cd "$invoked_from" && realpath -e -- "$tarball")" || {
        echo "release-verify: FAIL: no such tarball: $tarball" >&2
        exit 1
    }
    name="$(basename -- "$src")"
    if ! [[ "$name" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]*$ ]]; then
        echo "release-verify: FAIL: unexpected characters in tarball name '$name'" >&2
        exit 1
    fi
    if [[ ! -f "$src.sha256" ]]; then
        echo "release-verify: FAIL: $src.sha256 must sit next to the tarball" >&2
        exit 1
    fi
    mkdir -p dist
    work="$(mktemp -d "$PWD/dist/.verify.XXXXXX")"
    trap 'rm -r -- "$work"' EXIT
    mkdir "$work/art"
    cp -- "$src" "$work/art/$name"
    cp -- "$src.sha256" "$work/art/$name.sha256"
    (cd "$work/art" && printf '%s  %s\n' "$sha256" "$name" | sha256sum -c --strict -) || {
        echo "release-verify: FAIL: $name does not match the given sha256 $sha256; not running anything" >&2
        exit 1
    }
    cp -r -- packaging/release "$work/release"
    podman run --rm --security-opt label=disable \
        -v "$work/release":/release:ro -v "$work/art":/art:ro \
        -e SW_THROWAWAY_CONTAINER=1 -e PYTHONDONTWRITEBYTECODE=1 \
        "$SW_BUILDER_IMAGE" bash -c 'set -euo pipefail
            bash /release/install-deps.sh
            python3 /release/bridge_artifact.py verify --tarball "/art/$1" --expect-sha256 "$2" --check-ldd --run-flags
            bash /release/check-install.sh "/art/$1" "$2"' _ "$name" "$sha256"
