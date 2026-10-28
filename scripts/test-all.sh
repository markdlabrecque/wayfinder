#!/usr/bin/env bash
# Hermetic tests for both crates: bench/ is standalone, so a root `cargo test`
# never reaches it. Same pair CI runs.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test
cargo test --manifest-path bench/Cargo.toml
