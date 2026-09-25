# League evidence

The retained League report binds the deterministic Swiss/standings run to the release record without fabricating a completed Devnet session. The existing protocol and backend tests cover the pairing and standings algorithms; the retained artifact proves that the reviewed run used the expected inputs and exclusions.

Run the read-only validator with:

```bash
cargo run -p backend -- league-evidence-gate artifacts/release-evidence/league-100.json
```

The report must declare:

- 100 unique players across exactly 5 rounds;
- 250 finalized pairings, zero byes, zero rematches, and complete per-player exposure;
- a 100-player standings result plus deterministic replay and standings-input hashes;
- `FINALIZED_BATTLE_FACTS` as the achievement input, with no projected or replay-derived unlocks;
- zero economic reward claims for the free Devnet demonstration.

The beta evidence gate reads and validates this report whenever the manifest uses `mode: "evidence"`. The checked-in contract manifest intentionally references the expected path without claiming that the external report exists.
