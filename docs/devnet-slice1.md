# Devnet deployment foundation

Slice 1 keeps localnet as the default for deterministic tests and adds an
explicit Devnet program mapping. The deployment path is guarded because it
uses the operator's wallet to sign and submit a Devnet program deployment.

## Non-signing preflight

```bash
scripts/devnet-slice1-preflight.sh
```

This builds the program, checks the program ID in `Anchor.toml` and
`programs/tickersix/src/lib.rs`, and checks the public Devnet cluster. Before
deployment, on-chain verification is expected to report that the program is
not deployed yet.

## Operator-controlled deployment

First confirm that the wallet is a Devnet fee payer and has enough faucet SOL.
The following commands do not expose the private key:

```bash
export TICKERSIX_DEPLOY_WALLET="$HOME/.config/solana/id.json"
NO_DNA=1 solana address --keypair "$TICKERSIX_DEPLOY_WALLET"
NO_DNA=1 solana balance --url devnet --keypair "$TICKERSIX_DEPLOY_WALLET"
```

If the balance is insufficient, request Devnet SOL from the public faucet using
the wallet address. Do not use Mainnet SOL and do not commit the keypair.

After reviewing the fee payer, cluster, and program ID, explicitly authorize
the guarded script:

```bash
TICKERSIX_DEPLOY_APPROVED=YES \\
TICKERSIX_DEPLOY_WALLET="$HOME/.config/solana/id.json" \\
scripts/devnet-slice1-deploy.sh
```

The script stores deployment logs and public verification output under
`artifacts/phase8/slice1/`. Review `program-show.json` for the upgrade
authority and deployed program status.
