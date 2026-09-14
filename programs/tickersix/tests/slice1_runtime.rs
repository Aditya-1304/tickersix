//! LiteSVM acceptance tests for the immutable V2 protocol foundation.
//!
//! These tests intentionally exercise the deployed SBF rather than calling
//! instruction handlers directly. The Slice 1 contract is account topology
//! and transaction-level enforcement: frozen inputs must survive version
//! rotation, one rated exposure must be atomic, and stale round admission must
//! fail even when an account's state has not been cranked forward yet.

use anchor_lang::{
    prelude::Pubkey,
    solana_program::instruction::{AccountMeta, Instruction},
    AccountDeserialize, InstructionData,
};
use litesvm::LiteSVM;
use solana_address::Address;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use std::sync::Arc;

fn address(key: Pubkey) -> Address {
    Address::from(key.to_bytes())
}

fn key(keypair: &Keypair) -> Pubkey {
    Pubkey::from(keypair.pubkey().to_bytes())
}

fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &tickersix::ID).0
}

fn league_member_pda(league: Pubkey, player: &Keypair) -> Pubkey {
    pda(&[
        tickersix::LEAGUE_MEMBER_SEED,
        league.as_ref(),
        key(player).as_ref(),
    ])
}

fn readonly(key: Pubkey) -> AccountMeta {
    AccountMeta::new_readonly(address(key), false)
}

fn readonly_signer(key: Pubkey) -> AccountMeta {
    AccountMeta::new_readonly(address(key), true)
}

fn writable(key: Pubkey) -> AccountMeta {
    AccountMeta::new(address(key), false)
}

fn writable_signer(key: Pubkey) -> AccountMeta {
    AccountMeta::new(address(key), true)
}

fn system_program() -> AccountMeta {
    readonly(anchor_lang::solana_program::system_program::ID)
}

fn ed25519_instruction(message: &[u8], attestor: &Keypair) -> Instruction {
    // Native Ed25519 instructions use a two-byte header followed by one
    // fourteen-byte offset descriptor. All payloads are inline so the program
    // can bind the inspected key and message without cross-instruction reads.
    let signature_offset = 16u16;
    let public_key_offset = signature_offset + 64;
    let message_offset = public_key_offset + 32;
    let mut data = Vec::with_capacity(usize::from(message_offset) + message.len());
    data.extend_from_slice(&[1, 0]);
    data.extend_from_slice(&signature_offset.to_le_bytes());
    data.extend_from_slice(&u16::MAX.to_le_bytes());
    data.extend_from_slice(&public_key_offset.to_le_bytes());
    data.extend_from_slice(&u16::MAX.to_le_bytes());
    data.extend_from_slice(&message_offset.to_le_bytes());
    data.extend_from_slice(&(message.len() as u16).to_le_bytes());
    data.extend_from_slice(&u16::MAX.to_le_bytes());
    data.extend_from_slice(attestor.sign_message(message).as_array());
    data.extend_from_slice(attestor.pubkey().as_array());
    data.extend_from_slice(message);
    Instruction {
        program_id: solana_sdk_ids::ed25519_program::ID,
        accounts: Vec::new(),
        data,
    }
}

struct Harness {
    svm: LiteSVM,
    coordinator: Arc<Keypair>,
    config: Pubkey,
}

impl Harness {
    fn new() -> Self {
        let mut svm = LiteSVM::new();
        svm.add_program_from_file(
            address(tickersix::ID),
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../target/deploy/tickersix.so"
            ),
        )
        .unwrap();

        let coordinator = Arc::new(Keypair::new());
        svm.airdrop(&coordinator.pubkey(), 50_000_000_000).unwrap();
        let config = pda(&[tickersix::CONFIG_SEED]);
        let mut harness = Self {
            svm,
            coordinator,
            config,
        };
        harness.set_time(100, 100);
        harness
            .submit(
                Arc::clone(&harness.coordinator),
                Instruction {
                    program_id: address(tickersix::ID),
                    accounts: vec![
                        writable_signer(key(&harness.coordinator)),
                        writable(harness.config),
                        system_program(),
                    ],
                    data: tickersix::instruction::InitializeConfig {}.data(),
                },
                &[],
            )
            .unwrap();
        harness
    }

    fn set_time(&mut self, unix_timestamp: i64, slot: u64) {
        let clock = anchor_lang::solana_program::clock::Clock {
            slot,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp,
        };
        self.svm.set_sysvar(&clock);
    }

    fn submit(
        &mut self,
        fee_payer: Arc<Keypair>,
        instruction: Instruction,
        additional_signers: &[&Keypair],
    ) -> Result<(), String> {
        self.submit_transaction(&fee_payer, &[instruction], additional_signers)
    }

    fn submit_transaction(
        &mut self,
        fee_payer: &Arc<Keypair>,
        instructions: &[Instruction],
        additional_signers: &[&Keypair],
    ) -> Result<(), String> {
        let mut signers = Vec::with_capacity(additional_signers.len() + 1);
        signers.push(fee_payer.as_ref());
        signers.extend_from_slice(additional_signers);
        let message = Message::new_with_blockhash(
            instructions,
            Some(&fee_payer.pubkey()),
            &self.svm.latest_blockhash(),
        );
        let transaction =
            VersionedTransaction::try_new(VersionedMessage::Legacy(message), &signers)
                .map_err(|error| error.to_string())?;
        self.svm
            .send_transaction(transaction)
            .map(|_| ())
            .map_err(|error| format!("{error:?}"))
    }

    fn account<T: AccountDeserialize>(&self, account: Pubkey) -> T {
        let account = self.svm.get_account(&address(account)).unwrap();
        let mut data: &[u8] = &account.data;
        T::try_deserialize(&mut data).unwrap()
    }

    fn configure_policies_and_registry_with_attestors(
        &mut self,
        attestors: [Pubkey; 3],
    ) -> Vec<Pubkey> {
        let price_policy = pda(&[tickersix::PRICE_POLICY_SEED, &1u16.to_le_bytes()]);
        self.submit(
            Arc::clone(&self.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![
                    writable(self.config),
                    writable_signer(key(&self.coordinator)),
                    writable(price_policy),
                    system_program(),
                ],
                data: tickersix::instruction::CreatePricePolicy {
                    version: 1,
                    source_kind: tickersix::PriceSourceKind::JupiterTokenSpotV1,
                    observation_window_secs: 10,
                    attestation_grace_secs: 10,
                    sample_interval_secs: 5,
                    max_attestor_spread_bps: 100,
                    min_accepted_observations: 1,
                    min_unique_source_blocks: 1,
                    max_source_block_lag: 100,
                }
                .data(),
            },
            &[],
        )
        .unwrap();

        let quality_policy = pda(&[tickersix::QUALITY_POLICY_SEED, &1u16.to_le_bytes()]);
        self.submit(
            Arc::clone(&self.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![
                    writable(self.config),
                    writable_signer(key(&self.coordinator)),
                    writable(quality_policy),
                    system_program(),
                ],
                data: tickersix::instruction::CreateMarketQualityPolicy {
                    version: 1,
                    canonical_policy_hash: [9; 32],
                    min_eligible_assets: 6,
                }
                .data(),
            },
            &[],
        )
        .unwrap();

        let attestor_set = pda(&[tickersix::ATTESTOR_SET_SEED, &1u16.to_le_bytes()]);
        self.submit(
            Arc::clone(&self.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![
                    writable(self.config),
                    writable_signer(key(&self.coordinator)),
                    writable(attestor_set),
                    system_program(),
                ],
                data: tickersix::instruction::CreateAttestorSet {
                    version: 1,
                    attestors,
                }
                .data(),
            },
            &[],
        )
        .unwrap();

        for asset_id in 0u16..7 {
            let asset = pda(&[
                tickersix::ASSET_SEED,
                &1u32.to_le_bytes(),
                &asset_id.to_le_bytes(),
            ]);
            self.submit(
                Arc::clone(&self.coordinator),
                Instruction {
                    program_id: address(tickersix::ID),
                    accounts: vec![
                        writable(self.config),
                        writable_signer(key(&self.coordinator)),
                        writable(asset),
                        system_program(),
                    ],
                    data: tickersix::instruction::CreateRegistryEntry {
                        registry_version: 1,
                        asset_id,
                        symbol: [b'A' + (asset_id as u8), 0, 0, 0, 0, 0, 0, 0],
                        scoring_mint: Pubkey::new_from_array([100 + asset_id as u8; 32]),
                        issuer_kind: 1,
                    }
                    .data(),
                },
                &[],
            )
            .unwrap();
        }

        self.submit(
            Arc::clone(&self.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![
                    writable(self.config),
                    readonly_signer(key(&self.coordinator)),
                ],
                data: tickersix::instruction::FreezeRegistryVersion {}.data(),
            },
            &[],
        )
        .unwrap();

        (0u16..7)
            .map(|asset_id| Pubkey::new_from_array([100 + asset_id as u8; 32]))
            .collect()
    }

    fn create_and_freeze_round(&mut self) -> (Pubkey, Vec<Pubkey>) {
        self.create_and_freeze_round_with_attestors([
            Pubkey::new_from_array([11; 32]),
            Pubkey::new_from_array([12; 32]),
            Pubkey::new_from_array([13; 32]),
        ])
    }

    fn create_and_freeze_round_with_attestors(
        &mut self,
        attestors: [Pubkey; 3],
    ) -> (Pubkey, Vec<Pubkey>) {
        let mints = self.configure_policies_and_registry_with_attestors(attestors);
        let round = pda(&[tickersix::MARKET_ROUND_SEED, &1u64.to_le_bytes()]);
        let price_policy = pda(&[tickersix::PRICE_POLICY_SEED, &1u16.to_le_bytes()]);
        let quality_policy = pda(&[tickersix::QUALITY_POLICY_SEED, &1u16.to_le_bytes()]);
        let attestor_set = pda(&[tickersix::ATTESTOR_SET_SEED, &1u16.to_le_bytes()]);

        self.submit(
            Arc::clone(&self.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![
                    writable(self.config),
                    writable_signer(key(&self.coordinator)),
                    writable(round),
                    readonly(price_policy),
                    readonly(quality_policy),
                    readonly(attestor_set),
                    system_program(),
                ],
                data: tickersix::instruction::CreateMarketRoundDraft {
                    round_id: 1,
                    registry_version: 1,
                    eligibility_snapshot_hash: [7; 32],
                    eligibility_frozen_at: 90,
                    queue_close_at: 110,
                    commit_deadline: 120,
                    reveal_deadline: 130,
                    start_target_at: 140,
                    end_target_at: 150,
                    is_replay: false,
                }
                .data(),
            },
            &[],
        )
        .unwrap();

        let mut round_assets = Vec::new();
        for asset_id in 0u16..6 {
            let registry_entry = pda(&[
                tickersix::ASSET_SEED,
                &1u32.to_le_bytes(),
                &asset_id.to_le_bytes(),
            ]);
            let round_asset = pda(&[
                tickersix::ROUND_ASSET_SEED,
                round.as_ref(),
                &asset_id.to_le_bytes(),
            ]);
            let mut accounts = vec![
                readonly(self.config),
                writable(round),
                writable_signer(key(&self.coordinator)),
                writable(round_asset),
                readonly(registry_entry),
                readonly(price_policy),
                readonly(quality_policy),
                system_program(),
            ];
            accounts.extend(round_assets.iter().copied().map(readonly));
            self.submit(
                Arc::clone(&self.coordinator),
                Instruction {
                    program_id: address(tickersix::ID),
                    accounts,
                    data: tickersix::instruction::AddRoundAsset {
                        asset_id,
                        scoring_mint: mints[asset_id as usize],
                        issuer_kind: 1,
                        price_source_kind: tickersix::PriceSourceKind::JupiterTokenSpotV1,
                        price_policy_version: 1,
                        market_quality_policy_version: 1,
                    }
                    .data(),
                },
                &[],
            )
            .unwrap();
            round_assets.push(round_asset);
        }

        let mut accounts = vec![
            readonly(self.config),
            writable(round),
            readonly(price_policy),
            readonly(quality_policy),
            readonly(attestor_set),
            readonly_signer(key(&self.coordinator)),
        ];
        accounts.extend(round_assets.iter().copied().map(readonly));
        self.submit(
            Arc::clone(&self.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts,
                data: tickersix::instruction::FreezeMarketRound {}.data(),
            },
            &[],
        )
        .unwrap();

        (round, round_assets)
    }

    fn create_official_league(&mut self, league_id: u64, registration_close_at: i64) -> Pubkey {
        let league = pda(&[tickersix::LEAGUE_SEED, &league_id.to_le_bytes()]);
        self.submit(
            Arc::clone(&self.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![
                    readonly(self.config),
                    writable_signer(key(&self.coordinator)),
                    writable(league),
                    system_program(),
                ],
                data: tickersix::instruction::CreateOfficialLeague {
                    league_id,
                    max_players: 2,
                    total_rounds: 5,
                    pairing_policy_version: 1,
                    registration_close_at,
                }
                .data(),
            },
            &[],
        )
        .unwrap();
        league
    }

    fn join_league(&mut self, league: Pubkey, player: &Keypair) {
        let member = pda(&[
            tickersix::LEAGUE_MEMBER_SEED,
            league.as_ref(),
            key(player).as_ref(),
        ]);
        self.submit(
            Arc::clone(&self.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![
                    readonly(self.config),
                    writable_signer(key(player)),
                    writable(league),
                    writable(member),
                    system_program(),
                ],
                data: tickersix::instruction::JoinLeague {}.data(),
            },
            &[player],
        )
        .unwrap();
    }
}

fn create_ranked_battle(
    harness: &mut Harness,
    round: Pubkey,
    battle_id: u64,
    player_a: &Keypair,
    player_b: &Keypair,
) -> Pubkey {
    let battle = pda(&[
        tickersix::BATTLE_SEED,
        round.as_ref(),
        &battle_id.to_le_bytes(),
    ]);
    let slot_a = pda(&[
        tickersix::RATED_SLOT_SEED,
        round.as_ref(),
        key(player_a).as_ref(),
    ]);
    let slot_b = pda(&[
        tickersix::RATED_SLOT_SEED,
        round.as_ref(),
        key(player_b).as_ref(),
    ]);
    harness
        .submit(
            Arc::clone(&harness.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![
                    readonly(harness.config),
                    writable_signer(key(&harness.coordinator)),
                    writable(round),
                    readonly(key(player_a)),
                    readonly(key(player_b)),
                    // Anchor treats the program id placeholder as an omitted
                    // optional account in this legacy test transaction.
                    readonly(tickersix::ID),
                    readonly(tickersix::ID),
                    readonly(tickersix::ID),
                    writable(battle),
                    writable(slot_a),
                    writable(slot_b),
                    system_program(),
                ],
                data: tickersix::instruction::CreateRatedBattle {
                    battle_id,
                    mode: tickersix::BattleMode::Ranked,
                    league: Pubkey::default(),
                    league_round_no: 0,
                    rating_a_before: 1_500,
                    rating_b_before: 1_500,
                    rating_formula_version: 1,
                }
                .data(),
            },
            &[],
        )
        .unwrap();
    battle
}

#[test]
fn reveal_requires_authoritative_reveal_open_state() {
    // Regression target: wall-clock checks alone must not allow a reveal while
    // the on-chain Market Round is still in CommitOpen. This catches stale
    // keeper/account state being treated as an implicit phase transition.
    let mut harness = Harness::new();
    let (round, _) = harness.create_and_freeze_round();
    let player_a = Keypair::new();
    let player_b = Keypair::new();
    harness.set_time(110, 110);
    harness
        .submit(
            Arc::clone(&harness.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![writable(round), readonly_signer(key(&harness.coordinator))],
                data: tickersix::instruction::AdvanceMarketRound {}.data(),
            },
            &[],
        )
        .unwrap();
    let battle = create_ranked_battle(&mut harness, round, 91, &player_a, &player_b);
    let asset_ids = [0, 1, 2, 3, 4, 5];
    let salt = [42; 32];
    let commitment = tickersix::math::canonical_lineup_commitment(
        tickersix::ID,
        battle,
        key(&player_a),
        1,
        asset_ids,
        0,
        salt,
    );

    harness.set_time(115, 115);
    harness
        .submit(
            Arc::clone(&harness.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![
                    readonly(harness.config),
                    writable(battle),
                    readonly(round),
                    readonly_signer(key(&player_a)),
                ],
                data: tickersix::instruction::CommitLineup { commitment }.data(),
            },
            &[&player_a],
        )
        .unwrap();

    // Time is inside the reveal interval, but the keeper has intentionally
    // not advanced CommitOpen -> RevealOpen. The protocol must fail closed.
    harness.set_time(125, 125);
    let reveal_attempt = harness.submit(
        Arc::clone(&harness.coordinator),
        Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                writable(battle),
                readonly(round),
                readonly_signer(key(&player_a)),
            ],
            data: tickersix::instruction::RevealLineup {
                asset_ids,
                captain_asset_id: 0,
                salt,
            }
            .data(),
        },
        &[&player_a],
    );
    assert!(reveal_attempt.is_err());
    assert!(!harness.account::<tickersix::Battle>(battle).a.revealed);
}

#[test]
fn commit_reveal_is_hash_bound_canonical_and_one_shot() {
    // Regression target: a failed preimage must not partially mutate state,
    // while a valid permutation must be stored in the canonical order and a
    // later reveal must be rejected rather than overwrite the first reveal.
    let mut harness = Harness::new();
    let (round, _) = harness.create_and_freeze_round();
    let player_a = Keypair::new();
    let player_b = Keypair::new();

    harness.set_time(110, 110);
    harness
        .submit(
            Arc::clone(&harness.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![writable(round), readonly_signer(key(&harness.coordinator))],
                data: tickersix::instruction::AdvanceMarketRound {}.data(),
            },
            &[],
        )
        .unwrap();
    let battle = create_ranked_battle(&mut harness, round, 92, &player_a, &player_b);

    let submitted_asset_ids = [5, 1, 4, 2, 0, 3];
    let canonical_asset_ids = [0, 1, 2, 3, 4, 5];
    let captain_asset_id = 4;
    let salt = [43; 32];
    let commitment = tickersix::math::canonical_lineup_commitment(
        tickersix::ID,
        battle,
        key(&player_a),
        1,
        submitted_asset_ids,
        captain_asset_id,
        salt,
    );

    harness.set_time(115, 115);
    harness
        .submit(
            Arc::clone(&harness.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![
                    readonly(harness.config),
                    writable(battle),
                    readonly(round),
                    readonly_signer(key(&player_a)),
                ],
                data: tickersix::instruction::CommitLineup { commitment }.data(),
            },
            &[&player_a],
        )
        .unwrap();

    harness.set_time(120, 120);
    harness.svm.expire_blockhash();
    harness
        .submit(
            Arc::clone(&harness.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![writable(round), readonly_signer(key(&harness.coordinator))],
                data: tickersix::instruction::AdvanceMarketRound {}.data(),
            },
            &[],
        )
        .unwrap();

    harness.set_time(125, 125);
    let invalid_lineup = harness.submit(
        Arc::clone(&harness.coordinator),
        Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                writable(battle),
                readonly(round),
                readonly_signer(key(&player_a)),
            ],
            data: tickersix::instruction::RevealLineup {
                asset_ids: [0, 1, 2, 3, 4, 4],
                captain_asset_id: 4,
                salt,
            }
            .data(),
        },
        &[&player_a],
    );
    assert!(invalid_lineup.is_err());
    assert!(!harness.account::<tickersix::Battle>(battle).a.revealed);

    let invalid_reveal = harness.submit(
        Arc::clone(&harness.coordinator),
        Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                writable(battle),
                readonly(round),
                readonly_signer(key(&player_a)),
            ],
            data: tickersix::instruction::RevealLineup {
                asset_ids: submitted_asset_ids,
                captain_asset_id,
                salt: [44; 32],
            }
            .data(),
        },
        &[&player_a],
    );
    assert!(invalid_reveal.is_err());
    let after_invalid = harness.account::<tickersix::Battle>(battle);
    assert!(after_invalid.a.committed);
    assert!(!after_invalid.a.revealed);

    harness
        .submit(
            Arc::clone(&harness.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![
                    writable(battle),
                    readonly(round),
                    readonly_signer(key(&player_a)),
                ],
                data: tickersix::instruction::RevealLineup {
                    asset_ids: submitted_asset_ids,
                    captain_asset_id,
                    salt,
                }
                .data(),
            },
            &[&player_a],
        )
        .unwrap();
    let revealed = harness.account::<tickersix::Battle>(battle);
    assert_eq!(revealed.a.asset_ids, canonical_asset_ids);
    assert_eq!(revealed.a.captain_asset_id, captain_asset_id);
    assert!(revealed.a.revealed);

    let second_reveal = harness.submit(
        Arc::clone(&harness.coordinator),
        Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                writable(battle),
                readonly(round),
                readonly_signer(key(&player_a)),
            ],
            data: tickersix::instruction::RevealLineup {
                asset_ids: submitted_asset_ids,
                captain_asset_id,
                salt,
            }
            .data(),
        },
        &[&player_a],
    );
    assert!(second_reveal.is_err());
}

#[test]
fn attestation_requires_exact_native_signature_and_rejects_duplicate_report() {
    // Regression target: a relayer must not be able to submit an unsigned or
    // mismatched price report, and the same registered attestor must not create
    // a second report for one RoundAsset phase.
    let mut harness = Harness::new();
    let attestor_a = Keypair::new();
    let attestor_b = Keypair::new();
    let attestor_c = Keypair::new();
    let unregistered_attestor = Keypair::new();
    let (round, round_assets) = harness.create_and_freeze_round_with_attestors([
        key(&attestor_a),
        key(&attestor_b),
        key(&attestor_c),
    ]);
    let round_asset = round_assets[0];
    let price_policy = pda(&[tickersix::PRICE_POLICY_SEED, &1u16.to_le_bytes()]);
    let quality_policy = pda(&[tickersix::QUALITY_POLICY_SEED, &1u16.to_le_bytes()]);
    let attestor_set = pda(&[tickersix::ATTESTOR_SET_SEED, &1u16.to_le_bytes()]);
    let evidence_root = [21; 32];
    let price_q9 = 100_000_000_000;
    let phase = tickersix::PricePhase::Start;
    let message = tickersix::instructions::price::canonical_attestation_message(
        tickersix::ID,
        round,
        round_asset,
        0,
        Pubkey::new_from_array([100; 32]),
        1,
        1,
        1,
        phase,
        price_q9,
        1,
        1,
        149,
        149,
        evidence_root,
        140,
        150,
        150,
    );
    let price_attestation = pda(&[
        tickersix::PRICE_ATTESTATION_SEED,
        round_asset.as_ref(),
        &[phase as u8],
        key(&attestor_a).as_ref(),
    ]);
    let coordinator = Arc::clone(&harness.coordinator);
    let coordinator_key = key(&coordinator);
    let submit_accounts = |attestor: Pubkey, price_attestation: Pubkey| {
        vec![
            readonly(round_asset),
            readonly(round),
            readonly(price_policy),
            readonly(quality_policy),
            readonly(attestor_set),
            readonly(attestor),
            writable(price_attestation),
            writable_signer(coordinator_key),
            readonly(solana_instructions_sysvar::ID),
            system_program(),
        ]
    };

    harness.set_time(150, 150);
    harness
        .submit_transaction(
            &coordinator,
            &[
                ed25519_instruction(&message, &attestor_a),
                Instruction {
                    program_id: address(tickersix::ID),
                    accounts: submit_accounts(key(&attestor_a), price_attestation),
                    data: tickersix::instruction::SubmitPriceAttestation {
                        phase,
                        median_price_q9: price_q9,
                        accepted_observation_count: 1,
                        unique_source_block_count: 1,
                        first_source_block_id: 149,
                        last_source_block_id: 149,
                        evidence_root,
                        report_created_at: 150,
                    }
                    .data(),
                },
            ],
            &[],
        )
        .unwrap();

    let stored = harness.account::<tickersix::PriceAttestation>(price_attestation);
    assert_eq!(stored.attestor, key(&attestor_a));
    assert_eq!(stored.median_price_q9, price_q9);

    let attestor_b_report = pda(&[
        tickersix::PRICE_ATTESTATION_SEED,
        round_asset.as_ref(),
        &[phase as u8],
        key(&attestor_b).as_ref(),
    ]);

    // A registered attestor key without a preceding native verification
    // instruction must not be accepted merely because the relay payload is
    // otherwise well formed.
    let unsigned_attempt = harness.submit_transaction(
        &coordinator,
        &[Instruction {
            program_id: address(tickersix::ID),
            accounts: submit_accounts(key(&attestor_b), attestor_b_report),
            data: tickersix::instruction::SubmitPriceAttestation {
                phase,
                median_price_q9: price_q9,
                accepted_observation_count: 1,
                unique_source_block_count: 1,
                first_source_block_id: 149,
                last_source_block_id: 149,
                evidence_root,
                report_created_at: 150,
            }
            .data(),
        }],
        &[],
    );
    assert!(unsigned_attempt.is_err());

    // A valid Ed25519 signature over different bytes is equally insufficient:
    // the program must bind the native verification instruction to the exact
    // canonical report message it is consuming.
    let wrong_message_attempt = harness.submit_transaction(
        &coordinator,
        &[
            ed25519_instruction(b"wrong report", &attestor_b),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: submit_accounts(key(&attestor_b), attestor_b_report),
                data: tickersix::instruction::SubmitPriceAttestation {
                    phase,
                    median_price_q9: price_q9,
                    accepted_observation_count: 1,
                    unique_source_block_count: 1,
                    first_source_block_id: 149,
                    last_source_block_id: 149,
                    evidence_root,
                    report_created_at: 150,
                }
                .data(),
            },
        ],
        &[],
    );
    assert!(wrong_message_attempt.is_err());

    let second_stored = harness.submit_transaction(
        &coordinator,
        &[
            ed25519_instruction(&message, &attestor_b),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: submit_accounts(key(&attestor_b), attestor_b_report),
                data: tickersix::instruction::SubmitPriceAttestation {
                    phase,
                    median_price_q9: price_q9,
                    accepted_observation_count: 1,
                    unique_source_block_count: 1,
                    first_source_block_id: 149,
                    last_source_block_id: 149,
                    evidence_root,
                    report_created_at: 150,
                }
                .data(),
            },
        ],
        &[],
    );
    second_stored.unwrap();

    let unregistered_report = pda(&[
        tickersix::PRICE_ATTESTATION_SEED,
        round_asset.as_ref(),
        &[phase as u8],
        key(&unregistered_attestor).as_ref(),
    ]);
    let unregistered_attempt = harness.submit_transaction(
        &coordinator,
        &[
            ed25519_instruction(&message, &unregistered_attestor),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: submit_accounts(key(&unregistered_attestor), unregistered_report),
                data: tickersix::instruction::SubmitPriceAttestation {
                    phase,
                    median_price_q9: price_q9,
                    accepted_observation_count: 1,
                    unique_source_block_count: 1,
                    first_source_block_id: 149,
                    last_source_block_id: 149,
                    evidence_root,
                    report_created_at: 150,
                }
                .data(),
            },
        ],
        &[],
    );
    assert!(unregistered_attempt.is_err());

    // A third, incompatible report is validly signed but must be excluded by
    // the deterministic compatible-cluster rule rather than poisoning the
    // available two-of-three quorum.
    let outlier_price_q9 = 1_000_000_000_000;
    let outlier_evidence_root = [22; 32];
    let outlier_message = tickersix::instructions::price::canonical_attestation_message(
        tickersix::ID,
        round,
        round_asset,
        0,
        Pubkey::new_from_array([100; 32]),
        1,
        1,
        1,
        phase,
        outlier_price_q9,
        1,
        1,
        149,
        149,
        outlier_evidence_root,
        140,
        150,
        150,
    );
    let attestor_c_report = pda(&[
        tickersix::PRICE_ATTESTATION_SEED,
        round_asset.as_ref(),
        &[phase as u8],
        key(&attestor_c).as_ref(),
    ]);
    harness
        .submit_transaction(
            &coordinator,
            &[
                ed25519_instruction(&outlier_message, &attestor_c),
                Instruction {
                    program_id: address(tickersix::ID),
                    accounts: submit_accounts(key(&attestor_c), attestor_c_report),
                    data: tickersix::instruction::SubmitPriceAttestation {
                        phase,
                        median_price_q9: outlier_price_q9,
                        accepted_observation_count: 1,
                        unique_source_block_count: 1,
                        first_source_block_id: 149,
                        last_source_block_id: 149,
                        evidence_root: outlier_evidence_root,
                        report_created_at: 150,
                    }
                    .data(),
                },
            ],
            &[],
        )
        .unwrap();

    // A marker must not be able to suppress an available quorum simply by
    // racing the permissionless finalizer after the deadline.
    let premature_unavailable = harness.submit_transaction(
        &coordinator,
        &[Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                writable(round_asset),
                readonly(round),
                readonly(price_policy),
                readonly(attestor_set),
                writable_signer(coordinator_key),
                readonly(price_attestation),
                readonly(attestor_b_report),
                readonly(attestor_c_report),
            ],
            data: tickersix::instruction::MarkPricePhaseUnavailable { phase }.data(),
        }],
        &[],
    );
    assert!(premature_unavailable.is_err());

    // Finalization before the observation window plus grace period must fail;
    // otherwise a caller could resolve a phase before late reports arrive.
    harness.set_time(159, 159);
    let early_finalize = harness.submit_transaction(
        &coordinator,
        &[Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                writable(round_asset),
                readonly(round),
                readonly(price_policy),
                readonly(quality_policy),
                readonly(attestor_set),
                writable_signer(coordinator_key),
                readonly(price_attestation),
                readonly(attestor_b_report),
                readonly(attestor_c_report),
            ],
            data: tickersix::instruction::FinalizePricePhase { phase }.data(),
        }],
        &[],
    );
    assert!(early_finalize.is_err());

    // The two compatible reports finalize to their checked integer midpoint;
    // once written, the phase cannot be finalized a second time.
    // A report was already freshness-checked at submission time. Delayed
    // permissionless finalization must not make that accepted report stale.
    harness.set_time(260, 260);
    harness.svm.expire_blockhash();
    let finalize_attempt = harness.submit_transaction(
        &coordinator,
        &[Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                writable(round_asset),
                readonly(round),
                readonly(price_policy),
                readonly(quality_policy),
                readonly(attestor_set),
                writable_signer(coordinator_key),
                readonly(price_attestation),
                readonly(attestor_b_report),
                readonly(attestor_c_report),
            ],
            data: tickersix::instruction::FinalizePricePhase { phase }.data(),
        }],
        &[],
    );
    finalize_attempt.unwrap();
    let finalized = harness.account::<tickersix::RoundAsset>(round_asset);
    assert!(finalized.start_finalized);
    assert_eq!(finalized.start_price_q9, price_q9);
    assert_ne!(finalized.start_evidence_commitment, [0; 32]);

    let second_finalize = harness.submit_transaction(
        &coordinator,
        &[Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                writable(round_asset),
                readonly(round),
                readonly(price_policy),
                readonly(quality_policy),
                readonly(attestor_set),
                writable_signer(coordinator_key),
                readonly(price_attestation),
                readonly(attestor_b_report),
                readonly(attestor_c_report),
            ],
            data: tickersix::instruction::FinalizePricePhase { phase }.data(),
        }],
        &[],
    );
    assert!(second_finalize.is_err());

    // An asset with no compatible quorum cannot be finalized with caller-
    // supplied fallback data; it must transition to an explicit unavailable
    // state after the frozen deadline.
    let unavailable_asset = round_assets[1];
    let unavailable_attestor_reports = [
        attestor_a.pubkey(),
        attestor_b.pubkey(),
        attestor_c.pubkey(),
    ]
    .map(|attestor| {
        pda(&[
            tickersix::PRICE_ATTESTATION_SEED,
            unavailable_asset.as_ref(),
            &[phase as u8],
            attestor.as_ref(),
        ])
    });
    let no_quorum_attempt = harness.submit_transaction(
        &coordinator,
        &[Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                writable(unavailable_asset),
                readonly(round),
                readonly(price_policy),
                readonly(quality_policy),
                readonly(attestor_set),
                writable_signer(coordinator_key),
                readonly(unavailable_attestor_reports[0]),
                readonly(unavailable_attestor_reports[1]),
                readonly(unavailable_attestor_reports[2]),
            ],
            data: tickersix::instruction::FinalizePricePhase { phase }.data(),
        }],
        &[],
    );
    assert!(no_quorum_attempt.is_err());

    let mark_unavailable = harness.submit_transaction(
        &coordinator,
        &[Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                writable(unavailable_asset),
                readonly(round),
                readonly(price_policy),
                readonly(attestor_set),
                writable_signer(coordinator_key),
                readonly(unavailable_attestor_reports[0]),
                readonly(unavailable_attestor_reports[1]),
                readonly(unavailable_attestor_reports[2]),
            ],
            data: tickersix::instruction::MarkPricePhaseUnavailable { phase }.data(),
        }],
        &[],
    );
    mark_unavailable.unwrap();
    let unavailable = harness.account::<tickersix::RoundAsset>(unavailable_asset);
    assert!(unavailable.start_unavailable);
    assert!(!unavailable.available);

    // Reusing the same report PDA is rejected before the handler can mutate
    // any account, even when a fresh valid native signature is supplied.
    let duplicate_attempt = harness.submit_transaction(
        &coordinator,
        &[
            ed25519_instruction(&message, &attestor_a),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: submit_accounts(key(&attestor_a), price_attestation),
                data: tickersix::instruction::SubmitPriceAttestation {
                    phase,
                    median_price_q9: price_q9,
                    accepted_observation_count: 1,
                    unique_source_block_count: 1,
                    first_source_block_id: 149,
                    last_source_block_id: 149,
                    evidence_root,
                    report_created_at: 150,
                }
                .data(),
            },
        ],
        &[],
    );
    assert!(duplicate_attempt.is_err());
}

#[test]
fn end_phase_requires_a_finalized_start_price() {
    // Regression target: an end-phase finalizer must not resolve a price or
    // report a generic quorum failure when the shared start price is absent.
    let mut harness = Harness::new();
    let (round, round_assets) = harness.create_and_freeze_round();
    let round_asset = round_assets[0];
    let price_policy = pda(&[tickersix::PRICE_POLICY_SEED, &1u16.to_le_bytes()]);
    let quality_policy = pda(&[tickersix::QUALITY_POLICY_SEED, &1u16.to_le_bytes()]);
    let attestor_set = pda(&[tickersix::ATTESTOR_SET_SEED, &1u16.to_le_bytes()]);
    let coordinator = Arc::clone(&harness.coordinator);
    let finalizer = key(&coordinator);

    harness.set_time(171, 171);
    harness.svm.expire_blockhash();
    let attempt = harness.submit_transaction(
        &coordinator,
        &[Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                writable(round_asset),
                readonly(round),
                readonly(price_policy),
                readonly(quality_policy),
                readonly(attestor_set),
                writable_signer(finalizer),
            ],
            data: tickersix::instruction::FinalizePricePhase {
                phase: tickersix::PricePhase::End,
            }
            .data(),
        }],
        &[],
    );

    let error = attempt.unwrap_err();
    assert!(
        error.contains("InvalidBattleState"),
        "unexpected error: {error}"
    );
    let asset = harness.account::<tickersix::RoundAsset>(round_asset);
    assert!(!asset.end_finalized);
    assert_eq!(asset.end_price_q9, 0);
}

#[test]
fn frozen_round_admits_one_exposure_and_progresses_clock_state() {
    let mut harness = Harness::new();
    let (round, round_assets) = harness.create_and_freeze_round();
    let player_a = Keypair::new();
    let player_b = Keypair::new();
    let player_c = Keypair::new();
    let player_d = Keypair::new();
    for player in [&player_a, &player_b, &player_c, &player_d] {
        harness
            .svm
            .airdrop(&player.pubkey(), 2_000_000_000)
            .unwrap();
    }

    // Regression target: freezing the round must close the multi-transaction
    // asset-assembly phase, even when the registry contains another valid
    // asset that could otherwise be appended.
    let extra_round_asset = pda(&[
        tickersix::ROUND_ASSET_SEED,
        round.as_ref(),
        &6u16.to_le_bytes(),
    ]);
    let extra_registry_entry = pda(&[
        tickersix::ASSET_SEED,
        &1u32.to_le_bytes(),
        &6u16.to_le_bytes(),
    ]);
    let mut extra_asset_accounts = vec![
        readonly(harness.config),
        writable(round),
        writable_signer(key(&harness.coordinator)),
        writable(extra_round_asset),
        readonly(extra_registry_entry),
        readonly(pda(&[tickersix::PRICE_POLICY_SEED, &1u16.to_le_bytes()])),
        readonly(pda(&[tickersix::QUALITY_POLICY_SEED, &1u16.to_le_bytes()])),
        system_program(),
    ];
    extra_asset_accounts.extend(round_assets.iter().copied().map(readonly));
    let extra_asset_attempt = harness.submit(
        Arc::clone(&harness.coordinator),
        Instruction {
            program_id: address(tickersix::ID),
            accounts: extra_asset_accounts,
            data: tickersix::instruction::AddRoundAsset {
                asset_id: 6,
                scoring_mint: Pubkey::new_from_array([106; 32]),
                issuer_kind: 1,
                price_source_kind: tickersix::PriceSourceKind::JupiterTokenSpotV1,
                price_policy_version: 1,
                market_quality_policy_version: 1,
            }
            .data(),
        },
        &[],
    );
    assert!(extra_asset_attempt.is_err());

    // Regression target: policy rotation must affect only future rounds. The
    // already-created round remains bound to its original immutable version.
    let rotated_price_policy = pda(&[tickersix::PRICE_POLICY_SEED, &2u16.to_le_bytes()]);
    harness
        .submit(
            Arc::clone(&harness.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![
                    writable(harness.config),
                    writable_signer(key(&harness.coordinator)),
                    writable(rotated_price_policy),
                    system_program(),
                ],
                data: tickersix::instruction::CreatePricePolicy {
                    version: 2,
                    source_kind: tickersix::PriceSourceKind::JupiterTokenSpotV1,
                    observation_window_secs: 10,
                    attestation_grace_secs: 10,
                    sample_interval_secs: 5,
                    max_attestor_spread_bps: 100,
                    min_accepted_observations: 1,
                    min_unique_source_blocks: 1,
                    max_source_block_lag: 100,
                }
                .data(),
            },
            &[],
        )
        .unwrap();
    assert_eq!(
        harness
            .account::<tickersix::MarketRound>(round)
            .price_policy_version,
        1
    );

    harness.set_time(110, 110);
    harness
        .submit(
            Arc::clone(&harness.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![writable(round), readonly_signer(key(&harness.coordinator))],
                data: tickersix::instruction::AdvanceMarketRound {}.data(),
            },
            &[],
        )
        .unwrap();
    let state = harness.account::<tickersix::MarketRound>(round).state;
    assert_eq!(state, tickersix::MarketRoundState::CommitOpen);

    let battle = pda(&[tickersix::BATTLE_SEED, round.as_ref(), &1u64.to_le_bytes()]);
    let slot_a = pda(&[
        tickersix::RATED_SLOT_SEED,
        round.as_ref(),
        key(&player_a).as_ref(),
    ]);
    let slot_b = pda(&[
        tickersix::RATED_SLOT_SEED,
        round.as_ref(),
        key(&player_b).as_ref(),
    ]);
    harness
        .submit(
            Arc::clone(&harness.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![
                    readonly(harness.config),
                    writable_signer(key(&harness.coordinator)),
                    writable(round),
                    readonly(key(&player_a)),
                    readonly(key(&player_b)),
                    readonly(tickersix::ID),
                    readonly(tickersix::ID),
                    readonly(tickersix::ID),
                    writable(battle),
                    writable(slot_a),
                    writable(slot_b),
                    system_program(),
                ],
                data: tickersix::instruction::CreateRatedBattle {
                    battle_id: 1,
                    mode: tickersix::BattleMode::Ranked,
                    league: Pubkey::default(),
                    league_round_no: 0,
                    rating_a_before: 1_500,
                    rating_b_before: 1_500,
                    rating_formula_version: 1,
                }
                .data(),
            },
            &[],
        )
        .unwrap();

    let battle_state = harness.account::<tickersix::Battle>(battle);
    assert_eq!(battle_state.a.player, key(&player_a));
    assert_eq!(battle_state.b.player, key(&player_b));
    assert!(harness.account::<tickersix::RatedSlot>(slot_a).battle == battle);
    assert!(harness.account::<tickersix::RatedSlot>(slot_b).battle == battle);

    let duplicate_battle = pda(&[tickersix::BATTLE_SEED, round.as_ref(), &2u64.to_le_bytes()]);
    let duplicate_slot_a = pda(&[
        tickersix::RATED_SLOT_SEED,
        round.as_ref(),
        key(&player_a).as_ref(),
    ]);
    let duplicate_slot_c = pda(&[
        tickersix::RATED_SLOT_SEED,
        round.as_ref(),
        key(&player_c).as_ref(),
    ]);
    let duplicate_attempt = harness.submit(
        Arc::clone(&harness.coordinator),
        Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                readonly(harness.config),
                writable_signer(key(&harness.coordinator)),
                writable(round),
                readonly(key(&player_a)),
                readonly(key(&player_c)),
                readonly(tickersix::ID),
                readonly(tickersix::ID),
                readonly(tickersix::ID),
                writable(duplicate_battle),
                writable(duplicate_slot_a),
                writable(duplicate_slot_c),
                system_program(),
            ],
            data: tickersix::instruction::CreateRatedBattle {
                battle_id: 2,
                mode: tickersix::BattleMode::Ranked,
                league: Pubkey::default(),
                league_round_no: 0,
                rating_a_before: 1_500,
                rating_b_before: 1_500,
                rating_formula_version: 1,
            }
            .data(),
        },
        &[],
    );
    assert!(duplicate_attempt.is_err());

    harness.set_time(121, 121);
    let late_battle = pda(&[tickersix::BATTLE_SEED, round.as_ref(), &3u64.to_le_bytes()]);
    let late_slot_c = pda(&[
        tickersix::RATED_SLOT_SEED,
        round.as_ref(),
        key(&player_c).as_ref(),
    ]);
    let late_slot_d = pda(&[
        tickersix::RATED_SLOT_SEED,
        round.as_ref(),
        key(&player_d).as_ref(),
    ]);
    let late_attempt = harness.submit(
        Arc::clone(&harness.coordinator),
        Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                readonly(harness.config),
                writable_signer(key(&harness.coordinator)),
                writable(round),
                readonly(key(&player_c)),
                readonly(key(&player_d)),
                readonly(tickersix::ID),
                readonly(tickersix::ID),
                readonly(tickersix::ID),
                writable(late_battle),
                writable(late_slot_c),
                writable(late_slot_d),
                system_program(),
            ],
            data: tickersix::instruction::CreateRatedBattle {
                battle_id: 3,
                mode: tickersix::BattleMode::Ranked,
                league: Pubkey::default(),
                league_round_no: 0,
                rating_a_before: 1_500,
                rating_b_before: 1_500,
                rating_formula_version: 1,
            }
            .data(),
        },
        &[],
    );
    assert!(late_attempt.is_err());
}

#[test]
fn official_league_battle_requires_active_members_in_the_current_round() {
    // Regression target: a coordinator must not be able to attach arbitrary
    // wallets to an official rated League Battle without matching member
    // accounts and the League's current round.
    let mut harness = Harness::new();
    let (round, _) = harness.create_and_freeze_round();
    let league = harness.create_official_league(1, 105);
    let player_a = Keypair::new();
    let player_b = Keypair::new();
    let player_c = Keypair::new();
    let player_d = Keypair::new();
    for player in [&player_a, &player_b, &player_c, &player_d] {
        harness
            .svm
            .airdrop(&player.pubkey(), 2_000_000_000)
            .unwrap();
    }
    harness.join_league(league, &player_a);
    harness.join_league(league, &player_b);

    // A second initialization for the same LeagueMember PDA must not be
    // treated as another membership or increment the League count twice.
    let duplicate_member_attempt = harness.submit(
        Arc::clone(&harness.coordinator),
        Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                readonly(harness.config),
                writable_signer(key(&player_a)),
                writable(league),
                writable(league_member_pda(league, &player_a)),
                system_program(),
            ],
            data: tickersix::instruction::JoinLeague {}.data(),
        },
        &[&player_a],
    );
    assert!(duplicate_member_attempt.is_err());

    // Capacity is enforced before a third member can consume a membership
    // account or alter the authoritative joined-player count.
    let full_member_attempt = harness.submit(
        Arc::clone(&harness.coordinator),
        Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                readonly(harness.config),
                writable_signer(key(&player_c)),
                writable(league),
                writable(league_member_pda(league, &player_c)),
                system_program(),
            ],
            data: tickersix::instruction::JoinLeague {}.data(),
        },
        &[&player_c],
    );
    assert!(full_member_attempt.is_err());

    harness.set_time(110, 110);
    harness
        .submit(
            Arc::clone(&harness.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![
                    readonly(harness.config),
                    readonly_signer(key(&harness.coordinator)),
                    writable(league),
                ],
                data: tickersix::instruction::ActivateLeague {}.data(),
            },
            &[],
        )
        .unwrap();
    harness
        .submit(
            Arc::clone(&harness.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![writable(round), readonly_signer(key(&harness.coordinator))],
                data: tickersix::instruction::AdvanceMarketRound {}.data(),
            },
            &[],
        )
        .unwrap();

    let member_a = league_member_pda(league, &player_a);
    let member_b = league_member_pda(league, &player_b);

    let invalid_battle = pda(&[tickersix::BATTLE_SEED, round.as_ref(), &1u64.to_le_bytes()]);
    let invalid_slot_c = pda(&[
        tickersix::RATED_SLOT_SEED,
        round.as_ref(),
        key(&player_c).as_ref(),
    ]);
    let invalid_slot_d = pda(&[
        tickersix::RATED_SLOT_SEED,
        round.as_ref(),
        key(&player_d).as_ref(),
    ]);
    let invalid_attempt = harness.submit(
        Arc::clone(&harness.coordinator),
        Instruction {
            program_id: address(tickersix::ID),
            accounts: vec![
                readonly(harness.config),
                writable_signer(key(&harness.coordinator)),
                writable(round),
                readonly(key(&player_c)),
                readonly(key(&player_d)),
                readonly(league),
                readonly(member_a),
                readonly(member_b),
                writable(invalid_battle),
                writable(invalid_slot_c),
                writable(invalid_slot_d),
                system_program(),
            ],
            data: tickersix::instruction::CreateRatedBattle {
                battle_id: 1,
                mode: tickersix::BattleMode::League,
                league,
                league_round_no: 1,
                rating_a_before: 1_500,
                rating_b_before: 1_500,
                rating_formula_version: 1,
            }
            .data(),
        },
        &[],
    );
    assert!(invalid_attempt.is_err());

    let battle = pda(&[tickersix::BATTLE_SEED, round.as_ref(), &2u64.to_le_bytes()]);
    let slot_a = pda(&[
        tickersix::RATED_SLOT_SEED,
        round.as_ref(),
        key(&player_a).as_ref(),
    ]);
    let slot_b = pda(&[
        tickersix::RATED_SLOT_SEED,
        round.as_ref(),
        key(&player_b).as_ref(),
    ]);
    harness
        .submit(
            Arc::clone(&harness.coordinator),
            Instruction {
                program_id: address(tickersix::ID),
                accounts: vec![
                    readonly(harness.config),
                    writable_signer(key(&harness.coordinator)),
                    writable(round),
                    readonly(key(&player_a)),
                    readonly(key(&player_b)),
                    readonly(league),
                    readonly(member_a),
                    readonly(member_b),
                    writable(battle),
                    writable(slot_a),
                    writable(slot_b),
                    system_program(),
                ],
                data: tickersix::instruction::CreateRatedBattle {
                    battle_id: 2,
                    mode: tickersix::BattleMode::League,
                    league,
                    league_round_no: 1,
                    rating_a_before: 1_500,
                    rating_b_before: 1_500,
                    rating_formula_version: 1,
                }
                .data(),
            },
            &[],
        )
        .unwrap();

    let battle_state = harness.account::<tickersix::Battle>(battle);
    let league_state = harness.account::<tickersix::League>(league);
    assert_eq!(battle_state.mode, tickersix::BattleMode::League);
    assert_eq!(battle_state.league, league);
    assert_eq!(battle_state.league_round_no, 1);
    assert_eq!(league_state.current_round, 1);
}
