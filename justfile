default:
    @just --list

# Build all crates (excluding GPUI app which needs platform libs)
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

# SSH config (overridable via env)
export RIFT_SSH_HOST := env("RIFT_SSH_HOST", "localhost")
export RIFT_SSH_USER := env("RIFT_SSH_USER", "developer")
export RIFT_SSH_PORT := env("RIFT_SSH_PORT", "22")
export RIFT_SSH_KEY := env("RIFT_SSH_KEY", home_directory() / ".ssh" / "id_rsa")
windows_staging_dir := env("RIFT_WINDOWS_DIR", "/mnt/c/temp/rift")

dev:
    WAYLAND_DISPLAY="" \
    RUST_LOG=rift=debug,rift_ssh=debug \
    RIFT_SSH_HOST="{{RIFT_SSH_HOST}}" \
    RIFT_SSH_USER="{{RIFT_SSH_USER}}" \
    RIFT_SSH_PORT="{{RIFT_SSH_PORT}}" \
    RIFT_SSH_KEY="{{RIFT_SSH_KEY}}" \
    cargo run -p rift-app

# Build and run native Windows .exe (cross-compiled via cargo-xwin)
dev-windows:
    cargo xwin build -p rift-app --target x86_64-pc-windows-msvc
    mkdir -p {{windows_staging_dir}}
    cp target/x86_64-pc-windows-msvc/debug/rift.exe {{windows_staging_dir}}/rift.exe
    cp "{{RIFT_SSH_KEY}}" {{windows_staging_dir}}/ssh_key
    export WSLENV="RUST_LOG:RIFT_SSH_HOST:RIFT_SSH_USER:RIFT_SSH_PORT:RIFT_SSH_KEY/p" && \
    export RUST_LOG=rift=debug,rift_ssh=debug && \
    export RIFT_SSH_HOST="{{RIFT_SSH_HOST}}" && \
    export RIFT_SSH_USER="{{RIFT_SSH_USER}}" && \
    export RIFT_SSH_PORT="{{RIFT_SSH_PORT}}" && \
    export RIFT_SSH_KEY="{{windows_staging_dir}}/ssh_key" && \
    {{windows_staging_dir}}/rift.exe; \
    rm -f {{windows_staging_dir}}/ssh_key

# Build Windows .exe without running
build-windows:
    cargo xwin build -p rift-app --target x86_64-pc-windows-msvc

# Check licenses (requires cargo-deny)
deny:
    cargo deny check licenses
