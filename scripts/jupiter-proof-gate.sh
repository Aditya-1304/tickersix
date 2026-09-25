#!/usr/bin/env bash
set -euo pipefail

bundle_path=${1:?usage: scripts/jupiter-proof-gate.sh <bundle.json>}
cargo run -p backend -- jupiter-proof-gate "$bundle_path"
