#!/usr/bin/env bash
set -euo pipefail

# Deployment is intentionally guarded because it signs and submits a Devnet
# program deployment transaction with the wallet supplied by the operator.
if [[ "${TICKERSIX_DEPLOY_APPROVED:-}" != "YES" ]]; then
  echo 'Refusing to deploy. Re-run with TICKERSIX_DEPLOY_APPROVED=YES after reviewing the transaction scope.' >&2
  exit 2
fi

wallet="${TICKERSIX_DEPLOY_WALLET:-$HOME/.config/solana/id.json}"
rpc_url="${TICKERSIX_DEVNET_RPC_URL:-https://api.devnet.solana.com}"
program_id="${TICKERSIX_PROGRAM_ID:-8sehrRxpnLbvpzgJx8MqB5YApZAh69z5yVvdK1Zeyj6Z}"
artifact_dir="${TICKERSIX_DEPLOY_ARTIFACT_DIR:-artifacts/devnet-deployment}"

if [[ ! -r "$wallet" ]]; then
  echo "deployment wallet is missing or unreadable: $wallet" >&2
  echo 'Create or select the wallet yourself; this script never generates keypairs.' >&2
  exit 1
fi

scripts/devnet-deployment-preflight.sh
wallet_address="$(NO_DNA=1 solana address --keypair "$wallet")"
balance="$(NO_DNA=1 solana balance "$wallet_address" --url "$rpc_url")"
printf 'fee_payer=%s\n' "$wallet_address"
printf 'devnet_balance=%s\n' "$balance"
printf '%s\n' 'Review the fee payer, cluster, program ID, and balance above before continuing.'

mkdir -p "$artifact_dir"
printf 'cluster=devnet\nprogram_id=%s\nfee_payer=%s\nrpc_url=%s\n' \
  "$program_id" "$wallet_address" "$rpc_url" > "$artifact_dir/deployment-intent.txt"

NO_DNA=1 anchor deploy \
  --program-name tickersix \
  --provider.cluster devnet \
  --provider.wallet "$wallet" \
  --commitment finalized \
  --no-idl \
  2>&1 | tee "$artifact_dir/deploy.log"

TICKERSIX_VERIFY_ONCHAIN=1 \
TICKERSIX_DEVNET_RPC_URL="$rpc_url" \
TICKERSIX_PROGRAM_ID="$program_id" \
scripts/devnet-deployment-preflight.sh | tee "$artifact_dir/verification.log"

NO_DNA=1 solana program show "$program_id" \
  --url "$rpc_url" \
  --keypair "$wallet" \
  --output json > "$artifact_dir/program-show.json"

printf 'cluster=devnet\nprogram_id=%s\nfee_payer=%s\nrpc_url=%s\n' \
  "$program_id" "$wallet_address" "$rpc_url" > "$artifact_dir/deployment-record.txt"
printf 'Deployment verification passed. Evidence: %s\n' "$artifact_dir"
