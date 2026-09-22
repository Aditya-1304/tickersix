#!/usr/bin/env bash

# Reproducible Phase 0 Slice 0.1 gate. The live provider smoke test is kept
# separate because committed fixtures prove parser/policy behavior, while a
# live response is timestamped external evidence with different reproducibility
# and network requirements.
set -euo pipefail

cargo fmt --all -- --check
cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p backend -- phase0-slice1-gate fixtures/phase0 SPYx
git diff --check
