use anchor_lang::prelude::*;

pub const CONFIG_SEED: &[u8] = b"config";
pub const PRICE_POLICY_SEED: &[u8] = b"price-policy";
pub const QUALITY_POLICY_SEED: &[u8] = b"quality-policy";
pub const ATTESTOR_SET_SEED: &[u8] = b"attestor-set";
pub const ASSET_SEED: &[u8] = b"asset";
pub const MARKET_ROUND_SEED: &[u8] = b"market-round";
pub const ROUND_ASSET_SEED: &[u8] = b"round-asset";
pub const PRICE_ATTESTATION_SEED: &[u8] = b"price-attestation";
pub const BATTLE_SEED: &[u8] = b"battle";
pub const RATED_SLOT_SEED: &[u8] = b"rated-slot";
pub const LEAGUE_SEED: &[u8] = b"league";
pub const LEAGUE_MEMBER_SEED: &[u8] = b"league-member";

pub const PROTOCOL_VERSION: u16 = 2;
pub const LINEUP_SIZE: usize = 6;
pub const MAX_ASSETS_PER_REGISTRY: u16 = 256;
pub const MAX_ELIGIBLE_ASSETS: u16 = 256;
pub const ATTESTOR_COUNT: usize = 3;
pub const ATTESTOR_QUORUM: u8 = 2;
pub const PRICE_SCALE_Q9: i128 = 1_000_000_000;
pub const RETURN_SCALE_Q9: i128 = 1_000_000_000;
pub const RATING_FLOOR: i32 = 100;
pub const ATTESTATION_DOMAIN: &[u8] = b"TICKERSIX_PRICE_ATTESTATION_V1\0";

pub fn asset_bit(asset_id: u16) -> (usize, u64) {
    let word = usize::from(asset_id / 64);
    let bit = 1u64 << (asset_id % 64);
    (word, bit)
}
