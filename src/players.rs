//! Player directory, per-player position books, and move likelihood.
//!
//! The database index only aggregates all games together. Preparation needs a
//! second view: for one player, every mainline position that player moved from,
//! with the move they chose and the date/result of that game.
//!
//! Prediction uses an exponentially time-decayed Dirichlet model. A small
//! empirical prior comes from a reference database at the same position (or the
//! loaded database when no reference is configured), and a recent-window
//! component makes a repertoire switch visible without throwing away the
//! player's older history.
//!
//! The model deliberately reserves probability mass for legal moves that have
//! not been observed. A 2/2 move is therefore never displayed as certain, and
//! every estimate carries an approximate 90% credible interval.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::chess::{Color, UciMove};
use anyhow::Result;

use crate::domain::Game;
use crate::library::GameLibrary;
#[cfg(test)]
use crate::library::PgnLibrary;
use crate::position::{self, MoveStat};
use crate::titles::{self, PlayerTitle};

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
    /// Strongest recognized title seen in this player's games. The raw tags
    /// stay untouched; this is only a display aggregate.
    pub title: Option<PlayerTitle>,
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
                    title: None,
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
                    title: None,
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
    let (own_elo_tag, opponent_elo_tag, own_title_tag) = match color {
        Color::White => ("WhiteElo", "BlackElo", "WhiteTitle"),
        Color::Black => ("BlackElo", "WhiteElo", "BlackTitle"),
    };
    if let Some(title) = game.header(own_title_tag).and_then(titles::normalize)
        && summary
            .title
            .is_none_or(|current| title.rank() > current.rank())
    {
        summary.title = Some(title);
    }
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
/// These are deliberately conservative for sparse chess data. The background
/// prior starts at about three pseudo-games and grows with the background
/// sample size up to a cap, the recent window is two years, and the recent
/// component only dominates after several recent games. A separate
/// pseudo-count is reserved for legal moves that do not appear in the data, so
/// tiny samples cannot produce a probability of 100%.
#[derive(Clone, Copy, Debug)]
pub struct PredictionConfig {
    /// Half-life of the general time-decay weights, in days.
    pub half_life_days: f64,
    /// A game this recent belongs to the recent regime.
    pub recent_days: i64,
    /// Weight of a game with no usable date.
    pub unknown_date_weight: f64,
    /// Strength of the background prior, in pseudo-games.
    pub prior_games: f64,
    /// Smoothing added to every candidate move.
    pub smoothing: f64,
    /// Number of recent pseudo-games needed before the recent regime gets
    /// half of the mixture weight.
    pub recent_mix_games: f64,
    /// Total pseudo-games reserved for legal moves that are absent from both
    /// the player book and the background distribution.
    pub unseen_prior_games: f64,
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
            unseen_prior_games: 0.75,
        }
    }
}

/// A game that reached one position in the player's book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerGameRef {
    /// Index in the database that produced the book.
    pub index: usize,
    /// Parsed PGN date (`YYYYMMDD`), or zero when unknown.
    pub date: u32,
    /// The prepared player's colour in that game.
    pub color: Color,
}

/// A player's position book. Built off the UI thread.
pub struct PlayerBook {
    player_key: String,
    player_name: String,
    positions: HashMap<u64, BTreeMap<u32, PlayerMoveStat>>,
    /// Games that reached each position, used by the recent-games panel.
    game_refs: HashMap<u64, Vec<PlayerGameRef>>,
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
            game_refs: HashMap::new(),
            games: 0,
            as_of,
            config,
        };
        let total = summary.game_indices.len();
        for (position, &index) in summary.game_indices.iter().enumerate() {
            match library.load(index) {
                Ok(game) => book.add_game(&game, Some(index)),
                Err(err) => {
                    tracing::warn!("player book skipped game {index}: {err:#}");
                }
            }
            if position % 200 == 0 || position + 1 == total {
                progress(position + 1, total);
            }
        }
        book.sort_game_refs();
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
            game_refs: HashMap::new(),
            games: 0,
            as_of,
            config,
        };
        for game in games {
            book.add_game(game, None);
        }
        book.sort_game_refs();
        book
    }

    fn sort_game_refs(&mut self) {
        for games in self.game_refs.values_mut() {
            games.sort_by(|a, b| b.date.cmp(&a.date).then(b.index.cmp(&a.index)));
        }
    }

    fn add_game(&mut self, game: &Game, game_index: Option<usize>) {
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

        loop {
            let parent_pos = &game.nodes[node].pos;
            let key = position::position_key(parent_pos);
            if !seen.insert(key) {
                break;
            }
            // Record every position the game reached, including the final
            // position where no reply was played.
            if let Some(index) = game_index {
                self.game_refs
                    .entry(key)
                    .or_default()
                    .push(PlayerGameRef { index, date, color });
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
            if ply.color == color {
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

    /// Most recent games in this book that reached `key`, newest first.
    ///
    /// Games with unknown dates sort after dated games. Ties are broken by
    /// database index so the result is stable.
    pub fn recent_games(&self, key: u64, limit: usize) -> Vec<PlayerGameRef> {
        let Some(games) = self.game_refs.get(&key) else {
            return Vec::new();
        };
        games.iter().take(limit).copied().collect()
    }

    /// Most recent games that reached `key` with the prepared player on
    /// `color`. Preparation shows only the games from the side the opponent
    /// will play, so irrelevant colors must not consume the display limit.
    pub fn recent_games_for(&self, key: u64, color: Color, limit: usize) -> Vec<PlayerGameRef> {
        let Some(games) = self.game_refs.get(&key) else {
            return Vec::new();
        };
        games
            .iter()
            .filter(|game| game.color == color)
            .take(limit)
            .copied()
            .collect()
    }
}

/// A predicted move in the next game.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MovePrediction {
    pub mv: u32,
    /// Posterior mean of the move's probability.
    pub probability: f64,
    /// Lower bound of the approximate 90% credible interval.
    pub probability_low: f64,
    /// Upper bound of the approximate 90% credible interval.
    pub probability_high: f64,
    pub player_games: u32,
    pub recent_games: u32,
    pub last_date: u32,
    /// Background games supporting this move. For a current-database prior
    /// this excludes the prepared player's own games.
    pub prior_games: u32,
    /// Change in share between the old and recent windows, in percentage
    /// points. Positive means the player is moving toward this move.
    pub trend_pp: Option<f64>,
}

/// What supplies the background distribution for one prediction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PriorSource {
    /// The loaded player database. The prepared player's games are subtracted
    /// so the player is not used as their own evidence.
    #[default]
    CurrentDatabase,
    /// A separately loaded reference database. No player-game subtraction is
    /// applied because the reference is already a background corpus.
    Reference,
}

/// Prediction output for one position.
#[derive(Clone, Debug, Default)]
pub struct Prediction {
    pub moves: Vec<MovePrediction>,
    pub player_games: u32,
    pub recent_games: u32,
    pub recent_mix: f64,
    pub has_player_data: bool,
    /// Posterior probability that the player chooses a legal move that is not
    /// listed in `moves`.
    pub unseen_probability: f64,
    /// Number of legal moves not represented by any observation.
    pub unseen_moves: usize,
    pub legal_moves: usize,
    pub prior_games: u32,
    pub prior_source: PriorSource,
}

impl Prediction {
    /// A compact sample-size warning for the interface.
    pub fn confidence_label(&self) -> &'static str {
        if !self.has_player_data {
            "prior only"
        } else {
            match self.player_games {
                0..=2 => "low confidence",
                3..=8 => "moderate confidence",
                9..=19 => "good confidence",
                _ => "high confidence",
            }
        }
    }
}

/// Compute the predictive distribution for the selected player's next move.
///
/// The background prior comes from `prior_source`. For
/// [`PriorSource::CurrentDatabase`] the prepared player's own games are removed
/// from `global`, so the player is not used as their own evidence. Player moves
/// are weighted with an exponential time decay and combined with a recent-window
/// distribution. Both components are Dirichlet-smoothed.
///
/// `legal_moves` is the number of legal moves in the position. Moves that have
/// no observation receive a small residual mass, which keeps tiny samples from
/// being displayed as certain.
pub fn predict(
    book: &PlayerBook,
    key: u64,
    global: &[MoveStat],
    prior_source: PriorSource,
    legal_moves: usize,
) -> Prediction {
    let player_moves: BTreeMap<u32, PlayerMoveStat> = book
        .positions
        .get(&key)
        .map(|moves| moves.iter().map(|(&code, stat)| (code, *stat)).collect())
        .unwrap_or_default();

    let mut candidates: BTreeMap<u32, Candidate> = BTreeMap::new();
    for stat in global {
        let player_games = if prior_source == PriorSource::CurrentDatabase {
            player_moves.get(&stat.mv).map_or(0, |mv| mv.games)
        } else {
            0
        };
        let database_games = stat.games.saturating_sub(player_games);
        let entry = candidates.entry(stat.mv).or_default();
        entry.database_games = entry.database_games.saturating_add(database_games);
    }
    for &code in player_moves.keys() {
        candidates.entry(code).or_default();
    }
    if candidates.is_empty() {
        return Prediction {
            unseen_probability: if legal_moves > 0 { 1.0 } else { 0.0 },
            unseen_moves: legal_moves,
            legal_moves,
            prior_source,
            ..Prediction::default()
        };
    }

    let database_total: u64 = candidates
        .values()
        .map(|candidate| u64::from(candidate.database_games))
        .sum();
    let candidate_count = candidates.len() as f64;
    let total_weight: f64 = player_moves.values().map(|mv| mv.weighted).sum();
    let recent_weight: f64 = player_moves.values().map(|mv| mv.recent_weighted).sum();
    let unseen_moves = legal_moves.saturating_sub(candidates.len());
    let unseen_alpha = if unseen_moves > 0 {
        book.config.unseen_prior_games.max(0.0)
    } else {
        0.0
    };

    // The background corpus is itself evidence, but its influence is capped so
    // a multi-million-game reference cannot completely erase a player's
    // repertoire. Sample-size shrinkage remains explicit: sparse backgrounds
    // receive the configured floor, larger ones earn more strength.
    let background_strength = book.config.prior_games + (database_total as f64).sqrt().min(12.0);
    let mut prior_total = 0.0;
    for candidate in candidates.values_mut() {
        let share = if database_total > 0 {
            f64::from(candidate.database_games) / database_total as f64
        } else {
            1.0 / candidate_count
        };
        candidate.alpha = background_strength * share + book.config.smoothing;
        prior_total += candidate.alpha;
    }

    let decay_total = prior_total + unseen_alpha + total_weight;
    let recent_total = prior_total + unseen_alpha + recent_weight;
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

        // Approximate the marginal Beta posterior of this move against all
        // other moves. The mixture variance is conservative: it ignores the
        // positive covariance between the two shared-prior components.
        let decay_variance =
            beta_variance(decay_probability, prior_total + unseen_alpha + total_weight);
        let recent_variance = beta_variance(
            recent_probability,
            prior_total + unseen_alpha + recent_weight,
        );
        let variance =
            (1.0 - recent_mix).powi(2) * decay_variance + recent_mix.powi(2) * recent_variance;
        let margin = CREDIBLE_Z_90 * variance.sqrt();

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
            probability_low: (probability - margin).clamp(0.0, 1.0),
            probability_high: (probability + margin).clamp(0.0, 1.0),
            player_games: player.games,
            recent_games: player.recent_games,
            last_date: player.last_date,
            prior_games: candidate.database_games,
            trend_pp,
        });
    }

    moves.sort_by(|a, b| {
        b.probability
            .total_cmp(&a.probability)
            .then(b.player_games.cmp(&a.player_games))
            .then(a.mv.cmp(&b.mv))
    });

    let unseen_probability = if unseen_alpha > 0.0 {
        let decay_unseen = unseen_alpha / decay_total;
        let recent_unseen = if recent_weight > 0.0 {
            unseen_alpha / recent_total
        } else {
            decay_unseen
        };
        (1.0 - recent_mix) * decay_unseen + recent_mix * recent_unseen
    } else {
        0.0
    };

    Prediction {
        moves,
        player_games,
        recent_games,
        recent_mix,
        has_player_data: total_weight > 0.0,
        unseen_probability,
        unseen_moves,
        legal_moves,
        prior_games: database_total.min(u64::from(u32::MAX)) as u32,
        prior_source,
    }
}

/// Standard normal quantile for a two-sided 90% interval.
const CREDIBLE_Z_90: f64 = 1.644_853_626_951_472_2;

/// Beta variance for a marginal with mean `mean` and concentration
/// `alpha + beta`.
fn beta_variance(mean: f64, concentration: f64) -> f64 {
    if concentration <= 0.0 {
        return 0.0;
    }
    mean * (1.0 - mean) / (concentration + 1.0)
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

pub(crate) fn date_to_days(date: u32) -> Option<i64> {
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
    use crate::chess::{Chess, Position, SanPlus};
    use crate::domain;

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
    fn directory_keeps_the_strongest_recognized_title() {
        const TITLED_GAMES: &str = r#"[Event "Titles"]
[Site "Test"]
[Date "2024.01.01"]
[White "Carlsen, Magnus"]
[WhiteTitle "GM"]
[Black "Doe, Jane"]
[BlackTitle "WIM"]
[Result "1-0"]

1. e4 e5 1-0

[Event "Titles"]
[Site "Test"]
[Date "2024.02.01"]
[White "Carlsen, Magnus"]
[WhiteTitle "FM"]
[Black "Roe, Richard"]
[BlackTitle "2500"]
[Result "1/2-1/2"]

1. d4 d5 1/2-1/2
"#;
        let mut path = std::env::temp_dir();
        path.push(format!(
            "brett-titles-{}-{}.pgn",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, TITLED_GAMES).unwrap();
        let library = PgnLibrary::build(&path).unwrap();
        let directory = PlayerDirectory::build(&library);

        let magnus = directory
            .get(directory.find_key("carlsen magnus").unwrap())
            .unwrap();
        assert_eq!(magnus.title, Some(crate::titles::PlayerTitle::Gm));
        let jane = directory
            .get(directory.find_key("doe jane").unwrap())
            .unwrap();
        assert_eq!(jane.title, Some(crate::titles::PlayerTitle::Wim));
        let richard = directory
            .get(directory.find_key("richard roe").unwrap())
            .unwrap();
        assert_eq!(richard.title, None, "a rating is not a title");

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn recent_games_are_newest_first_and_limited() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "brett-recent-{}-{}.pgn",
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
        let summary = directory.get(index).unwrap();
        let book = PlayerBook::build(
            &library,
            summary,
            PredictionConfig::default(),
            2026_1001,
            |_, _| {},
        )
        .unwrap();

        let start = Chess::default();
        let key = position::position_key(&start);
        let recent = book.recent_games(key, 3);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].date, 2026_0601);
        assert_eq!(recent[1].date, 2026_0101);
        assert_eq!(recent[2].date, 2025_0601);
        assert!(recent.iter().all(|game| game.color == Color::Black));
        assert_eq!(recent[0].index, 7);
        assert!(
            book.recent_games_for(key, Color::White, 10).is_empty(),
            "the prepared player only has Black games in this fixture"
        );
        let black = book.recent_games_for(key, Color::Black, 2);
        assert_eq!(black.len(), 2);
        assert_eq!(black[0].index, 7);

        // The final position has no outgoing move, but the game still reached
        // it and must appear in recent-game lookups.
        let newest = library.load(7).unwrap();
        let leaf = newest.nodes.last().unwrap();
        let leaf_key = position::position_key(&leaf.pos);
        assert!(
            book.recent_games(leaf_key, 10)
                .iter()
                .any(|game| game.index == 7)
        );

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn recent_switch_beats_large_old_sample() {
        let book = trend_book();
        let start = Chess::default();
        let after_e4 = start
            .clone()
            .play(
                SanPlus::from_ascii(b"e4")
                    .unwrap()
                    .san
                    .to_move(&start)
                    .unwrap(),
            )
            .unwrap();
        let key = position::position_key(&after_e4);
        let prediction = predict(
            &book,
            key,
            &[],
            PriorSource::CurrentDatabase,
            after_e4.legal_moves().len(),
        );
        assert!(prediction.has_player_data);
        assert_eq!(prediction.player_games, 8);
        let c5 = prediction
            .moves
            .iter()
            .find(|mv| {
                position::decode_move(&after_e4, mv.mv)
                    .map(|mv| SanPlus::from_move(after_e4.clone(), mv).to_string())
                    .as_deref()
                    == Some("c5")
            })
            .unwrap();
        let e6 = prediction
            .moves
            .iter()
            .find(|mv| {
                position::decode_move(&after_e4, mv.mv)
                    .map(|mv| SanPlus::from_move(after_e4.clone(), mv).to_string())
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
        let start = Chess::default();
        let key = position::position_key(&start);
        let global = vec![
            MoveStat {
                key,
                mv: position::encode_move(
                    SanPlus::from_ascii(b"e4")
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
                    SanPlus::from_ascii(b"d4")
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
        let prediction = predict(
            &book,
            key,
            &global,
            PriorSource::CurrentDatabase,
            start.legal_moves().len(),
        );
        assert!(!prediction.has_player_data);
        assert_eq!(prediction.moves[0].mv, global[0].mv);
        assert!(prediction.moves[0].probability > 0.8);
        assert!(prediction.moves[0].probability < 1.0);
        assert!(prediction.moves[0].probability_high > prediction.moves[0].probability);
        assert!(prediction.unseen_probability > 0.0);
    }

    #[test]
    fn tiny_sample_is_not_certain() {
        let pgn = r#"
[Event "First"]
[Site "Test"]
[Date "2025.01.01"]
[White "Opponent"]
[Black "Twice"]
[Result "1-0"]

1. e4 e5 2. Nf3 Nc6 1-0

[Event "Second"]
[Site "Test"]
[Date "2025.02.01"]
[White "Opponent"]
[Black "Twice"]
[Result "0-1"]

1. e4 e5 2. Bc4 Nf6 0-1
"#;
        let games = domain::parse_pgn(pgn.as_bytes()).unwrap();
        let book = PlayerBook::from_games(
            "twice",
            "Twice",
            &games,
            PredictionConfig::default(),
            2025_0301,
        );
        let start = Chess::default();
        let after_e4 = start
            .clone()
            .play(
                SanPlus::from_ascii(b"e4")
                    .unwrap()
                    .san
                    .to_move(&start)
                    .unwrap(),
            )
            .unwrap();
        let key = position::position_key(&after_e4);
        let prediction = predict(
            &book,
            key,
            &[],
            PriorSource::CurrentDatabase,
            after_e4.legal_moves().len(),
        );
        assert_eq!(prediction.player_games, 2);
        assert_eq!(prediction.moves.len(), 1);
        let move_prediction = prediction.moves[0];
        assert!(move_prediction.probability < 1.0);
        assert!(move_prediction.probability > 0.8);
        assert!(move_prediction.probability_low < move_prediction.probability);
        assert!(move_prediction.probability_high > move_prediction.probability);
        assert!(prediction.unseen_probability > 0.0);
        assert_eq!(prediction.unseen_moves, after_e4.legal_moves().len() - 1);
        assert_eq!(prediction.confidence_label(), "low confidence");
        assert!((move_prediction.probability + prediction.unseen_probability - 1.0).abs() < 1e-9);
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
