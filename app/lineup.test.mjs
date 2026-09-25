import assert from "node:assert/strict";
import test from "node:test";
import { validateLineup } from "./lineup.mjs";

const assets = Array.from({ length: 6 }, (_, index) => ({ id: index + 1 }));

test("rejects a lineup with fewer than six assets", () => {
  assert.deepEqual(
    validateLineup([1, 2, 3, 4, 5], 1, assets),
    { valid: false, reason: "LINEUP_REQUIRES_SIX_ASSETS" },
  );
});

test("rejects duplicate assets even when the selected collection has six entries", () => {
  assert.deepEqual(
    validateLineup([1, 2, 3, 4, 5, 5], 1, assets),
    { valid: false, reason: "LINEUP_ASSETS_MUST_BE_UNIQUE" },
  );
});

test("rejects a captain that is not in the selected lineup", () => {
  assert.deepEqual(
    validateLineup([1, 2, 3, 4, 5, 6], 7, assets),
    { valid: false, reason: "CAPTAIN_MUST_BE_SELECTED" },
  );
});

test("accepts six unique assets from the frozen asset catalog", () => {
  assert.deepEqual(
    validateLineup([6, 1, 4, 2, 5, 3], 4, assets),
    { valid: true, assetIds: [6, 1, 4, 2, 5, 3], captainAssetId: 4 },
  );
});
