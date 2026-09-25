/**
 * Validates a player's lineup against the frozen RoundAsset catalog.
 *
 * The browser uses this guard before opening the commitment review screen. It
 * deliberately returns data instead of throwing so the UI can explain the
 * precise correction required without mutating the selected assets.
 */
export function validateLineup(assetIds, captainAssetId, assets) {
  const ids = Array.from(assetIds ?? [], (assetId) => Number(assetId));
  if (ids.some((assetId) => !Number.isSafeInteger(assetId))) {
    return { valid: false, reason: "LINEUP_ASSET_ID_INVALID" };
  }
  if (ids.length !== 6) {
    return { valid: false, reason: "LINEUP_REQUIRES_SIX_ASSETS" };
  }
  if (new Set(ids).size !== ids.length) {
    return { valid: false, reason: "LINEUP_ASSETS_MUST_BE_UNIQUE" };
  }

  const catalogIds = new Set(
    (assets ?? []).map((asset) => Number(asset.id ?? asset.asset_id)),
  );
  if (ids.some((assetId) => !catalogIds.has(assetId))) {
    return { valid: false, reason: "LINEUP_ASSET_NOT_IN_FROZEN_UNIVERSE" };
  }

  const captain = Number(captainAssetId);
  if (!ids.includes(captain)) {
    return { valid: false, reason: "CAPTAIN_MUST_BE_SELECTED" };
  }

  return { valid: true, assetIds: ids, captainAssetId: captain };
}
