#!/usr/bin/env bash
set -euo pipefail

# This script performs a non-signing beta evidence preflight. It builds the program,
# checks that every repository identity agrees, and optionally verifies the
# deployed Devnet program account through public RPC.
program_id="${TICKERSIX_PROGRAM_ID:-8sehrRxpnLbvpzgJx8MqB5YApZAh69z5yVvdK1Zeyj6Z}"
rpc_url="${TICKERSIX_DEVNET_RPC_URL:-https://api.devnet.solana.com}"

if ! command -v anchor >/dev/null 2>&1; then
  echo "anchor CLI is required" >&2
  exit 1
fi
if ! command -v solana >/dev/null 2>&1; then
  echo "solana CLI is required" >&2
  exit 1
fi
if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required" >&2
  exit 1
fi
if ! command -v node >/dev/null 2>&1; then
  echo "node is required for RPC response validation" >&2
  exit 1
fi

expected_declare_id="$(sed -n 's/.*declare_id!("\([^"]*\)").*/\1/p' programs/tickersix/src/lib.rs | head -n 1)"
devnet_anchor_id="$(awk '
  /^\[programs\.devnet\]$/ { in_devnet = 1; next }
  /^\[/ { in_devnet = 0 }
  in_devnet && $1 == "tickersix" { gsub(/"/, "", $3); print $3; exit }
' Anchor.toml)"

if [[ -z "$expected_declare_id" || "$expected_declare_id" != "$program_id" ]]; then
  echo "program ID mismatch: programs/tickersix/src/lib.rs" >&2
  exit 1
fi
if [[ "$devnet_anchor_id" != "$program_id" ]]; then
  echo "program ID mismatch: Anchor.toml [programs.devnet]" >&2
  exit 1
fi

NO_DNA=1 anchor build
cluster_version="$(NO_DNA=1 solana cluster-version --url "$rpc_url")"
if [[ -z "$cluster_version" ]]; then
  echo "Devnet cluster-version response was empty" >&2
  exit 1
fi

printf 'Devnet build preflight passed\n'
printf 'program_id=%s\n' "$program_id"
printf 'rpc_url=%s\n' "$rpc_url"
printf 'cluster_version=%s\n' "$cluster_version"

if [[ "${TICKERSIX_VERIFY_ONCHAIN:-0}" != "1" ]]; then
  printf '%s\n' 'on-chain verification skipped; set TICKERSIX_VERIFY_ONCHAIN=1 after deployment'
  exit 0
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf -- "$tmp_dir"' EXIT
response_path="$tmp_dir/account.json"

curl --fail --silent --show-error "$rpc_url" \
  -H 'content-type: application/json' \
  --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getAccountInfo\",\"params\":[\"$program_id\",{\"encoding\":\"base64\",\"commitment\":\"finalized\"}]}" \
  -o "$response_path"

node - "$response_path" "$program_id" <<'NODE'
const fs = require("node:fs");

const responsePath = process.argv[2];
const expectedProgramId = process.argv[3];
const response = JSON.parse(fs.readFileSync(responsePath, "utf8"));
if (response.error) throw new Error(`Devnet RPC error: ${response.error.message}`);
const value = response.result?.value;
if (!value) throw new Error(`program ${expectedProgramId} is not deployed on Devnet`);
if (value.executable !== true) throw new Error(`program ${expectedProgramId} is not executable`);
if (value.owner !== "BPFLoaderUpgradeab1e11111111111111111111111") {
  throw new Error(`program ${expectedProgramId} is not owned by the upgradeable loader`);
}
console.log(`Devnet program account verified: ${expectedProgramId}`);
NODE
