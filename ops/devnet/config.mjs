export const MIN_PUBLIC_ASSETS = 10;
export const MAX_PUBLIC_ASSETS = 256;
export const REQUIRED_ATTESTORS = 3;

const DEFAULT_TIMING = Object.freeze({
  queueCloseAfterSecs: 600,
  commitWindowSecs: 600,
  revealWindowSecs: 600,
  startDelaySecs: 60,
  sampleIntervalSecs: 5,
  observationWindowSecs: 120,
});

function invalid(message) {
  throw new Error(message);
}

function positiveInteger(value, field) {
  if (!Number.isSafeInteger(value) || value <= 0) {
    invalid(field + " must be a positive safe integer");
  }
  return value;
}

/**
 * Validates operator-supplied Devnet setup data before any wallet transaction
 * is constructed. The validator rejects incomplete public universes and
 * duplicate identities because either condition would make the frozen round
 * impossible to settle consistently.
 */
export function validateBootstrapConfig(input) {
  if (!input || typeof input !== "object") invalid("configuration must be an object");

  const roundId = positiveInteger(Number(input.roundId), "roundId");
  const registryVersion = positiveInteger(Number(input.registryVersion ?? 1), "registryVersion");
  const attestors = input.attestors;
  if (!Array.isArray(attestors) || attestors.length !== REQUIRED_ATTESTORS) {
    invalid("attestors must contain exactly " + REQUIRED_ATTESTORS + " public keys");
  }
  const distinctAttestors = new Set(attestors.map((value) => String(value).trim()));
  if (distinctAttestors.size !== REQUIRED_ATTESTORS || [...distinctAttestors].some((value) => !value)) {
    invalid("attestors must be non-empty and distinct");
  }

  const assets = input.assets;
  if (!Array.isArray(assets) || assets.length < MIN_PUBLIC_ASSETS || assets.length > MAX_PUBLIC_ASSETS) {
    invalid("assets must contain between " + MIN_PUBLIC_ASSETS + " and " + MAX_PUBLIC_ASSETS + " entries");
  }
  const assetIds = new Set();
  const mints = new Set();
  const normalizedAssets = assets.map((asset, index) => {
    if (!asset || typeof asset !== "object") invalid("assets[" + index + "] must be an object");
    const assetId = Number(asset.assetId);
    if (!Number.isSafeInteger(assetId) || assetId < 0 || assetId >= MAX_PUBLIC_ASSETS) {
      invalid("assets[" + index + "].assetId must be an integer from 0 through " + (MAX_PUBLIC_ASSETS - 1));
    }
    if (!assetIds.add(assetId)) invalid("duplicate assetId " + assetId);
    const scoringMint = String(asset.scoringMint ?? "").trim();
    if (!scoringMint) invalid("assets[" + index + "].scoringMint is required");
    if (mints.has(scoringMint)) invalid("duplicate scoringMint " + scoringMint);
    mints.add(scoringMint);
    const symbol = String(asset.symbol ?? "").trim();
    if (!symbol || Buffer.byteLength(symbol, "utf8") > 8) {
      invalid("assets[" + index + "].symbol must be 1-8 UTF-8 bytes");
    }
    const name = String(asset.name ?? symbol).trim();
    const representation = String(asset.representation ?? symbol).trim();
    const provider = String(asset.provider ?? "xStocks").trim();
    if (!name || !representation || !provider) invalid("assets[" + index + "] metadata is incomplete");
    const issuerKind = Number(asset.issuerKind ?? 1);
    if (!Number.isSafeInteger(issuerKind) || issuerKind < 0 || issuerKind > 255) {
      invalid("assets[" + index + "].issuerKind must be a byte");
    }
    const pythFeedId = Number(asset.pythFeedId ?? 0);
    if (!Number.isSafeInteger(pythFeedId) || pythFeedId < 0 || pythFeedId > 0xffffffff) {
      invalid("assets[" + index + "].pythFeedId must be a u32");
    }
    return { assetId, symbol, name, representation, provider, scoringMint, issuerKind, pythFeedId };
  });

  const suppliedTiming = input.timing && typeof input.timing === "object" ? input.timing : {};
  const timing = {};
  for (const [field, fallback] of Object.entries(DEFAULT_TIMING)) {
    timing[field] = positiveInteger(Number(suppliedTiming[field] ?? fallback), "timing." + field);
  }

  return {
    roundId,
    registryVersion,
    attestors: [...attestors].map((value) => String(value).trim()),
    assets: normalizedAssets,
    timing,
  };
}

/**
 * Produces strictly increasing on-chain timestamps from one captured clock
 * value. Keeping this calculation deterministic makes the generated manifest
 * and the submitted draft describe the same round window.
 */
export function buildRoundTimes(now, timing) {
  const base = positiveInteger(Number(now), "now");
  const queueCloseAt = base + timing.queueCloseAfterSecs;
  const commitDeadline = queueCloseAt + timing.commitWindowSecs;
  const revealDeadline = commitDeadline + timing.revealWindowSecs;
  const startTargetAt = revealDeadline + timing.startDelaySecs;
  const endTargetAt = startTargetAt + timing.observationWindowSecs;
  return {
    eligibilityFrozenAt: base,
    queueCloseAt,
    commitDeadline,
    revealDeadline,
    startTargetAt,
    endTargetAt,
  };
}

export { DEFAULT_TIMING };
