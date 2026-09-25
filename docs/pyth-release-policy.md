# Pyth release policy

The zero-cost Stocklana release keeps Pyth disabled and keeps Public Ranked on
Jupiter. Pyth remains an optional, trial-dependent sponsor path; it is not
represented as permanently free and it cannot become settlement authority from
payload parsing alone.

The decision is recorded in
fixtures/market-data/sponsors/pyth-release-decision.json and is checked by the
sponsor-readiness gate. The checked-in evidence records:

- no authorized trial token;
- no verified Devnet verifier transaction;
- no compute-unit, byte, lamport, or latency evidence;
- KeepJupiter as the activation decision;
- Jupiter as the Public Ranked source.

Run the read-only gate with:

    cargo run -p backend -- sponsor-readiness-gate fixtures/market-data/sponsors

Promotion to Pyth would require an explicit authorized trial, the pinned Devnet
verifier, fresh target-bound payloads, confidence and Q9 checks, a verified
transaction, cost evidence, and a separate source-specific proof bundle. None of
those claims are made by the current release record.
