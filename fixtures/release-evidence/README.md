# Release beta evidence

`beta-evidence.contract.json` is a checked-in contract fixture. It proves that
Release evidence records use the Devnet-only, zero-cost schema and preserve the
Public Ranked / Private Markets boundary. It intentionally does not claim that
real tester sessions, proof transactions, screenshots, or the 100-player
simulation have happened.

For a real beta run, create an operator-owned evidence record with
`"mode": "evidence"` and keep the referenced files under an untracked
`artifacts/release-evidence/` directory. Run:

```bash
cargo run -p backend -- beta-evidence-gate path/to/beta-evidence.json
```

The command exits successfully for the checked-in contract fixture. An
`evidence` record exits successfully only when it contains two recorded Battle
windows, 8-20 distinct testers, a Jupiter Devnet proof transaction, the
conditional Pyth proof when enabled, Private Market screenshots, and a
completed 100-player League report.

## Feature freeze

`feature-freeze.contract.json` pins the exact V2.1 P0/P1 scope, the 2026-09-24
23:59 IST freeze, the allowed Pyth/PreStocks/Tessera sponsor tracks, and the
no-new-sponsor-scope rule. Its `contract` mode is intentionally not release
ready. Change it to `freeze` only in an operator-owned record after beta evidence
evidence is real and every required status is verified.

Run the offline release freeze contract gate with:

```bash
cargo run -p backend -- release-freeze-gate \
  fixtures/release-evidence/feature-freeze.contract.json \
  fixtures/release-evidence/beta-evidence.contract.json
```
