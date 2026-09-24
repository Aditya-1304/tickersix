# Phase 7 beta evidence

`beta-evidence.contract.json` is a checked-in contract fixture. It proves that
Phase 7 evidence records use the Devnet-only, zero-cost schema and preserve the
Public Ranked / Private Markets boundary. It intentionally does not claim that
real tester sessions, proof transactions, screenshots, or the 100-player
simulation have happened.

For a real beta run, create an operator-owned evidence record with
`"mode": "evidence"` and keep the referenced files under an untracked
`artifacts/phase7/` directory. Run:

```bash
cargo run -p backend -- phase7-slice1-gate path/to/beta-evidence.json
```

The command exits successfully for the checked-in contract fixture. An
`evidence` record exits successfully only when it contains two recorded Battle
windows, 8-20 distinct testers, a Jupiter Devnet proof transaction, the
conditional Pyth proof when enabled, Private Market screenshots, and a
completed 100-player League report.
