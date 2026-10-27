//! In-memory opening-tree and move-trend statistics for recently downloaded
//! games.
//!
//! The on-disk [`crate::position::PositionIndex`] is the right tool for large
//! databases, but online references only contain a few hundred games. Building
//! a cache index for them would add latency and leave stale files behind, so
//! this module aggregates the games in memory instead. The resulting tree can
//! be rendered with the same columns as the normal opening tree.
//!
//! Trends compare a recent sample with a baseline distribution. The recent
//! sample is usually a ChessABC download; the baseline is the prepared player's
//! historical repertoire or the local reference tree. A positive delta means a
//! move is taking a larger share of recent games than it did historically.

use std::collections::{BTreeMap, HashMap, HashSet};

use shakmaty::uci::UciMove;

use crate::domain::Game;
use crate::players;
use crate::position::{self, MoveStat, PositionStats};

/// A compact position tree built from games that are already in memory.
#[derive(Clone, Debug, Default)]
pub struct RecentTree {
    positions: HashMap<u64, BTreeMap<u32, RecentMove>>,
    games: usize,
    as_of: u32,
    recent_days: u32,
}

#[derive(Clone, Copy, Debug, Default)]
struct RecentMove {
    games: u32,
    white: u32,
    draw: u32,
    black: u32,
    elo_sum: u64,
    elo_count: u32,
    first_date: u32,
    last_date: u32,
    recent_games: u32,
    older_games: u32,
}

impl RecentMove {
    fn add(&mut self, result: &str, elo: Option<u32>, date: u32, recent: bool) {
        self.games += 1;
        match result {
            "1-0" => self.white += 1,
            "0-1" => self.black += 1,
            "1/2-1/2" => self.draw += 1,
            _ => {}
        }
        if let Some(elo) = elo {
            self.elo_sum += u64::from(elo);
            self.elo_count += 1;
        }
        if date != 0 {
            if self.first_date == 0 || date < self.first_date {
                self.first_date = date;
            }
            self.last_date = self.last_date.max(date);
        }
        if recent {
            self.recent_games += 1;
        } else {
            self.older_games += 1;
        }
    }

    fn stat(&self, key: u64, mv: u32) -> MoveStat {
        MoveStat {
            key,
            mv,
            games: self.games,
            white: self.white,
            draw: self.draw,
            black: self.black,
            elo_sum: self.elo_sum,
            elo_count: self.elo_count,
            first_date: self.first_date,
            last_date: self.last_date,
        }
    }
}

impl RecentTree {
    /// Build a tree from `games`. Games dated within `recent_days` of `as_of`
    /// are counted as recent; the split is retained for callers that want to
    /// show a within-sample trend.
    pub fn from_games(games: &[Game], as_of: u32, recent_days: u32) -> Self {
        let mut tree = Self {
            positions: HashMap::new(),
            games: 0,
            as_of,
            recent_days,
        };
        for game in games {
            tree.add_game(game);
        }
        tree
    }

    pub fn games(&self) -> usize {
        self.games
    }

    pub fn as_of(&self) -> u32 {
        self.as_of
    }

    pub fn recent_days(&self) -> u32 {
        self.recent_days
    }

    fn add_game(&mut self, game: &Game) {
        let date = players::parse_date(game.date()).unwrap_or(0);
        let recent = players::date_to_days(date)
            .zip(players::date_to_days(self.as_of))
            .is_some_and(|(date, as_of)| as_of.saturating_sub(date) <= i64::from(self.recent_days));
        let elo = average_elo(game);
        let result = game.result();
        let mut seen = HashSet::new();
        let mut node = 0usize;

        loop {
            let parent_pos = &game.nodes[node].pos;
            let key = position::position_key(parent_pos);
            if !seen.insert(key) {
                break;
            }
            let Some(&child) = game.nodes[node].children.first() else {
                break;
            };
            let Some(ply) = &game.nodes[child].mv else {
                break;
            };
            if ply.uci == "0000" {
                break;
            }
            let Ok(uci) = ply.uci.parse::<UciMove>() else {
                break;
            };
            let Ok(mv) = uci.to_move(parent_pos) else {
                break;
            };
            let Some(code) = position::encode_move(mv) else {
                break;
            };
            self.positions
                .entry(key)
                .or_default()
                .entry(code)
                .or_default()
                .add(result, elo, date, recent);
            node = child;
        }
        self.games += 1;
    }

    /// Moves from `key`, sorted by frequency (then move code).
    pub fn moves(&self, key: u64) -> Vec<MoveStat> {
        let mut moves: Vec<_> = self
            .positions
            .get(&key)
            .map(|moves| moves.iter().map(|(&mv, stat)| stat.stat(key, mv)).collect())
            .unwrap_or_default();
        moves.sort_by(|a, b| b.games.cmp(&a.games).then(a.mv.cmp(&b.mv)));
        moves
    }

    /// Aggregated statistics for a position, or `None` when no game moved from
    /// it.
    pub fn stats(&self, key: u64) -> Option<PositionStats> {
        let moves = self.positions.get(&key)?;
        let mut stats = PositionStats::default();
        for mv in moves.values() {
            stats.games += mv.games;
            stats.white += mv.white;
            stats.draw += mv.draw;
            stats.black += mv.black;
            stats.elo_sum += mv.elo_sum;
            stats.elo_count += mv.elo_count;
            if mv.first_date != 0 && (stats.first_date == 0 || mv.first_date < stats.first_date) {
                stats.first_date = mv.first_date;
            }
            stats.last_date = stats.last_date.max(mv.last_date);
        }
        Some(stats)
    }

    /// Number of games that reached `key` and then made a move.
    pub fn position_games(&self, key: u64) -> u32 {
        self.positions
            .get(&key)
            .map(|moves| moves.values().map(|mv| mv.games).sum())
            .unwrap_or(0)
    }

    /// Number of moves at `key` that fall in the recent window.
    pub fn recent_move_games(&self, key: u64) -> u32 {
        self.positions
            .get(&key)
            .map(|moves| moves.values().map(|mv| mv.recent_games).sum())
            .unwrap_or(0)
    }

    /// Number of moves at `key` outside the recent window.
    pub fn older_move_games(&self, key: u64) -> u32 {
        self.positions
            .get(&key)
            .map(|moves| moves.values().map(|mv| mv.older_games).sum())
            .unwrap_or(0)
    }
}

/// One move's recent share compared with a baseline distribution.
#[derive(Clone, Debug, PartialEq)]
pub struct TrendRow {
    /// Recent move statistics (games, score, Elo, dates).
    pub stat: MoveStat,
    /// Games in the recent sample that played this move.
    pub recent_games: u32,
    /// Total recent games that moved from the position.
    pub recent_total: u32,
    /// Share of the recent sample, 0.0–1.0.
    pub recent_share: f64,
    /// Games in the baseline that played this move.
    pub baseline_games: u32,
    /// Total baseline games that moved from the position.
    pub baseline_total: u32,
    /// Baseline share, when the baseline has data.
    pub baseline_share: Option<f64>,
    /// Recent share minus baseline share, in percentage points.
    pub delta_pp: Option<f64>,
}

impl TrendRow {
    /// A short label for the direction of the trend.
    pub fn direction(&self) -> &'static str {
        match self.delta_pp {
            Some(delta) if delta >= 3.0 => "rising",
            Some(delta) if delta <= -3.0 => "falling",
            _ => "steady",
        }
    }
}

/// Compare recent move shares with a baseline. The result is sorted by the
/// size of the positive change, so a newly popular move appears first.
pub fn trends(tree: &RecentTree, key: u64, baseline: &[MoveStat]) -> Vec<TrendRow> {
    let recent_moves = tree.moves(key);
    if recent_moves.is_empty() {
        return Vec::new();
    }
    let recent_total: u32 = recent_moves.iter().map(|mv| mv.games).sum();
    if recent_total == 0 {
        return Vec::new();
    }
    let baseline_total: u32 = baseline.iter().map(|mv| mv.games).sum();
    let baseline_by_move: HashMap<u32, &MoveStat> =
        baseline.iter().map(|stat| (stat.mv, stat)).collect();

    let mut rows: Vec<TrendRow> = recent_moves
        .into_iter()
        .map(|stat| {
            let baseline_games = baseline_by_move
                .get(&stat.mv)
                .map_or(0, |baseline| baseline.games);
            let recent_share = f64::from(stat.games) / f64::from(recent_total);
            let baseline_share =
                (baseline_total > 0).then(|| f64::from(baseline_games) / f64::from(baseline_total));
            let delta_pp = baseline_share.map(|share| (recent_share - share) * 100.0);
            TrendRow {
                stat,
                recent_games: stat.games,
                recent_total,
                recent_share,
                baseline_games,
                baseline_total,
                baseline_share,
                delta_pp,
            }
        })
        .collect();

    rows.sort_by(|a, b| {
        b.delta_pp
            .unwrap_or(f64::NEG_INFINITY)
            .total_cmp(&a.delta_pp.unwrap_or(f64::NEG_INFINITY))
            .then(b.recent_games.cmp(&a.recent_games))
            .then(a.stat.mv.cmp(&b.stat.mv))
    });
    rows
}

fn average_elo(game: &Game) -> Option<u32> {
    let white = game.header("WhiteElo").and_then(parse_elo);
    let black = game.header("BlackElo").and_then(parse_elo);
    match (white, black) {
        (Some(white), Some(black)) => Some(((white + black) / 2).max(1)),
        (Some(elo), None) | (None, Some(elo)) => Some(elo),
        (None, None) => None,
    }
}

fn parse_elo(value: &str) -> Option<u32> {
    value.trim().parse::<u32>().ok().filter(|elo| *elo > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pgn(games: &str) -> Vec<Game> {
        crate::domain::parse_pgn(games.as_bytes()).unwrap()
    }

    #[test]
    fn tree_aggregates_moves_and_scores() {
        let games = pgn(r#"[Event "A"]
[White "One"]
[Black "Two"]
[Date "2026.01.01"]
[WhiteElo "2500"]
[BlackElo "2400"]
[Result "1-0"]

1. e4 e5 2. Nf3 Nc6 1-0

[Event "B"]
[White "Three"]
[Black "Four"]
[Date "2026.02.01"]
[WhiteElo "2600"]
[BlackElo "2500"]
[Result "1/2-1/2"]

1. e4 c5 1/2-1/2
"#);
        let tree = RecentTree::from_games(&games, 20260301, 60);
        let root = position::position_key(&shakmaty::Chess::default());
        let moves = tree.moves(root);
        assert_eq!(moves.len(), 1);
        let e4 = moves.iter().find(|mv| mv.games == 2).unwrap();
        assert_eq!(e4.white, 1);
        assert_eq!(e4.draw, 1);
        assert_eq!(e4.elo_avg(), Some(2500.0));
        assert_eq!(tree.stats(root).unwrap().games, 2);
        assert_eq!(tree.position_games(root), 2);
        assert_eq!(tree.recent_move_games(root), 2);
    }

    #[test]
    fn trends_compare_recent_share_with_baseline() {
        let games = pgn(r#"[Event "A"]
[White "One"]
[Black "Two"]
[Date "2026.02.01"]
[Result "1-0"]

1. e4 e5 1-0

[Event "B"]
[White "Three"]
[Black "Four"]
[Date "2026.02.02"]
[Result "1-0"]

1. e4 e5 1-0

[Event "C"]
[White "Five"]
[Black "Six"]
[Date "2026.02.03"]
[Result "1-0"]

1. d4 d5 1-0
"#);
        let tree = RecentTree::from_games(&games, 20260301, 60);
        let root = position::position_key(&shakmaty::Chess::default());
        let baseline = tree.moves(root);
        let rows = trends(&tree, root, &baseline);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].recent_games, 2);
        assert_eq!(rows[0].delta_pp, Some(0.0));
        assert_eq!(rows[1].delta_pp, Some(0.0));
    }
}
