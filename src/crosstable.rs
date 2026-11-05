//! Format-agnostic tournament crosstable engine.
//!
//! Builds a [`Crosstable`] from a flat list of [`TournamentGame`]s.
//! Classification:
//! - exactly 2 players and at least 2 games => [`CrosstableKind::Match`]
//! - at most 24 distinct players => [`CrosstableKind::RoundRobin`]
//! - more than 24 distinct players => [`CrosstableKind::Swiss`]

use std::collections::{BTreeSet, HashMap};

/// One game belonging to a tournament.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TournamentGame {
    pub game_index: usize,
    pub white: String,
    pub black: String,
    pub white_title: Option<String>,
    pub black_title: Option<String>,
    pub white_elo: Option<u32>,
    pub black_elo: Option<u32>,
    pub result: String,
    pub round: String,
    pub date: String,
}

/// One scored result from the perspective of the row player.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellResult {
    pub game_index: usize,
    pub half_points: u8,
    pub is_white: bool,
    pub round: String,
}

/// A single game in a two-player match, kept in round/date order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchGameEntry {
    pub game_index: usize,
    pub white: String,
    pub black: String,
    pub result: String,
    pub round: String,
    pub date: String,
}

/// Two-player match table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchTable {
    pub players: [String; 2],
    pub entries: Vec<MatchGameEntry>,
}

/// All-play-all table. All vectors are aligned with `players` order
/// (sorted by points -> SB -> direct encounter -> elo).
#[derive(Clone, Debug, PartialEq)]
pub struct RoundRobinTable {
    pub players: Vec<String>,
    pub elos: Vec<Option<u32>>,
    /// `matrix[i][j]` holds row-player-`i` results vs `players[j]`.
    pub matrix: Vec<Vec<Vec<CellResult>>>,
    /// Total points in full points (e.g. 2.5).
    pub points: Vec<f32>,
    /// Sonneborn-Berger scores.
    pub sb: Vec<f32>,
}

/// One ranked Swiss row.
#[derive(Clone, Debug, PartialEq)]
pub struct SwissRow {
    pub player: String,
    pub elo: Option<u32>,
    pub wins: u32,
    pub draws: u32,
    pub losses: u32,
    pub points: f32,
    pub sb: f32,
    pub tpr: Option<u32>,
}

/// Swiss standings, sorted by points -> SB -> direct encounter -> elo.
#[derive(Clone, Debug, PartialEq)]
pub struct SwissTable {
    pub rows: Vec<SwissRow>,
}

/// Which shape the tournament takes.
#[derive(Clone, Debug, PartialEq)]
pub enum CrosstableKind {
    Match(MatchTable),
    RoundRobin(RoundRobinTable),
    Swiss(SwissTable),
}

/// Tournament crosstable.
#[derive(Clone, Debug, PartialEq)]
pub struct Crosstable {
    pub event: String,
    pub site: String,
    pub year: Option<u32>,
    pub games: usize,
    pub average_elo: Option<u32>,
    pub category: Option<u32>,
    pub kind: CrosstableKind,
}

/// Score a result string as `(white_half_points, black_half_points)`.
fn score_result(result: &str) -> Option<(u8, u8)> {
    match result.trim() {
        "1-0" => Some((2, 0)),
        "0-1" => Some((0, 2)),
        "1/2-1/2" => Some((1, 1)),
        _ => None,
    }
}

fn round_key(round: &str) -> (u8, String) {
    match round.trim().parse::<f32>() {
        Ok(v) => (0, format!("{v:013.5}")),
        Err(_) => (1, round.to_string()),
    }
}

fn sorted_games(games: &[TournamentGame]) -> Vec<&TournamentGame> {
    let mut v: Vec<&TournamentGame> = games.iter().collect();
    v.sort_by(|a, b| {
        round_key(&a.round)
            .cmp(&round_key(&b.round))
            .then_with(|| a.date.cmp(&b.date))
            .then_with(|| a.game_index.cmp(&b.game_index))
    });
    v
}

fn player_elos(games: &[TournamentGame]) -> HashMap<String, Option<u32>> {
    let mut map: HashMap<String, Option<u32>> = HashMap::new();
    for g in games {
        map.entry(g.white.clone()).or_insert(g.white_elo);
        map.entry(g.black.clone()).or_insert(g.black_elo);
        if map.get(g.white.as_str()) == Some(&None) && g.white_elo.is_some() {
            map.insert(g.white.clone(), g.white_elo);
        }
        if map.get(g.black.as_str()) == Some(&None) && g.black_elo.is_some() {
            map.insert(g.black.clone(), g.black_elo);
        }
    }
    map
}

fn compute_average_elo(elo_map: &HashMap<String, Option<u32>>) -> Option<u32> {
    let elos: Vec<u32> = elo_map.values().filter_map(|e| *e).collect();
    if elos.is_empty() {
        return None;
    }
    let sum: u64 = elos.iter().map(|e| u64::from(*e)).sum();
    Some((sum as f64 / elos.len() as f64).round() as u32)
}

fn category_for(avg: Option<u32>) -> Option<u32> {
    match avg {
        Some(a) if a >= 2251 => Some(1 + (a - 2251) / 25),
        _ => None,
    }
}

/// Performance rating via inverted logistic curve, clamped at the ends.
fn performance_rating(avg_opp: f64, score: f64, played: usize) -> Option<u32> {
    if played == 0 {
        return None;
    }
    let p = (score / played as f64).clamp(0.01, 0.99);
    let dp = -400.0 * (1.0 / p - 1.0).log10();
    Some((avg_opp + dp).round().clamp(0.0, 4000.0) as u32)
}

/// Build a crosstable from a flat game list.
#[allow(clippy::too_many_lines)]
pub fn build_crosstable(
    event: &str,
    site: &str,
    year: Option<u32>,
    games: &[TournamentGame],
) -> Crosstable {
    let mut names = BTreeSet::new();
    for g in games {
        names.insert(g.white.clone());
        names.insert(g.black.clone());
    }
    let elo_map = player_elos(games);
    let average_elo = compute_average_elo(&elo_map);
    let category = category_for(average_elo);

    let base = Crosstable {
        event: event.to_string(),
        site: site.to_string(),
        year,
        games: games.len(),
        average_elo,
        category,
        kind: CrosstableKind::RoundRobin(RoundRobinTable {
            players: Vec::new(),
            elos: Vec::new(),
            matrix: Vec::new(),
            points: Vec::new(),
            sb: Vec::new(),
        }),
    };

    if names.len() == 2 && games.len() >= 2 {
        let ordered: Vec<String> = names.into_iter().collect();
        let entries = sorted_games(games)
            .into_iter()
            .map(|g| MatchGameEntry {
                game_index: g.game_index,
                white: g.white.clone(),
                black: g.black.clone(),
                result: g.result.clone(),
                round: g.round.clone(),
                date: g.date.clone(),
            })
            .collect();
        return Crosstable {
            kind: CrosstableKind::Match(MatchTable {
                players: [ordered[0].clone(), ordered[1].clone()],
                entries,
            }),
            ..base
        };
    }

    if names.len() <= 24 {
        return Crosstable {
            kind: CrosstableKind::RoundRobin(build_round_robin(games, &elo_map)),
            ..base
        };
    }

    Crosstable {
        kind: CrosstableKind::Swiss(build_swiss(games, &elo_map)),
        ..base
    }
}

fn build_round_robin(
    games: &[TournamentGame],
    elo_map: &HashMap<String, Option<u32>>,
) -> RoundRobinTable {
    let mut names: Vec<String> = {
        let mut s = BTreeSet::new();
        for g in games {
            s.insert(g.white.clone());
            s.insert(g.black.clone());
        }
        s.into_iter().collect()
    };
    names.sort();
    let n = names.len();
    let idx: HashMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, p)| (p.as_str(), i))
        .collect();

    // half-points per player and points-vs-opponent (full points).
    let mut halves = vec![0u32; n];
    let mut vs: HashMap<(usize, usize), f32> = HashMap::new();
    let mut matrix: Vec<Vec<Vec<CellResult>>> = vec![vec![Vec::new(); n]; n];

    for g in games {
        let Some((w, b)) = score_result(&g.result) else {
            continue;
        };
        let wi = idx[g.white.as_str()];
        let bi = idx[g.black.as_str()];
        halves[wi] += u32::from(w);
        halves[bi] += u32::from(b);
        *vs.entry((wi, bi)).or_insert(0.0) += f32::from(w) / 2.0;
        *vs.entry((bi, wi)).or_insert(0.0) += f32::from(b) / 2.0;
        matrix[wi][bi].push(CellResult {
            game_index: g.game_index,
            half_points: w,
            is_white: true,
            round: g.round.clone(),
        });
        matrix[bi][wi].push(CellResult {
            game_index: g.game_index,
            half_points: b,
            is_white: false,
            round: g.round.clone(),
        });
    }

    let totals: Vec<f32> = halves.iter().map(|h| *h as f32 / 2.0).collect();
    let mut sb = vec![0.0f32; n];
    for (i, sb_i) in sb.iter_mut().enumerate() {
        let mut s = 0.0;
        for (j, total_j) in totals.iter().enumerate() {
            if i == j {
                continue;
            }
            s += vs.get(&(i, j)).copied().unwrap_or(0.0) * *total_j;
        }
        *sb_i = s;
    }

    let elo_of = |i: usize| -> u32 { elo_map.get(&names[i]).copied().flatten().unwrap_or(0) };
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        totals[b]
            .partial_cmp(&totals[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                sb[b]
                    .partial_cmp(&sb[a])
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                let da = vs.get(&(a, b)).copied().unwrap_or(0.0);
                let db = vs.get(&(b, a)).copied().unwrap_or(0.0);
                db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| elo_of(b).cmp(&elo_of(a)))
            .then_with(|| names[a].cmp(&names[b]))
    });

    let permute = |v: &[f32]| order.iter().map(|&i| v[i]).collect::<Vec<_>>();
    let points = permute(&totals);
    let sb_sorted = permute(&sb);
    let players = order.iter().map(|&i| names[i].clone()).collect::<Vec<_>>();
    let elos = order
        .iter()
        .map(|&i| elo_map.get(&names[i]).copied().flatten())
        .collect::<Vec<_>>();
    let mut m2: Vec<Vec<Vec<CellResult>>> = vec![vec![Vec::new(); n]; n];
    for (ni, &oi) in order.iter().enumerate() {
        for (nj, &oj) in order.iter().enumerate() {
            m2[ni][nj] = matrix[oi][oj].clone();
        }
    }
    RoundRobinTable {
        players,
        elos,
        matrix: m2,
        points,
        sb: sb_sorted,
    }
}

fn build_swiss(games: &[TournamentGame], elo_map: &HashMap<String, Option<u32>>) -> SwissTable {
    #[derive(Default)]
    struct Acc {
        wins: u32,
        draws: u32,
        losses: u32,
        halves: u32,
        // (opponent name, half points scored in that game)
        scored: Vec<(String, u8)>,
    }
    let mut acc: HashMap<String, Acc> = HashMap::new();
    for g in games {
        acc.entry(g.white.clone()).or_default();
        acc.entry(g.black.clone()).or_default();
    }
    for g in games {
        let Some((w, b)) = score_result(&g.result) else {
            continue;
        };
        {
            let a = acc.get_mut(g.white.as_str()).expect("white entry");
            a.halves += u32::from(w);
            match w {
                2 => a.wins += 1,
                1 => a.draws += 1,
                _ => a.losses += 1,
            }
            a.scored.push((g.black.clone(), w));
        }
        {
            let a = acc.get_mut(g.black.as_str()).expect("black entry");
            a.halves += u32::from(b);
            match b {
                2 => a.wins += 1,
                1 => a.draws += 1,
                _ => a.losses += 1,
            }
            a.scored.push((g.white.clone(), b));
        }
    }

    let totals: HashMap<String, f32> = acc
        .iter()
        .map(|(p, a)| (p.clone(), a.halves as f32 / 2.0))
        .collect();
    // head-to-head helper
    let mut vs: HashMap<(String, String), f32> = HashMap::new();
    for (p, a) in &acc {
        for (opp, h) in &a.scored {
            *vs.entry((p.clone(), opp.clone())).or_insert(0.0) += f32::from(*h) / 2.0;
        }
    }

    let mut rows: Vec<SwissRow> = acc
        .iter()
        .map(|(p, a)| {
            let points = a.halves as f32 / 2.0;
            let sb = a
                .scored
                .iter()
                .map(|(opp, h)| f32::from(*h) / 2.0 * totals.get(opp).copied().unwrap_or(0.0))
                .sum();
            let opp_elos: Vec<u32> = a
                .scored
                .iter()
                .filter_map(|(opp, _)| elo_map.get(opp).copied().flatten())
                .collect();
            let tpr = if opp_elos.is_empty() {
                None
            } else {
                let avg =
                    opp_elos.iter().map(|e| f64::from(*e)).sum::<f64>() / opp_elos.len() as f64;
                performance_rating(avg, f64::from(points), a.scored.len())
            };
            SwissRow {
                player: p.clone(),
                elo: elo_map.get(p).copied().flatten(),
                wins: a.wins,
                draws: a.draws,
                losses: a.losses,
                points,
                sb,
                tpr,
            }
        })
        .collect();

    rows.sort_by(|a, b| {
        b.points
            .partial_cmp(&a.points)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.sb.partial_cmp(&a.sb).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| {
                let da = vs
                    .get(&(a.player.clone(), b.player.clone()))
                    .copied()
                    .unwrap_or(0.0);
                let db = vs
                    .get(&(b.player.clone(), a.player.clone()))
                    .copied()
                    .unwrap_or(0.0);
                db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| b.elo.unwrap_or(0).cmp(&a.elo.unwrap_or(0)))
            .then_with(|| a.player.cmp(&b.player))
    });
    SwissTable { rows }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(
        game_index: usize,
        white: &str,
        black: &str,
        white_elo: Option<u32>,
        black_elo: Option<u32>,
        result: &str,
        round: &str,
    ) -> TournamentGame {
        TournamentGame {
            game_index,
            white: white.to_string(),
            black: black.to_string(),
            white_title: None,
            black_title: None,
            white_elo,
            black_elo,
            result: result.to_string(),
            round: round.to_string(),
            date: "2024.11.01".to_string(),
        }
    }

    #[test]
    fn builds_match_table_for_two_player_world_championship() {
        let games = vec![
            game(
                0,
                "Ding Liren",
                "Gukesh D",
                Some(2728),
                Some(2783),
                "1/2-1/2",
                "1",
            ),
            game(
                1,
                "Gukesh D",
                "Ding Liren",
                Some(2783),
                Some(2728),
                "1-0",
                "2",
            ),
            game(
                2,
                "Ding Liren",
                "Gukesh D",
                Some(2728),
                Some(2783),
                "1-0",
                "3",
            ),
            game(
                3,
                "Gukesh D",
                "Ding Liren",
                Some(2783),
                Some(2728),
                "0-1",
                "4",
            ),
        ];
        let ct = build_crosstable("World Championship", "Singapore", Some(2024), &games);
        assert_eq!(ct.games, 4);
        let CrosstableKind::Match(m) = &ct.kind else {
            panic!("expected Match, got {:?}", ct.kind);
        };
        assert_eq!(m.players.len(), 2);
        assert!(m.players.contains(&"Ding Liren".to_string()));
        assert!(m.players.contains(&"Gukesh D".to_string()));
        // round/date order preserved
        let rounds: Vec<&str> = m.entries.iter().map(|e| e.round.as_str()).collect();
        assert_eq!(rounds, vec!["1", "2", "3", "4"]);
        // game_index click-through: entry order maps back to source games
        for e in &m.entries {
            let src = &games[e.game_index];
            assert_eq!(src.white, e.white);
            assert_eq!(src.black, e.black);
            assert_eq!(src.result, e.result);
        }
    }

    #[test]
    fn builds_all_play_all_crosstable_with_sonneborn_berger_tiebreak() {
        let games = vec![
            game(0, "Carlsen", "Nepo", Some(2830), Some(2758), "1-0", "1"),
            game(1, "Ding", "Gukesh", Some(2728), Some(2783), "1-0", "1"),
            game(2, "Carlsen", "Ding", Some(2830), Some(2728), "1/2-1/2", "2"),
            game(3, "Nepo", "Gukesh", Some(2758), Some(2783), "1-0", "2"),
            game(
                4,
                "Carlsen",
                "Gukesh",
                Some(2830),
                Some(2783),
                "1/2-1/2",
                "3",
            ),
            game(5, "Nepo", "Ding", Some(2758), Some(2728), "1/2-1/2", "3"),
        ];
        let ct = build_crosstable("Candidates Demo", "Madrid", Some(2024), &games);
        assert_eq!(ct.games, 6);
        let CrosstableKind::RoundRobin(rr) = &ct.kind else {
            panic!("expected RoundRobin, got {:?}", ct.kind);
        };
        // Carlsen and Ding both on 2.0; Carlsen wins on SB.
        assert_eq!(rr.players[0], "Carlsen");
        assert_eq!(rr.players[1], "Ding");
        assert!((rr.points[0] - 2.0).abs() < 1e-6);
        assert!((rr.points[1] - 2.0).abs() < 1e-6);
        assert!(rr.sb[0] > rr.sb[1]);
        // matrix dimensions and diagonal empty
        assert_eq!(rr.matrix.len(), 4);
        for (i, row) in rr.matrix.iter().enumerate() {
            assert_eq!(row.len(), 4);
            assert!(row[i].is_empty());
        }
        // Carlsen row vs Ding cell exists with a draw as white-or-black
        let ci = rr.players.iter().position(|p| p == "Carlsen").unwrap();
        let di = rr.players.iter().position(|p| p == "Ding").unwrap();
        assert_eq!(rr.matrix[ci][di].len(), 1);
        assert_eq!(rr.matrix[ci][di][0].half_points, 1);
        // category: avg of 2830,2758,2728,2783 = 2774.75 -> 2775
        // 1 + floor((2775-2251)/25) = 1 + 20 = 21
        assert_eq!(ct.average_elo, Some(2775));
        assert_eq!(ct.category, Some(21));
    }

    #[test]
    fn builds_swiss_standings_when_players_exceed_24() {
        let mut games = Vec::new();
        for i in 0..13 {
            let w = format!("Player{i:02}");
            let b = format!("Player{:02}", i + 13);
            games.push(game(
                i,
                &w,
                &b,
                Some(2000 + i as u32 * 10),
                Some(2000 + (i + 13) as u32 * 10),
                "1-0",
                "1",
            ));
        }
        let ct = build_crosstable("Open", "Nowhere", Some(2024), &games);
        let CrosstableKind::Swiss(swiss) = &ct.kind else {
            panic!("expected Swiss, got {:?}", ct.kind);
        };
        assert_eq!(swiss.rows.len(), 26);
        // winners sorted first
        assert!((swiss.rows[0].points - 1.0).abs() < 1e-6);
        assert_eq!(swiss.rows[0].wins, 1);
        assert_eq!(swiss.rows[0].draws, 0);
        assert_eq!(swiss.rows[0].losses, 0);
        // losers at the bottom
        let last = swiss.rows.last().unwrap();
        assert!((last.points - 0.0).abs() < 1e-6);
        assert_eq!(last.losses, 1);
        // TPR present for played games (winners beat rated opposition)
        assert!(swiss.rows[0].tpr.is_some());
        // points + SB present
        assert!(swiss.rows[0].sb >= 0.0);
    }
}
