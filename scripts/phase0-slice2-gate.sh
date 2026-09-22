#!/usr/bin/env bash
set -euo pipefail

fixture_dir="${1:-fixtures/phase0/sponsors}"

cargo fmt --all -- --check
cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p backend -- phase0-slice2-gate "$fixture_dir"
git diff --check
