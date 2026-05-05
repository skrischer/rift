default:
    @just --list

# Build all crates (excluding Tauri app which needs system libs)
build:
    cargo build --workspace --exclude rift-app

# Lint with zero warnings policy
lint:
    cargo clippy --workspace --exclude rift-app -- -D warnings

# Format all code
fmt:
    cargo fmt --all

# Check formatting without modifying
fmt-check:
    cargo fmt --all -- --check

# Run all tests
test:
    cargo test --workspace --exclude rift-app

# Full CI check (format + lint + test)
ci: fmt-check lint test

# Build daemon release binary for Linux (musl)
release-daemon:
    cargo build --release -p rift-daemon --target x86_64-unknown-linux-musl

# Run daemon locally
run-daemon *ARGS:
    cargo run -p rift-daemon -- {{ARGS}}

# Check licenses (requires cargo-deny)
deny:
    cargo deny check licenses
