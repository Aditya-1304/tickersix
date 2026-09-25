# Devnet round setup

This operator tool creates the on-chain accounts required for one Public
Ranked Jupiter round. It never creates wallets, requests SOL, or stores secret
keys. Use only public Devnet RPC and a fee-payer keypair that you control.

The asset catalog is intentionally supplied by the operator. Every scoring mint
must be a real Solana mint supported by the selected Jupiter settlement path;
placeholder strings, duplicate mints, and fewer than ten assets are rejected.

## Prepare the input

```bash
cp ops/devnet/config.example.json /tmp/tickersix-devnet-round.json
$EDITOR /tmp/tickersix-devnet-round.json
```

Fill in three distinct attestor public keys and at least ten real public-equity
scoring mints. Keep the corresponding attestor secret keys outside Git; the
bootstrap tool only stores their public identities on chain.

Install the isolated operator dependencies once:

```bash
npm --prefix ops/devnet install
```

## Review without signing

From the repository root:

```bash
TICKERSIX_DEPLOY_WALLET="$HOME/.config/solana/id.json" \
  npm --prefix ops/devnet run bootstrap -- \
  --config /tmp/tickersix-devnet-round.json
```

This prints the selected PDAs and transaction plan without submitting anything.
It also requires the IDL at target/idl/tickersix.json.

## Submit after review

Only after checking the printed program ID, payer, assets, timestamps, and PDAs:

```bash
TICKERSIX_BOOTSTRAP_APPROVED=YES \
TICKERSIX_DEPLOY_WALLET="$HOME/.config/solana/id.json" \
  npm --prefix ops/devnet run bootstrap -- \
  --config /tmp/tickersix-devnet-round.json \
  --send
```

--send is refused unless the approval variable is exactly YES. The tool
skips already-created accounts only when they belong to the configured program
and writes a public manifest under artifacts/devnet-round/. It does not
silently overwrite an existing protocol configuration.

After submission, import the manifest into the local projection only after
confirming the transaction output and finalized account state:

```bash
export TICKERSIX_DATABASE_URL="postgresql://stocklana:stocklana_devnet_local@127.0.0.1:55432/stocklana_devnet"
export TICKERSIX_DEVNET_RPC_URL="https://api.devnet.solana.com"
NO_DNA=1 cargo run -p backend -- index-round-manifest \
  artifacts/devnet-round/round-1-manifest.json
```

The importer rechecks canonical identities and finalized program ownership
before writing the MarketRound and RoundAsset projections. A successful import
is the point at which the API can expose the round to the ranked queue.
