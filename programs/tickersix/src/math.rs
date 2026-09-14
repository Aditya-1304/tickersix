use anchor_lang::prelude::*;

use crate::{
    constants::{LINEUP_SIZE, RETURN_SCALE_Q9},
    error::ErrorCode,
};

/// Builds the exact lineup preimage shared with the host-side protocol crate.
pub fn canonical_lineup_commitment(
    program_id: Pubkey,
    battle: Pubkey,
    player: Pubkey,
    registry_version: u32,
    mut asset_ids: [u16; LINEUP_SIZE],
    captain_asset_id: u16,
    salt: [u8; 32],
) -> [u8; 32] {
    asset_ids.sort_unstable();

    let mut bytes = Vec::with_capacity(166);
    bytes.extend_from_slice(b"TICKERSIX_LINEUP_V1\0");
    bytes.extend_from_slice(program_id.as_ref());
    bytes.extend_from_slice(battle.as_ref());
    bytes.extend_from_slice(player.as_ref());
    bytes.extend_from_slice(&registry_version.to_le_bytes());
    for asset_id in asset_ids {
        bytes.extend_from_slice(&asset_id.to_le_bytes());
    }
    bytes.extend_from_slice(&captain_asset_id.to_le_bytes());
    bytes.extend_from_slice(&salt);

    solana_sha256_hasher::hash(&bytes).to_bytes()
}

pub fn validate_lineup(
    asset_ids: [u16; LINEUP_SIZE],
    captain_asset_id: u16,
    eligible_asset_bitmap: &[u64; 4],
) -> Result<()> {
    if !asset_ids.contains(&captain_asset_id) {
        return err!(ErrorCode::CaptainNotInLineup);
    }

    for left in 0..LINEUP_SIZE {
        require!(asset_ids[left] < 256, ErrorCode::InvalidLineup);
        let (word, bit) = crate::constants::asset_bit(asset_ids[left]);
        require!(
            eligible_asset_bitmap[word] & bit != 0,
            ErrorCode::InvalidLineup
        );
        require!(
            !asset_ids[left + 1..].contains(&asset_ids[left]),
            ErrorCode::InvalidLineup
        );
    }

    Ok(())
}

pub fn return_q9(start_q9: i64, end_q9: i64) -> Result<i64> {
    require!(start_q9 > 0 && end_q9 > 0, ErrorCode::RoundAssetUnavailable);
    let numerator = i128::from(end_q9)
        .checked_sub(i128::from(start_q9))
        .and_then(|value| value.checked_mul(RETURN_SCALE_Q9))
        .ok_or_else(|| error!(ErrorCode::MathOverflow))?;
    let value = numerator
        .checked_div(i128::from(start_q9))
        .ok_or_else(|| error!(ErrorCode::MathOverflow))?;
    i64::try_from(value).map_err(|_| error!(ErrorCode::MathOverflow))
}

pub fn lineup_score_q9(
    returns: [i64; LINEUP_SIZE],
    asset_ids: [u16; LINEUP_SIZE],
    captain_asset_id: u16,
) -> Result<i64> {
    require!(
        asset_ids.contains(&captain_asset_id),
        ErrorCode::CaptainNotInLineup
    );
    let mut total = 0i128;
    for (asset_return, asset_id) in returns.into_iter().zip(asset_ids) {
        let weight = if asset_id == captain_asset_id {
            2i128
        } else {
            1
        };
        total = total
            .checked_add(
                i128::from(asset_return)
                    .checked_mul(weight)
                    .ok_or_else(|| error!(ErrorCode::MathOverflow))?,
            )
            .ok_or_else(|| error!(ErrorCode::MathOverflow))?;
    }
    let score = total
        .checked_div(7)
        .ok_or_else(|| error!(ErrorCode::MathOverflow))?;
    i64::try_from(score).map_err(|_| error!(ErrorCode::MathOverflow))
}
