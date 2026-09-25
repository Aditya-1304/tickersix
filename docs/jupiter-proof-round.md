# Jupiter proof round

The Jupiter proof gate validates an operator-retained, finalized Devnet round. It
is intentionally separate from the live Jupiter smoke check: a provider response
is market-data input, while the bundle below is the settlement evidence that can
be independently replayed.

## Bundle contract

Create an operator-owned JSON file outside Git, for example
artifacts/release-evidence/jupiter-proof.bundle.json, with these fields:

- schema_version: 1;
- cluster: "devnet";
- max_attestor_spread_bps, copied from the frozen Market Round;
- snapshot, the reconciled ProofSnapshot consumed by the public proof API;
- quorum_evidence, exactly one record for each of six assets and both price
  windows, retaining all three candidate attestor reports;
- optional expected_public_proof_sha256, recorded after the gate prints the
  deterministic public-proof hash.

Every retained attestor report must include its base58 Ed25519 public key, the
canonical Q9 median, evidence root, observation counts, and source-block bounds.
The snapshot must include the Market Round, Battle, commit/reveal signatures,
finalized start/end prices, exact returns, settlement signature, and finalized
chain/indexer slot.

Do not put API keys, secret keys, commitment preimages, or wallet seed phrases in
the bundle. The public snapshot intentionally contains lifecycle signatures and
public facts only.

## Validate and replay

Run the read-only gate:

    scripts/jupiter-proof-gate.sh artifacts/release-evidence/jupiter-proof.bundle.json

The gate checks:

- Devnet and Jupiter source binding;
- finalized six-asset Battle shape;
- unique transaction signatures;
- protocol-compatible two- or three-attestor quorum for every price window;
- three-candidate retention and deterministic outlier rejection when present;
- finalized prices and selected attestor identity agreement;
- deterministic public proof serialization and its SHA-256 replay hash.

A successful report is not a transaction sender. Creating the round, collecting
attestor reports, finalizing settlement, and retaining signatures still require
the operator's separately approved Devnet wallet workflow.

After reviewing the printed hash, add it to the operator bundle and rerun the
gate. Then point the beta evidence manifest at this bundle and record the
settlement transaction signature there.
