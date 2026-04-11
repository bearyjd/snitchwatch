default:
    @just --list

build:
    cargo build --workspace

test:
    cargo test --workspace

check:
    cargo check --workspace
    cargo clippy --workspace -- -D warnings

fmt:
    cargo fmt --all
