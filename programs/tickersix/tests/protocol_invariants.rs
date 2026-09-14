use anchor_lang::prelude::Pubkey;

#[test]
fn onchain_and_host_commitment_implementations_are_byte_identical() {
    // This catches the real cross-boundary bug where a frontend/backend hash
    // differs from the program because of ordering, endianness, or domain drift.
    let program_id = Pubkey::new_from_array([1u8; 32]);
    let battle = Pubkey::new_from_array([2u8; 32]);
    let player = Pubkey::new_from_array([3u8; 32]);
    let salt = [4u8; 32];
    let assets = [12, 1, 9, 4, 8, 3];

    let onchain = tickersix::math::canonical_lineup_commitment(
        program_id, battle, player, 1, assets, 9, salt,
    );
    let host = protocol::commitment(
        program_id.to_bytes(),
        battle.to_bytes(),
        player.to_bytes(),
        1,
        assets,
        9,
        salt,
    );

    assert_eq!(onchain, host);
}

#[test]
fn onchain_and_host_attestation_messages_are_byte_identical() {
    let program_id = Pubkey::new_from_array([1u8; 32]);
    let market_round = Pubkey::new_from_array([2u8; 32]);
    let round_asset = Pubkey::new_from_array([3u8; 32]);
    let scoring_mint = Pubkey::new_from_array([4u8; 32]);
    let evidence_root = [5u8; 32];

    let onchain = tickersix::instructions::price::canonical_attestation_message(
        program_id,
        market_round,
        round_asset,
        17,
        scoring_mint,
        2,
        3,
        4,
        tickersix::state::PricePhase::End,
        1_234_567_890,
        12,
        9,
        100,
        112,
        evidence_root,
        1_700_000_000,
        1_700_000_060,
        1_700_000_061,
    );
    let host = protocol::attestation_message(
        program_id.to_bytes(),
        market_round.to_bytes(),
        round_asset.to_bytes(),
        17,
        1,
        scoring_mint.to_bytes(),
        2,
        3,
        4,
        1_234_567_890,
        12,
        9,
        100,
        112,
        evidence_root,
        1_700_000_000,
        1_700_000_060,
        1_700_000_061,
    );

    assert_eq!(onchain, host);
}
