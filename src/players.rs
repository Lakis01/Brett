//! Player directory, per-player position books, and move likelihood.
//!
//! The database index only aggregates all games together. Preparation needs a
//! second view: for one player, every mainline position that player moved from,
//! with the move they chose and the date/result of that game.
//!
//! Prediction uses an exponentially time-decayed Dirichlet model. A small
//! empirical prior comes from the whole database at the same position, and a
//! recent-window component makes a repertoire switch visible without throwing
//! away the player's older history.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use shakmaty::Color;
use shakmaty::uci::UciMove;

use crate::domain::Game;
use crate::library::GameLibrary;
#[cfg(test)]
use crate::library::PgnLibrary;
use crate::position::{self, MoveStat};

/// One player's aggregate record in the loaded database.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayerRecord {
    pub games: u32,
    pub wins: u32,
    pub draws: u32,
    pub losses: u32,
}

impl PlayerRecord {
    pub fn decided(&self) -> u32 {
        self.wins + self.draws + self.losses
    }

    /// Score from the player's perspective (win = 1, draw = ½).
    pub fn score(&self) -> f32 {
        if self.decided() == 0 {
            return 0.0;
        }
        (self.wins as f32 + self.draws as f32 * 0.5) / self.decided() as f32
    }

    pub fn win_ratio(&self) -> f32 {
        if self.decided() == 0 {
            0.0
        } else {
            self.wins as f32 / self.decided() as f32
        }
    }

    fn add(&mut self, result: GameResult) {
        self.games += 1;
        match result {
            GameResult::Win => self.wins += 1,
            GameResult::Draw => self.draws += 1,
            GameResult::Loss => self.losses += 1,
            GameResult::Unknown => {}
        }
    }
}

/// A player in the loaded database.
#[derive(Clone, Debug)]
pub struct PlayerSummary {
    pub name: String,
    pub key: String,
    pub white: PlayerRecord,
    pub black: PlayerRecord,
    pub elo_sum: u64,
    pub elo_count: u32,
    pub elo_max: u32,
    pub opponent_elo_sum: u64,
    pub opponent_elo_count: u32,
    pub first_year: u16,
    pub last_year: u16,
    pub game_indices: Vec<usize>,
}

impl PlayerSummary {
    pub fn games(&self) -> u32 {
        self.white.games + self.black.games
    }

    pub fn wins(&self) -> u32 {
        self.white.wins + self.black.wins
    }

    pub fn draws(&self) -> u32 {
        self.white.draws + self.black.draws
    }

    pub fn losses(&self) -> u32 {
        self.white.losses + self.black.losses
    }

    pub fn decided(&self) -> u32 {
        self.white.decided() + self.black.decided()
    }

    pub fn score(&self) -> f32 {
        if self.decided() == 0 {
            return 0.0;
        }
        (self.wins() as f32 + self.draws() as f32 * 0.5) / self.decided() as f32
    }

    pub fn win_ratio(&self) -> f32 {
        if self.decided() == 0 {
            0.0
        } else {
            self.wins() as f32 / self.decided() as f32
        }
    }

    pub fn elo_avg(&self) -> Option<f64> {
        (self.elo_count > 0).then(|| self.elo_sum as f64 / self.elo_count as f64)
    }

    pub fn opponent_elo_avg(&self) -> Option<f64> {
        (self.opponent_elo_count > 0)
            .then(|| self.opponent_elo_sum as f64 / self.opponent_elo_count as f64)
    }

    pub fn years_label(&self) -> String {
        match (self.first_year, self.last_year) {
            (0, 0) => "—".to_string(),
            (0, last) => last.to_string(),
            (first, 0) => first.to_string(),
            (first, last) if first == last => first.to_string(),
            (first, last) => format!("{first}–{last}"),
        }
    }
}

/// All players found in the database, sorted by activity.
#[derive(Clone, Debug, Default)]
pub struct PlayerDirectory {
    players: Vec<PlayerSummary>,
}

impl PlayerDirectory {
    pub fn build(library: &dyn GameLibrary) -> Self {
        let mut by_key: HashMap<String, PlayerSummary> = HashMap::new();

        for (index, game) in library.games().iter().enumerate() {
            let white_key = player_key(game.white());
            let black_key = player_key(game.black());
            let same_player = white_key.is_some() && white_key == black_key;

            if let Some(key) = &white_key {
                let summary = by_key.entry(key.clone()).or_insert_with(|| PlayerSummary {
                    name: game.white().to_string(),
                    key: key.clone(),
                    white: PlayerRecord::default(),
                    black: PlayerRecord::default(),
                    elo_sum: 0,
                    elo_count: 0,
                    elo_max: 0,
                    opponent_elo_sum: 0,
                    opponent_elo_count: 0,
                    first_year: 0,
                    last_year: 0,
                    game_indices: Vec::new(),
                });
                add_game(
                    summary,
                    game,
                    Color::White,
                    index,
                    same_player.then_some(Color::Black),
                );
            }

            if !same_player && let Some(key) = &black_key {
                let summary = by_key.entry(key.clone()).or_insert_with(|| PlayerSummary {
                    name: game.black().to_string(),
                    key: key.clone(),
                    white: PlayerRecord::default(),
                    black: PlayerRecord::default(),
                    elo_sum: 0,
                    elo_count: 0,
                    elo_max: 0,
                    opponent_elo_sum: 0,
                    opponent_elo_count: 0,
                    first_year: 0,
                    last_year: 0,
                    game_indices: Vec::new(),
                });
                add_game(summary, game, Color::Black, index, None);
            }
        }

        let mut players: Vec<_> = by_key.into_values().collect();
        players.sort_by(|a, b| {
            b.games()
                .cmp(&a.games())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Self { players }
    }

    pub fn players(&self) -> &[PlayerSummary] {
        &self.players
    }

    pub fn len(&self) -> usize {
        self.players.len()
    }

    pub fn is_empty(&self) -> bool {
        self.players.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&PlayerSummary> {
        self.players.get(index)
    }

    pub fn find_key(&self, key: &str) -> Option<usize> {
        self.players.iter().position(|player| player.key == key)
    }

    pub fn filter(&self, query: &str) -> Vec<usize> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return (0..self.players.len()).collect();
        }
        self.players
            .iter()
            .enumerate()
            .filter(|(_, player)| player.name.to_lowercase().contains(&needle))
            .map(|(index, _)| index)
            .collect()
    }
}

fn add_game(
    summary: &mut PlayerSummary,
    game: &crate::library::IndexedGame,
    color: Color,
    index: usize,
    self_play_other: Option<Color>,
) {
    let (own_elo_tag, opponent_elo_tag) = match color {
        Color::White => ("WhiteElo", "BlackElo"),
        Color::Black => ("BlackElo", "WhiteElo"),
    };
    let own_elo = game.header(own_elo_tag).and_then(parse_elo);
    let opponent_elo = game.header(opponent_elo_tag).and_then(parse_elo);
    let result = result_for(color, game.result());

    let record = match color {
        Color::White => &mut summary.white,
        Color::Black => &mut summary.black,
    };
    record.add(result);

    if let Some(elo) = own_elo {
        summary.elo_sum += u64::from(elo);
        summary.elo_count += 1;
        summary.elo_max = summary.elo_max.max(elo);
    }
    if let Some(elo) = opponent_elo {
        summary.opponent_elo_sum += u64::from(elo);
        summary.opponent_elo_count += 1;
    }
    if let Some(year) = parse_date(game.date()).map(date_year) {
        if summary.first_year == 0 || year < summary.first_year {
            summary.first_year = year;
        }
        summary.last_year = summary.last_year.max(year);
    }
    summary.game_indices.push(index);

    // Self-play is rare; count the game once and record the other colour too
    // only for the side breakdown, not as a second appearance.
    if let Some(other) = self_play_other {
        let other_record = match other {
            Color::White => &mut summary.white,
            Color::Black => &mut summary.black,
        };
        other_record.add(result_for(other, game.result()));
    }
}

fn result_for(color: Color, tag: &str) -> GameResult {
    match (color, tag) {
        (Color::White, "1-0") | (Color::Black, "0-1") => GameResult::Win,
        (_, "1/2-1/2") => GameResult::Draw,
        (Color::White, "0-1") | (Color::Black, "1-0") => GameResult::Loss,
        _ => GameResult::Unknown,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GameResult {
    Win,
    Draw,
    Loss,
    Unknown,
}

/// Canonical player identity. Word order and punctuation are ignored so
/// `Carlsen, Magnus` and `Magnus Carlsen` resolve to the same person.
pub fn player_key(name: &str) -> Option<String> {
    let mut words: Vec<String> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.to_lowercase())
        .collect();
    if words.is_empty() || (words.len() == 1 && words[0] == "?") {
        return None;
    }
    words.sort_unstable();
    words.dedup();
    Some(words.join(" "))
}

/// One move played by the selected player from one position.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayerMoveStat {
    pub key: u64,
    pub mv: u32,
    pub games: u32,
    pub wins: u32,
    pub draws: u32,
    pub losses: u32,
    pub first_date: u32,
    pub last_date: u32,
    pub recent_games: u32,
    pub weighted: f64,
    pub recent_weighted: f64,
}

impl PlayerMoveStat {
    pub fn score(&self) -> f32 {
        let decided = self.wins + self.draws + self.losses;
        if decided == 0 {
            0.0
        } else {
            (self.wins as f32 + self.draws as f32 * 0.5) / decided as f32
        }
    }
}

/// Aggregate record for one player at one position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayerPositionStats {
    pub games: u32,
    pub wins: u32,
    pub draws: u32,
    pub losses: u32,
    pub recent_games: u32,
    pub first_date: u32,
    pub last_date: u32,
}

impl PlayerPositionStats {
    pub fn score(&self) -> f32 {
        let decided = self.wins + self.draws + self.losses;
        if decided == 0 {
            0.0
        } else {
            (self.wins as f32 + self.draws as f32 * 0.5) / decided as f32
        }
    }
}

/// Parameters of the likelihood model.
///
/// These are deliberately conservative for sparse chess data. The database
/// prior contributes about three pseudo-games, the recent window is two years,
/// and the recent component only dominates after several recent games.
#[derive(Clone, Copy, Debug)]
pub struct PredictionConfig {
    /// Half-life of the general time-decay weights, in days.
    pub half_life_days: f64,
    /// A game this recent belongs to the recent regime.
    pub recent_days: i64,
    /// Weight of a game with no usable date.
    pub unknown_date_weight: f64,
    /// Strength of the database prior, in pseudo-games.
    pub prior_games: f64,
    /// Smoothing added to every candidate move.
    pub smoothing: f64,
    /// Number of recent pseudo-games needed before the recent regime gets
    /// half of the mixture weight.
    pub recent_mix_games: f64,
}

impl Default for PredictionConfig {
    fn default() -> Self {
        Self {
            half_life_days: 547.5, // 18 months
            recent_days: 730,      // 24 months
            unknown_date_weight: 0.20,
            prior_games: 3.0,
            smoothing: 0.05,
            recent_mix_games: 2.0,
        }
    }
}

/// A player's position book. Built off the UI thread.
pub struct PlayerBook {
    player_key: String,
    player_name: String,
    positions: HashMap<u64, BTreeMap<u32, PlayerMoveStat>>,
    games: usize,
    as_of: u32,
    config: PredictionConfig,
}

impl PlayerBook {
    pub fn player_key(&self) -> &str {
        &self.player_key
    }

    pub fn player_name(&self) -> &str {
        &self.player_name
    }

    pub fn games(&self) -> usize {
        self.games
    }

    pub fn as_of(&self) -> u32 {
        self.as_of
    }

    pub fn config(&self) -> PredictionConfig {
        self.config
    }

    /// Build a book for one player. Individual unreadable games are skipped
    /// and reported through `tracing`; the rest of the preparation data stays
    /// useful.
    pub fn build(
        library: &dyn GameLibrary,
        summary: &PlayerSummary,
        config: PredictionConfig,
        as_of: u32,
        mut progress: impl FnMut(usize, usize),
    ) -> Result<Self> {
        let mut book = Self {
            player_key: summary.key.clone(),
            player_name: summary.name.clone(),
            positions: HashMap::new(),
            games: 0,
            as_of,
            config,
        };
        let total = summary.game_indices.len();
        for (position, &index) in summary.game_indices.iter().enumerate() {
            match library.load(index) {
                Ok(game) => book.add_game(&game),
                Err(err) => {
                    tracing::warn!("player book skipped game {index}: {err:#}");
                }
            }
            if position % 200 == 0 || position + 1 == total {
                progress(position + 1, total);
            }
        }
        Ok(book)
    }

    /// Build directly from already parsed games (used by tests).
    pub fn from_games(
        player_key: impl Into<String>,
        player_name: impl Into<String>,
        games: &[Game],
        config: PredictionConfig,
        as_of: u32,
    ) -> Self {
        let mut book = Self {
            player_key: player_key.into(),
            player_name: player_name.into(),
            positions: HashMap::new(),
            games: 0,
            as_of,
            config,
        };
        for game in games {
            book.add_game(game);
        }
        book
    }

    fn add_game(&mut self, game: &Game) {
        let white_key = player_key(game.white());
        let black_key = player_key(game.black());
        let color = if white_key.as_deref() == Some(self.player_key.as_str()) {
            Color::White
        } else if black_key.as_deref() == Some(self.player_key.as_str()) {
            Color::Black
        } else {
            return;
        };

        let date = parse_date(game.date()).unwrap_or(0);
        let result = result_for(color, game.result());
        let mut seen = HashSet::new();
        let mut node = 0;

        while let Some(&child) = game.nodes[node].children.first() {
            let parent_pos = &game.nodes[node].pos;
            let key = position::position_key(parent_pos);
            if !seen.insert(key) {
                break;
            }
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
            if ply.color != color {
                node = child;
                continue;
            }

            let (weight, recent) = recency(date, self.as_of, self.config);
            let entry = self
                .positions
                .entry(key)
                .or_default()
                .entry(code)
                .or_insert(PlayerMoveStat {
                    key,
                    mv: code,
                    ..PlayerMoveStat::default()
                });
            entry.games += 1;
            entry.weighted += weight;
            match result {
                GameResult::Win => entry.wins += 1,
                GameResult::Draw => entry.draws += 1,
                GameResult::Loss => entry.losses += 1,
                GameResult::Unknown => {}
            }
            if recent {
                entry.recent_games += 1;
                entry.recent_weighted += weight;
            }
            if date != 0 {
                if entry.first_date == 0 || date < entry.first_date {
                    entry.first_date = date;
                }
                entry.last_date = entry.last_date.max(date);
            }
            node = child;
        }
        self.games += 1;
    }

    pub fn moves(&self, key: u64) -> Vec<PlayerMoveStat> {
        let mut moves: Vec<_> = self
            .positions
            .get(&key)
            .map(|moves| moves.values().copied().collect())
            .unwrap_or_default();
        moves.sort_by(|a, b| {
            b.weighted
                .total_cmp(&a.weighted)
                .then(b.games.cmp(&a.games))
                .then(a.mv.cmp(&b.mv))
        });
        moves
    }

    pub fn stats(&self, key: u64) -> Option<PlayerPositionStats> {
        let moves = self.positions.get(&key)?;
        let mut stats = PlayerPositionStats::default();
        for mv in moves.values() {
            stats.games += mv.games;
            stats.wins += mv.wins;
            stats.draws += mv.draws;
            stats.losses += mv.losses;
            stats.recent_games += mv.recent_games;
            if mv.first_date != 0 && (stats.first_date == 0 || mv.first_date < stats.first_date) {
                stats.first_date = mv.first_date;
            }
            stats.last_date = stats.last_date.max(mv.last_date);
        }
        Some(stats)
    }
}

/// A predicted move in the next game.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MovePrediction {
    pub mv: u32,
    pub probability: f64,
    pub player_games: u32,
    pub recent_games: u32,
    pub last_date: u32,
    /// Change in share between the old and recent windows, in percentage
    /// points. Positive means the player is moving toward this move.
    pub trend_pp: Option<f64>,
}

/// Prediction output for one position.
#[derive(Clone, Debug, Default)]
pub struct Prediction {
    pub moves: Vec<MovePrediction>,
    pub player_games: u32,
    pub recent_games: u32,
    pub recent_mix: f64,
    pub has_player_data: bool,
}

/// Compute the predictive distribution for the selected player's next move.
///
/// The database prior is the global distribution with this player's own games
/// removed (so the player is not used as their own evidence). Player moves are
/// weighted with an exponential time decay and combined with a recent-window
/// distribution. Both components are Dirichlet-smoothed.
pub fn predict(book: &PlayerBook, key: u64, global: &[MoveStat]) -> Prediction {
    let player_moves: BTreeMap<u32, PlayerMoveStat> = book
        .positions
        .get(&key)
        .map(|moves| moves.iter().map(|(&code, stat)| (code, *stat)).collect())
        .unwrap_or_default();

    let mut candidates: BTreeMap<u32, Candidate> = BTreeMap::new();
    for stat in global {
        let player_games = player_moves.get(&stat.mv).map_or(0, |mv| mv.games);
        let database_games = stat.games.saturating_sub(player_games);
        let entry = candidates.entry(stat.mv).or_default();
        entry.database_games = entry.database_games.saturating_add(database_games);
    }
    for &code in player_moves.keys() {
        candidates.entry(code).or_default();
    }
    if candidates.is_empty() {
        return Prediction::default();
    }

    let database_total: u64 = candidates
        .values()
        .map(|candidate| u64::from(candidate.database_games))
        .sum();
    let candidate_count = candidates.len() as f64;
    let total_weight: f64 = player_moves.values().map(|mv| mv.weighted).sum();
    let recent_weight: f64 = player_moves.values().map(|mv| mv.recent_weighted).sum();

    let mut prior_total = 0.0;
    for candidate in candidates.values_mut() {
        let share = if database_total > 0 {
            f64::from(candidate.database_games) / database_total as f64
        } else {
            1.0 / candidate_count
        };
        candidate.alpha = book.config.prior_games * share + book.config.smoothing;
        prior_total += candidate.alpha;
    }

    let decay_total = prior_total + total_weight;
    let recent_total = prior_total + recent_weight;
    let recent_mix = if recent_weight > 0.0 {
        recent_weight / (recent_weight + book.config.recent_mix_games)
    } else {
        0.0
    };

    let mut moves = Vec::with_capacity(candidates.len());
    let mut player_games = 0;
    let mut recent_games = 0;
    for (code, candidate) in candidates {
        let player = player_moves.get(&code).copied().unwrap_or_default();
        let decay_probability = (candidate.alpha + player.weighted) / decay_total;
        let recent_probability = if recent_weight > 0.0 {
            (candidate.alpha + player.recent_weighted) / recent_total
        } else {
            decay_probability
        };
        let probability = (1.0 - recent_mix) * decay_probability + recent_mix * recent_probability;

        let trend_pp = if recent_weight > 0.0 && total_weight > recent_weight {
            let recent_share = player.recent_weighted / recent_weight;
            let historical_weight = (player.weighted - player.recent_weighted).max(0.0);
            let historical_total = total_weight - recent_weight;
            Some((recent_share - historical_weight / historical_total) * 100.0)
        } else {
            None
        };

        player_games += player.games;
        recent_games += player.recent_games;
        moves.push(MovePrediction {
            mv: code,
            probability,
            player_games: player.games,
            recent_games: player.recent_games,
            last_date: player.last_date,
            trend_pp,
        });
    }

    moves.sort_by(|a, b| {
        b.probability
            .total_cmp(&a.probability)
            .then(b.player_games.cmp(&a.player_games))
            .then(a.mv.cmp(&b.mv))
    });

    Prediction {
        moves,
        player_games,
        recent_games,
        recent_mix,
        has_player_data: total_weight > 0.0,
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Candidate {
    database_games: u32,
    alpha: f64,
}

/// Exponential recency weight and whether the game falls in the recent window.
fn recency(date: u32, as_of: u32, config: PredictionConfig) -> (f64, bool) {
    if date == 0 {
        return (config.unknown_date_weight, false);
    }
    let Some(date) = date_to_days(date) else {
        return (config.unknown_date_weight, false);
    };
    let Some(as_of) = date_to_days(as_of) else {
        return (1.0, false);
    };
    let age = (as_of - date).max(0) as f64;
    let weight = 0.5f64.powf(age / config.half_life_days);
    (weight, age <= config.recent_days as f64)
}

/// Parse a PGN date tag (`2024.03.07`) into `YYYYMMDD`, returning `None` for
/// unknown or invalid dates.
pub fn parse_date(value: &str) -> Option<u32> {
    let mut parts = value.split('.');
    let year = parts.next()?.parse::<u32>().ok()?;
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    if year == 0 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(year * 10_000 + month * 100 + day)
}

fn parse_elo(value: &str) -> Option<u32> {
    value.trim().parse().ok().filter(|elo| *elo > 0)
}

fn date_year(date: u32) -> u16 {
    (date / 10_000).min(u32::from(u16::MAX)) as u16
}

/// Current UTC date as `YYYYMMDD`, used for recency weights.
pub fn today() -> u32 {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        / 86_400;
    let (year, month, day) = civil_from_days(days);
    year as u32 * 10_000 + month * 100 + day
}

fn date_to_days(date: u32) -> Option<i64> {
    let year = (date / 10_000) as i32;
    let month = (date / 100) % 100;
    let day = date % 100;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

// Howard Hinnant's civil-date algorithms, valid for the proleptic Gregorian
// calendar and avoiding a date-time dependency for one arithmetic operation.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = (year - era * 400) as u32;
    let month_prime = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era as i64 * 146_097 + day_of_era as i64 - 719_468
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u32;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i32 + era as i32 * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain;
    use shakmaty::Position;

    const TREND_GAMES: &str = r#"[Event "Old"]
[Site "Test"]
[Date "2020.01.01"]
[White "Opponent"]
[Black "Trendy"]
[Result "1-0"]

1. e4 e6 2. d4 d5 1-0

[Event "Old"]
[Site "Test"]
[Date "2020.06.01"]
[White "Opponent"]
[Black "Trendy"]
[Result "0-1"]

1. e4 e6 2. d4 d5 0-1

[Event "Old"]
[Site "Test"]
[Date "2021.01.01"]
[White "Opponent"]
[Black "Trendy"]
[Result "1-0"]

1. e4 e6 2. d4 d5 1-0

[Event "Old"]
[Site "Test"]
[Date "2021.06.01"]
[White "Opponent"]
[Black "Trendy"]
[Result "0-1"]

1. e4 e6 2. d4 d5 0-1

[Event "Recent"]
[Site "Test"]
[Date "2025.01.01"]
[White "Opponent"]
[Black "Trendy"]
[Result "1-0"]

1. e4 c5 2. Nf3 d6 1-0

[Event "Recent"]
[Site "Test"]
[Date "2025.06.01"]
[White "Opponent"]
[Black "Trendy"]
[Result "0-1"]

1. e4 c5 2. Nf3 d6 0-1

[Event "Recent"]
[Site "Test"]
[Date "2026.01.01"]
[White "Opponent"]
[Black "Trendy"]
[Result "1-0"]

1. e4 c5 2. Nf3 d6 1-0

[Event "Recent"]
[Site "Test"]
[Date "2026.06.01"]
[White "Opponent"]
[Black "Trendy"]
[Result "0-1"]

1. e4 c5 2. Nf3 d6 0-1
"#;

    fn trend_book() -> PlayerBook {
        let games = domain::parse_pgn(TREND_GAMES.as_bytes()).unwrap();
        PlayerBook::from_games(
            "trendy",
            "Trendy",
            &games,
            PredictionConfig::default(),
            2026_1001,
        )
    }

    #[test]
    fn player_keys_ignore_word_order_and_punctuation() {
        assert_eq!(
            player_key("Carlsen, Magnus").as_deref(),
            Some("carlsen magnus")
        );
        assert_eq!(
            player_key("Magnus Carlsen").as_deref(),
            Some("carlsen magnus")
        );
        assert_eq!(player_key("?"), None);
        assert_eq!(player_key("  "), None);
    }

    #[test]
    fn directory_groups_both_colours_and_records_results() {
        let games = domain::parse_pgn(TREND_GAMES.as_bytes()).unwrap();
        let mut path = std::env::temp_dir();
        path.push(format!(
            "brett-players-{}-{}.pgn",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, TREND_GAMES).unwrap();
        let library = PgnLibrary::build(&path).unwrap();
        let directory = PlayerDirectory::build(&library);
        let index = directory.find_key("trendy").unwrap();
        let player = directory.get(index).unwrap();
        assert_eq!(player.games(), 8);
        assert_eq!(player.black.games, 8);
        assert_eq!(player.white.games, 0);
        assert_eq!(player.wins() + player.draws() + player.losses(), 8);
        std::fs::remove_file(path).unwrap();

        let opponent = directory.find_key("opponent").unwrap();
        let opponent = directory.get(opponent).unwrap();
        assert_eq!(opponent.white.games, 8);
        assert_eq!(opponent.wins() + opponent.draws() + opponent.losses(), 8);
        drop(games);
    }

    #[test]
    fn recent_switch_beats_large_old_sample() {
        let book = trend_book();
        let start = shakmaty::Chess::default();
        let after_e4 = start
            .clone()
            .play(
                shakmaty::san::SanPlus::from_ascii(b"e4")
                    .unwrap()
                    .san
                    .to_move(&start)
                    .unwrap(),
            )
            .unwrap();
        let key = position::position_key(&after_e4);
        let prediction = predict(&book, key, &[]);
        assert!(prediction.has_player_data);
        assert_eq!(prediction.player_games, 8);
        let c5 = prediction
            .moves
            .iter()
            .find(|mv| {
                position::decode_move(&after_e4, mv.mv)
                    .map(|mv| shakmaty::san::SanPlus::from_move(after_e4.clone(), mv).to_string())
                    .as_deref()
                    == Some("c5")
            })
            .unwrap();
        let e6 = prediction
            .moves
            .iter()
            .find(|mv| {
                position::decode_move(&after_e4, mv.mv)
                    .map(|mv| shakmaty::san::SanPlus::from_move(after_e4.clone(), mv).to_string())
                    .as_deref()
                    == Some("e6")
            })
            .unwrap();
        assert!(
            c5.probability > e6.probability,
            "recent c5 ({:.3}) should beat old e6 ({:.3})",
            c5.probability,
            e6.probability
        );
        assert!(c5.probability > 0.60);
        assert_eq!(prediction.recent_games, 4);
    }

    #[test]
    fn database_prior_is_used_when_player_has_no_games_here() {
        let book = trend_book();
        let start = shakmaty::Chess::default();
        let key = position::position_key(&start);
        let global = vec![
            MoveStat {
                key,
                mv: position::encode_move(
                    shakmaty::san::SanPlus::from_ascii(b"e4")
                        .unwrap()
                        .san
                        .to_move(&start)
                        .unwrap(),
                )
                .unwrap(),
                games: 90,
                ..MoveStat::default()
            },
            MoveStat {
                key,
                mv: position::encode_move(
                    shakmaty::san::SanPlus::from_ascii(b"d4")
                        .unwrap()
                        .san
                        .to_move(&start)
                        .unwrap(),
                )
                .unwrap(),
                games: 10,
                ..MoveStat::default()
            },
        ];
        let prediction = predict(&book, key, &global);
        assert!(!prediction.has_player_data);
        assert_eq!(prediction.moves[0].mv, global[0].mv);
        assert!(prediction.moves[0].probability > 0.8);
    }

    #[test]
    fn civil_dates_round_trip() {
        for date in [1970_0101, 2000_0229, 2024_1231, 2026_1001] {
            let days = date_to_days(date).unwrap();
            let (year, month, day) = civil_from_days(days);
            assert_eq!(year as u32 * 10_000 + month * 100 + day, date);
        }
    }
}
