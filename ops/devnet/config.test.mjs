import assert from "node:assert/strict";
import test from "node:test";
import { buildRoundTimes, validateBootstrapConfig } from "./config.mjs";

function asset(assetId, scoringMint = "Mint" + assetId) {
  return {
    assetId,
    symbol: "A" + assetId,
    name: "Asset " + assetId,
    representation: "Asset" + assetId,
    provider: "xStocks",
    scoringMint,
  };
}

function validConfig() {
  return {
    roundId: 1,
    registryVersion: 1,
    attestors: ["AttestorA", "AttestorB", "AttestorC"],
    assets: Array.from({ length: 10 }, (_, index) => asset(index)),
  };
}

test("rejects a public round with fewer than ten frozen assets", () => {
  const config = validConfig();
  config.assets = config.assets.slice(0, 9);
  assert.throws(() => validateBootstrapConfig(config), /between 10 and 256/);
});

test("rejects duplicate scoring mints before account creation", () => {
  const config = validConfig();
  config.assets[1].scoringMint = config.assets[0].scoringMint;
  assert.throws(() => validateBootstrapConfig(config), /duplicate scoringMint/);
});

test("rejects duplicate attestors because the on-chain quorum requires independent keys", () => {
  const config = validConfig();
  config.attestors[2] = config.attestors[0];
  assert.throws(() => validateBootstrapConfig(config), /attestors must be non-empty and distinct/);
});

test("builds a strictly ordered window from one captured clock", () => {
  const config = validateBootstrapConfig(validConfig());
  const times = buildRoundTimes(1700000000, config.timing);
  assert.ok(times.eligibilityFrozenAt < times.queueCloseAt);
  assert.ok(times.queueCloseAt < times.commitDeadline);
  assert.ok(times.commitDeadline < times.revealDeadline);
  assert.ok(times.revealDeadline < times.startTargetAt);
  assert.ok(times.startTargetAt < times.endTargetAt);
});
