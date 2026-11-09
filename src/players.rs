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

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::chess::{Color, Position, UciMove};
use anyhow::{Context, Result, bail};

use crate::country::{self, Country};
use crate::domain::Game;
use crate::fide;
#[cfg(test)]
use crate::library::PgnLibrary;
use crate::library::{GameLibrary, SourceMeta};
use crate::online::OnlineSource;
use crate::position::{self, MoveStat, TREND_BUCKETS};
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
    /// First real numeric `WhiteFideId`/`BlackFideId` seen in this player's
    /// games. The raw tag stays untouched; this is only a display aggregate.
    pub fide_id: Option<String>,
    /// First recognized country or federation seen in this player's games.
    /// The raw tag stays untouched; this is only a display aggregate.
    pub country: Option<Country>,
    /// All federations resolved from Wikipedia/Wikidata enrichment, ordered
    /// oldest → latest (last is the latest/current federation). Empty until
    /// a wiki lookup enriches this player.
    pub wiki_countries: Vec<Country>,
    /// Birth year from Wikidata `P569`, when enrichment has supplied it.
    pub birth_year: Option<i32>,
    /// Peak Elo from Wikidata `P1087`, when enrichment has supplied it.
    pub peak_elo: Option<u16>,
    pub white: PlayerRecord,
    pub black: PlayerRecord,
    pub elo_sum: u64,
    pub elo_count: u32,
    pub elo_max: u32,
    pub latest_otb_elo: Option<u16>,
    pub latest_otb_date: u32,
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

    pub fn display_elo(&self) -> Option<u16> {
        self.latest_otb_elo
            .or_else(|| (self.elo_max > 0).then_some(self.elo_max as u16))
    }
}

pub type Player = PlayerSummary;
pub type PlayerIndex = PlayerDirectory;

/// All players found in the database, sorted by activity.
#[derive(Clone, Debug, Default)]
pub struct PlayerDirectory {
    players: Vec<PlayerSummary>,
    /// One-entry memo for `filter`: (normalized query, result).
    memo: std::cell::RefCell<Option<(String, Vec<usize>)>>,
}

impl PlayerDirectory {
    pub fn build(library: &dyn GameLibrary) -> Self {
        Self::from_library(library)
    }

    /// Build the directory from a game library, using a backend accelerator
    /// (`.2lgd`, `.brettacc`) for O(1) event game IDs and player lists when
    /// the backend provides one, and falling back to a full scan otherwise.
    pub fn from_library(library: &dyn GameLibrary) -> Self {
        if let Some(accelerated) = library.accelerated_player_games() {
            return Self::from_accelerated(library, &accelerated);
        }
        Self::from_scan(library)
    }

    /// Group pre-resolved accelerator player lists into summaries. When the
    /// library keeps headers in memory the colour split, titles, Elos and
    /// years are exact; for lazy backends the per-colour game counts,
    /// Elo peak and years are exact and the combined WDL totals are kept
    /// on the majority colour's record (only the colour split is approximate).
    fn from_accelerated(
        library: &dyn GameLibrary,
        accelerated: &[crate::library::AcceleratedPlayer],
    ) -> Self {
        let eager = !library.games().is_empty();
        let mut players: Vec<PlayerSummary> = Vec::with_capacity(accelerated.len());
        for accelerated in accelerated {
            let key = player_key(&accelerated.name)
                .unwrap_or_else(|| accelerated.name.trim().to_lowercase());
            let mut summary = PlayerSummary {
                name: accelerated.name.clone(),
                key,
                title: None,
                fide_id: None,
                country: None,
                wiki_countries: Vec::new(),
                birth_year: None,
                peak_elo: None,
                white: PlayerRecord::default(),
                black: PlayerRecord::default(),
                elo_sum: 0,
                elo_count: 0,
                elo_max: accelerated.elo_max,
                latest_otb_elo: None,
                latest_otb_date: 0,
                opponent_elo_sum: 0,
                opponent_elo_count: 0,
                first_year: accelerated.first_year,
                last_year: accelerated.last_year,
                game_indices: Vec::new(),
            };
            if eager {
                for &index in &accelerated.indices {
                    let Some(game) = library.games().get(index) else {
                        continue;
                    };
                    let white_match =
                        player_key(game.white()).as_deref() == Some(summary.key.as_str());
                    let black_match =
                        player_key(game.black()).as_deref() == Some(summary.key.as_str());
                    if white_match && !black_match {
                        add_game(&mut summary, game, Color::White, index, None);
                    } else if black_match && !white_match {
                        add_game(&mut summary, game, Color::Black, index, None);
                    } else if white_match && black_match {
                        add_game(&mut summary, game, Color::White, index, Some(Color::Black));
                    }
                }
                // Accelerators that resolve to no loadable game keep their
                // pre-aggregated counts instead of vanishing.
                if summary.game_indices.is_empty() {
                    apply_accelerated_counts(&mut summary, accelerated);
                }
            } else {
                apply_accelerated_counts(&mut summary, accelerated);
            }
            players.push(summary);
        }
        players.sort_by(|a, b| {
            b.games()
                .cmp(&a.games())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Self {
            players,
            memo: std::cell::RefCell::new(None),
        }
    }

    fn from_scan(library: &dyn GameLibrary) -> Self {
        let mut by_key: HashMap<String, PlayerSummary> = HashMap::new();

        if !library.games().is_empty() {
            for (index, game) in library.games().iter().enumerate() {
                let white_key = player_key(game.white());
                let black_key = player_key(game.black());
                let same_player = white_key.is_some() && white_key == black_key;

                if let Some(key) = &white_key {
                    let summary = by_key.entry(key.clone()).or_insert_with(|| PlayerSummary {
                        name: game.white().to_string(),
                        key: key.clone(),
                        title: None,
                        fide_id: None,
                        country: None,
                        wiki_countries: Vec::new(),
                        birth_year: None,
                        peak_elo: None,
                        white: PlayerRecord::default(),
                        black: PlayerRecord::default(),
                        elo_sum: 0,
                        elo_count: 0,
                        elo_max: 0,
                        latest_otb_elo: None,
                        latest_otb_date: 0,
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
                        fide_id: None,
                        country: None,
                        wiki_countries: Vec::new(),
                        birth_year: None,
                        peak_elo: None,
                        white: PlayerRecord::default(),
                        black: PlayerRecord::default(),
                        elo_sum: 0,
                        elo_count: 0,
                        elo_max: 0,
                        latest_otb_elo: None,
                        latest_otb_date: 0,
                        opponent_elo_sum: 0,
                        opponent_elo_count: 0,
                        first_year: 0,
                        last_year: 0,
                        game_indices: Vec::new(),
                    });
                    add_game(summary, game, Color::Black, index, None);
                }
            }
        } else {
            let total = library.len();
            for index in 0..total {
                let (white_name, black_name) = library.game_players(index);
                let white_key = player_key(&white_name);
                let black_key = player_key(&black_name);
                let same_player = white_key.is_some() && white_key == black_key;

                if let Some(key) = &white_key {
                    let summary = by_key.entry(key.clone()).or_insert_with(|| PlayerSummary {
                        name: white_name.clone(),
                        key: key.clone(),
                        title: None,
                        fide_id: None,
                        country: None,
                        wiki_countries: Vec::new(),
                        birth_year: None,
                        peak_elo: None,
                        white: PlayerRecord::default(),
                        black: PlayerRecord::default(),
                        elo_sum: 0,
                        elo_count: 0,
                        elo_max: 0,
                        latest_otb_elo: None,
                        latest_otb_date: 0,
                        opponent_elo_sum: 0,
                        opponent_elo_count: 0,
                        first_year: 0,
                        last_year: 0,
                        game_indices: Vec::new(),
                    });
                    summary.white.games += 1;
                    summary.game_indices.push(index);
                }

                if !same_player && let Some(key) = &black_key {
                    let summary = by_key.entry(key.clone()).or_insert_with(|| PlayerSummary {
                        name: black_name.clone(),
                        key: key.clone(),
                        title: None,
                        fide_id: None,
                        country: None,
                        wiki_countries: Vec::new(),
                        birth_year: None,
                        peak_elo: None,
                        white: PlayerRecord::default(),
                        black: PlayerRecord::default(),
                        elo_sum: 0,
                        elo_count: 0,
                        elo_max: 0,
                        latest_otb_elo: None,
                        latest_otb_date: 0,
                        opponent_elo_sum: 0,
                        opponent_elo_count: 0,
                        first_year: 0,
                        last_year: 0,
                        game_indices: Vec::new(),
                    });
                    summary.black.games += 1;
                    summary.game_indices.push(index);
                }
            }
        }

        let mut players: Vec<_> = by_key.into_values().collect();
        players.sort_by(|a, b| {
            b.games()
                .cmp(&a.games())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Self {
            players,
            memo: std::cell::RefCell::new(None),
        }
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

    pub fn get_mut(&mut self, index: usize) -> Option<&mut PlayerSummary> {
        self.players.get_mut(index)
    }

    pub fn players_mut(&mut self) -> &mut [PlayerSummary] {
        &mut self.players
    }

    pub fn find_key(&self, key: &str) -> Option<usize> {
        self.players.iter().position(|player| player.key == key)
    }

    /// Return the index of a player, adding an empty summary if not yet present.
    pub fn ensure_player(&mut self, name: &str) -> usize {
        let key = player_key(name).unwrap_or_else(|| name.trim().to_lowercase());
        if let Some(index) = self.find_key(&key) {
            return index;
        }
        let summary = PlayerSummary {
            name: name.trim().to_string(),
            key,
            title: None,
            fide_id: None,
            country: None,
            wiki_countries: Vec::new(),
            birth_year: None,
            peak_elo: None,
            white: PlayerRecord::default(),
            black: PlayerRecord::default(),
            elo_sum: 0,
            elo_count: 0,
            elo_max: 0,
            latest_otb_elo: None,
            latest_otb_date: 0,
            opponent_elo_sum: 0,
            opponent_elo_count: 0,
            first_year: 0,
            last_year: 0,
            game_indices: Vec::new(),
        };
        self.players.push(summary);
        self.players.len() - 1
    }

    pub fn filter(&self, query: &str) -> Vec<usize> {
        // On huge directories fuzzy only runs on Enter: `filter` is the
        // per-frame path, so it stays exact/substring-only above 150k.
        let allow_fuzzy = self.players.len() <= 150_000;
        self.filter_impl(query, allow_fuzzy)
    }

    /// Fuzzy-capable filter used on Enter (large directories) and in tests.
    pub fn filter_on_enter(&self, query: &str) -> Vec<usize> {
        self.filter_impl(query, true)
    }

    /// Filter with an explicit fuzzy allowance (25 ms budget when fuzzy).
    pub fn filter_with_fuzzy(&self, query: &str, allow_fuzzy: bool) -> Vec<usize> {
        self.filter_impl(query, allow_fuzzy)
    }

    fn filter_impl(&self, query: &str, allow_fuzzy: bool) -> Vec<usize> {
        let q_norm = normalize_search_text(query);
        if q_norm.is_empty() {
            return (0..self.players.len()).collect();
        }
        let memo_key = format!("{q_norm}|{allow_fuzzy}");
        {
            let memo = self.memo.borrow();
            if let Some((cached_key, cached)) = memo.as_ref()
                && cached_key == &memo_key
            {
                return cached.clone();
            }
        }
        let q_tokens: Vec<String> = q_norm.split_whitespace().map(str::to_string).collect();
        let q_collapsed: String = q_tokens.join("");
        let q_len = q_collapsed.chars().count();
        let started = std::time::Instant::now();
        let mut exact: Vec<(u8, usize)> = Vec::new(); // tier 0=exact 1=substring
        for (index, player) in self.players.iter().enumerate() {
            let p_norm = normalize_search_text(&player.name);
            if p_norm.is_empty() {
                continue;
            }
            let p_tokens: Vec<String> = p_norm.split_whitespace().map(str::to_string).collect();
            let p_collapsed: String = p_tokens.join("");
            if p_norm == q_norm || p_collapsed == q_collapsed {
                exact.push((0, index));
                continue;
            }
            // Substring: whole normalized string, collapsed string, or every
            // query token (or its prefix) appears in the player name.
            let substring = p_norm.contains(&q_norm)
                || p_collapsed.contains(&q_collapsed)
                || (!q_tokens.is_empty()
                    && q_tokens.iter().all(|qt| {
                        p_tokens.iter().any(|pt| pt.contains(qt.as_str()))
                            || p_collapsed.contains(qt.as_str())
                    }));
            if substring {
                exact.push((1, index));
            }
        }
        exact.sort_by_key(|(tier, idx)| (*tier, *idx));
        let mut result: Vec<usize> = exact.iter().map(|(_, idx)| *idx).collect();

        let do_fuzzy = allow_fuzzy && result.len() < 8 && q_len >= 4;
        if do_fuzzy {
            let mut matched: std::collections::HashSet<usize> = result.iter().copied().collect();
            let mut fuzzy: Vec<(usize, usize)> = Vec::new(); // (distance, index)
            for (index, player) in self.players.iter().enumerate() {
                if matched.contains(&index) {
                    continue;
                }
                if index % 1024 == 0 && started.elapsed() > std::time::Duration::from_millis(25) {
                    break;
                }
                let p_norm = normalize_search_text(&player.name);
                let p_tokens: Vec<String> = p_norm.split_whitespace().map(str::to_string).collect();
                if p_tokens.is_empty() {
                    continue;
                }
                // Bounded edit distance: <=1 for len 4..=6, <=2 for >=7.
                let mut total_dist = 0usize;
                let mut all_matched = true;
                for qt in &q_tokens {
                    let qt_len = qt.chars().count();
                    let max_dist = if qt_len >= 7 {
                        2
                    } else if qt_len >= 4 {
                        1
                    } else {
                        0
                    };
                    if max_dist == 0 {
                        all_matched = false;
                        break;
                    }
                    let best = p_tokens
                        .iter()
                        .map(|pt| levenshtein(qt, pt))
                        .min()
                        .unwrap_or(usize::MAX);
                    // Also consider collapsed player string for single-token
                    // queries spanning a space (e.g. makefrenhc vs make french).
                    let collapsed_best = if q_tokens.len() == 1 {
                        let pc: String = p_tokens.join("");
                        levenshtein(qt, &pc)
                    } else {
                        usize::MAX
                    };
                    let best = best.min(collapsed_best);
                    if best > max_dist {
                        all_matched = false;
                        break;
                    }
                    total_dist += best;
                }
                if all_matched {
                    matched.insert(index);
                    fuzzy.push((total_dist, index));
                }
            }
            fuzzy.sort();
            result.extend(fuzzy.into_iter().map(|(_, idx)| idx));
        }
        *self.memo.borrow_mut() = Some((memo_key, result.clone()));
        result
    }

    pub fn find_player(&self, query: &str) -> Option<&PlayerSummary> {
        let key = player_key(query).unwrap_or_else(|| query.trim().to_lowercase());
        self.find_key(&key)
            .and_then(|idx| self.get(idx))
            .or_else(|| {
                self.players
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(query.trim()))
            })
    }
}

// ---------------------------------------------------------------------------
// PlayerDirectory sidecar (`.brettplayers`)
// ---------------------------------------------------------------------------

/// On-disk format version for [`PlayerDirectory`] sidecars.
///
/// Bump this (and the loader's expected version) whenever the layout changes;
/// older files are ignored so a stale sidecar is rebuilt instead of decoded.
pub const BRETT_PLAYERS_V: u32 = 1;

/// Minimum library size (games) for which the sidecar is used.
///
/// Below this threshold the directory is built in memory and never cached; the
/// build cost is negligible and a sidecar adds only overhead. The app checks
/// this cap before consulting the sidecar; the helpers here are usable at any
/// size (tests exercise them with tiny directories).
pub const PLAYERS_SIDECAR_MIN_GAMES: usize = 100_000;

const PLAYERS_MAGIC: &[u8; 8] = b"BRETTPLY";
const PLAYERS_HEADER_LEN: usize = 80;

/// Cache location for the player directory of `source`.
///
/// The index is never written next to a database or PGN file in the cache
/// layout: the file is `<hash>.brettplayers` under `~/.cache/brett/players/`
/// (or `$XDG_CACHE_HOME/brett/players/`) where `<hash>` is the same FNV-1a of
/// the canonical source path used for position indexes. The `SourceMeta`
/// fingerprint (len + mtime) inside the file still decides freshness.
pub fn players_cache_path(source: &Path) -> PathBuf {
    players_cache_dir().join(format!("{:016x}.brettplayers", fnv_path_hash(source)))
}

/// Sidecar location next to `source` (e.g. `mega.brettplayers`).
///
/// Kept as an alternative to the hashed cache path for the same database.
/// The app currently prefers [`players_cache_path`]; both layouts are
/// recognised by the format and either may be used by tests.
pub fn players_sidecar_path(source: &Path) -> PathBuf {
    source.with_extension("brettplayers")
}

fn players_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(dir).join("brett").join("players");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("brett")
            .join("players");
    }
    std::env::temp_dir().join("brett-players")
}

fn fnv_path_hash(source: &Path) -> u64 {
    let canonical = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in canonical.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// FNV-1a hash of the source path plus its [`SourceMeta`] fingerprint.
///
/// Convenience fingerprint for logging; the canonical freshness check in the
/// sidecar is exact equality of `len`/`mtime_secs`/`mtime_nanos` against the
/// header, not this hash, so a hash collision can never admit a stale file.
pub fn players_fingerprint(source: &Path, meta: &SourceMeta) -> u64 {
    let mut hash = fnv_path_hash(source);
    for value in [meta.len, meta.mtime_secs] {
        for byte in value.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    for byte in meta.mtime_nanos.to_le_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// Binary sidecar format (`BRETT_PLAYERS_V = 1`).
///
/// All integers are little-endian unless noted.
///
/// Header, 80 bytes:
/// ```text
///  0..8   magic `BRETTPLY`
///  8..12  version u32 == BRETT_PLAYERS_V
/// 12..16  reserved (0)
/// 16..24  source_len u64 (SourceMeta.len)
/// 24..32  source_mtime_secs u64
/// 32..36  source_mtime_nanos u32
/// 36..40  reserved (0)
/// 40..48  player_count u64
/// 48..56  total_game_indices u64 (informational sum)
/// 56..64  reserved (0)
/// 64..72  build_millis u64 (informational, 0 from save_to)
/// 72..80  reserved (0)
/// ```
///
/// Then `player_count` records in directory order, each:
/// ```text
/// name_len u32, name bytes (UTF-8)
/// key_len u32, key bytes
/// title u8 (0=None, 1=Gm, 2=Igm, 3=Cgm, 4=Im, 5=Sim, 6=Ccm,
///           7=Fm, 8=Cm, 9=Wgm, 10=Wim, 11=Wfm, 12=Wcm, 13=Nm, 14=Lm)
/// country_present u8; if 1: code_len u8, code bytes (FIDE 3-letter code)
/// fide_present u8; if 1: fide_len u32, fide bytes
/// wiki_count u32; then wiki_count entries, each: code_len u8, code bytes
/// birth_present u8; if 1: birth_year i32 LE
/// peak_present u8; if 1: peak_elo u16 LE
/// white games/wins/draws/losses: u32 LE x 4
/// black games/wins/draws/losses: u32 LE x 4
/// elo_sum u64, elo_count u32, elo_max u32
/// latest_otb_elo_present u8; if 1: u16 LE
/// latest_otb_date u32
/// opponent_elo_sum u64, opponent_elo_count u32
/// first_year u16, last_year u16
/// indices_count u32; then indices_count varints
/// ```
///
/// `game_indices` are unsigned LEB128 varints of zigzag-encoded deltas: the
/// first delta is `indices[0] - 0`, each later delta is
/// `indices[i] - indices[i-1]`, zigzag mapped as `(n << 1) ^ (n >> 63)`
/// (decoded as `(v >> 1) ^ -(v & 1)`). Sorted postings — the common case for
/// both accelerator and scan builds — cost roughly 2-3 bytes per index, so a
/// 476k-player / 11M-game directory (about 22M indices) stays in the tens of
/// megabytes and decodes in well under the ~50 s rebuild cost. Unsorted
/// postings stay lossless. Country/wiki codes are resolved on load through
/// [`crate::country::normalize`], so an unknown code degrades to `None`
/// instead of failing the whole file.
impl PlayerDirectory {
    /// Serialize this directory to `path`, fingerprinted by `meta`.
    ///
    /// The write is atomic: data goes to a temporary sibling and is renamed
    /// over `path`. Parent directories are created best-effort. Callers
    /// should treat errors as non-fatal (best-effort cache) — the app always
    /// has the in-memory directory to install.
    pub fn save_to(&self, path: &Path, meta: &SourceMeta) -> Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating player cache {}", parent.display()))?;
        }
        let mut header = [0u8; PLAYERS_HEADER_LEN];
        header[0..8].copy_from_slice(PLAYERS_MAGIC);
        header[8..12].copy_from_slice(&BRETT_PLAYERS_V.to_le_bytes());
        header[16..24].copy_from_slice(&meta.len.to_le_bytes());
        header[24..32].copy_from_slice(&meta.mtime_secs.to_le_bytes());
        header[32..36].copy_from_slice(&meta.mtime_nanos.to_le_bytes());
        header[40..48].copy_from_slice(&(self.players.len() as u64).to_le_bytes());
        let total_indices: u64 = self
            .players
            .iter()
            .map(|player| player.game_indices.len() as u64)
            .sum();
        header[48..56].copy_from_slice(&total_indices.to_le_bytes());
        // build_millis at 64..72 left 0: the build time is logged by the
        // caller and is not needed for validation.

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp = path.with_file_name(format!(
            "{}.tmp-{}-{}",
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "players".to_string()),
            std::process::id(),
            unique
        ));
        {
            let file = std::fs::File::create(&tmp)
                .with_context(|| format!("creating player sidecar {}", tmp.display()))?;
            let mut writer = std::io::BufWriter::with_capacity(64 * 1024, file);
            writer
                .write_all(&header)
                .with_context(|| format!("writing player sidecar {}", path.display()))?;
            let mut scratch: Vec<u8> = Vec::with_capacity(4096);
            for player in &self.players {
                scratch.clear();
                encode_player(&mut scratch, player)
                    .with_context(|| format!("encoding player {}", player.key))?;
                writer
                    .write_all(&scratch)
                    .with_context(|| format!("writing player sidecar {}", path.display()))?;
            }
            writer
                .flush()
                .with_context(|| format!("writing player sidecar {}", path.display()))?;
        }
        std::fs::rename(&tmp, path)
            .with_context(|| format!("renaming player sidecar {}", path.display()))?;
        Ok(())
    }

    /// Load a directory from `path` iff its fingerprint matches `expected`.
    ///
    /// Returns `Ok(None)` for a missing, stale (fingerprint mismatch),
    /// version-mismatched or truncated/corrupt file — the caller should
    /// rebuild. Only unexpected I/O failures are also mapped to `Ok(None)`
    /// (with a warning) because the sidecar is a pure cache: a failed load
    /// must never block database opening.
    pub fn load_from(path: &Path, expected: &SourceMeta) -> Result<Option<Self>> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                tracing::warn!(
                    "ignoring unreadable player cache {}: {err:#}",
                    path.display()
                );
                return Ok(None);
            }
        };
        match Self::decode_bytes(&bytes, expected) {
            Ok(directory) => Ok(Some(directory)),
            Err(err) => {
                tracing::warn!(
                    "ignoring stale or invalid player cache {}: {err:#}",
                    path.display()
                );
                Ok(None)
            }
        }
    }

    fn decode_bytes(bytes: &[u8], expected: &SourceMeta) -> Result<Self> {
        let mut pos = 0usize;
        decode_header(bytes, &mut pos, expected)?;
        let player_count = decode_u64_le(bytes, &mut pos)?;
        // Header reserved fields are ignored; total_game_indices is
        // informational and not re-validated.
        let _total_indices = decode_u64_le(bytes, &mut pos)?;
        let _reserved = decode_u64_le(bytes, &mut pos)?;
        let _build_millis = decode_u64_le(bytes, &mut pos)?;
        let _reserved = decode_u64_le(bytes, &mut pos)?;
        if pos != PLAYERS_HEADER_LEN {
            bail!("corrupt player sidecar header");
        }
        let mut players = Vec::with_capacity(player_count.min(1_000_000) as usize);
        for _ in 0..player_count {
            players.push(decode_player(bytes, &mut pos)?);
        }
        if pos != bytes.len() {
            bail!("trailing bytes in player sidecar");
        }
        Ok(Self {
            players,
            memo: std::cell::RefCell::new(None),
        })
    }
}

fn title_to_byte(title: Option<PlayerTitle>) -> u8 {
    match title {
        None => 0,
        Some(PlayerTitle::Gm) => 1,
        Some(PlayerTitle::Igm) => 2,
        Some(PlayerTitle::Cgm) => 3,
        Some(PlayerTitle::Im) => 4,
        Some(PlayerTitle::Sim) => 5,
        Some(PlayerTitle::Ccm) => 6,
        Some(PlayerTitle::Fm) => 7,
        Some(PlayerTitle::Cm) => 8,
        Some(PlayerTitle::Wgm) => 9,
        Some(PlayerTitle::Wim) => 10,
        Some(PlayerTitle::Wfm) => 11,
        Some(PlayerTitle::Wcm) => 12,
        Some(PlayerTitle::Nm) => 13,
        Some(PlayerTitle::Lm) => 14,
    }
}

fn title_from_byte(value: u8) -> Result<Option<PlayerTitle>> {
    Ok(match value {
        0 => None,
        1 => Some(PlayerTitle::Gm),
        2 => Some(PlayerTitle::Igm),
        3 => Some(PlayerTitle::Cgm),
        4 => Some(PlayerTitle::Im),
        5 => Some(PlayerTitle::Sim),
        6 => Some(PlayerTitle::Ccm),
        7 => Some(PlayerTitle::Fm),
        8 => Some(PlayerTitle::Cm),
        9 => Some(PlayerTitle::Wgm),
        10 => Some(PlayerTitle::Wim),
        11 => Some(PlayerTitle::Wfm),
        12 => Some(PlayerTitle::Wcm),
        13 => Some(PlayerTitle::Nm),
        14 => Some(PlayerTitle::Lm),
        _ => bail!("unknown player title code {value}"),
    })
}

fn zigzag_encode(delta: i64) -> u64 {
    ((delta << 1) ^ (delta >> 63)) as u64
}

fn zigzag_decode(value: u64) -> i64 {
    ((value >> 1) as i64) ^ (-((value & 1) as i64))
}

fn write_varint(buf: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn encode_player(buf: &mut Vec<u8>, player: &PlayerSummary) -> Result<()> {
    write_string(buf, &player.name)?;
    write_string(buf, &player.key)?;
    buf.push(title_to_byte(player.title));
    match player.country {
        Some(country) => {
            buf.push(1);
            write_code(buf, country.code());
        }
        None => buf.push(0),
    }
    match &player.fide_id {
        Some(fide_id) => {
            buf.push(1);
            write_string(buf, fide_id)?;
        }
        None => buf.push(0),
    }
    let wiki_count = u32::try_from(player.wiki_countries.len())
        .map_err(|_| anyhow::anyhow!("too many wiki countries for one player"))?;
    buf.extend_from_slice(&wiki_count.to_le_bytes());
    for country in &player.wiki_countries {
        write_code(buf, country.code());
    }
    match player.birth_year {
        Some(year) => {
            buf.push(1);
            buf.extend_from_slice(&year.to_le_bytes());
        }
        None => buf.push(0),
    }
    match player.peak_elo {
        Some(elo) => {
            buf.push(1);
            buf.extend_from_slice(&elo.to_le_bytes());
        }
        None => buf.push(0),
    }
    encode_record(buf, &player.white);
    encode_record(buf, &player.black);
    buf.extend_from_slice(&player.elo_sum.to_le_bytes());
    buf.extend_from_slice(&player.elo_count.to_le_bytes());
    buf.extend_from_slice(&player.elo_max.to_le_bytes());
    match player.latest_otb_elo {
        Some(elo) => {
            buf.push(1);
            buf.extend_from_slice(&elo.to_le_bytes());
        }
        None => buf.push(0),
    }
    buf.extend_from_slice(&player.latest_otb_date.to_le_bytes());
    buf.extend_from_slice(&player.opponent_elo_sum.to_le_bytes());
    buf.extend_from_slice(&player.opponent_elo_count.to_le_bytes());
    buf.extend_from_slice(&player.first_year.to_le_bytes());
    buf.extend_from_slice(&player.last_year.to_le_bytes());
    let count = u32::try_from(player.game_indices.len())
        .map_err(|_| anyhow::anyhow!("too many game indices for one player"))?;
    buf.extend_from_slice(&count.to_le_bytes());
    let mut prev: i64 = 0;
    for &index in &player.game_indices {
        let delta = index as i64 - prev;
        write_varint(buf, zigzag_encode(delta));
        prev = index as i64;
    }
    Ok(())
}

fn encode_record(buf: &mut Vec<u8>, record: &PlayerRecord) {
    buf.extend_from_slice(&record.games.to_le_bytes());
    buf.extend_from_slice(&record.wins.to_le_bytes());
    buf.extend_from_slice(&record.draws.to_le_bytes());
    buf.extend_from_slice(&record.losses.to_le_bytes());
}

fn write_string(buf: &mut Vec<u8>, value: &str) -> Result<()> {
    let len = u32::try_from(value.len())
        .map_err(|_| anyhow::anyhow!("string too long for player sidecar"))?;
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(value.as_bytes());
    Ok(())
}

fn write_code(buf: &mut Vec<u8>, code: &str) {
    let bytes = code.as_bytes();
    let len = bytes.len().min(u8::MAX as usize) as u8;
    buf.push(len);
    buf.extend_from_slice(&bytes[..len as usize]);
}

fn decode_header(bytes: &[u8], pos: &mut usize, expected: &SourceMeta) -> Result<()> {
    if bytes.len() < PLAYERS_HEADER_LEN {
        bail!("player sidecar is shorter than {PLAYERS_HEADER_LEN} bytes");
    }
    if &bytes[0..8] != PLAYERS_MAGIC {
        bail!("not a Brett player sidecar");
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    if version != BRETT_PLAYERS_V {
        bail!("unsupported player sidecar version {version} (expected {BRETT_PLAYERS_V})");
    }
    *pos = 16;
    let source_len = decode_u64_le(bytes, pos)?;
    let source_mtime_secs = decode_u64_le(bytes, pos)?;
    let source_mtime_nanos = decode_u32_le(bytes, pos)?;
    let _reserved = decode_u32_le(bytes, pos)?;
    if source_len != expected.len
        || source_mtime_secs != expected.mtime_secs
        || source_mtime_nanos != expected.mtime_nanos
    {
        bail!(
            "player sidecar fingerprint mismatch (sidecar len={source_len} mtime={source_mtime_secs}, expected len={} mtime={})",
            expected.len,
            expected.mtime_secs
        );
    }
    Ok(())
}

fn need(bytes: &[u8], pos: &mut usize, count: usize) -> Result<()> {
    if bytes.len().saturating_sub(*pos) < count {
        bail!("truncated player sidecar");
    }
    *pos += count;
    Ok(())
}

fn decode_u8(bytes: &[u8], pos: &mut usize) -> Result<u8> {
    need(bytes, pos, 1)?;
    Ok(bytes[*pos - 1])
}

fn decode_u16_le(bytes: &[u8], pos: &mut usize) -> Result<u16> {
    need(bytes, pos, 2)?;
    let value = u16::from_le_bytes(bytes[*pos - 2..*pos].try_into().unwrap());
    Ok(value)
}

fn decode_u32_le(bytes: &[u8], pos: &mut usize) -> Result<u32> {
    need(bytes, pos, 4)?;
    let value = u32::from_le_bytes(bytes[*pos - 4..*pos].try_into().unwrap());
    Ok(value)
}

fn decode_u64_le(bytes: &[u8], pos: &mut usize) -> Result<u64> {
    need(bytes, pos, 8)?;
    let value = u64::from_le_bytes(bytes[*pos - 8..*pos].try_into().unwrap());
    Ok(value)
}

fn decode_i32_le(bytes: &[u8], pos: &mut usize) -> Result<i32> {
    need(bytes, pos, 4)?;
    let value = i32::from_le_bytes(bytes[*pos - 4..*pos].try_into().unwrap());
    Ok(value)
}

fn decode_string(bytes: &[u8], pos: &mut usize) -> Result<String> {
    let len = decode_u32_le(bytes, pos)? as usize;
    need(bytes, pos, len)?;
    let str_bytes = &bytes[*pos - len..*pos];
    String::from_utf8(str_bytes.to_vec())
        .map_err(|_| anyhow::anyhow!("invalid UTF-8 in player sidecar string"))
}

fn decode_code(bytes: &[u8], pos: &mut usize) -> Result<String> {
    let len = decode_u8(bytes, pos)? as usize;
    need(bytes, pos, len)?;
    let str_bytes = &bytes[*pos - len..*pos];
    String::from_utf8(str_bytes.to_vec())
        .map_err(|_| anyhow::anyhow!("invalid UTF-8 in player sidecar code"))
}

fn decode_varint(bytes: &[u8], pos: &mut usize) -> Result<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    for _ in 0..10 {
        if *pos >= bytes.len() {
            bail!("truncated varint in player sidecar");
        }
        let byte = bytes[*pos];
        *pos += 1;
        let low = u64::from(byte & 0x7f);
        result |= low.checked_shl(shift).unwrap_or(0);
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
    }
    bail!("varint overflow in player sidecar")
}

fn decode_record(bytes: &[u8], pos: &mut usize) -> Result<PlayerRecord> {
    Ok(PlayerRecord {
        games: decode_u32_le(bytes, pos)?,
        wins: decode_u32_le(bytes, pos)?,
        draws: decode_u32_le(bytes, pos)?,
        losses: decode_u32_le(bytes, pos)?,
    })
}

fn decode_player(bytes: &[u8], pos: &mut usize) -> Result<PlayerSummary> {
    let name = decode_string(bytes, pos)?;
    let key = decode_string(bytes, pos)?;
    let title = title_from_byte(decode_u8(bytes, pos)?)?;
    let country_present = decode_u8(bytes, pos)?;
    let country = match country_present {
        0 => None,
        1 => {
            let code = decode_code(bytes, pos)?;
            country::normalize(&code)
        }
        other => bail!("unknown country flag {other}"),
    };
    let fide_present = decode_u8(bytes, pos)?;
    let fide_id = match fide_present {
        0 => None,
        1 => Some(decode_string(bytes, pos)?),
        other => bail!("unknown fide flag {other}"),
    };
    let wiki_count = decode_u32_le(bytes, pos)?;
    let mut wiki_countries = Vec::with_capacity(wiki_count as usize);
    for _ in 0..wiki_count {
        let code = decode_code(bytes, pos)?;
        if let Some(country) = country::normalize(&code) {
            wiki_countries.push(country);
        }
    }
    let birth_present = decode_u8(bytes, pos)?;
    let birth_year = match birth_present {
        0 => None,
        1 => Some(decode_i32_le(bytes, pos)?),
        other => bail!("unknown birth flag {other}"),
    };
    let peak_present = decode_u8(bytes, pos)?;
    let peak_elo = match peak_present {
        0 => None,
        1 => Some(decode_u16_le(bytes, pos)?),
        other => bail!("unknown peak flag {other}"),
    };
    let white = decode_record(bytes, pos)?;
    let black = decode_record(bytes, pos)?;
    let elo_sum = decode_u64_le(bytes, pos)?;
    let elo_count = decode_u32_le(bytes, pos)?;
    let elo_max = decode_u32_le(bytes, pos)?;
    let latest_present = decode_u8(bytes, pos)?;
    let latest_otb_elo = match latest_present {
        0 => None,
        1 => Some(decode_u16_le(bytes, pos)?),
        other => bail!("unknown latest_otb_elo flag {other}"),
    };
    let latest_otb_date = decode_u32_le(bytes, pos)?;
    let opponent_elo_sum = decode_u64_le(bytes, pos)?;
    let opponent_elo_count = decode_u32_le(bytes, pos)?;
    let first_year = decode_u16_le(bytes, pos)?;
    let last_year = decode_u16_le(bytes, pos)?;
    let indices_count = decode_u32_le(bytes, pos)?;
    let mut game_indices = Vec::with_capacity(indices_count as usize);
    let mut prev: i64 = 0;
    for _ in 0..indices_count {
        let delta = zigzag_decode(decode_varint(bytes, pos)?);
        let index = prev + delta;
        if index < 0 {
            bail!("negative game index in player sidecar");
        }
        game_indices.push(index as usize);
        prev = index;
    }
    Ok(PlayerSummary {
        name,
        key,
        title,
        fide_id,
        country,
        wiki_countries,
        birth_year,
        peak_elo,
        white,
        black,
        elo_sum,
        elo_count,
        elo_max,
        latest_otb_elo,
        latest_otb_date,
        opponent_elo_sum,
        opponent_elo_count,
        first_year,
        last_year,
        game_indices,
    })
}

/// Normalize for player search: deunicode diacritics, lowercase, and turn
/// `_`, `-`, `,`, `.` (and any other punctuation) into spaces/tokens.
pub fn normalize_search_text(raw: &str) -> String {
    let deuni = deunicode::deunicode(raw);
    let mut out = String::with_capacity(deuni.len());
    for ch in deuni.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
        } else {
            // Covers `_`, `-`, `,`, `.` and other punctuation as separators.
            out.push(' ');
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn levenshtein(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.is_empty() {
        return right.len();
    }
    if right.is_empty() {
        return left.len();
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0usize; right.len() + 1];
    for (i, left_char) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, right_char) in right.iter().enumerate() {
            let substitution = previous[j] + usize::from(left_char != right_char);
            current[j + 1] = (previous[j + 1] + 1).min(current[j] + 1).min(substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

/// Fill a summary from pre-aggregated accelerator counts when headers are
/// not in memory. Colour game counts, Elo peak and years are exact; the
/// combined WDL totals sit on the majority colour's record so
/// [`PlayerSummary::wins`] and friends stay exact and only the colour split
/// is approximate.
fn apply_accelerated_counts(
    summary: &mut PlayerSummary,
    accelerated: &crate::library::AcceleratedPlayer,
) {
    summary.white.games = accelerated.white_games;
    summary.black.games = accelerated.black_games;
    summary.game_indices = accelerated.indices.clone();
    if accelerated.white_games >= accelerated.black_games {
        summary.white.wins = accelerated.wins;
        summary.white.draws = accelerated.draws;
        summary.white.losses = accelerated.losses;
    } else {
        summary.black.wins = accelerated.wins;
        summary.black.draws = accelerated.draws;
        summary.black.losses = accelerated.losses;
    }
}

fn add_game(
    summary: &mut PlayerSummary,
    game: &crate::library::IndexedGame,
    color: Color,
    index: usize,
    self_play_other: Option<Color>,
) {
    let (own_elo_tag, opponent_elo_tag, own_title_tag, own_fide_tag, own_country_tag, own_fed_tag) =
        match color {
            Color::White => (
                "WhiteElo",
                "BlackElo",
                "WhiteTitle",
                "WhiteFideId",
                "WhiteCountry",
                "WhiteFed",
            ),
            Color::Black => (
                "BlackElo",
                "WhiteElo",
                "BlackTitle",
                "BlackFideId",
                "BlackCountry",
                "BlackFed",
            ),
        };
    if let Some(title) = game.header(own_title_tag).and_then(titles::normalize)
        && summary
            .title
            .is_none_or(|current| title.rank() > current.rank())
    {
        summary.title = Some(title);
    }
    if summary.fide_id.is_none()
        && let Some(id) = fide::from_tag(game.header(own_fide_tag))
    {
        summary.fide_id = Some(id.to_string());
    }
    if summary.country.is_none() {
        let raw = game
            .header(own_country_tag)
            .or_else(|| game.header(own_fed_tag));
        if let Some(country) = country::from_header(raw) {
            summary.country = Some(country);
        }
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

        let date_num = parse_date(game.date()).unwrap_or(0);
        let site = game.header("Site").unwrap_or("?");
        if !looks_online(game.event(), site) && elo > 0 && date_num >= summary.latest_otb_date {
            summary.latest_otb_elo = Some(elo as u16);
            summary.latest_otb_date = date_num;
        }
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

/// Merge one colour's stat into a combined stat (for colour-blind views).
fn merge_stat(into: &mut PlayerMoveStat, from: &PlayerMoveStat) {
    into.games += from.games;
    into.wins += from.wins;
    into.draws += from.draws;
    into.losses += from.losses;
    into.recent_games += from.recent_games;
    into.weighted += from.weighted;
    into.recent_weighted += from.recent_weighted;
    into.otb_weighted += from.otb_weighted;
    into.online_weighted += from.online_weighted;
    for i in 0..4 {
        into.tc_weighted[i] += from.tc_weighted[i];
    }
    if from.first_date != 0 && (into.first_date == 0 || from.first_date < into.first_date) {
        into.first_date = from.first_date;
    }
    into.last_date = into.last_date.max(from.last_date);
}

fn accumulate_stats<'a>(
    stats: &mut PlayerPositionStats,
    moves: impl Iterator<Item = &'a PlayerMoveStat>,
) {
    for mv in moves {
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
}

fn is_recent_date(date: u32, as_of: u32, config: PredictionConfig) -> bool {
    if date == 0 {
        return false;
    }
    let (Some(d), Some(a)) = (date_to_days(date), date_to_days(as_of)) else {
        return false;
    };
    (a - d).max(0) <= config.recent_days
}

const HONORIFICS: &[&str] = &["dr", "prof", "ing", "mag", "dipl", "jr", "sr"];
const PLACEHOLDERS: &[&str] = &[
    "bye",
    "forfeit",
    "unpaired",
    "vacant",
    "unknown",
    "none",
    "n/a",
    "na",
    "tbd",
    "anonymous",
    "?",
];

/// Canonical player identity. Word order and punctuation are ignored so
/// `Carlsen, Magnus` and `Magnus Carlsen` resolve to the same person.
pub fn player_key(name: &str) -> Option<String> {
    let trimmed = name.trim().to_ascii_lowercase();
    if PLACEHOLDERS.contains(&trimmed.as_str()) {
        return None;
    }
    let words: Vec<String> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.to_lowercase())
        .collect();
    if words.is_empty() {
        return None;
    }
    if words.len() == 1 && PLACEHOLDERS.contains(&words[0].as_str()) {
        return None;
    }
    // Strip common academic/generational honorific tokens when at least one
    // non-honorific token remains. Generational suffixes ("jr", "sr") are
    // stripped when appended after punctuation (e.g. "Mustermann, Max, Jr."),
    // preserving distinct generational names like "Magnus Carlsen Jr".
    let non_honorifics: Vec<String> = words
        .iter()
        .filter(|word| {
            if !HONORIFICS.contains(&word.as_str()) {
                return true;
            }
            if *word == "jr" || *word == "sr" {
                let lower = name.to_ascii_lowercase();
                let is_suffixed = lower.match_indices(word.as_str()).any(|(idx, _)| {
                    let prefix = name[..idx].trim_end();
                    prefix.ends_with(',') || prefix.ends_with('.')
                });
                !is_suffixed
            } else {
                false
            }
        })
        .cloned()
        .collect();
    let mut words = if !non_honorifics.is_empty() {
        non_honorifics
    } else {
        words
    };
    words.sort_unstable();
    words.dedup();
    let key = words.join(" ");
    if PLACEHOLDERS.contains(&key.as_str())
        || (words.len() == 1 && PLACEHOLDERS.contains(&words[0].as_str()))
    {
        return None;
    }
    Some(key)
}

/// Split a name into normalized alphanumeric tokens, dropping standalone
/// numbers (ChessABC enumeration suffixes such as the `1` in `M 1`).
fn name_tokens(name: &str) -> Vec<String> {
    name.split(|c: char| !c.is_alphabetic())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .filter(|token| token.chars().any(|c| c.is_alphabetic()))
        .collect()
}

/// Surname of a normalized token list: the comma head (`Surname, Given`) or
/// the last token (`First Surname`).
fn surname_of(full: &str, tokens: &[String]) -> String {
    if let Some((head, _)) = full.split_once(',') {
        name_tokens(head).join("")
    } else {
        tokens.last().cloned().unwrap_or_default()
    }
}

/// Match a canonical full name (`Surname, First`) against a truncated or
/// enumerated ChessABC-style candidate (`Surname, F`, `Surname, F.`,
/// `Surname, F 1`, `Surname,F`, or a reordered `First Surname`).
///
/// Comparison is case- and punctuation-insensitive; the surname must match
/// exactly while the given name may match exactly or by initial. A missing
/// given part on either side still matches on the surname alone.
pub fn matches_truncated_or_enumerated_name(full: &str, cand: &str) -> bool {
    let full_tokens = name_tokens(full);
    let cand_tokens = name_tokens(cand);
    if full_tokens.is_empty() || cand_tokens.is_empty() {
        return false;
    }
    let surname = surname_of(full, &full_tokens);
    if surname.is_empty() || !cand_tokens.contains(&surname) {
        return false;
    }
    let full_given: Vec<&String> = full_tokens
        .iter()
        .filter(|token| **token != surname)
        .collect();
    let cand_given: Vec<&String> = cand_tokens
        .iter()
        .filter(|token| **token != surname)
        .collect();
    // Surname-only on either side matches on the surname alone. Repeated
    // surnames (e.g. double-barrelled names tokenized twice) are ignored.
    let Some(full_first) = full_given.first().map(|token| token.as_str()) else {
        return true;
    };
    let Some(cand_first) = cand_given.first().map(|token| token.as_str()) else {
        return true;
    };
    if full_first == cand_first {
        return true;
    }
    match (full_first.chars().next(), cand_first.chars().next()) {
        (Some(f), Some(c)) if f == c => {
            // An initial on either side matches the full token: `M` matches
            // `martin` and vice versa.
            full_first.chars().count() == 1 || cand_first.chars().count() == 1
        }
        _ => false,
    }
}

/// Normalize the `White`/`Black` tags of a game to the canonical account
/// name. The tag is rewritten when it names the same player as `username`
/// (compared with [`player_key`], so spelling and word order do not matter)
/// or matches `canonical_name` through the truncated/enumerated spelling
/// rules in [`canonicalize_player_tags`]. Online exports
/// (Lichess/Chess.com/ChessABC) truncate or enumerate player names; without
/// this the merged book drops those games on its canonical-key colour match.
pub fn canonicalize_account_tags(game: &mut Game, username: &str, canonical_name: &str) {
    let user_key = player_key(username);
    if user_key.is_some() {
        for side in ["White", "Black"] {
            let Some(current) = game.header(side) else {
                continue;
            };
            if current == canonical_name {
                continue;
            }
            if player_key(current).as_deref() == user_key.as_deref() {
                game.set_header(side, canonical_name.to_string());
            }
        }
    }
    canonicalize_player_tags(game, canonical_name);
}

/// Batch version of [`canonicalize_account_tags`] for imported game lists.
pub fn canonicalize_account_games(games: &mut [Game], username: &str, canonical_name: &str) {
    for game in games {
        canonicalize_account_tags(game, username, canonical_name);
    }
}

/// Normalize the `White`/`Black` tags of a game to the canonical opponent
/// name when they name the same player through a truncated or enumerated
/// ChessABC spelling (e.g. `Hinterreiter,M 1` vs `Hinterreiter, Martin`).
/// Without this the book's canonical-key colour match drops those games.
pub fn canonicalize_player_tags(game: &mut Game, canonical_name: &str) {
    let canonical_key = player_key(canonical_name);
    for side in ["White", "Black"] {
        let Some(current) = game.header(side) else {
            continue;
        };
        if current == canonical_name {
            continue;
        }
        if player_key(current).as_deref() == canonical_key.as_deref()
            || matches_truncated_or_enumerated_name(canonical_name, current)
        {
            game.set_header(side, canonical_name.to_string());
        }
    }
}

fn ascii_tokens(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            current.push(c.to_ascii_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn contains_token_subslice(tokens: &[String], sub: &[&str]) -> bool {
    if sub.is_empty() || tokens.len() < sub.len() {
        return false;
    }
    tokens
        .windows(sub.len())
        .any(|window| window.iter().zip(sub.iter()).all(|(w, s)| w == *s))
}

fn is_chesscom_tournament_event_name(event: &str) -> bool {
    let tokens = ascii_tokens(event);
    contains_token_subslice(&tokens, &["titled", "tuesday"])
        || contains_token_subslice(&tokens, &["arena", "kings"])
        || contains_token_subslice(&tokens, &["pro", "chess", "league"])
        || contains_token_subslice(&tokens, &["pro", "league"])
        || contains_token_subslice(&tokens, &["speed", "chess", "championship"])
        || contains_token_subslice(&tokens, &["bullet", "chess", "championship"])
}

fn is_chesscom_int_site(site: &str) -> bool {
    let tokens = ascii_tokens(site);
    let has_int = tokens.iter().any(|t| t == "int");
    let has_chesscom = tokens.iter().any(|t| t == "chesscom")
        || contains_token_subslice(&tokens, &["chess", "com"]);
    has_int && has_chesscom
}

fn has_tournament_keyword(event: &str) -> bool {
    let tokens = ascii_tokens(event);
    for t in &tokens {
        match t.as_str() {
            "tournament" | "championship" | "league" | "cup" | "arena" | "swiss" | "qualifier"
            | "playoff" | "final" | "open" | "tuesday" | "kings" | "grandprix" => return true,
            _ => {}
        }
    }
    contains_token_subslice(&tokens, &["grand", "prix"])
}

/// Determine whether a game belongs to a Chess.com online tournament that
/// should be excluded from serious opening preparation.
///
/// Returns `true` only for Chess.com tournament events (Titled Tuesday, Arena
/// Kings, Speed/Bullet Chess Championship, Pro Chess League, or `Site`
/// `Chess.com INT` games whose event carries tournament keywords). Ordinary
/// online account games — even when their `Site` mentions Chess.com — are
/// kept, so user-added online accounts are never filtered out by this check.
/// Physical OTB tournaments sponsored by Chess.com (e.g.
/// `Chess.com Isle of Man International` with Site `Douglas, Isle of Man`)
/// are also kept.
pub fn is_chesscom_tournament_game(event: &str, site: &str) -> bool {
    let site_trimmed = site.trim();
    let event_trimmed = event.trim();

    // 1. Named Chess.com tournament events, wherever they were reported.
    if is_chesscom_tournament_event_name(event_trimmed) {
        return true;
    }

    // 2. Chess.com INT site with an event that reads like a tournament.
    if is_chesscom_int_site(site_trimmed) && has_tournament_keyword(event_trimmed) {
        return true;
    }

    false
}

/// Backwards-compatible alias for [`is_chesscom_tournament_game`].
pub fn is_chesscom_event(event: &str, site: &str) -> bool {
    is_chesscom_tournament_game(event, site)
}

/// Broad time-control bucket for preparation filtering.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TimeControlClass {
    Classical,
    #[default]
    Rapid,
    Blitz,
    Bullet,
}

impl TimeControlClass {
    fn index(self) -> usize {
        match self {
            Self::Classical => 0,
            Self::Rapid => 1,
            Self::Blitz => 2,
            Self::Bullet => 3,
        }
    }
}

/// Parse a `TimeControl` tag into estimated seconds (`base + 40 * increment`).
fn parse_time_control_seconds(tag: &str) -> Option<f64> {
    let tag = tag.trim();
    if tag.is_empty() || tag == "?" || tag == "-" {
        return None;
    }
    // Common forms: `600+5`, `180+2`, `40/7200`, `300`.
    let main = tag.split_whitespace().next().unwrap_or(tag);
    if let Some(slash) = main.find('/') {
        // `moves/seconds`, e.g. `40/7200`: use the sudden-death portion.
        let tail = &main[slash + 1..];
        let secs: f64 = tail.parse().ok()?;
        if secs.is_finite() && secs > 0.0 {
            return Some(secs);
        }
        return None;
    }
    let (base_s, inc_s) = match main.split_once('+') {
        Some((b, i)) => (b, Some(i)),
        None => (main, None),
    };
    let base: f64 = base_s.parse().ok()?;
    if !base.is_finite() || base <= 0.0 {
        return None;
    }
    let inc: f64 = match inc_s {
        Some(i) => {
            let head: String = i
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if head.is_empty() {
                0.0
            } else {
                head.parse().unwrap_or(0.0)
            }
        }
        None => 0.0,
    };
    if !inc.is_finite() || inc < 0.0 {
        return None;
    }
    Some(base + 40.0 * inc)
}

fn tc_from_seconds(secs: f64) -> TimeControlClass {
    if secs < 180.0 {
        TimeControlClass::Bullet
    } else if secs < 600.0 {
        TimeControlClass::Blitz
    } else if secs < 3600.0 {
        TimeControlClass::Rapid
    } else {
        TimeControlClass::Classical
    }
}

fn tc_from_event_keywords(event: &str) -> Option<TimeControlClass> {
    let lower = event.to_ascii_lowercase();
    if lower.contains("bullet") {
        Some(TimeControlClass::Bullet)
    } else if lower.contains("blitz") {
        Some(TimeControlClass::Blitz)
    } else if lower.contains("rapid") || lower.contains("schnellschach") {
        Some(TimeControlClass::Rapid)
    } else if lower.contains("titled tuesday") {
        // Titled Tuesday is an 11-round blitz event.
        Some(TimeControlClass::Blitz)
    } else if lower.contains("classical") {
        Some(TimeControlClass::Classical)
    } else {
        None
    }
}

fn looks_online(event: &str, site: &str) -> bool {
    let hay = format!("{event} {site}").to_ascii_lowercase();
    // Covers Lichess / Chess.com servers plus named online series and the
    // generic markers used by online PGN exports.
    for needle in [
        "lichess",
        "chess.com",
        "chesscom",
        "chessabc",
        "titled tuesday",
        "arena kings",
        "speed chess",
        "pro chess league",
        "bullet chess championship",
        "tornelo",
        "internet",
        "online",
        " chess.com int",
    ] {
        if hay.contains(needle) {
            return true;
        }
    }
    let site_t = site.trim().to_ascii_lowercase();
    if site_t == "?" || site_t == "int" || site_t == "internet" || site_t == "online" {
        // Bare online marker with no physical venue. A named OTB event with a
        // real venue (e.g. `Wijk aan Zee`) does not match here because the
        // combined haystack check above already ran; this catches exports that
        // only carry the marker.
        if event.trim().is_empty() || event.trim() == "?" {
            return true;
        }
    }
    // Chess.com tournament events per the detector above also count.
    is_chesscom_tournament_game(event, site)
}

fn is_rated_game(game: &Game) -> bool {
    let event = game.event().to_ascii_lowercase();
    !(event.contains("casual") || event.contains("unrated") || event.contains("friendly"))
}

/// Classify a game for preparation filtering.
///
/// Returns `(time-control class, is_online, is_rated)`.
pub fn classify_game_metadata(game: &Game) -> (TimeControlClass, bool, bool) {
    let event = game.event();
    let site = game.site();
    let is_online = looks_online(event, site);
    let is_rated = is_rated_game(game);
    let tc = game
        .header("TimeControl")
        .and_then(parse_time_control_seconds)
        .map(tc_from_seconds)
        .or_else(|| tc_from_event_keywords(event))
        .unwrap_or(if is_online {
            TimeControlClass::Rapid
        } else {
            // Untagged non-online games are classical by default. Faster
            // classes only come from an explicit TimeControl tag or event
            // keywords.
            TimeControlClass::Classical
        });
    (tc, is_online, is_rated)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GameCategory {
    OtbClassical,
    OtbRapid,
    OtbBlitz,
    OnlineClassical,
    OnlineRapid,
    OnlineBlitz,
    OnlineBullet,
    Correspondence,
    Unknown,
}

impl GameCategory {
    pub const ALL: [Self; 9] = [
        Self::OtbClassical,
        Self::OtbRapid,
        Self::OtbBlitz,
        Self::OnlineClassical,
        Self::OnlineRapid,
        Self::OnlineBlitz,
        Self::OnlineBullet,
        Self::Correspondence,
        Self::Unknown,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::OtbClassical => "OTB Classical",
            Self::OtbRapid => "OTB Rapid",
            Self::OtbBlitz => "OTB Blitz",
            Self::OnlineClassical => "Online Classical",
            Self::OnlineRapid => "Online Rapid",
            Self::OnlineBlitz => "Online Blitz",
            Self::OnlineBullet => "Online Bullet",
            Self::Correspondence => "Correspondence",
            Self::Unknown => "Unknown",
        }
    }
}

pub fn classify_headers_category(
    event: Option<&str>,
    site: Option<&str>,
    time_control: Option<&str>,
) -> GameCategory {
    let e = event.unwrap_or("");
    let s = site.unwrap_or("");
    let hay = format!("{e} {s}").to_ascii_lowercase();
    if hay.contains("correspondence") || hay.contains("fernschach") {
        return GameCategory::Correspondence;
    }
    let tokens = ascii_tokens(&hay);
    if tokens.iter().any(|t| {
        matches!(
            t.as_str(),
            "corr" | "iccf" | "bdg" | "email" | "post" | "postal"
        )
    }) {
        return GameCategory::Correspondence;
    }
    if event.is_none() && site.is_none() && time_control.is_none() {
        return GameCategory::Unknown;
    }
    let event_str = event.unwrap_or("?");
    let site_str = site.unwrap_or("?");
    let is_online = looks_online(event_str, site_str);
    let tc = time_control
        .and_then(parse_time_control_seconds)
        .map(tc_from_seconds)
        .or_else(|| tc_from_event_keywords(event_str))
        .unwrap_or(if is_online {
            TimeControlClass::Rapid
        } else {
            TimeControlClass::Classical
        });

    match (is_online, tc) {
        (true, TimeControlClass::Classical) => GameCategory::OnlineClassical,
        (true, TimeControlClass::Rapid) => GameCategory::OnlineRapid,
        (true, TimeControlClass::Blitz) => GameCategory::OnlineBlitz,
        (true, TimeControlClass::Bullet) => GameCategory::OnlineBullet,
        (false, TimeControlClass::Classical) => GameCategory::OtbClassical,
        (false, TimeControlClass::Rapid) => GameCategory::OtbRapid,
        (false, TimeControlClass::Blitz) => GameCategory::OtbBlitz,
        (false, TimeControlClass::Bullet) => GameCategory::OtbBlitz,
    }
}

pub fn classify_game_category(game: &Game) -> GameCategory {
    classify_headers_category(
        Some(game.event()),
        Some(game.site()),
        game.header("TimeControl"),
    )
}

/// Which time controls pass a preparation filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeControlMask {
    pub classical: bool,
    pub rapid: bool,
    pub blitz: bool,
    pub bullet: bool,
}

impl Default for TimeControlMask {
    fn default() -> Self {
        Self {
            classical: true,
            rapid: true,
            blitz: true,
            bullet: true,
        }
    }
}

impl TimeControlMask {
    pub fn matches(self, tc: TimeControlClass) -> bool {
        match tc {
            TimeControlClass::Classical => self.classical,
            TimeControlClass::Rapid => self.rapid,
            TimeControlClass::Blitz => self.blitz,
            TimeControlClass::Bullet => self.bullet,
        }
    }
}

/// Date cutoff for one preparation source (OTB or online).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DateCutoff {
    #[default]
    AllTime,
    Months(u32),
    SinceDate(u32),
}

impl DateCutoff {
    pub fn matches(self, date: u32, as_of: u32) -> bool {
        match self {
            Self::AllTime => true,
            Self::SinceDate(since) => date == 0 || date >= since,
            Self::Months(months) => {
                if date == 0 {
                    return true;
                }
                let (Some(d), Some(a)) = (date_to_days(date), date_to_days(as_of)) else {
                    return true;
                };
                let age = (a - d).max(0);
                // Average Gregorian month length.
                let limit = (f64::from(months) * 365.25 / 12.0).ceil() as i64;
                age <= limit
            }
        }
    }
}

/// Preparation source filter. OTB and online games are filtered independently
/// so toggling `include_online` never requires rebuilding the book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrepFilterConfig {
    pub otb_time_controls: TimeControlMask,
    pub otb_date_cutoff: DateCutoff,
    pub include_online: bool,
    pub online_time_controls: TimeControlMask,
    pub online_date_cutoff: DateCutoff,
    pub include_rated: bool,
    pub include_casual: bool,
}

impl Default for PrepFilterConfig {
    fn default() -> Self {
        Self {
            otb_time_controls: TimeControlMask::default(),
            otb_date_cutoff: DateCutoff::AllTime,
            include_online: true,
            online_time_controls: TimeControlMask::default(),
            online_date_cutoff: DateCutoff::AllTime,
            include_rated: true,
            include_casual: true,
        }
    }
}

impl PrepFilterConfig {
    /// Check one pre-classified observation against this filter.
    pub fn matches_obs(
        self,
        is_online: bool,
        tc: TimeControlClass,
        date: u32,
        is_rated: bool,
        as_of: u32,
    ) -> bool {
        if is_online {
            if !self.include_online {
                return false;
            }
            if is_rated {
                if !self.include_rated {
                    return false;
                }
            } else if !self.include_casual {
                return false;
            }
            if !self.online_time_controls.matches(tc) {
                return false;
            }
            if !self.online_date_cutoff.matches(date, as_of) {
                return false;
            }
            true
        } else {
            if !self.otb_time_controls.matches(tc) {
                return false;
            }
            if !self.otb_date_cutoff.matches(date, as_of) {
                return false;
            }
            true
        }
    }

    pub fn matches_game(self, game: &Game, as_of: u32) -> bool {
        let (tc, is_online, is_rated) = classify_game_metadata(game);
        let date = parse_date(game.date()).unwrap_or(0);
        self.matches_obs(is_online, tc, date, is_rated, as_of)
    }
}

/// Per-account filter for one linked online account. Applied when the
/// account's games are fetched, so enabling an account never requires
/// rebuilding the book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountFilter {
    pub include_rated: bool,
    pub include_casual: bool,
    pub min_elo: Option<u16>,
    pub max_elo: Option<u16>,
    pub date_cutoff: DateCutoff,
    pub time_controls: TimeControlMask,
}

impl Default for AccountFilter {
    fn default() -> Self {
        Self {
            include_rated: true,
            include_casual: true,
            min_elo: None,
            max_elo: None,
            date_cutoff: DateCutoff::AllTime,
            time_controls: TimeControlMask::default(),
        }
    }
}

/// Per-filter counts showing exactly which filter removed games.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExclusionBreakdown {
    pub total: usize,
    pub kept: usize,
    /// Number of games classified as bullet among `total`.
    pub bullet: usize,
    pub classical: usize,
    pub rapid: usize,
    pub blitz: usize,
    /// Games excluded by each individual reason (a game may count in
    /// several buckets).
    pub excluded_rated: usize,
    pub excluded_casual: usize,
    pub excluded_time_control: usize,
    pub excluded_date: usize,
    pub excluded_elo: usize,
    /// Games excluded by bullet specifically.
    pub excluded_bullet: usize,
}

impl AccountFilter {
    /// Reason this filter cannot match any game, if any. Used for the
    /// zero-overlap warning badge on the account row.
    pub fn has_zero_overlap(&self) -> Option<&'static str> {
        if !self.include_rated && !self.include_casual {
            return Some("rated and casual are both off — no games can pass");
        }
        if !self.time_controls.classical
            && !self.time_controls.rapid
            && !self.time_controls.blitz
            && !self.time_controls.bullet
        {
            return Some("all time controls are off — no games can pass");
        }
        if let (Some(min), Some(max)) = (self.min_elo, self.max_elo)
            && min > max
        {
            return Some("minimum Elo is above maximum Elo — no games can pass");
        }
        None
    }

    /// Check one fetched game against this account filter. The Elo bounds
    /// apply to the higher of the two recorded Elos; games without Elo tags
    /// pass the Elo check.
    pub fn matches_game(&self, game: &Game, as_of: u32) -> bool {
        let (tc, _is_online, is_rated) = classify_game_metadata(game);
        if is_rated {
            if !self.include_rated {
                return false;
            }
        } else if !self.include_casual {
            return false;
        }
        if !self.time_controls.matches(tc) {
            return false;
        }
        if !self
            .date_cutoff
            .matches(parse_date(game.date()).unwrap_or(0), as_of)
        {
            return false;
        }
        if self.min_elo.is_some() || self.max_elo.is_some() {
            let min = self.min_elo.unwrap_or(0);
            let max = self.max_elo.unwrap_or(u16::MAX);
            let mut found = false;
            let mut in_range = false;
            for tag in [game.header("WhiteElo"), game.header("BlackElo")] {
                if let Some(elo) = tag.and_then(|text| text.trim().parse::<u16>().ok()) {
                    found = true;
                    if elo >= min && elo <= max {
                        in_range = true;
                    }
                }
            }
            if found && !in_range {
                return false;
            }
        }
        true
    }

    /// Per-filter counts for `games` under this filter.
    pub fn exclusion_breakdown(&self, games: &[Game], as_of: u32) -> ExclusionBreakdown {
        let mut breakdown = ExclusionBreakdown {
            total: games.len(),
            ..Default::default()
        };
        for game in games {
            let (tc, _is_online, is_rated) = classify_game_metadata(game);
            match tc {
                TimeControlClass::Classical => breakdown.classical += 1,
                TimeControlClass::Rapid => breakdown.rapid += 1,
                TimeControlClass::Blitz => breakdown.blitz += 1,
                TimeControlClass::Bullet => breakdown.bullet += 1,
            }
            if self.matches_game(game, as_of) {
                breakdown.kept += 1;
                continue;
            }
            if is_rated && !self.include_rated {
                breakdown.excluded_rated += 1;
            }
            if !is_rated && !self.include_casual {
                breakdown.excluded_casual += 1;
            }
            if !self.time_controls.matches(tc) {
                breakdown.excluded_time_control += 1;
                if tc == TimeControlClass::Bullet {
                    breakdown.excluded_bullet += 1;
                }
            }
            if !self
                .date_cutoff
                .matches(parse_date(game.date()).unwrap_or(0), as_of)
            {
                breakdown.excluded_date += 1;
            }
            if self.min_elo.is_some() || self.max_elo.is_some() {
                let min = self.min_elo.unwrap_or(0);
                let max = self.max_elo.unwrap_or(u16::MAX);
                let mut found = false;
                let mut in_range = false;
                for tag in [game.header("WhiteElo"), game.header("BlackElo")] {
                    if let Some(elo) = tag.and_then(|text| text.trim().parse::<u16>().ok()) {
                        found = true;
                        if elo >= min && elo <= max {
                            in_range = true;
                        }
                    }
                }
                if found && !in_range {
                    breakdown.excluded_elo += 1;
                }
            }
        }
        breakdown
    }
}

/// One linked online account used for preparation imports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrepOnlineAccount {
    pub source: OnlineSource,
    pub username: String,
    pub enabled: bool,
    pub filter: AccountFilter,
    /// Filter schema version. Old JSON without this key defaults to 1;
    /// loading version < 2 enables bullet once and writes 2.
    pub filter_version: u32,
}

impl Default for PrepOnlineAccount {
    fn default() -> Self {
        Self {
            source: OnlineSource::Lichess,
            username: String::new(),
            enabled: true,
            filter: AccountFilter::default(),
            filter_version: 2,
        }
    }
}

impl PrepOnlineAccount {
    pub fn service_slug(source: OnlineSource) -> &'static str {
        match source {
            OnlineSource::Lichess => "lichess",
            OnlineSource::ChessCom => "chesscom",
            OnlineSource::ChessAbc => "chessabc",
        }
    }

    pub fn service_from_slug(slug: &str) -> Option<OnlineSource> {
        match slug.trim().to_ascii_lowercase().as_str() {
            "lichess" => Some(OnlineSource::Lichess),
            "chesscom" | "chess.com" | "chess_com" => Some(OnlineSource::ChessCom),
            "chessabc" | "chess_abc" | "abc" => Some(OnlineSource::ChessAbc),
            _ => None,
        }
    }

    pub fn to_value(&self) -> serde_json::Value {
        serde_json::json!({
            "service": Self::service_slug(self.source),
            "username": self.username,
            "enabled": self.enabled,
            "filter_version": self.filter_version,
            "filter": {
                "include_rated": self.filter.include_rated,
                "include_casual": self.filter.include_casual,
                "min_elo": self.filter.min_elo,
                "max_elo": self.filter.max_elo,
                "date_cutoff": match self.filter.date_cutoff {
                    DateCutoff::AllTime => serde_json::json!("all"),
                    DateCutoff::Months(months) => serde_json::json!({"months": months}),
                    DateCutoff::SinceDate(date) => serde_json::json!({"since": date}),
                },
                "time_controls": {
                    "classical": self.filter.time_controls.classical,
                    "rapid": self.filter.time_controls.rapid,
                    "blitz": self.filter.time_controls.blitz,
                    "bullet": self.filter.time_controls.bullet,
                },
            },
        })
    }

    fn account_filter_from_value(value: &serde_json::Value) -> AccountFilter {
        let mut filter = AccountFilter::default();
        if let Some(rated) = value
            .get("include_rated")
            .and_then(serde_json::Value::as_bool)
        {
            filter.include_rated = rated;
        }
        if let Some(casual) = value
            .get("include_casual")
            .and_then(serde_json::Value::as_bool)
        {
            filter.include_casual = casual;
        }
        filter.min_elo = value
            .get("min_elo")
            .and_then(serde_json::Value::as_u64)
            .filter(|elo| *elo > 0 && *elo <= 4000)
            .map(|elo| elo as u16);
        filter.max_elo = value
            .get("max_elo")
            .and_then(serde_json::Value::as_u64)
            .filter(|elo| *elo > 0 && *elo <= 4000)
            .map(|elo| elo as u16);
        if let Some(cutoff) = value.get("date_cutoff") {
            if cutoff.as_str().is_some_and(|text| text == "all") {
                filter.date_cutoff = DateCutoff::AllTime;
            } else if let Some(months) = cutoff.get("months").and_then(serde_json::Value::as_u64) {
                filter.date_cutoff = DateCutoff::Months(months.clamp(1, 1200) as u32);
            } else if let Some(since) = cutoff.get("since").and_then(serde_json::Value::as_u64) {
                filter.date_cutoff = DateCutoff::SinceDate(since as u32);
            }
        }
        if let Some(mask) = value.get("time_controls") {
            for (key, slot) in [
                ("classical", &mut filter.time_controls.classical),
                ("rapid", &mut filter.time_controls.rapid),
                ("blitz", &mut filter.time_controls.blitz),
                ("bullet", &mut filter.time_controls.bullet),
            ] {
                if let Some(enabled) = mask.get(key).and_then(serde_json::Value::as_bool) {
                    *slot = enabled;
                }
            }
        }
        filter
    }

    pub fn from_value(value: &serde_json::Value) -> Option<Self> {
        let username = value
            .get("username")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())?
            .to_string();
        let source = value
            .get("service")
            .and_then(serde_json::Value::as_str)
            .and_then(Self::service_from_slug)
            .unwrap_or(OnlineSource::Lichess);
        // Old JSON (pre-bullet default) has no `filter_version`; serde
        // default would be 1. Anything < 2 migrates bullet on once.
        let raw_version = value
            .get("filter_version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1) as u32;
        let mut filter = value
            .get("filter")
            .map(Self::account_filter_from_value)
            .unwrap_or_default();
        let filter_version = if raw_version < 2 {
            filter.time_controls.bullet = true;
            2
        } else {
            raw_version
        };
        Some(Self {
            source,
            username,
            enabled: value
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            filter,
            filter_version,
        })
    }
}

/// Where a deduplicated game originally came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceTag {
    Owoju,
    Online(OnlineSource),
    Other(String),
}

/// Content fingerprint used to spot the same game arriving from two sources
/// (for example Mega Database and a ChessABC export).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameFingerprint {
    pub white_key: String,
    pub black_key: String,
    pub result: String,
    pub spine_hash: u64,
    pub ply_count: usize,
    pub date: u32,
}

/// Hash the first `min(mainline, 30)` plies of UCI moves.
fn spine_hash(game: &Game) -> (u64, usize) {
    let mut hasher = DefaultHasher::new();
    let mut count = 0usize;
    let mut node = 0usize;
    while let Some(&child) = game.nodes[node].children.first() {
        if count >= 30 {
            break;
        }
        let Some(ply) = &game.nodes[child].mv else {
            break;
        };
        if ply.uci == "0000" {
            break;
        }
        ply.uci.hash(&mut hasher);
        count += 1;
        node = child;
    }
    (hasher.finish(), count)
}

pub fn fingerprint_game(game: &Game) -> GameFingerprint {
    let (spine_hash, ply_count) = spine_hash(game);
    GameFingerprint {
        white_key: player_key(game.white()).unwrap_or_default(),
        black_key: player_key(game.black()).unwrap_or_default(),
        result: game.result().trim().to_string(),
        spine_hash,
        ply_count,
        date: parse_date(game.date()).unwrap_or(0),
    }
}

fn day_unknown(date: u32) -> bool {
    date == 0 || date.is_multiple_of(100) || date % 100 > 31
}

fn dates_compatible(a: u32, b: u32) -> bool {
    if day_unknown(a) || day_unknown(b) {
        if a == 0 || b == 0 {
            return true;
        }
        // Same YYYYMM when the day is unknown.
        return a / 100 == b / 100;
    }
    let (Some(da), Some(db)) = (date_to_days(a), date_to_days(b)) else {
        return a / 100 == b / 100;
    };
    (da - db).abs() <= 3
}

fn richer_header_value(current: &str, candidate: &str) -> bool {
    if candidate.trim().is_empty() || candidate.trim() == "?" {
        return false;
    }
    if current.trim().is_empty() || current.trim() == "?" {
        return true;
    }
    candidate.len() > current.len()
}

/// Merge headers from `incoming` into `base`, keeping the richest value per
/// tag (never dropping clocks, Elo, FideId or TimeControl data).
fn merge_headers(base: &mut Game, incoming: &Game) {
    for (key, value) in &incoming.headers {
        if let Some((_, current)) = base
            .headers
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
        {
            if richer_header_value(current, value) {
                *current = value.clone();
            }
        } else {
            base.headers.push((key.clone(), value.clone()));
        }
    }
    // Keep the longer mainline and any clock/eval annotations the other copy
    // carried.
    let base_clocks = base.nodes.iter().filter(|n| n.clock.is_some()).count();
    let inc_clocks = incoming.nodes.iter().filter(|n| n.clock.is_some()).count();
    if incoming.nodes.len() > base.nodes.len() && inc_clocks >= base_clocks {
        let headers = std::mem::take(&mut base.headers);
        let errors = std::mem::take(&mut base.errors);
        *base = incoming.clone();
        // `incoming` headers were already merged above only into the old base;
        // re-merge so nothing is lost in either direction.
        let mut merged_headers = base.headers.clone();
        for (key, value) in &headers {
            if !merged_headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(key))
            {
                merged_headers.push((key.clone(), value.clone()));
            }
        }
        base.headers = merged_headers;
        base.errors.extend(errors);
    }
}

/// Deduplicate games arriving from several sources.
///
/// Two games are duplicates when the canonical player keys and result match,
/// the opening spine (first 30 plies) matches, and the dates are within ±3
/// days (or share YYYYMM when the day is unknown). The richest headers win;
/// the first occurrence keeps its [`SourceTag`].
pub fn deduplicate_and_merge_games(sources: Vec<(SourceTag, Vec<Game>)>) -> Vec<(SourceTag, Game)> {
    let mut out: Vec<(SourceTag, Game, GameFingerprint)> = Vec::new();
    for (tag, games) in sources {
        for game in games {
            let fp = fingerprint_game(&game);
            let mut merged = false;
            for (_, existing, efp) in out.iter_mut() {
                if efp.white_key == fp.white_key
                    && efp.black_key == fp.black_key
                    && efp.result == fp.result
                    && efp.spine_hash == fp.spine_hash
                    && efp.ply_count == fp.ply_count
                    && dates_compatible(efp.date, fp.date)
                {
                    merge_headers(existing, &game);
                    // Prefer the most informative date.
                    if efp.date == 0 && fp.date != 0 {
                        efp.date = fp.date;
                    }
                    merged = true;
                    break;
                }
            }
            if !merged {
                out.push((tag.clone(), game, fp));
            }
        }
    }
    out.into_iter().map(|(tag, game, _)| (tag, game)).collect()
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
    /// Time-decayed weight from OTB games (kept for instant refiltering).
    pub otb_weighted: f64,
    /// Time-decayed weight from online games.
    pub online_weighted: f64,
    /// Time-decayed weight per [`TimeControlClass`] index (Classical=0,
    /// Rapid=1, Blitz=2, Bullet=3).
    pub tc_weighted: [f64; 4],
    /// Quarterly game counts for the player's sparkline, aligned to
    /// `anchor_quarter` like [`MoveStat`].
    pub anchor_quarter: u32,
    pub older_dated: u32,
    pub buckets: [u32; TREND_BUCKETS],
}

/// One stored observation backing a [`PlayerMoveStat`]. Kept per move so
/// [`PrepFilterConfig`] can be applied instantly without rebuilding the book.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerObs {
    pub weight: f64,
    pub is_online: bool,
    pub is_rated: bool,
    pub tc: TimeControlClass,
    pub date: u32,
    pub result: GameResult,
    /// True when the mover was the opponent rather than the prepared player,
    /// i.e. this observation is a reply the player faced, not one of their
    /// own moves. Replies share the player's colour index so preparation can
    /// show what the player faced, but they must never count as the player's
    /// own repertoire in predictions or own-move statistics.
    pub reply: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GameResult {
    Win,
    Draw,
    Loss,
    #[default]
    Unknown,
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
    /// Exclude online Chess.com tournaments to prevent database pollution.
    pub exclude_chesscom: bool,
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
            exclude_chesscom: false,
        }
    }
}

/// A game that reached one position in the player's book.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerGameRef {
    /// Index in the database that produced the book.
    pub index: usize,
    /// Parsed PGN date (`YYYYMMDD`), or zero when unknown.
    pub date: u32,
    /// The prepared player's colour in that game.
    pub color: Color,
    /// Pre-classified preparation metadata so filters apply without reparsing.
    pub is_online: bool,
    pub is_rated: bool,
    pub tc: TimeControlClass,
}

impl Eq for PlayerGameRef {}

fn color_idx(color: Color) -> usize {
    match color {
        Color::White => 0,
        Color::Black => 1,
    }
}

/// A player's position book. Built off the UI thread.
///
/// Move statistics are stored per player colour (`White = 0`, `Black = 1`) so
/// preparation can look at one side without mixing the other. Both the
/// player's own moves and the replies played against them are recorded under
/// the player's colour index, so preparation sees the responses the player
/// actually faced. Every
/// observation is also stored individually, so [`PrepFilterConfig`] filtering
/// is a cheap in-memory pass with no rebuild.
pub struct PlayerBook {
    player_key: String,
    player_name: String,
    positions: [HashMap<u64, BTreeMap<u32, PlayerMoveStat>>; 2],
    /// Exact per-observation data for instant refiltering, keyed by
    /// `(color_idx, position_key, move_code)`.
    obs: HashMap<(u8, u64, u32), Vec<PlayerObs>>,
    /// Mainline depth (plies from the start) of each recorded position.
    depths: HashMap<u64, u32>,
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
            positions: [HashMap::new(), HashMap::new()],
            obs: HashMap::new(),
            depths: HashMap::new(),
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
            positions: [HashMap::new(), HashMap::new()],
            obs: HashMap::new(),
            depths: HashMap::new(),
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

    /// Build from already-parsed games merged across several sources.
    ///
    /// Game-ref indices address `games` positionally, so the caller must keep
    /// that slice alive next to the book (the UI stores it as
    /// `prep_merged_games`) before resolving recent rows.
    pub fn from_merged_games(
        player_key: impl Into<String>,
        player_name: impl Into<String>,
        games: &[Game],
        config: PredictionConfig,
        as_of: u32,
    ) -> Self {
        let mut book = Self {
            player_key: player_key.into(),
            player_name: player_name.into(),
            positions: [HashMap::new(), HashMap::new()],
            obs: HashMap::new(),
            depths: HashMap::new(),
            game_refs: HashMap::new(),
            games: 0,
            as_of,
            config,
        };
        for (position, game) in games.iter().enumerate() {
            book.add_game(game, Some(position));
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
        if self.config.exclude_chesscom && is_chesscom_tournament_game(game.event(), game.site()) {
            return;
        }
        let white_key = player_key(game.white());
        let black_key = player_key(game.black());
        let color = if white_key.as_deref() == Some(self.player_key.as_str()) {
            Color::White
        } else if black_key.as_deref() == Some(self.player_key.as_str()) {
            Color::Black
        } else {
            return;
        };
        let cidx = color_idx(color);

        let date = parse_date(game.date()).unwrap_or(0);
        let result = result_for(color, game.result());
        let (tc, is_online, is_rated) = classify_game_metadata(game);
        let mut seen_positions = HashSet::new();
        let mut seen_edges: HashSet<(u64, u32)> = HashSet::new();
        let mut node = 0;
        let mut depth: u32 = 0;

        loop {
            if depth >= 400 {
                break;
            }
            let parent_pos = &game.nodes[node].pos;
            let key = position::position_key(parent_pos);
            if seen_positions.insert(key) {
                self.depths.entry(key).or_insert(depth);
                // Record every position the game reached, including the final
                // position where no reply was played.
                if let Some(index) = game_index {
                    self.game_refs.entry(key).or_default().push(PlayerGameRef {
                        index,
                        date,
                        color,
                        is_online,
                        is_rated,
                        tc,
                    });
                }
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
            // Record both the player's own moves and the replies played against
            // them, all under the player's colour index. The result stays from
            // the prepared player's perspective. Each observation remembers
            // which side moved so own-move readers can exclude replies.
            let reply = parent_pos.turn() != color;
            if seen_edges.insert((key, code)) {
                let (weight, recent) = recency(date, self.as_of, self.config);
                let entry = self.positions[cidx]
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
                if is_online {
                    entry.online_weighted += weight;
                } else {
                    entry.otb_weighted += weight;
                }
                entry.tc_weighted[tc.index()] += weight;
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
                    if let Some(q) = position::date_to_quarter(date) {
                        if entry.anchor_quarter == 0 {
                            entry.anchor_quarter = q;
                            entry.buckets[TREND_BUCKETS - 1] += 1;
                        } else if q > entry.anchor_quarter {
                            let shift = (q - entry.anchor_quarter) as usize;
                            if shift >= TREND_BUCKETS {
                                let moved: u32 = entry.buckets.iter().sum();
                                entry.older_dated = entry.older_dated.saturating_add(moved);
                                entry.buckets = [0; TREND_BUCKETS];
                            } else {
                                let moved: u32 = entry.buckets[..shift].iter().sum();
                                entry.older_dated = entry.older_dated.saturating_add(moved);
                                entry.buckets.copy_within(shift.., 0);
                                entry.buckets[TREND_BUCKETS - shift..].fill(0);
                            }
                            entry.anchor_quarter = q;
                            entry.buckets[TREND_BUCKETS - 1] =
                                entry.buckets[TREND_BUCKETS - 1].saturating_add(1);
                        } else {
                            let age = (entry.anchor_quarter - q) as usize;
                            if age < TREND_BUCKETS {
                                let idx = TREND_BUCKETS - 1 - age;
                                entry.buckets[idx] = entry.buckets[idx].saturating_add(1);
                            } else {
                                entry.older_dated = entry.older_dated.saturating_add(1);
                            }
                        }
                    }
                }
                self.obs
                    .entry((cidx as u8, key, code))
                    .or_default()
                    .push(PlayerObs {
                        weight,
                        is_online,
                        is_rated,
                        tc,
                        date,
                        result,
                        reply,
                    });
            }
            node = child;
            depth = depth.saturating_add(1);
        }
        self.games += 1;
    }

    /// Rebuild one move's aggregate from the stored observations, keeping only
    /// observations on the wanted side of the reply flag (and passing `filter`
    /// when given). Returns `None` when nothing matches, so own-move readers
    /// never mistake a faced reply for one of the player's moves (and reply
    /// readers never pick up own moves).
    fn rebuild_stat(
        &self,
        cidx: usize,
        key: u64,
        code: u32,
        reply: bool,
        filter: Option<&PrepFilterConfig>,
    ) -> Option<PlayerMoveStat> {
        let obs = self.obs.get(&(cidx as u8, key, code))?;
        let base = self.positions[cidx].get(&key)?.get(&code)?;
        let mut stat = PlayerMoveStat {
            key,
            mv: code,
            anchor_quarter: base.anchor_quarter,
            ..PlayerMoveStat::default()
        };
        let mut weighted = 0.0;
        let mut recent_weighted = 0.0;
        for ob in obs {
            if ob.reply != reply {
                continue;
            }
            if let Some(f) = filter
                && !f.matches_obs(ob.is_online, ob.tc, ob.date, ob.is_rated, self.as_of)
            {
                continue;
            }
            stat.games += 1;
            match ob.result {
                GameResult::Win => stat.wins += 1,
                GameResult::Draw => stat.draws += 1,
                GameResult::Loss => stat.losses += 1,
                GameResult::Unknown => {}
            }
            weighted += ob.weight;
            if is_recent_date(ob.date, self.as_of, self.config) {
                stat.recent_games += 1;
                recent_weighted += ob.weight;
            }
            if ob.date != 0 {
                if stat.first_date == 0 || ob.date < stat.first_date {
                    stat.first_date = ob.date;
                }
                stat.last_date = stat.last_date.max(ob.date);
            }
            if ob.is_online {
                stat.online_weighted += ob.weight;
            } else {
                stat.otb_weighted += ob.weight;
            }
            stat.tc_weighted[ob.tc.index()] += ob.weight;
        }
        if stat.games == 0 {
            return None;
        }
        stat.weighted = weighted;
        stat.recent_weighted = recent_weighted;
        Some(stat)
    }

    /// Stats for one player colour, rebuilt from observations on the wanted
    /// side of the reply flag. `reply == false` selects the player's own
    /// moves; `reply == true` selects the replies they faced.
    fn stats_for_map(
        &self,
        cidx: usize,
        key: u64,
        reply: bool,
        filter: Option<&PrepFilterConfig>,
    ) -> Vec<PlayerMoveStat> {
        let mut out = Vec::new();
        let Some(moves) = self.positions[cidx].get(&key) else {
            return out;
        };
        for &code in moves.keys() {
            if let Some(stat) = self.rebuild_stat(cidx, key, code, reply, filter) {
                out.push(stat);
            }
        }
        out.sort_by(|a, b| {
            b.weighted
                .total_cmp(&a.weighted)
                .then(b.games.cmp(&a.games))
                .then(a.mv.cmp(&b.mv))
        });
        out
    }

    /// Combined own-move stats across both colours (backwards compatible),
    /// rebuilt from observations so faced replies never leak in. At any
    /// position only the side to move can own moves, so merging stays exact.
    fn own_moves_combined(&self, key: u64) -> BTreeMap<u32, PlayerMoveStat> {
        let mut combined: BTreeMap<u32, PlayerMoveStat> = BTreeMap::new();
        for (cidx, map) in self.positions.iter().enumerate() {
            let Some(moves) = map.get(&key) else {
                continue;
            };
            for &code in moves.keys() {
                if let Some(stat) = self.rebuild_stat(cidx, key, code, false, None) {
                    let entry = combined.entry(code).or_insert(PlayerMoveStat {
                        key,
                        mv: code,
                        ..PlayerMoveStat::default()
                    });
                    merge_stat(entry, &stat);
                }
            }
        }
        combined
    }

    /// Combined own moves from both colours (backwards compatible). Faced
    /// replies are excluded: they are not the player's moves.
    pub fn moves(&self, key: u64) -> Vec<PlayerMoveStat> {
        let mut moves: Vec<_> = self.own_moves_combined(key).into_values().collect();
        moves.sort_by(|a, b| {
            b.weighted
                .total_cmp(&a.weighted)
                .then(b.games.cmp(&a.games))
                .then(a.mv.cmp(&b.mv))
        });
        moves
    }

    /// Own moves from one player colour only. Faced replies are excluded.
    pub fn moves_for_color(&self, key: u64, color: Color) -> Vec<PlayerMoveStat> {
        self.stats_for_map(color_idx(color), key, false, None)
    }

    /// Filtered own moves for one colour: only observations passing `filter`
    /// contribute. Cheap in-memory pass over the stored observations. Faced
    /// replies are excluded; see [`Self::filtered_replies`] for those.
    pub fn filtered_moves(
        &self,
        key: u64,
        color: Color,
        filter: &PrepFilterConfig,
    ) -> Vec<PlayerMoveStat> {
        self.stats_for_map(color_idx(color), key, false, Some(filter))
    }

    /// Filtered replies faced by the player for one colour: only reply
    /// observations passing `filter` contribute. Replies live under the
    /// player's game-colour index at positions where the opponent was to
    /// move, so query them with the player's colour (e.g. the opponent's
    /// colour in preparation) at our-turn keys.
    pub fn filtered_replies(
        &self,
        key: u64,
        color: Color,
        filter: &PrepFilterConfig,
    ) -> Vec<PlayerMoveStat> {
        self.stats_for_map(color_idx(color), key, true, Some(filter))
    }

    pub fn stats(&self, key: u64) -> Option<PlayerPositionStats> {
        let combined = self.own_moves_combined(key);
        if combined.is_empty() {
            return None;
        }
        let mut stats = PlayerPositionStats::default();
        accumulate_stats(&mut stats, combined.values());
        Some(stats)
    }

    /// Aggregated filtered own-move stats for one colour.
    pub fn filtered_stats(
        &self,
        key: u64,
        color: Color,
        filter: &PrepFilterConfig,
    ) -> Option<PlayerPositionStats> {
        let moves = self.filtered_moves(key, color, filter);
        if moves.is_empty() {
            return None;
        }
        let mut stats = PlayerPositionStats::default();
        accumulate_stats(&mut stats, moves.iter());
        Some(stats)
    }

    /// Aggregated filtered reply stats for one colour: the replies the player
    /// faced at `key`, from the player's perspective.
    pub fn replies_stats(
        &self,
        key: u64,
        color: Color,
        filter: &PrepFilterConfig,
    ) -> Option<PlayerPositionStats> {
        let moves = self.filtered_replies(key, color, filter);
        if moves.is_empty() {
            return None;
        }
        let mut stats = PlayerPositionStats::default();
        accumulate_stats(&mut stats, moves.iter());
        Some(stats)
    }

    /// Total filtered games at `key` for `color` (cheap count for swap checks).
    pub fn filtered_game_count(
        &self,
        key: u64,
        color: Color,
        filter: &PrepFilterConfig,
    ) -> (u32, f64) {
        let moves = self.filtered_moves(key, color, filter);
        let n: u32 = moves.iter().map(|m| m.games).sum();
        let w: f64 = moves.iter().map(|m| m.weighted).sum();
        (n, w)
    }

    /// Does the player have meaningful experience on the *opposite* side of
    /// the board from where preparation expects them?
    ///
    /// `our_color` is the colour the opponent (us) will play; the book is the
    /// prepared player's. `ply` is the current mainline depth.
    pub fn has_swapped_color_experience(
        &self,
        key: u64,
        our_color: Color,
        filter: &PrepFilterConfig,
        ply: u32,
    ) -> bool {
        // Shallow positions (fewer than 4 plies) never count: the colours
        // have barely been established, so a "swapped" sample here is noise.
        if ply < 4 {
            return false;
        }
        let (n, w) = self.filtered_game_count(key, our_color, filter);
        if n >= 3 {
            return true;
        }
        if n >= 2 && (ply >= 8 || w >= 1.2) {
            return true;
        }
        if ply >= 14 && n >= 1 {
            // At least one recent (<= 12 months) game this deep is meaningful.
            let cidx = color_idx(our_color) as u8;
            if let Some(moves) = self.positions[color_idx(our_color)].get(&key) {
                for &code in moves.keys() {
                    if let Some(obs) = self.obs.get(&(cidx, key, code)) {
                        for ob in obs {
                            if ob.reply {
                                continue;
                            }
                            if !filter.matches_obs(
                                ob.is_online,
                                ob.tc,
                                ob.date,
                                ob.is_rated,
                                self.as_of,
                            ) {
                                continue;
                            }
                            if ob.date != 0
                                && let (Some(d), Some(a)) =
                                    (date_to_days(ob.date), date_to_days(self.as_of))
                                && (a - d).max(0) <= 365
                            {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }

    /// Repertoire profile for one colour under `filter`. Only the player's own
    /// moves contribute; faced replies are not repertoire.
    pub fn profile(
        &self,
        color: Color,
        filter: &PrepFilterConfig,
        reference: Option<&crate::position::PositionIndex>,
    ) -> RepertoireProfile {
        let cidx = color_idx(color);
        let mut inv_perplexities: Vec<(f64, f64)> = Vec::new();
        let mut sample_games = 0u32;
        let mut exit_plies: Vec<u32> = Vec::new();
        let mut booked_moves = 0u32;
        let mut total_moves = 0u32;
        for (key, moves) in &self.positions[cidx] {
            let depth = self.depths.get(key).copied().unwrap_or(0);
            if depth == 0 || depth > 20 {
                continue;
            }
            // Rebuild filtered weights for this node.
            let mut weights: Vec<(u32, f64, u32)> = Vec::new();
            let mut node_games = 0u32;
            for &code in moves.keys() {
                let mut w = 0.0;
                let mut g = 0u32;
                if let Some(obs) = self.obs.get(&(cidx as u8, *key, code)) {
                    for ob in obs {
                        if ob.reply {
                            continue;
                        }
                        if filter.matches_obs(ob.is_online, ob.tc, ob.date, ob.is_rated, self.as_of)
                        {
                            w += ob.weight;
                            g += 1;
                        }
                    }
                }
                if g > 0 {
                    weights.push((code, w, g));
                    node_games += g;
                }
            }
            if node_games < 3 || weights.is_empty() {
                continue;
            }
            sample_games += node_games;
            let total_w: f64 = weights.iter().map(|(_, w, _)| *w).sum();
            if total_w <= 0.0 {
                continue;
            }
            let mut h = 0.0;
            for (_, w, _) in &weights {
                let p = *w / total_w;
                if p > 0.0 {
                    h -= p * p.ln();
                }
            }
            inv_perplexities.push(((-h).exp(), f64::from(node_games)));
            if let Some(index) = reference {
                total_moves += 1;
                let ref_moves = index.moves(*key);
                if !ref_moves.is_empty() {
                    exit_plies.push(depth);
                    // Booked-up if the player's top move appears in the
                    // reference's top three.
                    weights.sort_by(|a, b| b.1.total_cmp(&a.1));
                    let mut ref_sorted = ref_moves;
                    ref_sorted.sort_by_key(|m| std::cmp::Reverse(m.games));
                    let top3: HashSet<u32> = ref_sorted.into_iter().take(3).map(|m| m.mv).collect();
                    if top3.contains(&weights[0].0) {
                        booked_moves += 1;
                    }
                }
            }
        }
        let consistency_score = if inv_perplexities.is_empty() {
            0.0
        } else {
            let (num, den) = inv_perplexities
                .iter()
                .fold((0.0, 0.0), |(n, d), (inv, w)| (n + inv * w, d + w));
            (num / den).clamp(0.0, 1.0) as f32
        };
        let consistency_label = if consistency_score >= 0.8 {
            "specialist"
        } else if consistency_score >= 0.55 {
            "focused"
        } else if consistency_score >= 0.35 {
            "varied"
        } else {
            "exploratory"
        };
        let mean_book_exit_ply = if exit_plies.is_empty() {
            0.0
        } else {
            exit_plies.iter().sum::<u32>() as f32 / exit_plies.len() as f32
        };
        let booked_up_score = if total_moves == 0 {
            0.0
        } else {
            booked_moves as f32 / total_moves as f32
        };
        RepertoireProfile {
            consistency_score,
            consistency_label: consistency_label.to_string(),
            mean_book_exit_ply,
            booked_up_score,
            sample_games,
        }
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

    /// Filtered recent games: only games passing `filter` are returned.
    pub fn recent_games_filtered(
        &self,
        key: u64,
        filter: &PrepFilterConfig,
        limit: usize,
    ) -> Vec<PlayerGameRef> {
        let Some(games) = self.game_refs.get(&key) else {
            return Vec::new();
        };
        games
            .iter()
            .filter(|game| {
                filter.matches_obs(
                    game.is_online,
                    game.tc,
                    game.date,
                    game.is_rated,
                    self.as_of,
                )
            })
            .take(limit)
            .copied()
            .collect()
    }

    /// Filtered recent games for one colour.
    pub fn recent_games_for_filtered(
        &self,
        key: u64,
        color: Color,
        filter: &PrepFilterConfig,
        limit: usize,
    ) -> Vec<PlayerGameRef> {
        let Some(games) = self.game_refs.get(&key) else {
            return Vec::new();
        };
        games
            .iter()
            .filter(|game| {
                game.color == color
                    && filter.matches_obs(
                        game.is_online,
                        game.tc,
                        game.date,
                        game.is_rated,
                        self.as_of,
                    )
            })
            .take(limit)
            .copied()
            .collect()
    }
}

/// Repertoire fingerprint for one colour under a preparation filter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepertoireProfile {
    /// Time-decayed inverse perplexity averaged over qualifying nodes
    /// (`0 = unpredictable`, `1 = always the same move`).
    pub consistency_score: f32,
    /// Human label for [`Self::consistency_score`].
    pub consistency_label: String,
    /// Mean mainline depth where the reference tree runs out.
    pub mean_book_exit_ply: f32,
    /// Share of the player's top moves that appear in the reference top 3.
    pub booked_up_score: f32,
    pub sample_games: u32,
}

/// Free-function wrapper for swapped-colour experience checks.
pub fn has_meaningful_swapped_color_experience(
    book: &PlayerBook,
    key: u64,
    our_color: Color,
    filter: &PrepFilterConfig,
    ply: u32,
) -> bool {
    book.has_swapped_color_experience(key, our_color, filter, ply)
}

/// A predicted move in the next game.
#[derive(Clone, Debug, Default, PartialEq)]
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
    /// True when the move was never played by the player and comes purely
    /// from the background prior.
    pub is_inferred_only: bool,
    /// Player probability minus field (background) share, in percentage
    /// points. Positive means the player favours this move vs the field.
    pub vs_field_pp: Option<f64>,
    /// Normalized sparkline points `(x, y)` for the move's popularity trend.
    pub sparkline: Vec<(f32, f32)>,
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
    // Own moves only: faced replies share the colour maps but are not the
    // player's repertoire and must not count as player data.
    let player_moves = book.own_moves_combined(key);

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
        let field_share = if database_total > 0 {
            f64::from(candidate.database_games) / database_total as f64
        } else {
            1.0 / candidate_count
        };
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
            is_inferred_only: player.games == 0,
            vs_field_pp: Some((probability - field_share) * 100.0),
            sparkline: Vec::new(),
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

/// Trend-boosted prediction (`v2`).
///
/// On top of [`predict`] this adds:
/// - a trend-boosted prior `pi ~ p_base + gamma * max(0, shrunk_recent - base)`
///   computed from `reference` quarterly buckets via
///   [`crate::recent::position_trends`],
/// - a sample-size-adaptive prior strength `alpha0 = 2.5 * (1.25 - 0.85 * C)
///   / (1 + 0.25 * N)` where `C` is the local inverse perplexity from the
///   player's weights and `N` the player's game count at the position, with
///   unseen mass `0.35 / (1 + 0.5 * N)`,
/// - player evidence `E(m) = w_all(m) + w_recent(m)` combining the decayed
///   and recent-window weights,
/// - inference caps: moves the player never played (`player_games == 0`) keep
///   `probability >= 0.08` for at most three moves; the rest are capped below.
/// - `is_inferred_only`, `vs_field_pp` and `sparkline` on every move.
///
/// `filter`/`color` select the filtered player view; pass `None`/`None` for
/// the unfiltered colour-blind book. `reference` supplies the background
/// quarterly buckets for the trend boost (typically the same `global` moves).
#[allow(clippy::too_many_arguments)]
pub fn predict_v2(
    book: &PlayerBook,
    key: u64,
    global: &[MoveStat],
    prior_source: PriorSource,
    legal_moves: usize,
    filter: Option<&PrepFilterConfig>,
    color: Option<Color>,
    reference: Option<&[MoveStat]>,
) -> Prediction {
    let player_moves: BTreeMap<u32, PlayerMoveStat> = match (filter, color) {
        (Some(f), Some(c)) => book
            .filtered_moves(key, c, f)
            .into_iter()
            .map(|m| (m.mv, m))
            .collect(),
        _ => {
            // Unfiltered colour-blind book: own moves only, merged across both
            // maps. Faced replies must not count as player data.
            book.own_moves_combined(key)
        }
    };

    // Trend boost from the reference background, if supplied.
    let mut boost: HashMap<u32, f64> = HashMap::new();
    let mut sparklines: HashMap<u32, Vec<(f32, f32)>> = HashMap::new();
    if let Some(ref_moves) = reference {
        let trends =
            crate::recent::position_trends(ref_moves, crate::recent::TrendConfig::default());
        for trend in trends {
            if let (Some(shrunk), Some(base)) = (trend.shrunk_recent_share, trend.baseline_share) {
                let delta = (shrunk - base).max(0.0);
                if delta > 0.0 {
                    // gamma = 0.5 on the shrunk share delta.
                    boost.insert(trend.mv, 0.5 * delta);
                }
            }
            sparklines.insert(trend.mv, trend.sparkline_points());
        }
    }

    // Local consistency C: inverse perplexity of the player's weighted
    // distribution at this position.
    let total_w: f64 = player_moves.values().map(|m| m.weighted).sum();
    let recent_w: f64 = player_moves.values().map(|m| m.recent_weighted).sum();
    // Player sample size at this position (raw game count).
    let n_player: f64 = player_moves.values().map(|m| f64::from(m.games)).sum();
    let c_local = if total_w > 0.0 {
        let mut h = 0.0;
        for m in player_moves.values() {
            let p = m.weighted / total_w;
            if p > 0.0 {
                h -= p * p.ln();
            }
        }
        (-h).exp().clamp(0.0, 1.0)
    } else {
        0.5
    };

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
    let unseen_moves = legal_moves.saturating_sub(candidates.len());
    // Sample-size-adaptive background strength: a large exclusive player
    // sample earns a tiny prior, while unseen positions keep the floor.
    let alpha0 = 2.5 * (1.25 - 0.85 * c_local) / (1.0 + 0.25 * n_player);
    let unseen_alpha = if unseen_moves > 0 {
        0.35 / (1.0 + 0.5 * n_player)
    } else {
        0.0
    };
    let mut prior_total = 0.0;
    for (mv, candidate) in candidates.iter_mut() {
        let mut share = if database_total > 0 {
            f64::from(candidate.database_games) / database_total as f64
        } else {
            1.0 / candidate_count
        };
        if let Some(b) = boost.get(mv) {
            share += *b;
        }
        candidate.alpha = alpha0 * share + book.config.smoothing;
        prior_total += candidate.alpha;
    }

    // Player evidence combines the decayed and recent-window weights.
    let evidence_total = total_w + recent_w;
    let decay_total = prior_total + unseen_alpha + evidence_total;
    let recent_total = prior_total + unseen_alpha + recent_w;
    let recent_mix = if recent_w > 0.0 {
        recent_w / (recent_w + book.config.recent_mix_games)
    } else {
        0.0
    };

    let mut moves = Vec::with_capacity(candidates.len());
    let mut player_games = 0;
    let mut recent_games = 0;
    for (code, candidate) in candidates {
        let player = player_moves.get(&code).copied().unwrap_or_default();
        let evidence = player.weighted + player.recent_weighted;
        let decay_probability = (candidate.alpha + evidence) / decay_total;
        let recent_probability = if recent_w > 0.0 {
            (candidate.alpha + player.recent_weighted) / recent_total
        } else {
            decay_probability
        };
        let probability = (1.0 - recent_mix) * decay_probability + recent_mix * recent_probability;
        let decay_variance = beta_variance(
            decay_probability,
            prior_total + unseen_alpha + evidence_total,
        );
        let recent_variance =
            beta_variance(recent_probability, prior_total + unseen_alpha + recent_w);
        let variance =
            (1.0 - recent_mix).powi(2) * decay_variance + recent_mix.powi(2) * recent_variance;
        let margin = CREDIBLE_Z_90 * variance.sqrt();
        let trend_pp = if recent_w > 0.0 && total_w > recent_w {
            let recent_share = player.recent_weighted / recent_w;
            let historical_weight = (player.weighted - player.recent_weighted).max(0.0);
            let historical_total = total_w - recent_w;
            Some((recent_share - historical_weight / historical_total) * 100.0)
        } else {
            None
        };
        let field_share = if database_total > 0 {
            f64::from(candidate.database_games) / database_total as f64
        } else {
            1.0 / candidate_count
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
            is_inferred_only: player.games == 0,
            vs_field_pp: Some((probability - field_share) * 100.0),
            sparkline: sparklines.get(&code).cloned().unwrap_or_default(),
        });
    }

    moves.sort_by(|a, b| {
        b.probability
            .total_cmp(&a.probability)
            .then(b.player_games.cmp(&a.player_games))
            .then(a.mv.cmp(&b.mv))
    });

    let mut unseen_probability = if unseen_alpha > 0.0 {
        let decay_unseen = unseen_alpha / decay_total;
        let recent_unseen = if recent_w > 0.0 {
            unseen_alpha / recent_total
        } else {
            decay_unseen
        };
        (1.0 - recent_mix) * decay_unseen + recent_mix * recent_unseen
    } else {
        0.0
    };
    // Renormalize the listed moves plus unseen mass back to 1.
    let listed: f64 = moves.iter().map(|m| m.probability).sum();
    let total = listed + unseen_probability;
    if total > 0.0 && (total - 1.0).abs() > 1e-9 {
        let scale = 1.0 / total;
        for mv in moves.iter_mut() {
            mv.probability *= scale;
            mv.probability_low = (mv.probability_low * scale).clamp(0.0, 1.0);
            mv.probability_high = (mv.probability_high * scale).clamp(0.0, 1.0);
        }
        unseen_probability *= scale;
    }
    // Cap inferred moves: at most three keep probability >= 0.08. Capped
    // moves are pinned just below the threshold and the remaining mass is
    // redistributed over the uncapped moves (and unseen mass) so the total
    // stays exactly 1 without pushing the capped moves back over. Repeat
    // until stable: rescaling can lift a sub-threshold move over the line.
    let mut capped = vec![false; moves.len()];
    loop {
        let mut inferred_rank: Vec<usize> = moves
            .iter()
            .enumerate()
            .filter(|(_, mv)| mv.player_games == 0)
            .map(|(i, _)| i)
            .collect();
        inferred_rank.sort_by(|&a, &b| {
            moves[b]
                .probability
                .total_cmp(&moves[a].probability)
                .then(a.cmp(&b))
        });
        let mut newly_capped = false;
        for (rank, &i) in inferred_rank.iter().enumerate() {
            if rank >= 3 && !capped[i] && moves[i].probability >= 0.08 {
                moves[i].probability = 0.08_f64.next_down();
                moves[i].probability_low = moves[i].probability_low.min(moves[i].probability);
                moves[i].probability_high = moves[i]
                    .probability_high
                    .min(moves[i].probability + 0.02)
                    .max(moves[i].probability);
                capped[i] = true;
                newly_capped = true;
            }
        }
        if !newly_capped {
            break;
        }
        let fixed: f64 = moves
            .iter()
            .enumerate()
            .filter(|(i, _)| capped[*i])
            .map(|(_, mv)| mv.probability)
            .sum();
        let flex: f64 = moves
            .iter()
            .enumerate()
            .filter(|(i, _)| !capped[*i])
            .map(|(_, mv)| mv.probability)
            .sum::<f64>()
            + unseen_probability;
        if flex <= 0.0 {
            // Degenerate (no player data, no unseen mass): scale to 1.
            let all: f64 = moves.iter().map(|mv| mv.probability).sum();
            if all > 0.0 {
                for mv in moves.iter_mut() {
                    mv.probability /= all;
                }
            }
            break;
        }
        let scale = (1.0 - fixed) / flex;
        for (i, mv) in moves.iter_mut().enumerate() {
            if !capped[i] {
                mv.probability = (mv.probability * scale).clamp(0.0, 1.0);
                mv.probability_low = (mv.probability_low * scale).clamp(0.0, 1.0);
                mv.probability_high = (mv.probability_high * scale).clamp(0.0, 1.0);
            }
        }
        unseen_probability = (unseen_probability * scale).clamp(0.0, 1.0);
    }

    Prediction {
        moves,
        player_games,
        recent_games,
        recent_mix,
        has_player_data: total_w > 0.0,
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
    fn player_key_rejects_bye_and_placeholders() {
        assert_eq!(player_key("bye"), None);
        assert_eq!(player_key("BYE"), None);
        assert_eq!(player_key("forfeit"), None);
        assert_eq!(player_key("unknown"), None);
        assert_eq!(player_key("unpaired"), None);
        assert_eq!(player_key("vacant"), None);
        assert_eq!(player_key("none"), None);
        assert_eq!(player_key("n/a"), None);
        assert_eq!(player_key("na"), None);
        assert_eq!(player_key("tbd"), None);
        assert_eq!(player_key("anonymous"), None);
        assert_eq!(player_key("?"), None);
    }

    #[test]
    fn player_key_strips_honorifics() {
        assert_eq!(
            player_key("Kern, Johannes, Dr."),
            player_key("Johannes Kern")
        );
        assert_eq!(
            player_key("Prof. Dr. Carlsen, Magnus"),
            player_key("Magnus Carlsen")
        );
        assert_eq!(
            player_key("Dipl.-Ing. Max Mustermann, Jr."),
            player_key("Max Mustermann")
        );
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
    fn directory_keeps_a_real_numeric_fide_id() {
        const FIDE_GAMES: &str = r#"[Event "FIDE"]
[Site "Test"]
[Date "2024.01.01"]
[White "Carlsen, Magnus"]
[WhiteFideId " 1503014 "]
[Black "Doe, Jane"]
[BlackFideId "not-an-id"]
[Result "1-0"]

1. e4 e5 1-0

[Event "FIDE"]
[Site "Test"]
[Date "2024.02.01"]
[White "Carlsen, Magnus"]
[WhiteFideId "1503015"]
[Black "Roe, Richard"]
[Result "1/2-1/2"]

1. d4 d5 1/2-1/2
"#;
        let mut path = std::env::temp_dir();
        path.push(format!(
            "brett-fide-{}-{}.pgn",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, FIDE_GAMES).unwrap();
        let library = PgnLibrary::build(&path).unwrap();
        let directory = PlayerDirectory::build(&library);

        let magnus = directory
            .get(directory.find_key("carlsen magnus").unwrap())
            .unwrap();
        assert_eq!(magnus.fide_id.as_deref(), Some("1503014"));
        let jane = directory
            .get(directory.find_key("doe jane").unwrap())
            .unwrap();
        assert_eq!(jane.fide_id, None, "a non-numeric value is not a FIDE id");
        let richard = directory
            .get(directory.find_key("richard roe").unwrap())
            .unwrap();
        assert_eq!(richard.fide_id, None, "no id is inferred from a name");

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn directory_keeps_a_real_normalized_country() {
        const COUNTRY_GAMES: &str = r#"[Event "Olympiad"]
[Site "Budapest"]
[Date "2024.09.11"]
[White "Carlsen, Magnus"]
[WhiteCountry "NOR"]
[Black "Nakamura, Hikaru"]
[BlackFed "USA"]
[Result "1-0"]

1. e4 e5 1-0

[Event "Open"]
[Site "Test"]
[Date "2024.09.12"]
[White "Carlsen, Magnus"]
[WhiteCountry "DEU"]
[Black "Doe, Jane"]
[BlackCountry "XYZ"]
[Result "1-0"]

1. d4 d5 1-0

[Event "Open"]
[Site "Test"]
[Date "2024.09.13"]
[White "Smith, John"]
[WhiteCountry "?"]
[Black "Roe, Richard"]
[Result "1/2-1/2"]

1. c4 c5 1/2-1/2
"#;
        let mut path = std::env::temp_dir();
        path.push(format!(
            "brett-country-{}-{}.pgn",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, COUNTRY_GAMES).unwrap();
        let library = PgnLibrary::build(&path).unwrap();
        let directory = PlayerDirectory::build(&library);

        // First country tag seen is kept
        let magnus = directory
            .get(directory.find_key("carlsen magnus").unwrap())
            .unwrap();
        assert_eq!(magnus.country.map(|c| c.code), Some("NOR"));
        assert_eq!(magnus.country.map(|c| c.flag), Some("🇳🇴"));

        // BlackFed fallback
        let hikaru = directory
            .get(
                directory
                    .find_key(&player_key("Nakamura, Hikaru").unwrap())
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(hikaru.country.map(|c| c.code), Some("USA"));
        assert_eq!(hikaru.country.map(|c| c.flag), Some("🇺🇸"));

        // Unknown country code XYZ yields None
        let jane = directory
            .get(directory.find_key("doe jane").unwrap())
            .unwrap();
        assert_eq!(jane.country, None, "unknown country code is discarded");

        // Placeholder ? yields None
        let john = directory
            .get(
                directory
                    .find_key(&player_key("Smith, John").unwrap())
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(john.country, None, "placeholder is discarded");

        // Absent country tag yields None, never guessed
        let richard = directory
            .get(directory.find_key("richard roe").unwrap())
            .unwrap();
        assert_eq!(richard.country, None, "no country inferred from name");

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
        // 1. d4 is never reached in the fixture (every game opens 1. e4), so
        // the player has no data here — neither own moves nor faced replies.
        let after_d4 = start
            .clone()
            .play(
                SanPlus::from_ascii(b"d4")
                    .unwrap()
                    .san
                    .to_move(&start)
                    .unwrap(),
            )
            .unwrap();
        let key = position::position_key(&after_d4);
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
            after_d4.legal_moves().len(),
        );
        assert!(!prediction.has_player_data);
        assert_eq!(prediction.moves[0].mv, global[0].mv);
        assert!(prediction.moves[0].probability > 0.8);
        assert!(prediction.moves[0].probability < 1.0);
        assert!(prediction.moves[0].probability_high > prediction.moves[0].probability);
        assert!(prediction.unseen_probability > 0.0);
    }

    #[test]
    fn replies_excluded_from_own_prediction() {
        // Trendy is Black in every fixture game, so at the root (White to
        // move) the book holds only White replies — never Trendy's own moves.
        let book = trend_book();
        let start = Chess::default();
        let key = position::position_key(&start);
        let filter = PrepFilterConfig::default();
        let e4 = position::encode_move(
            SanPlus::from_ascii(b"e4")
                .unwrap()
                .san
                .to_move(&start)
                .unwrap(),
        )
        .unwrap();
        let replies = book.filtered_replies(key, Color::Black, &filter);
        assert_eq!(
            replies.iter().map(|m| m.games).sum::<u32>(),
            8,
            "all eight faced e4 replies stay indexed"
        );
        assert!(replies.iter().any(|m| m.mv == e4), "the faced reply is e4");
        assert!(
            book.filtered_moves(key, Color::Black, &filter).is_empty(),
            "replies are not the player's own moves"
        );
        assert!(book.moves(key).is_empty());
        assert!(book.stats(key).is_none());
        let global = vec![
            MoveStat {
                key,
                mv: e4,
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
        assert_eq!(prediction.player_games, 0);
        assert_eq!(prediction.moves[0].mv, global[0].mv);
        // The filtered and unfiltered v2 paths agree: no own moves at a
        // reply-only position.
        for (f, c) in [(Some(&filter), Some(Color::Black)), (None, None)] {
            let prediction_v2 = predict_v2(
                &book,
                key,
                &global,
                PriorSource::CurrentDatabase,
                start.legal_moves().len(),
                f,
                c,
                None,
            );
            assert!(!prediction_v2.has_player_data);
            assert_eq!(prediction_v2.player_games, 0);
        }
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
        let move_prediction = &prediction.moves[0];
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

    #[test]
    fn chesscom_tournament_exclusion_scoping() {
        // Named Chess.com tournament events are excluded
        assert!(is_chesscom_tournament_game(
            "Titled Tuesday intern op 21st Feb Early",
            "Chess.com INT"
        ));
        assert!(is_chesscom_tournament_game("Titled Tuesday", ""));
        assert!(is_chesscom_tournament_game("Arena Kings Season 9", "?"));
        assert!(is_chesscom_tournament_game(
            "Speed Chess Championship Main Event",
            "chess.com"
        ));
        assert!(is_chesscom_tournament_game(
            "Bullet Chess Championship",
            "Internet"
        ));
        assert!(is_chesscom_tournament_game(
            "PRO Chess League Prelims",
            "chess.com INT"
        ));
        // Chess.com INT site with a tournament event is excluded
        assert!(is_chesscom_tournament_game(
            "Club Championship Qualifier",
            "Chess.com INT"
        ));

        // Ordinary online account games are kept, even on Chess.com
        assert!(!is_chesscom_tournament_game(
            "Random Online Blitz",
            "Chess.com"
        ));
        assert!(!is_chesscom_tournament_game("Casual game", "Chess.com"));
        assert!(!is_chesscom_tournament_game("Live Chess", "Chess.com INT"));

        // OTB tournaments are preserved, even if sponsored by Chess.com
        assert!(!is_chesscom_tournament_game(
            "Tata Steel Masters",
            "Wijk aan Zee"
        ));
        assert!(!is_chesscom_tournament_game(
            "World Championship Match",
            "Dubai"
        ));
        assert!(!is_chesscom_tournament_game(
            "Chess.com Isle of Man International",
            "Douglas, Isle of Man"
        ));
        assert!(!is_chesscom_tournament_game(
            "FIDE Chess.com Grand Swiss",
            "Douglas IOM"
        ));

        // The legacy alias agrees with the scoped detector.
        assert!(is_chesscom_event("Titled Tuesday", ""));
        assert!(!is_chesscom_event("Random Online Blitz", "Chess.com"));
    }

    #[test]
    fn player_book_excludes_chesscom_online_games() {
        let pgn = r#"[Event "Test"]
[Site "City"]
[Date "2024.01.01"]
[White "Carlsen, Magnus"]
[Black "Nakamura, Hikaru"]
[Result "1-0"]

1. e4 e5 2. Nf3 Nc6 1-0
"#;
        let base_game = crate::domain::parse_pgn(pgn.as_bytes())
            .unwrap()
            .into_iter()
            .next()
            .unwrap();

        let mut online_game = base_game.clone();
        online_game.set_header("Event", "Titled Tuesday 2024".to_string());
        online_game.set_header("Site", "Chess.com INT".to_string());

        let mut otb_game = base_game.clone();
        otb_game.set_header("Event", "Tata Steel Masters".to_string());
        otb_game.set_header("Site", "Wijk aan Zee".to_string());

        let player = "Carlsen, Magnus";
        let key = player_key(player).unwrap();

        // With exclusion disabled (default):
        let book_default = PlayerBook::from_games(
            &key,
            player,
            &[online_game.clone(), otb_game.clone()],
            PredictionConfig::default(),
            2025_0101,
        );
        assert_eq!(book_default.games(), 2);

        // With exclusion enabled:
        let config_exclude = PredictionConfig {
            exclude_chesscom: true,
            ..PredictionConfig::default()
        };
        let book_filtered = PlayerBook::from_games(
            &key,
            player,
            &[online_game, otb_game],
            config_exclude,
            2025_0101,
        );
        assert_eq!(book_filtered.games(), 1);
    }

    fn game_with_headers(movetext: &str, headers: &[(&str, &str)]) -> Game {
        let mut game = domain::parse_pgn(
            format!(
                "{}\n\n{movetext}\n",
                headers
                    .iter()
                    .map(|(k, v)| format!("[{k} \"{v}\"]"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
            .as_bytes(),
        )
        .unwrap()
        .remove(0);
        // parse_pgn requires a Result tag for complete games; tolerate `*`.
        let _ = &mut game;
        game
    }

    fn otb_game(white: &str, black: &str, date: &str, movetext: &str) -> Game {
        game_with_headers(
            movetext,
            &[
                ("Event", "Masters"),
                ("Site", "Wijk aan Zee"),
                ("Date", date),
                ("White", white),
                ("Black", black),
                ("Result", "1-0"),
            ],
        )
    }

    fn online_game(white: &str, black: &str, date: &str, movetext: &str) -> Game {
        game_with_headers(
            movetext,
            &[
                ("Event", "Titled Tuesday"),
                ("Site", "Chess.com INT"),
                ("Date", date),
                ("White", white),
                ("Black", black),
                ("Result", "1-0"),
                ("TimeControl", "180+1"),
            ],
        )
    }

    fn casual_online_game(white: &str, black: &str, date: &str, movetext: &str) -> Game {
        game_with_headers(
            movetext,
            &[
                ("Event", "Casual game"),
                ("Site", "lichess.org"),
                ("Date", date),
                ("White", white),
                ("Black", black),
                ("Result", "1-0"),
                ("TimeControl", "180+1"),
            ],
        )
    }

    #[test]
    fn truncated_and_enumerated_names_match_full_name() {
        assert!(matches_truncated_or_enumerated_name(
            "Hinterreiter, Martin",
            "Hinterreiter, M"
        ));
        assert!(matches_truncated_or_enumerated_name(
            "Hinterreiter, Martin",
            "Hinterreiter,M"
        ));
        assert!(matches_truncated_or_enumerated_name(
            "Hinterreiter, Martin",
            "Hinterreiter, M."
        ));
        assert!(matches_truncated_or_enumerated_name(
            "Hinterreiter, Martin",
            "Hinterreiter, M 1"
        ));
        assert!(matches_truncated_or_enumerated_name(
            "Carlsen, Magnus",
            "Carlsen, M 2"
        ));
        assert!(matches_truncated_or_enumerated_name(
            "Carlsen, Magnus",
            "Magnus Carlsen"
        ));
        assert!(matches_truncated_or_enumerated_name(
            "CARLSEN, magnus",
            "carlsen m."
        ));
        assert!(matches_truncated_or_enumerated_name("Carlsen", "Carlsen"));
        assert!(!matches_truncated_or_enumerated_name(
            "Carlsen, Magnus",
            "Carlsen, H"
        ));
        assert!(!matches_truncated_or_enumerated_name(
            "Carlsen, Magnus",
            "Nakamura, M"
        ));
        assert!(!matches_truncated_or_enumerated_name(
            "Carlsen, Magnus",
            "Carlsen, Hikaru"
        ));
        assert!(!matches_truncated_or_enumerated_name("", "Carlsen, M"));
        assert!(!matches_truncated_or_enumerated_name(
            "Carlsen, Magnus",
            "?"
        ));
    }

    #[test]
    fn canonicalized_tags_keep_truncated_chessabc_games() {
        let mut game = otb_game("Hinterreiter,M 1", "Opponent", "2025.05.01", "1. e4 e5 1-0");
        canonicalize_player_tags(&mut game, "Hinterreiter, Martin");
        assert_eq!(game.white(), "Hinterreiter, Martin");
        assert_eq!(game.black(), "Opponent");
        // A genuinely different player is untouched.
        let mut other = otb_game("Nakamura, H", "Opponent", "2025.05.01", "1. e4 e5 1-0");
        canonicalize_player_tags(&mut other, "Hinterreiter, Martin");
        assert_eq!(other.white(), "Nakamura, H");
    }

    #[test]
    fn rated_and_casual_toggles_filter_online_games() {
        let rated = online_game("Opponent", "Trendy", "2025.06.01", "1. e4 c5 2. Nf3 d6 1-0");
        let casual =
            casual_online_game("Opponent", "Trendy", "2025.06.01", "1. e4 c5 2. Nf3 d6 0-1");
        let book = PlayerBook::from_games(
            "trendy",
            "Trendy",
            &[rated, casual],
            PredictionConfig::default(),
            2026_1001,
        );
        let after_e4 = Chess::default()
            .play(
                SanPlus::from_ascii(b"e4")
                    .unwrap()
                    .san
                    .to_move(&Chess::default())
                    .unwrap(),
            )
            .unwrap();
        let key = position::position_key(&after_e4);
        let both = PrepFilterConfig::default();
        assert!(both.include_rated && both.include_casual);
        assert_eq!(
            book.filtered_moves(key, Color::Black, &both)
                .iter()
                .map(|m| m.games)
                .sum::<u32>(),
            2
        );
        let rated_only = PrepFilterConfig {
            include_casual: false,
            ..PrepFilterConfig::default()
        };
        assert_eq!(
            book.filtered_moves(key, Color::Black, &rated_only)
                .iter()
                .map(|m| m.games)
                .sum::<u32>(),
            1
        );
        let casual_only = PrepFilterConfig {
            include_rated: false,
            ..PrepFilterConfig::default()
        };
        assert_eq!(
            book.filtered_moves(key, Color::Black, &casual_only)
                .iter()
                .map(|m| m.games)
                .sum::<u32>(),
            1
        );
        let neither = PrepFilterConfig {
            include_rated: false,
            include_casual: false,
            ..PrepFilterConfig::default()
        };
        assert!(book.filtered_moves(key, Color::Black, &neither).is_empty());
    }

    #[test]
    fn account_filter_zero_overlap_badge_logic() {
        assert!(AccountFilter::default().has_zero_overlap().is_none());
        assert!(
            AccountFilter {
                include_rated: false,
                include_casual: false,
                ..AccountFilter::default()
            }
            .has_zero_overlap()
            .is_some()
        );
        assert!(
            AccountFilter {
                time_controls: TimeControlMask {
                    classical: false,
                    rapid: false,
                    blitz: false,
                    bullet: false,
                },
                ..AccountFilter::default()
            }
            .has_zero_overlap()
            .is_some()
        );
        assert!(
            AccountFilter {
                min_elo: Some(2500),
                max_elo: Some(2000),
                ..AccountFilter::default()
            }
            .has_zero_overlap()
            .is_some()
        );
        // A bounded but consistent range overlaps.
        assert!(
            AccountFilter {
                min_elo: Some(2000),
                max_elo: Some(2500),
                ..AccountFilter::default()
            }
            .has_zero_overlap()
            .is_none()
        );
    }

    #[test]
    fn account_filter_matches_games() {
        let filter = AccountFilter::default();
        let as_of = 2026_1001;
        let rated = online_game("Opponent", "Trendy", "2025.06.01", "1. e4 e5 1-0");
        assert!(filter.matches_game(&rated, as_of));
        let casual = casual_online_game("Opponent", "Trendy", "2025.06.01", "1. e4 e5 1-0");
        assert!(filter.matches_game(&casual, as_of));
        let no_casual = AccountFilter {
            include_casual: false,
            ..AccountFilter::default()
        };
        assert!(no_casual.matches_game(&rated, as_of));
        assert!(!no_casual.matches_game(&casual, as_of));
        // Elo bounds apply when tags are present; untagged games pass.
        let mut elo_game = rated.clone();
        elo_game.set_header("WhiteElo", "2600".to_string());
        elo_game.set_header("BlackElo", "2550".to_string());
        let bounded = AccountFilter {
            min_elo: Some(2400),
            max_elo: Some(2700),
            ..AccountFilter::default()
        };
        assert!(bounded.matches_game(&elo_game, as_of));
        let too_strong = AccountFilter {
            min_elo: Some(2700),
            ..AccountFilter::default()
        };
        assert!(!too_strong.matches_game(&elo_game, as_of));
        assert!(too_strong.matches_game(&rated, as_of));
        // Stale games pass the default all-time cutoff, but fall outside an explicit 24-month window.
        let stale = online_game("Opponent", "Trendy", "2020.03.01", "1. e4 e5 1-0");
        assert!(filter.matches_game(&stale, as_of));
        let cutoff_filter = AccountFilter {
            date_cutoff: DateCutoff::Months(24),
            ..filter
        };
        assert!(!cutoff_filter.matches_game(&stale, as_of));
    }

    #[test]
    fn account_filter_round_trips_through_json() {
        let account = PrepOnlineAccount {
            source: OnlineSource::Lichess,
            username: "DrNykterstein".to_string(),
            enabled: true,
            filter: AccountFilter {
                include_casual: false,
                min_elo: Some(2200),
                max_elo: Some(2800),
                date_cutoff: DateCutoff::Months(12),
                ..AccountFilter::default()
            },
            filter_version: 2,
        };
        let restored = PrepOnlineAccount::from_value(&account.to_value()).expect("round trip");
        assert_eq!(restored, account);
        // Legacy entries without a filter object keep working.
        let legacy = serde_json::json!({
            "service": "chesscom",
            "username": "hikaru",
            "enabled": false,
        });
        let restored = PrepOnlineAccount::from_value(&legacy).expect("legacy entry");
        assert_eq!(restored.filter, AccountFilter::default());
        assert!(!restored.enabled);
    }

    #[test]
    fn deduplicates_across_mega_and_chessabc() {
        let moves = "1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 1-0";
        let mut mega = otb_game(
            "Carlsen, Magnus",
            "Nepomniachtchi, Ian",
            "2024.01.01",
            moves,
        );
        mega.set_header("Event", "Masters".to_string());
        let mut abc = otb_game(
            "Carlsen, Magnus",
            "Nepomniachtchi, Ian",
            "2024.01.02",
            moves,
        );
        abc.set_header("TimeControl", "3600+30".to_string());
        abc.set_header("WhiteElo", "2830".to_string());
        abc.set_header("Event", "Masters".to_string());

        let merged = deduplicate_and_merge_games(vec![
            (SourceTag::Owoju, vec![mega]),
            (SourceTag::Online(OnlineSource::Lichess), vec![abc.clone()]),
        ]);
        assert_eq!(merged.len(), 1, "same game from two sources merges");
        assert_eq!(merged[0].0, SourceTag::Owoju, "first source tag is kept");
        assert_eq!(
            merged[0].1.header("TimeControl"),
            Some("3600+30"),
            "richest headers win"
        );
        assert_eq!(merged[0].1.header("WhiteElo"), Some("2830"));

        // A different result is a different game.
        let mut other = abc.clone();
        other.set_header("Result", "0-1".to_string());
        let merged = deduplicate_and_merge_games(vec![
            (SourceTag::Owoju, vec![abc.clone()]),
            (SourceTag::Owoju, vec![other]),
        ]);
        assert_eq!(merged.len(), 2);

        // Unknown day shares YYYYMM with a dated duplicate.
        let undated = otb_game(
            "Carlsen, Magnus",
            "Nepomniachtchi, Ian",
            "2024.01.??",
            moves,
        );
        let mut dated = undated.clone();
        dated.set_header("Date", "2024.01.15".to_string());
        let merged = deduplicate_and_merge_games(vec![
            (SourceTag::Owoju, vec![undated]),
            (SourceTag::Owoju, vec![dated]),
        ]);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn recency_trumps_tc() {
        // Same online blitz move, one old and one recent. The TC mask allows
        // blitz, but the 24-month online cutoff still drops the old game.
        let old = online_game("Opponent", "Trendy", "2020.03.01", "1. e4 c5 2. Nf3 d6 1-0");
        let new = online_game("Opponent", "Trendy", "2026.02.01", "1. e4 c5 2. Nf3 d6 1-0");
        let book = PlayerBook::from_games(
            "trendy",
            "Trendy",
            &[old, new],
            PredictionConfig::default(),
            2026_1001,
        );
        let after_e4 = Chess::default()
            .play(
                SanPlus::from_ascii(b"e4")
                    .unwrap()
                    .san
                    .to_move(&Chess::default())
                    .unwrap(),
            )
            .unwrap();
        let key = position::position_key(&after_e4);
        let filter = PrepFilterConfig {
            online_date_cutoff: DateCutoff::Months(24),
            ..PrepFilterConfig::default()
        };
        assert!(filter.online_time_controls.blitz);
        let moves = book.filtered_moves(key, Color::Black, &filter);
        assert_eq!(moves.len(), 1);
        assert_eq!(
            moves[0].games, 1,
            "only the recent game survives the date cutoff"
        );
        assert_eq!(moves[0].last_date, 2026_0201);
    }

    #[test]
    fn toggling_include_online_filters_instantly() {
        let otb = otb_game("Opponent", "Trendy", "2025.05.01", "1. e4 c5 2. Nf3 d6 1-0");
        let online = online_game("Opponent", "Trendy", "2025.06.01", "1. e4 c5 2. Nf3 d6 0-1");
        let book = PlayerBook::from_games(
            "trendy",
            "Trendy",
            &[otb, online],
            PredictionConfig::default(),
            2026_1001,
        );
        let after_e4 = Chess::default()
            .play(
                SanPlus::from_ascii(b"e4")
                    .unwrap()
                    .san
                    .to_move(&Chess::default())
                    .unwrap(),
            )
            .unwrap();
        let key = position::position_key(&after_e4);
        let with_online = PrepFilterConfig::default();
        let without_online = PrepFilterConfig {
            include_online: false,
            ..PrepFilterConfig::default()
        };
        let on = book.filtered_moves(key, Color::Black, &with_online);
        let off = book.filtered_moves(key, Color::Black, &without_online);
        assert_eq!(on.iter().map(|m| m.games).sum::<u32>(), 2);
        assert_eq!(off.iter().map(|m| m.games).sum::<u32>(), 1);
        // Same book, no rebuild: toggling back restores the online game.
        let on_again = book.filtered_moves(key, Color::Black, &with_online);
        assert_eq!(on_again.iter().map(|m| m.games).sum::<u32>(), 2);
        assert_eq!(
            book.filtered_stats(key, Color::Black, &without_online)
                .map(|s| s.games),
            Some(1)
        );
    }

    #[test]
    fn detects_swapped_color() {
        let games = [
            otb_game(
                "Swappy",
                "Opponent",
                "2025.01.01",
                "1. e4 e5 2. Nf3 Nc6 1-0",
            ),
            otb_game(
                "Swappy",
                "Opponent",
                "2025.06.01",
                "1. e4 e5 2. Nf3 Nc6 1-0",
            ),
            otb_game(
                "Swappy",
                "Opponent",
                "2026.01.01",
                "1. e4 e5 2. Nf3 Nc6 1-0",
            ),
        ];
        let book = PlayerBook::from_games(
            "swappy",
            "Swappy",
            &games,
            PredictionConfig::default(),
            2026_1001,
        );
        let filter = PrepFilterConfig::default();
        let start = position::position_key(&Chess::default());
        assert!(
            !book.has_swapped_color_experience(start, Color::White, &filter, 2),
            "positions shallower than 4 plies never count as swapped experience"
        );
        assert!(
            book.has_swapped_color_experience(start, Color::White, &filter, 4),
            "three games with our colour is meaningful once the line is deep enough"
        );
        assert!(
            has_meaningful_swapped_color_experience(&book, start, Color::White, &filter, 4),
            "free-function wrapper agrees"
        );
        assert!(
            !book.has_swapped_color_experience(start, Color::Black, &filter, 4),
            "no Black games exist in this fixture"
        );
        // Two games are meaningful once the line is deep.
        let pair = PlayerBook::from_games(
            "swappy",
            "Swappy",
            &games[..2],
            PredictionConfig::default(),
            2026_1001,
        );
        assert!(!pair.has_swapped_color_experience(start, Color::White, &filter, 4));
        assert!(pair.has_swapped_color_experience(start, Color::White, &filter, 8));
    }

    #[test]
    fn specialist_vs_exploratory() {
        let specialist_games: Vec<Game> = (1..=6)
            .map(|m| {
                otb_game(
                    "Specialist",
                    "Opponent",
                    &format!("2025.{m:02}.01"),
                    "1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 1-0",
                )
            })
            .collect();
        let specialist = PlayerBook::from_games(
            "specialist",
            "Specialist",
            &specialist_games,
            PredictionConfig::default(),
            2026_1001,
        );
        let exploratory_sans = ["Nf3", "Bc4", "Nc3", "d4", "f4", "c3", "Qh5", "g3"];
        // Replies are filtered out of the profile (they are faced, not played),
        // so the fixture keeps a constant reply and varies only the player's
        // own second moves.
        let exploratory_games: Vec<Game> = exploratory_sans
            .iter()
            .enumerate()
            .map(|(i, san)| {
                otb_game(
                    "Explorer",
                    "Opponent",
                    &format!("2025.{:02}.01", i + 1),
                    &format!("1. e4 e5 2. {san} 1-0"),
                )
            })
            .collect();
        let exploratory = PlayerBook::from_games(
            "explorer",
            "Explorer",
            &exploratory_games,
            PredictionConfig::default(),
            2026_1001,
        );
        let filter = PrepFilterConfig::default();
        let spec = specialist.profile(Color::White, &filter, None);
        let expl = exploratory.profile(Color::White, &filter, None);
        assert!(spec.sample_games >= 6, "profile sees qualifying nodes");
        assert!(expl.sample_games >= 8);
        assert!(
            spec.consistency_score > expl.consistency_score,
            "specialist {spec:?} vs exploratory {expl:?}"
        );
        assert_eq!(spec.consistency_label, "specialist");
        assert_eq!(expl.consistency_label, "exploratory");
    }

    #[test]
    fn unplayed_capped() {
        let solo = otb_game("Solo", "Opponent", "2025.06.01", "1. e4 e5 1-0");
        let book = PlayerBook::from_games(
            "solo",
            "Solo",
            std::slice::from_ref(&solo),
            PredictionConfig::default(),
            2026_1001,
        );
        let start = Chess::default();
        let key = position::position_key(&start);
        let legal = start.legal_moves();
        let mut heavy = std::collections::HashSet::new();
        for san in ["e4", "d4", "c4", "Nf3", "g3"] {
            let mv = SanPlus::from_ascii(san.as_bytes())
                .unwrap()
                .san
                .to_move(&start)
                .unwrap();
            heavy.insert(position::encode_move(mv).unwrap());
        }
        let global: Vec<MoveStat> = legal
            .iter()
            .map(|&mv| {
                let code = position::encode_move(mv).unwrap();
                MoveStat {
                    key,
                    mv: code,
                    games: if heavy.contains(&code) { 60 } else { 1 },
                    ..MoveStat::default()
                }
            })
            .collect();
        let prediction = predict_v2(
            &book,
            key,
            &global,
            PriorSource::Reference,
            legal.len(),
            None,
            None,
            None,
        );
        assert!(prediction.has_player_data);
        let inferred: Vec<_> = prediction
            .moves
            .iter()
            .filter(|m| m.player_games == 0 && m.is_inferred_only)
            .collect();
        let above: Vec<_> = inferred.iter().filter(|m| m.probability >= 0.08).collect();
        assert!(
            above.len() <= 3,
            "at most 3 unplayed moves keep >= 8%: got {}",
            above.len()
        );
        assert!(
            inferred.len() >= 4,
            "fixture needs enough inferred moves to test the cap"
        );
        assert!(
            prediction.moves.iter().any(|m| m.player_games == 1),
            "the played move stays listed"
        );
    }

    #[test]
    fn book_records_opponent_replies() {
        let game = otb_game(
            "Preppie",
            "Opponent",
            "2025.05.01",
            "1. e4 e5 2. Nf3 Nc6 1-0",
        );
        let book = PlayerBook::from_games(
            "preppie",
            "Preppie",
            std::slice::from_ref(&game),
            PredictionConfig::default(),
            2026_1001,
        );
        let start = Chess::default();
        let start_key = position::position_key(&start);
        let own = book.moves_for_color(start_key, Color::White);
        assert_eq!(own.len(), 1, "the player's own move is recorded");

        // After 1. e4 Black replies 1...e5. The reply is recorded under the
        // player's (White) colour index.
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
        let reply_key = position::position_key(&after_e4);
        // Replies are not the player's moves: every own-move reader excludes
        // them, even though they share the player's colour index.
        assert!(
            book.moves_for_color(reply_key, Color::White).is_empty(),
            "the opponent reply is not an own move"
        );
        assert!(
            book.filtered_moves(reply_key, Color::White, &PrepFilterConfig::default())
                .is_empty()
        );
        assert!(book.moves(reply_key).is_empty());
        assert!(book.stats(reply_key).is_none());
        // The reply is still indexed, on the reply side of the book.
        let filter = PrepFilterConfig::default();
        let replies = book.filtered_replies(reply_key, Color::White, &filter);
        assert_eq!(replies.len(), 1, "the opponent reply is indexed");
        let san = SanPlus::from_move(
            after_e4.clone(),
            position::decode_move(&after_e4, replies[0].mv).unwrap(),
        )
        .to_string();
        assert_eq!(san, "e5");
        // The result stays from the prepared player's perspective.
        assert_eq!((replies[0].wins, replies[0].losses), (1, 0));
        // The aggregated reply stats agree without a rebuild.
        let reply_stats = book
            .replies_stats(reply_key, Color::White, &filter)
            .expect("reply stats exist");
        assert_eq!(reply_stats.games, 1);
        assert!(
            book.filtered_stats(reply_key, Color::White, &filter)
                .is_none()
        );
    }

    #[test]
    fn untagged_otb_games_default_to_classical() {
        let otb = otb_game("A", "B", "2025.05.01", "1. e4 e5 1-0");
        let (tc, is_online, _) = classify_game_metadata(&otb);
        assert!(!is_online);
        assert_eq!(tc, TimeControlClass::Classical);
        // An explicit TimeControl tag still wins.
        let mut blitz = otb.clone();
        blitz.set_header("TimeControl", "180+2".to_string());
        assert_eq!(classify_game_metadata(&blitz).0, TimeControlClass::Blitz);
        // Event keywords still win.
        let mut keyworded = otb.clone();
        keyworded.set_header("Event", "World Blitz Championship".to_string());
        assert_eq!(
            classify_game_metadata(&keyworded).0,
            TimeControlClass::Blitz
        );
        // Untagged online games keep the Rapid default.
        let online_untagged = game_with_headers(
            "1. e4 e5 1-0",
            &[
                ("Event", "Casual game"),
                ("Site", "lichess.org"),
                ("Date", "2025.05.01"),
                ("White", "A"),
                ("Black", "B"),
                ("Result", "1-0"),
            ],
        );
        let (tc, is_online, _) = classify_game_metadata(&online_untagged);
        assert!(is_online);
        assert_eq!(tc, TimeControlClass::Rapid);
    }

    #[test]
    fn account_tags_canonicalize_by_username_key() {
        let mut game = otb_game("Magnus Carlsen", "Opponent", "2025.05.01", "1. e4 e5 1-0");
        canonicalize_account_tags(&mut game, "Carlsen, Magnus", "Carlsen, Magnus");
        assert_eq!(game.white(), "Carlsen, Magnus");
        assert_eq!(game.black(), "Opponent");
        // The other side matches too.
        let mut black_game = otb_game("Opponent", "Carlsen Magnus", "2025.05.01", "1. e4 e5 1-0");
        canonicalize_account_tags(&mut black_game, "Carlsen, Magnus", "Carlsen, Magnus");
        assert_eq!(black_game.black(), "Carlsen, Magnus");
        // Garbage usernames leave tags alone.
        let mut other = otb_game("Someone", "Else", "2025.05.01", "1. e4 e5 1-0");
        canonicalize_account_tags(&mut other, "?", "Carlsen, Magnus");
        assert_eq!(other.white(), "Someone");
        assert_eq!(other.black(), "Else");
    }

    #[test]
    fn exclusive_repertoire_overrides_large_background() {
        // 18/18 exclusive (7 recent) against a 1000-game background where the
        // player's move is rare: the calibrated prior must stay small.
        let as_of = 2026_1001;
        let mut games = Vec::new();
        for i in 0..18 {
            let date = if i < 7 {
                format!("2025.{:02}.01", i + 1)
            } else {
                format!("2020.{:02}.01", i - 7 + 1)
            };
            games.push(otb_game(
                "Exclusive",
                "Opponent",
                &date,
                "1. e4 e5 2. Nf3 Nc6 1-0",
            ));
        }
        let book = PlayerBook::from_games(
            "exclusive",
            "Exclusive",
            &games,
            PredictionConfig::default(),
            as_of,
        );
        let start = Chess::default();
        let key = position::position_key(&start);
        let code = |san: &[u8]| {
            position::encode_move(
                SanPlus::from_ascii(san)
                    .unwrap()
                    .san
                    .to_move(&start)
                    .unwrap(),
            )
            .unwrap()
        };
        let global = vec![
            MoveStat {
                key,
                mv: code(b"d4"),
                games: 950,
                ..MoveStat::default()
            },
            MoveStat {
                key,
                mv: code(b"e4"),
                games: 10,
                ..MoveStat::default()
            },
            MoveStat {
                key,
                mv: code(b"c4"),
                games: 40,
                ..MoveStat::default()
            },
        ];
        let prediction = predict_v2(
            &book,
            key,
            &global,
            PriorSource::Reference,
            start.legal_moves().len(),
            None,
            None,
            None,
        );
        assert!(prediction.has_player_data);
        assert_eq!(prediction.player_games, 18);
        assert_eq!(prediction.recent_games, 7);
        let exclusive = prediction
            .moves
            .iter()
            .find(|mv| mv.mv == code(b"e4"))
            .expect("exclusive move is listed");
        assert!(
            exclusive.probability >= 0.90,
            "exclusive e4 should dominate, got {:.3}",
            exclusive.probability
        );
        assert!(exclusive.probability_low <= exclusive.probability);
        assert!(exclusive.probability_high >= exclusive.probability);
    }

    #[test]
    fn player_book_indexes_moves_after_single_position_repetition() {
        let pgn = b"[White \"Kern, Johannes\"]\n[Black \"Opponent, Test\"]\n[Result \"1-0\"]\n\n1. Nf3 Nf6 2. Ng1 Ng8 3. e4 e5 4. Bc4 Nc6 1-0\n";
        let games = crate::domain::parse_pgn(pgn).expect("valid pgn");
        let book = PlayerBook::from_games(
            "johannes kern",
            "Kern, Johannes",
            &games,
            PredictionConfig::default(),
            20261107,
        );
        let start = crate::chess::Chess::default();
        let key = crate::position::position_key(&start);
        let filter = PrepFilterConfig::default();
        let moves = book.filtered_moves(key, Color::White, &filter);
        let sans: Vec<String> = moves
            .iter()
            .filter_map(|m| crate::position::decode_move(&start, m.mv))
            .map(|mv| crate::chess::SanPlus::from_move(start.clone(), mv).to_string())
            .collect();
        assert!(
            sans.contains(&"Nf3".to_string()),
            "expected Nf3 in {sans:?}"
        );
        assert!(
            sans.contains(&"e4".to_string()),
            "expected post-repetition e4 in {sans:?}"
        );
        let refs = book.recent_games_for_filtered(key, Color::White, &filter, 10);
        assert!(
            refs.len() <= 1,
            "same game must not be duplicated in game_refs"
        );
    }

    #[test]
    fn truncated_name_matches_attached_enumeration_digits_and_schnellschach_is_otb_rapid() {
        assert!(matches_truncated_or_enumerated_name(
            "Kern, Johannes",
            "Kern, J1"
        ));
        assert!(matches_truncated_or_enumerated_name(
            "Kern, Johannes",
            "Kern, J2"
        ));
        assert!(matches_truncated_or_enumerated_name(
            "Hinterreiter, Martin",
            "Hinterreiter,M1"
        ));

        let mut g = Game::default();
        g.set_header("Event", "Bezirks-Schnellschachmeisterschaft".to_string());
        g.set_header("Site", "?".to_string());
        let (tc, is_online, _) = classify_game_metadata(&g);
        assert_eq!(tc, TimeControlClass::Rapid);
        assert!(!is_online);
    }

    #[test]
    fn default_prep_and_account_filters_use_all_time_cutoff() {
        assert_eq!(
            PrepFilterConfig::default().online_date_cutoff,
            DateCutoff::AllTime
        );
        assert_eq!(
            PrepFilterConfig::default().otb_date_cutoff,
            DateCutoff::AllTime
        );
        assert_eq!(AccountFilter::default().date_cutoff, DateCutoff::AllTime);
    }

    #[test]
    fn classify_game_category_distinguishes_otb_online_and_correspondence() {
        let make_game = |event: &str, site: &str, tc: Option<&str>| {
            let mut g = Game::default();
            g.set_header("Event", event.to_string());
            g.set_header("Site", site.to_string());
            if let Some(tc) = tc {
                g.set_header("TimeControl", tc.to_string());
            }
            g
        };

        let g_classical = make_game("World Championship Match", "London", Some("7200+30"));
        assert_eq!(
            classify_game_category(&g_classical),
            GameCategory::OtbClassical
        );

        let g_rapid = make_game("Bezirks-Schnellschachmeisterschaft", "Munich", None);
        assert_eq!(classify_game_category(&g_rapid), GameCategory::OtbRapid);

        let g_blitz = make_game("World Blitz Championship", "Almaty", Some("180+2"));
        assert_eq!(classify_game_category(&g_blitz), GameCategory::OtbBlitz);

        let g_on_classical = make_game("Lichess Classical", "lichess.org", Some("5400+30"));
        assert_eq!(
            classify_game_category(&g_on_classical),
            GameCategory::OnlineClassical
        );

        let g_on_rapid = make_game("Chess.com Rapid", "chess.com", Some("600+0"));
        assert_eq!(
            classify_game_category(&g_on_rapid),
            GameCategory::OnlineRapid
        );

        let g_on_blitz = make_game("Lichess Blitz", "https://lichess.org", Some("180+0"));
        assert_eq!(
            classify_game_category(&g_on_blitz),
            GameCategory::OnlineBlitz
        );

        let g_on_bullet = make_game("Lichess Bullet", "lichess.org", Some("60+0"));
        assert_eq!(
            classify_game_category(&g_on_bullet),
            GameCategory::OnlineBullet
        );

        let g_corr_event = make_game("ICCF World Cup", "Internet", None);
        assert_eq!(
            classify_game_category(&g_corr_event),
            GameCategory::Correspondence
        );

        let g_corr_site = make_game("Tournament", "ICCF", None);
        assert_eq!(
            classify_game_category(&g_corr_site),
            GameCategory::Correspondence
        );
    }

    #[test]
    fn player_latest_otb_elo_prefers_most_recent_otb_game_over_online() {
        let pgn = r#"[Event "Older OTB"]
[Site "Vienna"]
[Date "2024.05.01"]
[White "Kern, Johannes"]
[Black "Opponent, A"]
[WhiteElo "2110"]
[Result "1-0"]

1. e4 e5 1-0

[Event "Newer OTB"]
[Site "Linz"]
[Date "2026.03.10"]
[White "Kern, Johannes"]
[Black "Opponent, B"]
[WhiteElo "2185"]
[Result "1-0"]

1. e4 e5 1-0

[Event "Online Blitz"]
[Site "https://lichess.org/abcd1234"]
[Date "2026.10.01"]
[White "Kern, Johannes"]
[Black "Opponent, C"]
[WhiteElo "2450"]
[Result "1-0"]

1. e4 e5 1-0
"#;
        let mut path = std::env::temp_dir();
        path.push(format!(
            "brett-otb-elo-test-{}-{}.pgn",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, pgn).unwrap();
        let library = PgnLibrary::build(&path).unwrap();
        let directory = PlayerIndex::build(&library);
        let player = directory
            .find_player("Kern, Johannes")
            .expect("player exists");
        assert_eq!(player.latest_otb_elo, Some(2185));
        assert_eq!(player.display_elo(), Some(2185));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn correspondence_category_requires_whole_token_for_short_keywords() {
        assert_eq!(
            classify_headers_category(Some("Open Postojna 2024"), Some("Postojna SLO"), None),
            GameCategory::OtbClassical
        );
        assert_eq!(
            classify_headers_category(Some("Corrales Memorial"), Some("Havana CUB"), None),
            GameCategory::OtbClassical
        );
        assert_eq!(
            classify_headers_category(Some("ICCF World Cup"), None, None),
            GameCategory::Correspondence
        );
        assert_eq!(
            classify_headers_category(Some("Postal Championship"), None, None),
            GameCategory::Correspondence
        );
        assert_eq!(
            classify_headers_category(Some("World Correspondence Championship"), None, None),
            GameCategory::Correspondence
        );
    }

    #[test]
    fn bullet_defaults_true_and_migration() {
        assert!(TimeControlMask::default().bullet);
        assert!(AccountFilter::default().time_controls.bullet);
        // Old JSON (no filter_version, bullet:false) migrates to bullet:true v2.
        let old = serde_json::json!({
            "service": "lichess",
            "username": "olduser",
            "enabled": true,
            "filter": {
                "include_rated": true,
                "include_casual": true,
                "min_elo": null,
                "max_elo": null,
                "date_cutoff": "all",
                "time_controls": {"classical": true, "rapid": true, "blitz": true, "bullet": false}
            }
        });
        let restored = PrepOnlineAccount::from_value(&old).unwrap();
        assert!(
            restored.filter.time_controls.bullet,
            "migration enables bullet"
        );
        assert_eq!(restored.filter_version, 2);
        // Round-trip writes filter_version 2.
        let value = restored.to_value();
        assert_eq!(
            value.get("filter_version").and_then(|v| v.as_u64()),
            Some(2)
        );
        // New entries default to version 2.
        assert_eq!(PrepOnlineAccount::default().filter_version, 2);
    }

    #[test]
    fn exclusion_breakdown_counts_bullet_and_kept() {
        let as_of = 2026_1108;
        // Bullet online game: TimeControl 60+0 (<180s => bullet)
        let bullet = online_game("Opponent", "Trendy", "2026.01.01", "1. e4 e5 1-0");
        // online_game helper uses default TC? Ensure bullet via header override below.
        let mut bullet = bullet;
        bullet.set_header("TimeControl", "60+0".to_string());
        bullet.set_header("Site", "lichess.org".to_string());
        let rapid = online_game("Opponent", "Trendy", "2026.02.01", "1. e4 e5 1-0");
        let filter = AccountFilter::default();
        let breakdown = filter.exclusion_breakdown(&[bullet, rapid.clone()], as_of);
        assert_eq!(breakdown.total, 2);
        assert_eq!(breakdown.kept, 2);
        assert_eq!(breakdown.bullet, 1);
        // With bullet off, bullet game is excluded.
        let mut no_bullet = filter;
        no_bullet.time_controls.bullet = false;
        let breakdown = no_bullet.exclusion_breakdown(
            &[
                {
                    let mut b = online_game("Opponent", "Trendy", "2026.01.01", "1. e4 e5 1-0");
                    b.set_header("TimeControl", "60+0".to_string());
                    b.set_header("Site", "lichess.org".to_string());
                    b
                },
                rapid,
            ],
            as_of,
        );
        assert_eq!(breakdown.kept, 1);
        assert_eq!(breakdown.excluded_bullet, 1);
        assert_eq!(breakdown.excluded_time_control, 1);
    }

    fn directory_with_names(names: &[&str]) -> PlayerDirectory {
        // Build via ensure_player on a default directory.
        let mut dir = PlayerDirectory::default();
        for name in names {
            dir.ensure_player(name);
        }
        dir
    }

    #[test]
    fn fuzzy_search_normalizes_punctuation_and_diacritics() {
        let dir = directory_with_names(&[
            "Make_french",
            "Carlsen, Magnus",
            "José Müller",
            "Other Player",
        ]);
        // Underscore/hyphen equivalence.
        let hits = dir.filter("Makefrench");
        assert!(
            hits.iter()
                .any(|&i| dir.get(i).unwrap().name == "Make_french")
        );
        let hits = dir.filter("make-french");
        assert!(
            hits.iter()
                .any(|&i| dir.get(i).unwrap().name == "Make_french")
        );
        // Diacritics via deunicode.
        let hits = dir.filter("jose");
        assert!(
            hits.iter()
                .any(|&i| dir.get(i).unwrap().name == "José Müller")
        );
        let hits = dir.filter("muller");
        assert!(
            hits.iter()
                .any(|&i| dir.get(i).unwrap().name == "José Müller")
        );
    }

    #[test]
    fn fuzzy_search_edit_distance_bounds() {
        let dir = directory_with_names(&["Magnus Carlsen", "Hikaru Nakamura"]);
        // len 6 token, distance 1 => match
        let hits = dir.filter("Magnux");
        assert!(
            hits.iter()
                .any(|&i| dir.get(i).unwrap().name.contains("Carlsen")),
            "distance-1 len-6 should fuzzy match: {hits:?}"
        );
        // len >=7, distance 2 => match (carlsen -> carlsne transposition-ish distance 2? use carlson)
        let hits = dir.filter("Carlson");
        assert!(
            hits.iter()
                .any(|&i| dir.get(i).unwrap().name.contains("Carlsen")),
            "distance-1 len-7 should fuzzy match"
        );
        // Short query <4 chars: no fuzzy (exact/substring only) – "mag" substring matches anyway.
        // Memo: same query twice returns same.
        let a = dir.filter("Magnux");
        let b = dir.filter("Magnux");
        assert_eq!(a, b);
        // Exact/substring ranks before fuzzy: exact player first.
        let hits = dir.filter("Carlsen");
        assert_eq!(dir.get(hits[0]).unwrap().name, "Magnus Carlsen");
    }

    fn sidecar_temp_path(tag: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "brett-players-test-{tag}-{}-{}.brettplayers",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        path
    }

    fn synthetic_sidecar_directory() -> PlayerDirectory {
        let mut dir = PlayerDirectory::default();
        let carlsen = dir.ensure_player("Carlsen, Magnus");
        {
            let player = dir.get_mut(carlsen).unwrap();
            player.title = Some(crate::titles::PlayerTitle::Gm);
            player.fide_id = Some("1503014".to_string());
            player.country = crate::country::normalize("NOR");
            player.wiki_countries = vec![
                crate::country::normalize("NOR").unwrap(),
                crate::country::normalize("USA").unwrap(),
            ];
            player.birth_year = Some(1990);
            player.peak_elo = Some(2882);
            player.white = PlayerRecord {
                games: 10,
                wins: 6,
                draws: 2,
                losses: 2,
            };
            player.black = PlayerRecord {
                games: 8,
                wins: 3,
                draws: 4,
                losses: 1,
            };
            player.elo_sum = 50_000;
            player.elo_count = 18;
            player.elo_max = 2830;
            player.latest_otb_elo = Some(2830);
            player.latest_otb_date = 20240101;
            player.opponent_elo_sum = 46_000;
            player.opponent_elo_count = 17;
            player.first_year = 2009;
            player.last_year = 2024;
            player.game_indices = vec![0, 5, 10, 10_000, 200_000];
        }
        let jose = dir.ensure_player("José Müller");
        {
            let player = dir.get_mut(jose).unwrap();
            player.title = Some(crate::titles::PlayerTitle::Im);
            player.fide_id = None;
            player.country = crate::country::normalize("GER");
            player.wiki_countries = Vec::new();
            player.birth_year = None;
            player.peak_elo = None;
            player.white = PlayerRecord {
                games: 1,
                wins: 0,
                draws: 1,
                losses: 0,
            };
            player.black = PlayerRecord::default();
            player.elo_sum = 2100;
            player.elo_count = 1;
            player.elo_max = 2100;
            player.latest_otb_elo = None;
            player.latest_otb_date = 0;
            player.opponent_elo_sum = 0;
            player.opponent_elo_count = 0;
            player.first_year = 2020;
            player.last_year = 2020;
            player.game_indices = Vec::new();
        }
        let odd = dir.ensure_player("Odd Order");
        {
            let player = dir.get_mut(odd).unwrap();
            player.title = None;
            player.game_indices = vec![10, 2, 7, 3_500_000_000];
        }
        dir
    }

    fn assert_player_matches(expected: &PlayerSummary, actual: &PlayerSummary) {
        assert_eq!(expected.name, actual.name);
        assert_eq!(expected.key, actual.key);
        assert_eq!(expected.title, actual.title);
        assert_eq!(expected.fide_id, actual.fide_id);
        assert_eq!(
            expected.country.map(|country| country.code),
            actual.country.map(|country| country.code)
        );
        assert_eq!(
            expected
                .wiki_countries
                .iter()
                .map(|country| country.code)
                .collect::<Vec<_>>(),
            actual
                .wiki_countries
                .iter()
                .map(|country| country.code)
                .collect::<Vec<_>>()
        );
        assert_eq!(expected.birth_year, actual.birth_year);
        assert_eq!(expected.peak_elo, actual.peak_elo);
        assert_eq!(expected.white, actual.white);
        assert_eq!(expected.black, actual.black);
        assert_eq!(expected.elo_sum, actual.elo_sum);
        assert_eq!(expected.elo_count, actual.elo_count);
        assert_eq!(expected.elo_max, actual.elo_max);
        assert_eq!(expected.latest_otb_elo, actual.latest_otb_elo);
        assert_eq!(expected.latest_otb_date, actual.latest_otb_date);
        assert_eq!(expected.opponent_elo_sum, actual.opponent_elo_sum);
        assert_eq!(expected.opponent_elo_count, actual.opponent_elo_count);
        assert_eq!(expected.first_year, actual.first_year);
        assert_eq!(expected.last_year, actual.last_year);
        assert_eq!(expected.game_indices, actual.game_indices);
    }

    #[test]
    fn sidecar_round_trip_preserves_all_player_fields() {
        let dir = synthetic_sidecar_directory();
        let meta = SourceMeta {
            len: 123_456_789,
            mtime_secs: 1_700_000_000,
            mtime_nanos: 42,
        };
        let path = sidecar_temp_path("roundtrip");
        dir.save_to(&path, &meta).unwrap();
        let loaded = PlayerDirectory::load_from(&path, &meta)
            .unwrap()
            .expect("sidecar with matching fingerprint must load");
        assert_eq!(dir.len(), loaded.len());
        for (expected, actual) in dir.players().iter().zip(loaded.players().iter()) {
            assert_player_matches(expected, actual);
        }
        // The loaded directory stays searchable (memo reset, lookup works).
        assert!(
            loaded
                .find_player("Carlsen, Magnus")
                .is_some_and(|player| player.name == "Carlsen, Magnus")
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn sidecar_fingerprint_invalidation_on_mtime_or_len_change() {
        let dir = synthetic_sidecar_directory();
        let meta = SourceMeta {
            len: 987_654_321,
            mtime_secs: 1_800_000_000,
            mtime_nanos: 123_456_789,
        };
        let path = sidecar_temp_path("fingerprint");
        dir.save_to(&path, &meta).unwrap();
        // Exact fingerprint loads.
        assert!(
            PlayerDirectory::load_from(&path, &meta).unwrap().is_some(),
            "matching fingerprint must load"
        );
        // Any change in len, mtime_secs or mtime_nanos invalidates.
        let stale_len = SourceMeta {
            len: meta.len + 1,
            ..meta
        };
        assert!(
            PlayerDirectory::load_from(&path, &stale_len)
                .unwrap()
                .is_none(),
            "len change must invalidate the sidecar"
        );
        let stale_mtime = SourceMeta {
            mtime_secs: meta.mtime_secs + 1,
            ..meta
        };
        assert!(
            PlayerDirectory::load_from(&path, &stale_mtime)
                .unwrap()
                .is_none(),
            "mtime_secs change must invalidate the sidecar"
        );
        let stale_nanos = SourceMeta {
            mtime_nanos: meta.mtime_nanos + 1,
            ..meta
        };
        assert!(
            PlayerDirectory::load_from(&path, &stale_nanos)
                .unwrap()
                .is_none(),
            "mtime_nanos change must invalidate the sidecar"
        );
        // Missing file is a miss, not an error.
        let missing = sidecar_temp_path("missing");
        assert!(
            PlayerDirectory::load_from(&missing, &meta)
                .unwrap()
                .is_none()
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn sidecar_paths_are_hashed_and_stable() {
        let source = std::path::Path::new("/tmp/brett-synthetic-mega.cbh");
        let cache = players_cache_path(source);
        assert_eq!(
            cache.extension().and_then(|ext| ext.to_str()),
            Some("brettplayers")
        );
        assert!(
            cache
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.ends_with(".brettplayers"))
                .unwrap_or(false)
        );
        // Same source maps to the same cache file; a different source does not.
        assert_eq!(players_cache_path(source), cache);
        assert_ne!(
            players_cache_path(std::path::Path::new("/tmp/brett-other.pgn")),
            cache
        );
        // Sidecar next to the database replaces the extension.
        assert_eq!(
            players_sidecar_path(std::path::Path::new("/tmp/mega.cbh")),
            std::path::Path::new("/tmp/mega.brettplayers")
        );
        // Fingerprint distinguishes databases and fingerprints.
        let meta = SourceMeta {
            len: 100,
            mtime_secs: 200,
            mtime_nanos: 300,
        };
        let other_meta = SourceMeta {
            mtime_secs: 201,
            ..meta
        };
        assert_ne!(
            players_fingerprint(source, &meta),
            players_fingerprint(source, &other_meta)
        );
        assert_eq!(
            players_fingerprint(source, &meta),
            players_fingerprint(source, &meta)
        );
    }
}
