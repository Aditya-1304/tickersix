use protocol::{
    apply_elo_update, league_pairing_seed, league_standings_input_hash, pair_ranked, pair_swiss,
    EloOutcome, PairingPlayer, RankedPlayer,
};

fn player(id: u8, league_points: u32, rating: i32, byes: u16, opponents: &[u8]) -> PairingPlayer {
    PairingPlayer {
        wallet: [id; 32],
        league_points,
        rating,
        bye_count: byes,
        prior_opponents: opponents.iter().map(|value| [*value; 32]).collect(),
    }
}

#[test]
fn swiss_pairing_is_deterministic_complete_and_repeat_avoiding() {
    let players = vec![
        player(1, 6, 1500, 0, &[2]),
        player(2, 6, 1490, 0, &[1]),
        player(3, 3, 1600, 0, &[]),
        player(4, 3, 1590, 0, &[]),
        player(5, 0, 1400, 0, &[]),
        player(6, 0, 1410, 0, &[]),
    ];

    let first = pair_swiss(&players, [7; 32]).unwrap();
    let second = pair_swiss(&players, [7; 32]).unwrap();
    assert_eq!(first, second);
    assert!(first.bye.is_none());
    assert_eq!(first.pairs.len(), 3);

    let mut seen = Vec::new();
    for (left, right) in first.pairs {
        assert_ne!(left, right);
        assert!(!players[left]
            .prior_opponents
            .contains(&players[right].wallet));
        seen.extend([left, right]);
    }
    seen.sort_unstable();
    assert_eq!(seen, (0..players.len()).collect::<Vec<_>>());
}

#[test]
fn odd_population_assigns_at_most_one_new_bye_to_an_eligible_low_score_player() {
    let players = vec![
        player(1, 9, 1500, 0, &[]),
        player(2, 6, 1500, 0, &[]),
        player(3, 3, 1500, 1, &[]),
        player(4, 0, 1500, 0, &[]),
        player(5, 0, 1500, 0, &[]),
    ];

    let result = pair_swiss(&players, [8; 32]).unwrap();
    assert_eq!(result.pairs.len(), 2);
    assert!(result.bye == Some(3) || result.bye == Some(4));
    assert_ne!(result.bye, Some(2));
}

#[test]
fn bye_policy_uses_another_score_group_before_repeating_a_bye() {
    let players = vec![
        player(1, 3, 1500, 1, &[]),
        player(2, 0, 1500, 1, &[]),
        player(3, 3, 1500, 0, &[]),
    ];

    let result = pair_swiss(&players, [9; 32]).unwrap();
    assert_eq!(result.bye, Some(2));
}

#[test]
fn elo_uses_the_player_specific_k_factor_and_rating_floor() {
    let established_loss = apply_elo_update(101, 100, EloOutcome::Loss, 30).unwrap();
    assert_eq!(established_loss.k_factor, 24);
    assert_eq!(established_loss.rating_after, 100);

    let placement_win = apply_elo_update(1500, 1500, EloOutcome::Win, 0).unwrap();
    assert_eq!(placement_win.k_factor, 64);
    assert_eq!(placement_win.rating_after, 1532);
    assert_eq!(placement_win.actual_score, 1.0);
}

#[test]
fn ranked_pairing_prefers_nearest_rating_and_avoids_a_recent_rematch() {
    let players = vec![
        RankedPlayer {
            wallet: [1; 32],
            rating: 1500,
            recent_opponents: vec![[2; 32]],
        },
        RankedPlayer {
            wallet: [2; 32],
            rating: 1501,
            recent_opponents: vec![[1; 32]],
        },
        RankedPlayer {
            wallet: [3; 32],
            rating: 1502,
            recent_opponents: vec![],
        },
        RankedPlayer {
            wallet: [4; 32],
            rating: 1503,
            recent_opponents: vec![],
        },
    ];

    let result = pair_ranked(&players);

    assert_eq!(result.unmatched, None);
    assert_eq!(result.pairs, vec![(0, 2), (1, 3)]);
}

#[test]
fn ranked_pairing_is_deterministic_and_leaves_odd_player_unmatched() {
    let players = vec![
        RankedPlayer {
            wallet: [9; 32],
            rating: 1700,
            recent_opponents: vec![],
        },
        RankedPlayer {
            wallet: [8; 32],
            rating: 1500,
            recent_opponents: vec![],
        },
        RankedPlayer {
            wallet: [7; 32],
            rating: 1501,
            recent_opponents: vec![],
        },
    ];

    let first = pair_ranked(&players);
    let second = pair_ranked(&players);

    assert_eq!(first, second);
    assert_eq!(first.pairs, vec![(2, 1)]);
    assert_eq!(first.unmatched, Some(0));
}

#[test]
fn league_pairing_seed_binds_domain_league_round_and_chain_entropy() {
    let seed = league_pairing_seed([1; 32], 7, [2; 32]);

    assert_eq!(
        hex::encode(seed),
        "9a516936c220652c5d1ebc63f8ae276fda7fa4173b771777dc29ce1f8cdb1cc4"
    );
    assert_ne!(seed, league_pairing_seed([1; 32], 8, [2; 32]));
    assert_ne!(seed, league_pairing_seed([1; 32], 7, [3; 32]));
}

#[test]
fn league_standings_input_hash_is_canonical_and_order_independent() {
    let first = vec![player(2, 3, 1700, 1, &[1, 9]), player(1, 6, 1500, 0, &[2])];
    let second = vec![player(1, 6, 1900, 0, &[2]), player(2, 3, 1200, 1, &[9, 1])];

    let first_hash = league_standings_input_hash(&first);
    assert_eq!(first_hash, league_standings_input_hash(&second));
    assert_eq!(
        hex::encode(first_hash),
        "8e8dfd281f6622000963829a77cad38650db821db8b4f58bd67b0bb22dda0288"
    );
}
