# Final submission gate

Run this command only after reviewing and committing the release tree:

```bash
scripts/final-submission-gate.sh
```

The gate requires a clean Git worktree and delegates to the existing read-only submission readiness checks. Those checks cover:

- workspace formatting, tests, the established Clippy profile, and frontend checks;
- the Devnet deployment record, exact IDL hash, upgradeable-loader metadata, and finalized public program account;
- the release-freeze contract and beta evidence contract;
- the repository credential audit.

The gate never signs or submits a Solana transaction. A passing local result does not invent the remaining operator evidence: hosted app/repository links, real tester sessions, finalized Jupiter proof records, screenshots, videos, and the retained 100-player League report are still required before public submission.
