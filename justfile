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
