#!/usr/bin/env bash
set -euo pipefail

fixture_dir="${1:-fixtures/market-data/sponsors}"

cargo fmt --all -- --check
cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p backend -- sponsor-readiness-gate "$fixture_dir"
git diff --check
