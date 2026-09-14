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
        let mut signers = Vec::with_capacity(additional_signers.len() + 1);
        signers.push(fee_payer.as_ref());
        signers.extend_from_slice(additional_signers);
        let message = Message::new_with_blockhash(
            &[instruction],
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

    fn configure_policies_and_registry(&mut self) -> Vec<Pubkey> {
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
                    attestors: [
                        Pubkey::new_from_array([11; 32]),
                        Pubkey::new_from_array([12; 32]),
                        Pubkey::new_from_array([13; 32]),
                    ],
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
        let mints = self.configure_policies_and_registry();
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
