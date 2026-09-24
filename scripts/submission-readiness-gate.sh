#!/usr/bin/env bash
set -euo pipefail

# Runs the final Stocklana readiness checks from one repository root. This
# command is read-only: it validates local artifacts, executes test suites, and
# performs a public Devnet account lookup without signing or submitting a
# transaction.
repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

program_id="${TICKERSIX_PROGRAM_ID:-8sehrRxpnLbvpzgJx8MqB5YApZAh69z5yVvdK1Zeyj6Z}"
rpc_url="${TICKERSIX_DEVNET_RPC_URL:-https://api.devnet.solana.com}"
artifact_dir="${TICKERSIX_DEPLOY_ARTIFACT_DIR:-artifacts/devnet-deployment}"
idl_path="target/idl/tickersix.json"
idl_hash_record="$artifact_dir/idl-sha256.txt"
program_record="$artifact_dir/program-show.json"

for required_path in \
  STOCKLANA_V2.1.md \
  Anchor.toml \
  "$idl_path" \
  "$idl_hash_record" \
  "$program_record" \
  "$artifact_dir/deployment-record.txt" \
  "$artifact_dir/verification.log"; do
  if [[ ! -r "$required_path" ]]; then
    echo "required release artifact is missing or unreadable: $required_path" >&2
    exit 1
  fi
done

recorded_idl_hash="$(awk -v path="$idl_path" '$2 == path { print $1; exit }' "$idl_hash_record")"
actual_idl_hash="$(sha256sum "$idl_path" | awk '{ print $1 }')"
if [[ -z "$recorded_idl_hash" || "$recorded_idl_hash" != "$actual_idl_hash" ]]; then
  echo "IDL hash does not match the recorded Devnet artifact" >&2
  printf 'recorded=%s actual=%s\n' "$recorded_idl_hash" "$actual_idl_hash" >&2
  exit 1
fi

node - "$program_record" "$program_id" <<'NODE'
const fs = require("node:fs");

const recordPath = process.argv[2];
const expectedProgramId = process.argv[3];
const record = JSON.parse(fs.readFileSync(recordPath, "utf8"));
if (record.programId !== expectedProgramId) {
  throw new Error(`deployment record program ID mismatch: ${record.programId}`);
}
if (record.owner !== "BPFLoaderUpgradeab1e11111111111111111111111") {
  throw new Error(`unexpected program owner: ${record.owner}`);
}
if (!record.programdataAddress || !record.authority) {
  throw new Error("deployment record is missing upgradeable-program metadata");
}
console.log(`Local deployment artifact verified: ${expectedProgramId}`);
NODE

grep -Eq '^cluster=devnet$' "$artifact_dir/deployment-record.txt"
grep -Eq "^program_id=$program_id$" "$artifact_dir/deployment-record.txt"
grep -Eq '^buffer_status=closed$' "$artifact_dir/deployment-record.txt"
grep -Eq '^idl_status=exact_sha256_match$' "$artifact_dir/deployment-record.txt"

scripts/release-freeze-gate.sh

TICKERSIX_VERIFY_ONCHAIN=1 \
TICKERSIX_DEVNET_RPC_URL="$rpc_url" \
TICKERSIX_PROGRAM_ID="$program_id" \
scripts/devnet-deployment-preflight.sh

git diff --check

printf '%s\n' 'Submission readiness checks passed.'
printf 'program_id=%s\n' "$program_id"
printf 'rpc_url=%s\n' "$rpc_url"
printf 'idl_sha256=%s\n' "$actual_idl_hash"
printf '%s\n' 'External tester, screenshot, video, and submission-link evidence remain operator-supplied.'
