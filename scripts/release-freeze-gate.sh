#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all -- --check
cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -A clippy::too_many_arguments
scripts/security-audit.sh
cargo run -p backend -- release-freeze-gate \
  fixtures/release-evidence/feature-freeze.contract.json \
  fixtures/release-evidence/beta-evidence.contract.json
npm --prefix app run check
node --check app/app.js
git diff --check
