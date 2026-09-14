use anchor_lang::prelude::Pubkey;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ProtocolVector {
    lineup: LineupVector,
    attestation: AttestationVector,
}

#[derive(Debug, Deserialize)]
struct LineupVector {
    program_id_hex: String,
    battle_hex: String,
    player_hex: String,
    registry_version: u32,
    asset_ids: [u16; 6],
    captain_asset_id: u16,
    salt_hex: String,
    expected_commitment_hex: String,
}

#[derive(Debug, Deserialize)]
struct AttestationVector {
    program_id_hex: String,
    market_round_hex: String,
    round_asset_hex: String,
    asset_id: u16,
    phase: u8,
    scoring_mint_hex: String,
    price_policy_version: u16,
    market_quality_policy_version: u16,
    attestor_set_version: u16,
    median_price_q9: i64,
    accepted_observation_count: u16,
    unique_source_block_count: u16,
    first_source_block_id: u64,
    last_source_block_id: u64,
    evidence_root_hex: String,
    observation_window_start: i64,
    observation_window_end: i64,
    report_created_at: i64,
    expected_message_parts: Vec<String>,
}

fn decode_hex<const N: usize>(value: &str) -> [u8; N] {
    (0..N)
        .map(|index| u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).unwrap())
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}

fn decode_hex_vec(value: &str) -> Vec<u8> {
    (0..value.len() / 2)
        .map(|index| u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).unwrap())
        .collect()
}

#[test]
fn onchain_implementation_matches_checked_in_v2_vector() {
    // Regression target: on-chain byte encoding must remain compatible with
    // the externally published vector even if host and on-chain code drift in
    // the same way.
    let vector: ProtocolVector =
        serde_json::from_str(include_str!("../../../fixtures/protocol_v2.json")).unwrap();
    let lineup = vector.lineup;
    assert_eq!(
        tickersix::math::canonical_lineup_commitment(
            Pubkey::new_from_array(decode_hex(&lineup.program_id_hex)),
            Pubkey::new_from_array(decode_hex(&lineup.battle_hex)),
            Pubkey::new_from_array(decode_hex(&lineup.player_hex)),
            lineup.registry_version,
            lineup.asset_ids,
            lineup.captain_asset_id,
            decode_hex(&lineup.salt_hex),
        ),
        decode_hex(&lineup.expected_commitment_hex)
    );

    let attestation = vector.attestation;
    let phase = match attestation.phase {
        0 => tickersix::PricePhase::Start,
        1 => tickersix::PricePhase::End,
        _ => panic!("fixture contains an unsupported price phase"),
    };
    assert_eq!(
        tickersix::instructions::price::canonical_attestation_message(
            Pubkey::new_from_array(decode_hex(&attestation.program_id_hex)),
            Pubkey::new_from_array(decode_hex(&attestation.market_round_hex)),
            Pubkey::new_from_array(decode_hex(&attestation.round_asset_hex)),
            attestation.asset_id,
            Pubkey::new_from_array(decode_hex(&attestation.scoring_mint_hex)),
            attestation.price_policy_version,
            attestation.market_quality_policy_version,
            attestation.attestor_set_version,
            phase,
            attestation.median_price_q9,
            attestation.accepted_observation_count,
            attestation.unique_source_block_count,
            attestation.first_source_block_id,
            attestation.last_source_block_id,
            decode_hex(&attestation.evidence_root_hex),
            attestation.observation_window_start,
            attestation.observation_window_end,
            attestation.report_created_at,
        ),
        attestation
            .expected_message_parts
            .iter()
            .flat_map(|part| decode_hex_vec(part))
            .collect::<Vec<_>>()
    );
}

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
