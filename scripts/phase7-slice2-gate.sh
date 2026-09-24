#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all -- --check
cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -A clippy::too_many_arguments
cargo run -p backend -- phase7-slice2-gate \
  fixtures/phase7/feature-freeze.contract.json \
  fixtures/phase7/beta-evidence.contract.json
npm --prefix app run check
node --check app/app.js
git diff --check
