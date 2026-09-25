# Submission readiness

This document separates checks the repository can prove from evidence that
must be collected by the operator. A passing local gate does not fabricate
real users, screenshots, videos, or finalized Battles.

## Automated gate

Run from the repository root:

```bash
scripts/submission-readiness-gate.sh
```

The gate verifies:

- Rust formatting, workspace tests, and the established Clippy profile;
- frontend syntax and contract checks;
- release-contract validation for the Public Ranked and Private Markets
  boundary;
- exact local IDL hash against the recorded Devnet artifact;
- deployed program ID, upgradeable-loader ownership, and metadata fields;
- finalized public Devnet account state;
- clean patch formatting;
- repository credential audit over source, app, scripts, docs, and fixtures.

The gate is read-only. It does not sign, deploy, upgrade the program, create a
wallet, or request Devnet funds.

## Operator evidence still required

Before public submission, collect and review:

- a reachable hosted app URL and a public repository URL;
- the recorded Jupiter proof Battle and its finalized transaction signatures;
- the validated Jupiter proof bundle and replay hash described in [jupiter-proof-round.md](jupiter-proof-round.md);
- at least two real Battle windows and 8–20 distinct testers;
- the Private Markets screenshot path with the separate rating-domain label;
- the 100-player, five-round League simulation report;
- the pitch video and optional technical walkthrough;
- logged-out checks for every submitted link;
- backup copies of the evidence and videos.

Do not change a contract fixture from `contract` to `evidence` unless the
referenced artifacts actually exist and have been reviewed. Do not enable Pyth
in a release record without an authorized trial, verified Devnet transaction,
and its source-specific proof record.

## Deployment records

The current verified deployment records are under
`artifacts/devnet-deployment/`:

- `deployment-record.txt` binds the cluster, program, fee payer, and IDL
  metadata status;
- `program-show.json` records executable status, loader ownership, program-data
  address, and upgrade authority;
- `verification.log` records the public Devnet account check;
- `idl-sha256.txt` binds the checked-in IDL bytes to the verified artifact.

The deployment authority is intentionally recorded as a public address only;
the wallet file remains outside the repository.

## Credential audit

Run the standalone read-only audit with:

```bash
scripts/security-audit.sh
```

It rejects private-key PEM material and non-placeholder values assigned to API-key, private-key, secret-key, seed-phrase, or mnemonic fields in the release-relevant repository roots. Configuration names, environment lookups, typed fields, and documented `...` placeholders are allowed.
