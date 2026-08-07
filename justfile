# Linux-Patch-Api task runner — single source of truth for local + CI.
# Local:   just check           (dev loop; warm cache on the dev box)
# Release: just release patch    (bump -> commit -> tag -> push; CI builds official packages)
# Verify:  just verify-matrix    (locally build all 9 distros before tagging)

default:
    @just --list

# one-time: install the cargo tools the gates need
tools:
    cargo install cargo-audit --locked

# --- quality gates (the dev loop; `just check` runs all) ---
fmt:
    cargo fmt --all -- --check

clippy:
    cargo clippy --all-targets --all-features -- -D warnings

test:
    cargo test --all-features

enrollment-test:
    cargo test --test enroll_identity --test enrollment_test --test enrollment_e2e

audit:
    cargo audit --ignore RUSTSEC-2025-0134 --ignore RUSTSEC-2026-0190 --ignore RUSTSEC-2026-0204 --ignore RUSTSEC-2026-0205

check: fmt clippy test audit
    @echo "all gates passed"

# --- build / package (per-distro) ---
build:
    cargo build --release

build-musl:
    rustup target add x86_64-unknown-linux-musl
    cargo build --release --target x86_64-unknown-linux-musl

# system deps (lifted from ci.yml; run on the matching distro)
deps-deb:
    sudo apt-get update && sudo apt-get install -y build-essential libsystemd-dev pkg-config libssl-dev
deps-rpm:
    sudo dnf install -y systemd-devel openssl-devel pkg-config gcc make
deps-arch:
    sudo pacman -Syu --noconfirm systemd openssl pkg-config gcc
deps-alpine:
    apk add --no-cache bash git curl tar gcc musl-dev openssl-dev openssl elogind-dev alpine-sdk abuild

pkg-deb:
    chmod +x scripts/build-package.sh && ./scripts/build-package.sh
pkg-rpm: build
    chmod +x build-rpm.sh && SKIP_CARGO_BUILD=1 sudo -E ./build-rpm.sh
pkg-arch: build
    chmod +x build-arch.sh && SKIP_CARGO_BUILD=1 ./build-arch.sh
pkg-alpine: build-musl
    chmod +x build-alpine.sh && SKIP_CARGO_BUILD=1 ./build-alpine.sh

# --- release (CI remains the official builder) ---
release KIND:
    ./scripts/release.sh "{{KIND}}"

# --- pre-tag 9-distro verification (local SSH driver) ---
verify-matrix:
    ./scripts/verify-matrix.sh
