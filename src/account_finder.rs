//! Clean-room ChessBase '27-inspired Lichess Team & Chess.com Club account
//! finder.
//!
//! Compares an opponent's known OTB/classical opening repertoire (expressed as
//! Zobrist position move distributions for White and Black between plies 2 and
//! 14) against sampled public games of candidates from a Lichess team or
//! Chess.com club.

use std::collections::{HashMap, HashSet};

use crate::chess::{Chess, Color, Position, SanPlus, UciMove};
use crate::domain::Game;
use crate::online::{OnlineSource, TeamMemberInfo};
use crate::players::player_key;
use crate::position::{encode_move, position_key};

const FINGERPRINT_MIN_PLY: usize = 2;
const FINGERPRINT_MAX_PLY: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountFinderConfidence {
    High,
    Medium,
    Low,
}

impl AccountFinderConfidence {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::High => "High",
            Self::Medium => "Med",
            Self::Low => "Low",
        }
    }
}

/// Position-indexed opening move frequencies extracted from the target player's
/// known OTB / classical games.
pub type ColorPositionMap = HashMap<u64, (u32, HashMap<u32, u32>)>;

#[derive(Clone, Debug)]
pub struct RepertoireFingerprint {
    pub target_key: String,
    pub target_elo: Option<u32>,
    pub total_games: usize,
    /// `by_color[0]` = White positions, `by_color[1]` = Black positions.
    /// Each entry maps `position_key` -> `(total_pos_games, map<encoded_move, count>)`.
    pub by_color: [ColorPositionMap; 2],
    /// Global move samples at shared plies (used for rarity / specificity bonus).
    pub distinct_lines: HashSet<(u64, u32)>,
}

impl RepertoireFingerprint {
    #[must_use]
    pub fn from_games(target_key: &str, target_elo: Option<u32>, games: &[Game]) -> Self {
        let mut by_color: [ColorPositionMap; 2] = [HashMap::new(), HashMap::new()];
        let mut distinct_lines = HashSet::new();
        let mut total_games = 0usize;

        for game in games {
            let w_key = player_key(game.white()).unwrap_or_default();
            let b_key = player_key(game.black()).unwrap_or_default();
            let as_white = w_key == target_key;
            let as_black = b_key == target_key;
            if !as_white && !as_black {
                continue;
            }
            total_games += 1;

            for (side, is_target) in [(Color::White, as_white), (Color::Black, as_black)] {
                if !is_target {
                    continue;
                }
                let c_idx = usize::from(side == Color::Black);
                let mut node_idx = 0usize;
                for ply_idx in 0..FINGERPRINT_MAX_PLY {
                    let node = &game.nodes[node_idx];
                    let Some(&next_idx) = node.children.first() else {
                        break;
                    };
                    if ply_idx >= FINGERPRINT_MIN_PLY
                        && node.pos.turn() == side
                        && let Some(ply) = &game.nodes[next_idx].mv
                        && ply.uci != "0000"
                        && let Ok(uci) = ply.uci.parse::<UciMove>()
                        && let Ok(mv) = uci.to_move(&node.pos)
                        && let Some(code) = encode_move(mv)
                    {
                        let pkey = position_key(&node.pos);
                        let (tot, moves) = by_color[c_idx].entry(pkey).or_default();
                        *tot += 1;
                        *moves.entry(code).or_insert(0) += 1;
                        distinct_lines.insert((pkey, code));
                    }
                    node_idx = next_idx;
                }
            }
        }

        Self {
            target_key: target_key.to_string(),
            target_elo,
            total_games,
            by_color,
            distinct_lines,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CandidateMatch {
    pub source: OnlineSource,
    pub username: String,
    pub rating: Option<u32>,
    pub games_sampled: usize,
    pub white_score_pct: f32,
    pub black_score_pct: f32,
    pub combined_score_pct: f32,
    pub matched_decisions: usize,
    pub total_decisions: usize,
    pub shared_rare_lines: Vec<String>,
    pub confidence: AccountFinderConfidence,
}

/// Rank team/club members before fetching games so plausible Elo / active members
/// are scanned first (e.g. closest rating band to target OTB Elo +200..+450 online boost).
#[must_use]
pub fn rank_team_members_for_scan(
    members: Vec<TeamMemberInfo>,
    target_elo: Option<u32>,
    cap: usize,
) -> Vec<TeamMemberInfo> {
    let mut ranked = members;
    let target_online = target_elo.map(|elo| elo.saturating_add(220));
    ranked.sort_by(|a, b| {
        let score_member = |m: &TeamMemberInfo| -> (u8, u32, i64) {
            let active_tier = if m.seen_recently { 0 } else { 1 };
            let dist = match (target_online, m.best_rating()) {
                (Some(t), Some(r)) => t.abs_diff(r),
                (Some(_), None) => 9_000,
                (None, Some(r)) => 4_000u32.saturating_sub(r),
                (None, None) => 8_000,
            };
            (active_tier, dist, -i64::from(m.best_rating().unwrap_or(0)))
        };
        score_member(a)
            .cmp(&score_member(b))
            .then_with(|| a.username.to_lowercase().cmp(&b.username.to_lowercase()))
    });
    if cap > 0 && ranked.len() > cap {
        ranked.truncate(cap);
    }
    ranked
}

/// Score one candidate account's sampled online games against `fp`.
#[must_use]
pub fn score_candidate(
    fp: &RepertoireFingerprint,
    member: &TeamMemberInfo,
    games: &[Game],
) -> CandidateMatch {
    let cand_key =
        player_key(&member.username).unwrap_or_else(|| member.username.trim().to_ascii_lowercase());

    let mut white_weight = 0.0f64;
    let mut white_total = 0.0f64;
    let mut black_weight = 0.0f64;
    let mut black_total = 0.0f64;
    let mut matched_decisions = 0usize;
    let mut total_decisions = 0usize;
    let mut line_hits: HashMap<String, usize> = HashMap::new();

    for game in games {
        let w_match = game.white().trim().eq_ignore_ascii_case(&member.username)
            || player_key(game.white()).as_deref() == Some(cand_key.as_str());
        let b_match = game.black().trim().eq_ignore_ascii_case(&member.username)
            || player_key(game.black()).as_deref() == Some(cand_key.as_str());
        if !w_match && !b_match {
            continue;
        }
        let sides: &[(Color, bool)] = &[(Color::White, w_match), (Color::Black, b_match)];
        for &(side, is_cand) in sides {
            if !is_cand {
                continue;
            }
            let c_idx = usize::from(side == Color::Black);
            let mut node_idx = 0usize;
            for ply_idx in 0..FINGERPRINT_MAX_PLY {
                let node = &game.nodes[node_idx];
                let Some(&next_idx) = node.children.first() else {
                    break;
                };
                if ply_idx >= FINGERPRINT_MIN_PLY
                    && node.pos.turn() == side
                    && let Some(ply) = &game.nodes[next_idx].mv
                    && ply.uci != "0000"
                    && let Ok(uci) = ply.uci.parse::<UciMove>()
                    && let Ok(mv) = uci.to_move(&node.pos)
                    && let Some(code) = encode_move(mv)
                {
                    let pkey = position_key(&node.pos);
                    if let Some((pos_tot, pos_moves)) = fp.by_color[c_idx].get(&pkey)
                        && *pos_tot > 0
                    {
                        total_decisions += 1;
                        let depth_bonus = 1.0 + (ply_idx as f64) * 0.18;
                        let specificity_bonus = if pos_moves.len() <= 2 { 1.4 } else { 1.0 };
                        let denom = depth_bonus * specificity_bonus;
                        let obs = pos_moves.get(&code).copied().unwrap_or(0);
                        let share = if obs > 0 {
                            matched_decisions += 1;
                            let ratio = f64::from(obs) / f64::from(*pos_tot);
                            // Credit agreement proportional to how consistently target plays it + floor for shared line.
                            let score = (0.45 + 0.55 * ratio) * denom;
                            if ply_idx >= 4 {
                                let label = format_line_snippet(game, next_idx, &node.pos, mv);
                                *line_hits.entry(label).or_insert(0) += 1;
                            }
                            score
                        } else {
                            0.0
                        };
                        if side == Color::White {
                            white_weight += share;
                            white_total += denom;
                        } else {
                            black_weight += share;
                            black_total += denom;
                        }
                    }
                }
                node_idx = next_idx;
            }
        }
    }

    let white_score_pct = if white_total > 0.0 {
        ((white_weight / white_total) * 100.0).clamp(0.0, 100.0) as f32
    } else {
        0.0
    };
    let black_score_pct = if black_total > 0.0 {
        ((black_weight / black_total) * 100.0).clamp(0.0, 100.0) as f32
    } else {
        0.0
    };
    let both_total = white_total + black_total;
    let combined_score_pct = if both_total > 0.0 {
        (((white_weight + black_weight) / both_total) * 100.0).clamp(0.0, 100.0) as f32
    } else {
        0.0
    };

    let mut shared_rare_lines: Vec<(String, usize)> = line_hits.into_iter().collect();
    shared_rare_lines.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let shared_rare_lines = shared_rare_lines
        .into_iter()
        .take(3)
        .map(|(s, _)| s)
        .collect();

    let confidence = if total_decisions >= 12 && combined_score_pct >= 62.0 {
        AccountFinderConfidence::High
    } else if total_decisions >= 6 && combined_score_pct >= 42.0 {
        AccountFinderConfidence::Medium
    } else {
        AccountFinderConfidence::Low
    };

    CandidateMatch {
        source: member.source,
        username: member.username.clone(),
        rating: member.best_rating(),
        games_sampled: games.len(),
        white_score_pct,
        black_score_pct,
        combined_score_pct,
        matched_decisions,
        total_decisions,
        shared_rare_lines,
        confidence,
    }
}

fn format_line_snippet(
    game: &Game,
    target_node: usize,
    before: &Chess,
    mv: crate::chess::Move,
) -> String {
    let mut seq = Vec::new();
    let mut cur = target_node;
    while seq.len() < 4 {
        if let Some(ply) = &game.nodes[cur].mv {
            let prefix = if ply.color == Color::White {
                format!("{}.{}", ply.number, ply.san)
            } else {
                format!("{}...{}", ply.number, ply.san)
            };
            seq.push(prefix);
        }
        let Some(p) = game.nodes[cur].parent else {
            break;
        };
        if p == 0 {
            break;
        }
        cur = p;
    }
    if seq.is_empty() {
        SanPlus::from_move(before.clone(), mv).to_string()
    } else {
        seq.reverse();
        seq.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::parse_pgn;

    #[test]
    fn repertoire_fingerprint_distinguishes_matching_secret_account_from_other_member() {
        // Target OTB player reliably plays London (1.d4 Nf6 2.Bf4 d5 3.e3 c5 4.c3) as White
        // and Caro-Kann (1.e4 c6 2.d4 d5 3.Nc3 dxe4 4.Nxe4 Bf5) as Black.
        let otb_pgn = b"\
[White \"Mueller, Lukas\"]
[Black \"Opp1\"]
[Result \"1-0\"]

1. d4 Nf6 2. Bf4 d5 3. e3 c5 4. c3 Nc6 5. Nd2 *

[White \"Mueller, Lukas\"]
[Black \"Opp2\"]
[Result \"1-0\"]

1. d4 d5 2. Bf4 Nf6 3. e3 e6 4. Nf3 Bd6 *

[White \"Opp3\"]
[Black \"Mueller, Lukas\"]
[Result \"0-1\"]

1. e4 c6 2. d4 d5 3. Nc3 dxe4 4. Nxe4 Bf5 *

[White \"Opp4\"]
[Black \"Mueller, Lukas\"]
[Result \"1/2-1/2\"]

1. e4 c6 2. d4 d5 3. Nd2 dxe4 4. Nxe4 Bf5 *
";
        let otb = parse_pgn(otb_pgn).unwrap();
        let target_key = player_key("Mueller, Lukas").unwrap();
        let fp = RepertoireFingerprint::from_games(&target_key, Some(2250), &otb);
        assert_eq!(fp.total_games, 4);

        // Candidate A: true secret online account playing London + Caro-Kann Bf5!
        let cand_a_pgn = b"\
[White \"lukas_secret\"]
[Black \"Rand1\"]
[Result \"1-0\"]

1. d4 Nf6 2. Bf4 d5 3. e3 c5 4. c3 *

[White \"Rand2\"]
[Black \"lukas_secret\"]
[Result \"0-1\"]

1. e4 c6 2. d4 d5 3. Nc3 dxe4 4. Nxe4 Bf5 *
";
        let cand_a_games = parse_pgn(cand_a_pgn).unwrap();
        let member_a = TeamMemberInfo {
            source: OnlineSource::Lichess,
            username: "lukas_secret".to_string(),
            blitz_rating: Some(2460),
            rapid_rating: Some(2490),
            classical_rating: None,
            seen_recently: true,
        };
        let match_a = score_candidate(&fp, &member_a, &cand_a_games);

        // Candidate B: unrelated player playing King's Gambit + Sicilian Najdorf!
        let cand_b_pgn = b"\
[White \"other_club_member\"]
[Black \"Rand1\"]
[Result \"1-0\"]

1. e4 e5 2. f4 exf4 3. Nf3 *

[White \"Rand2\"]
[Black \"other_club_member\"]
[Result \"0-1\"]

1. e4 c5 2. Nf3 d6 3. d4 cxd4 4. Nxd4 Nf6 5. Nc3 a6 *
";
        let cand_b_games = parse_pgn(cand_b_pgn).unwrap();
        let member_b = TeamMemberInfo {
            source: OnlineSource::Lichess,
            username: "other_club_member".to_string(),
            blitz_rating: Some(2440),
            rapid_rating: None,
            classical_rating: None,
            seen_recently: true,
        };
        let match_b = score_candidate(&fp, &member_b, &cand_b_games);

        assert!(
            match_a.combined_score_pct > 75.0,
            "matching secret account got {}",
            match_a.combined_score_pct
        );
        assert!(
            match_b.combined_score_pct < 25.0,
            "non-matching account got {}",
            match_b.combined_score_pct
        );
        assert!(match_a.combined_score_pct > match_b.combined_score_pct + 50.0);
        assert!(!match_a.shared_rare_lines.is_empty());
    }
}
