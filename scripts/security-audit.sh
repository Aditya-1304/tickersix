#!/usr/bin/env bash
set -euo pipefail

# Runs the repository credential audit without changing files or contacting
# Solana. A non-empty finding list is a release-blocking result.
repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

cargo run -p backend -- security-audit "$repo_root"
