default:
    @just --list

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

# Run the Tauri shell in dev mode (live bridge + native window)
tauri-dev:
    cargo run -p snitchwatch-tauri

# Build a release Tauri bundle (deb/rpm/appimage as configured in tauri.conf.json)
tauri-build:
    cargo build -p snitchwatch-tauri --release

# Playwright smoke test for the Tauri shell (requires `npm install` in tests/tauri_smoke first)
tauri-smoke:
    cd tests/tauri_smoke && npx playwright test

# One-time install of the Playwright deps
tauri-smoke-install:
    cd tests/tauri_smoke && npm install && npx playwright install firefox

# Serve test fixtures over HTTP for manual blocklist subscription smoke testing.
blocklist-fixture-server:
    cd tests/fixtures/blocklists && python3 -m http.server 8731

# Run only the blocklist test suite.
test-blocklists:
    cargo test -p snitchwatch-bridge blocklists -- --nocapture
    cargo test -p snitchwatch-bridge --test blocklists_e2e -- --nocapture

# Validate the Phase 2 packaging artifacts' syntax without a Bazzite host.
# Parses the YAML/JSON, checks the systemd unit (systemd-analyze if present),
# and shellchecks any scripts. Runs the Rust packaging shape test too.
package-check:
    python3 -c "import yaml,sys; yaml.safe_load(open('packaging/bluebuild/recipe.yml')); yaml.safe_load(open('packaging/flatpak/org.snitchwatch.Snitchwatch.yml')); print('YAML ok')"
    python3 -c "import json; json.load(open('packaging/bluebuild/files/system/etc/opensnitchd/default-config.json')); print('JSON ok')"
    @command -v systemd-analyze >/dev/null 2>&1 && systemd-analyze verify --user packaging/systemd/snitchwatch-bridge.service && echo "systemd unit ok" || echo "systemd-analyze not available — skipped unit verify"
    cargo test -p snitchwatch-bridge --test packaging_shape
