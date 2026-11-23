//! Database abstraction and streaming PGN index.
//!
//! [`PgnLibrary`] keeps only the tag pairs and the byte range of every game.
//! Full move trees are built on demand by [`PgnLibrary::load`], so opening a
//! 50,000-game file costs megabytes instead of gigabytes.
//!
//! [`GameLibrary`] is the common interface shared with native SCID databases
//! ([`crate::scid::ScidLibrary`]) and classic ChessBase CBH databases
//! ([`crate::cbh::CbhLibrary`]). The application, player directory and
//! position index all work against this interface rather than against a
//! particular file format.
//!
//! The byte ranges are obtained from the Brett PGN parser's logical
//! [`Reader::stream_position`], which excludes buffered lookahead.

use std::borrow::Borrow;
use std::collections::HashSet;
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::ops::{ControlFlow, Deref};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use memmap2::Mmap;

use crate::chess::{CastlingMode, Chess, Fen, Position, San, SanPlus};
use crate::domain::{self, Game};
use crate::pgn::reader::{Reader, Visitor};

/// ChessBase / Mega 16-bit game classification medals bitmask
/// (stored LE at `0x82..0x84` in `.2cbh`, BE at `0x25..0x27` in `.cbh`).
pub mod medals {
    pub const BEST_GAME: u16 = 1 << 0;
    pub const DECIDED_TOURNAMENT: u16 = 1 << 1;
    pub const MODEL_GAME: u16 = 1 << 2;
    pub const NOVELTY: u16 = 1 << 3;
    pub const PAWN_STRUCTURE: u16 = 1 << 4;
    pub const STRATEGY: u16 = 1 << 5;
    pub const TACTICS: u16 = 1 << 6;
    pub const WITH_ATTACK: u16 = 1 << 7;
    pub const SACRIFICE: u16 = 1 << 8;
    pub const DEFENSE: u16 = 1 << 9;
    pub const MATERIAL: u16 = 1 << 10;
    pub const PIECE_PLAY: u16 = 1 << 11;
    pub const ENDGAME: u16 = 1 << 12;
    pub const TACTICAL_BLUNDER: u16 = 1 << 13;
    pub const STRATEGICAL_BLUNDER: u16 = 1 << 14;
    pub const USER: u16 = 1 << 15;

    pub const ALL: &[(u16, &str, &str)] = &[
        (MODEL_GAME, "Model Game", "Mod"),
        (BEST_GAME, "Best Game", "Best"),
        (DECIDED_TOURNAMENT, "Decided Tournament", "Tour"),
        (NOVELTY, "Novelty", "Nov"),
        (PAWN_STRUCTURE, "Pawn Structure", "Pawn"),
        (STRATEGY, "Strategy", "Str"),
        (TACTICS, "Tactics", "Tac"),
        (WITH_ATTACK, "With Attack", "Atk"),
        (SACRIFICE, "Sacrifice", "Sac"),
        (DEFENSE, "Defense", "Def"),
        (MATERIAL, "Material", "Mat"),
        (PIECE_PLAY, "Piece Play", "Pce"),
        (ENDGAME, "Endgame", "End"),
        (TACTICAL_BLUNDER, "Tactical Blunder", "??T"),
        (STRATEGICAL_BLUNDER, "Strategical Blunder", "??S"),
        (USER, "User Marker", "Usr"),
    ];

    #[must_use]
    pub fn format_short(mask: u16) -> String {
        if mask == 0 {
            return String::new();
        }
        let mut out = Vec::new();
        for &(bit, _label, code) in ALL {
            if mask & bit != 0 {
                out.push(code);
            }
        }
        out.join(",")
    }
}

/// ChessBase VCS / annotation flags bitmask
/// (stored LE at `0x84..0x88` in `.2cbh`, BE at `0x27..0x2b` in `.cbh`).
pub mod vcs_flags {
    pub const CUSTOM_START: u32 = 0x0000_0001;
    pub const VARIATIONS: u32 = 0x0000_0002;
    pub const COMMENTARY: u32 = 0x0000_0004;
    pub const SYMBOLS: u32 = 0x0000_0008;
    pub const SQUARES: u32 = 0x0000_0010;
    pub const ARROWS: u32 = 0x0000_0020;
    pub const CLOCKS: u32 = 0x0000_0080;
    pub const TRAINING: u32 = 0x0000_0200;
    pub const CRITICAL: u32 = 0x0100_0000;

    pub const ALL: &[(u32, &str, &str)] = &[
        (VARIATIONS, "Variations (V)", "V"),
        (COMMENTARY, "Commentary (C)", "C"),
        (SYMBOLS, "Symbols / NAGs (S)", "S"),
        (SQUARES, "Colored Squares", "Sq"),
        (ARROWS, "Arrows", "Arr"),
        (CLOCKS, "Clocks / Times", "Clk"),
        (TRAINING, "Training Questions (T)", "T"),
        (CUSTOM_START, "Custom Start / Setup (P)", "P"),
        (CRITICAL, "Critical Position", "Crit"),
    ];
}

/// Zero-allocation fast metadata summary of one database game record for
/// O(1) pre-filtering before decoding moves or resolving name strings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GameRecordMeta {
    pub white_elo: u16,
    pub black_elo: u16,
    pub year: u16,
    /// `0 = 0-1`, `1 = 1/2-1/2`, `2 = 1-0`, `3 = *`.
    pub result: u8,
    /// `0` if unknown, else `1..=500` where `A00 = 1 .. E99 = 500`.
    pub eco_code: u16,
    pub medals: u16,
    pub vcs_flags: u32,
    pub annotated: bool,
    pub deleted: bool,
}

/// Parse an ECO string (`"A00"`..`"E99"`) into `1..=500` (`0` if invalid/absent).
#[must_use]
pub fn eco_str_to_code(s: &str) -> u16 {
    let bytes = s.trim().as_bytes();
    if bytes.len() < 3 {
        return 0;
    }
    let letter = match bytes[0].to_ascii_uppercase() {
        b'A'..=b'E' => u16::from(bytes[0].to_ascii_uppercase() - b'A'),
        _ => return 0,
    };
    if !bytes[1].is_ascii_digit() || !bytes[2].is_ascii_digit() {
        return 0;
    }
    let num = u16::from(bytes[1] - b'0') * 10 + u16::from(bytes[2] - b'0');
    letter * 100 + num + 1
}

/// Format an encoded ECO code (`1..=500`) back to `"A00"`..`"E99"`.
#[must_use]
pub fn eco_code_to_str(code: u16) -> Option<String> {
    if !(1..=500).contains(&code) {
        return None;
    }
    let idx = code - 1;
    let letter = (b'A' + (idx / 100) as u8) as char;
    Some(format!("{letter}{:02}", idx % 100))
}

pub(crate) fn parse_year_header(date: &str) -> u16 {
    let trimmed = date.trim();
    if trimmed.len() < 4 {
        return 0;
    }
    trimmed[..4].parse::<u16>().unwrap_or(0)
}

pub(crate) fn parse_result_code(result: &str) -> u8 {
    match result.trim() {
        "0-1" => 0,
        "1/2-1/2" | "1/2" => 1,
        "1-0" => 2,
        _ => 3,
    }
}

/// Metadata used to decide whether a cached position index is still current.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceMeta {
    pub len: u64,
    pub mtime_secs: u64,
    pub mtime_nanos: u32,
}

impl SourceMeta {
    pub fn read(source: &Path) -> Result<Self> {
        let metadata = std::fs::metadata(source)
            .with_context(|| format!("reading metadata for {}", source.display()))?;
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        let since_epoch = modified.duration_since(UNIX_EPOCH).unwrap_or_default();
        Ok(Self {
            len: metadata.len(),
            mtime_secs: since_epoch.as_secs(),
            mtime_nanos: since_epoch.subsec_nanos(),
        })
    }

    /// Combine several files (SCID uses an index, namebase and game file)
    /// into one cache fingerprint.
    pub fn combine(parts: &[Self]) -> Self {
        let mut combined = Self {
            len: 0,
            mtime_secs: 0,
            mtime_nanos: 0,
        };
        for part in parts {
            combined.len = combined.len.wrapping_add(part.len);
            if (part.mtime_secs, part.mtime_nanos) > (combined.mtime_secs, combined.mtime_nanos) {
                combined.mtime_secs = part.mtime_secs;
                combined.mtime_nanos = part.mtime_nanos;
            }
        }
        combined
    }
}

/// Where a lazily loaded game lives inside its database.
#[derive(Clone, Copy, Debug)]
enum GameLocation {
    Pgn {
        start: u64,
        end: u64,
    },
    Scid {
        offset: u64,
        length: u32,
    },
    Cbh {
        id: u32,
        moves_offset: u64,
        annotations_offset: u64,
        deleted: bool,
    },
}

/// An immutable string shared by many indexed games.
///
/// A large classic ChessBase database repeats the same player, event, date and
/// result strings millions of times. Storing one `String` per tag made the
/// Mega Database alone consume many gigabytes; interning turns each repeated
/// value into a cheap reference-counted pointer instead.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SharedStr(Arc<str>);

impl SharedStr {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for SharedStr {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for SharedStr {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for SharedStr {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SharedStr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl From<String> for SharedStr {
    fn from(value: String) -> Self {
        Self(Arc::from(value))
    }
}

impl From<&str> for SharedStr {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

/// Deduplicates header strings while a database index is being built.
#[derive(Debug, Default)]
pub struct StringPool {
    values: HashSet<Arc<str>>,
}

impl StringPool {
    pub fn intern(&mut self, value: impl AsRef<str>) -> SharedStr {
        let value = value.as_ref();
        if let Some(existing) = self.values.get(value) {
            return SharedStr(existing.clone());
        }
        let shared: Arc<str> = Arc::from(value);
        self.values.insert(shared.clone());
        SharedStr(shared)
    }
}

pub(crate) fn intern_headers(
    headers: Vec<(String, String)>,
    pool: &mut StringPool,
) -> Vec<(SharedStr, SharedStr)> {
    headers
        .into_iter()
        .map(|(key, value)| (pool.intern(key), pool.intern(value)))
        .collect()
}

/// One player's game list resolved through a backend accelerator
/// (`.2lgd`, `.brettacc`, or classic entity counts) instead of a full scan.
#[derive(Clone, Debug, Default)]
pub struct AcceleratedPlayer {
    pub name: String,
    pub fide_id: Option<String>,
    pub indices: Vec<usize>,
    pub white_games: u32,
    pub black_games: u32,
    pub wins: u32,
    pub draws: u32,
    pub losses: u32,
    pub elo_max: u32,
    pub elo_sum: u64,
    pub elo_count: u32,
    pub latest_otb_elo: Option<u16>,
    pub latest_otb_date: u32,
    pub first_year: u16,
    pub last_year: u16,
}

/// One indexed game: its headers and where its data lives in the source.
#[derive(Clone, Debug)]
pub struct IndexedGame {
    headers: Vec<(SharedStr, SharedStr)>,
    /// Lowercase concatenation of the searchable header values.
    ///
    /// Very large databases skip this cache to avoid hundreds of megabytes of
    /// duplicate text; searches fall back to inspecting the headers directly.
    search: Option<Box<str>>,
    location: GameLocation,
}

impl IndexedGame {
    fn with_location(
        headers: Vec<(SharedStr, SharedStr)>,
        location: GameLocation,
        build_search: bool,
    ) -> Self {
        let search = build_search.then(|| {
            let mut search = String::with_capacity(128);
            for key in SEARCH_HEADER_KEYS {
                if let Some(value) = headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(key))
                    .map(|(_, v)| v)
                {
                    search.push_str(&value.to_lowercase());
                    search.push(' ');
                }
            }
            search.into_boxed_str()
        });
        Self {
            headers,
            search,
            location,
        }
    }

    fn pgn(headers: Vec<(SharedStr, SharedStr)>, start: u64, end: u64) -> Self {
        Self::with_location(headers, GameLocation::Pgn { start, end }, true)
    }

    pub(crate) fn scid(headers: Vec<(SharedStr, SharedStr)>, offset: u64, length: u32) -> Self {
        Self::with_location(headers, GameLocation::Scid { offset, length }, true)
    }

    pub(crate) fn cbh(
        headers: Vec<(SharedStr, SharedStr)>,
        id: u32,
        moves_offset: u64,
        annotations_offset: u64,
        deleted: bool,
        build_search: bool,
    ) -> Self {
        Self::with_location(
            headers,
            GameLocation::Cbh {
                id,
                moves_offset,
                annotations_offset,
                deleted,
            },
            build_search,
        )
    }

    pub fn headers(&self) -> &[(SharedStr, SharedStr)] {
        &self.headers
    }

    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    pub fn white(&self) -> &str {
        self.header("White").unwrap_or("?")
    }

    pub fn black(&self) -> &str {
        self.header("Black").unwrap_or("?")
    }

    pub fn event(&self) -> &str {
        self.header("Event").unwrap_or("?")
    }

    pub fn date(&self) -> &str {
        self.header("Date").unwrap_or("????.??.??")
    }

    pub fn result(&self) -> &str {
        self.header("Result").unwrap_or("*")
    }

    /// The classic CBH record number, when this game came from a `.cbh`.
    pub fn cbh_record_id(&self) -> Option<u32> {
        match self.location {
            GameLocation::Cbh { id, .. } => Some(id),
            _ => None,
        }
    }

    /// Where this game's `.cba` annotations start, when it has any.
    pub fn cbh_annotations_offset(&self) -> Option<u64> {
        match self.location {
            GameLocation::Cbh {
                annotations_offset, ..
            } if annotations_offset != 0 => Some(annotations_offset),
            _ => None,
        }
    }

    /// Whether the classic CBH header marks this game deleted.
    pub fn cbh_deleted(&self) -> bool {
        matches!(self.location, GameLocation::Cbh { deleted: true, .. })
    }

    /// Byte range of the game inside its source file. For PGN this is the
    /// exact text range; for SCID it is the range inside the `.sg4` file.
    pub fn byte_range(&self) -> (u64, u64) {
        match self.location {
            GameLocation::Pgn { start, end } => (start, end),
            GameLocation::Scid { offset, length } => {
                (offset, offset.saturating_add(u64::from(length)))
            }
            GameLocation::Cbh { moves_offset, .. } => (moves_offset, moves_offset),
        }
    }

    pub fn matches(&self, needle_lower: &str) -> bool {
        if let Some(search) = &self.search {
            return search.contains(needle_lower);
        }
        SEARCH_HEADER_KEYS.iter().any(|key| {
            self.header(key)
                .is_some_and(|value| value.to_lowercase().contains(needle_lower))
        })
    }
}

const SEARCH_HEADER_KEYS: [&str; 7] = ["White", "Black", "Event", "Site", "Date", "ECO", "Opening"];

/// A database whose games can be listed and loaded lazily.
///
/// Implementations must be safe to use from a background thread.
pub trait GameLibrary: Send + Sync {
    fn path(&self) -> &Path;
    fn file_len(&self) -> u64;
    fn len(&self) -> usize;
    fn indexed_in(&self) -> Duration;
    fn games(&self) -> &[IndexedGame];
    fn load(&self, index: usize) -> Result<Game>;
    /// Cache fingerprint of every file that can change the games.
    fn source_meta(&self) -> Result<SourceMeta>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn get(&self, index: usize) -> Option<&IndexedGame> {
        self.games().get(index)
    }

    fn filter(&self, needle: &str) -> Vec<usize> {
        filter_games(self.games(), needle)
    }

    /// Fast O(1) metadata & flag summary for pre-filtering before move decoding.
    fn game_record_meta(&self, index: usize) -> GameRecordMeta {
        let deleted = self.get(index).is_some_and(IndexedGame::cbh_deleted);
        let headers_vec;
        let headers = if let Some(game) = self.get(index) {
            game.headers()
        } else {
            headers_vec = self.game_headers(index);
            &headers_vec
        };
        let header = |key: &str| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v.as_str())
        };
        let white_elo = header("WhiteElo")
            .and_then(|v| v.trim().parse::<u16>().ok())
            .unwrap_or(0);
        let black_elo = header("BlackElo")
            .and_then(|v| v.trim().parse::<u16>().ok())
            .unwrap_or(0);
        let year = header("Date").map_or(0, parse_year_header);
        let result = header("Result").map_or(3, parse_result_code);
        let eco_code = header("ECO").map_or(0, eco_str_to_code);
        GameRecordMeta {
            white_elo,
            black_elo,
            year,
            result,
            eco_code,
            medals: 0,
            vcs_flags: 0,
            annotated: header("Annotator").is_some_and(|v| !v.trim().is_empty() && v.trim() != "?"),
            deleted,
        }
    }

    /// Targeted substring player search via backend accelerator (`.2lgd`,
    /// `.brettacc`). Returns `None` if no accelerator is available.
    fn accelerated_games_matching_player(&self, _needle_lower: &str) -> Option<Vec<usize>> {
        None
    }

    /// Stream mainline board positions (`ply = 0..=max_ply`) to `visitor`
    /// without allocating a full `Game` tree or SAN strings. Stops early as
    /// soon as `visitor` returns `false`. Returns `Ok(true)` if streamed, or
    /// `Ok(false)` to fall back to [`Self::load`].
    fn scan_mainline_positions(
        &self,
        index: usize,
        max_ply: u32,
        visitor: &mut dyn FnMut(u32, &Chess) -> bool,
    ) -> Result<bool> {
        let _ = (index, max_ply, visitor);
        Ok(false)
    }

    /// Fast path for the streaming PGN position-index builder.
    fn as_pgn(&self) -> Option<&PgnLibrary> {
        None
    }

    /// Stream mainline moves directly to a consumer without allocating game trees.
    /// Returns `Ok(true)` if supported and streamed, or `Ok(false)` to fall back
    /// to loading full `Game` objects.
    fn stream_mainline(
        &self,
        index: usize,
        max_plies: u32,
        consumer: &mut dyn FnMut(u64, u32),
    ) -> Result<bool> {
        let _ = (index, max_plies, consumer);
        Ok(false)
    }

    /// Headers for a game by index, falling back to on-demand decoding when
    /// the full game list is not pre-allocated in memory.
    fn game_headers(&self, index: usize) -> Vec<(SharedStr, SharedStr)> {
        self.games()
            .get(index)
            .map(|game| game.headers().to_vec())
            .unwrap_or_default()
    }

    /// Player names for a game by index (White, Black).
    fn game_players(&self, index: usize) -> (String, String) {
        if let Some(game) = self.games().get(index) {
            (game.white().to_string(), game.black().to_string())
        } else {
            let headers = self.game_headers(index);
            let white = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("White"))
                .map(|(_, v)| v.to_string())
                .unwrap_or_else(|| "?".to_string());
            let black = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("Black"))
                .map(|(_, v)| v.to_string())
                .unwrap_or_else(|| "?".to_string());
            (white, black)
        }
    }

    /// Tournament index categories, when the database carries one (the Mega
    /// Database keeps its HTML tournament index in a text record). The base
    /// implementation reports none.
    fn tournament_categories(
        &self,
    ) -> Option<Vec<crate::twocbh::tournaments::MegaTournamentCategory>> {
        None
    }

    /// Guiding-text record ids carried by ChessBase databases (flag bit 1).
    fn guiding_text_ids(&self) -> Vec<u32> {
        Vec::new()
    }

    /// Title of one guiding-text record, if the backend can read it.
    fn guiding_text_title(&self, _id: u32) -> Option<String> {
        None
    }

    /// Raw HTML blocks of one guiding-text record, if readable.
    fn guiding_text_html(&self, _id: u32) -> Option<Vec<String>> {
        None
    }

    /// Guiding-text record matching an event, if any.
    fn find_guiding_text(&self, _event: &str, _site: &str) -> Option<u32> {
        None
    }

    /// One-based source record id backing a game index, when the backend
    /// numbers its game slots (2CBH `.2cbh` record ids).
    fn source_record_id(&self, _index: usize) -> Option<u32> {
        None
    }

    /// Game index backing a one-based source record id, when the backend
    /// numbers its game slots.
    fn source_index_for_record(&self, _record_id: u32) -> Option<usize> {
        None
    }

    /// Whether a one-based source record id is a game rather than a text or
    /// unused slot. Backends without record ids treat every slot as a game.
    fn is_game_record(&self, _record_id: u32) -> bool {
        true
    }

    /// Game indices for one `(Event, Site, year)` tournament, resolved through
    /// a backend accelerator when one is available. `None` means "no
    /// accelerator; the caller falls back to scanning headers".
    fn event_game_indices(
        &self,
        _event: &str,
        _site: &str,
        _year: Option<u32>,
    ) -> Option<Vec<usize>> {
        None
    }

    /// Per-player game lists resolved through a backend accelerator.
    /// `None` means "no accelerator; the caller falls back to scanning".
    fn accelerated_player_games(&self) -> Option<Vec<AcceleratedPlayer>> {
        None
    }

    /// Targeted per-player lookup that filters the name store before decoding
    /// accelerator postings. `None` means "no targeted fast path; the caller
    /// falls back to [`Self::accelerated_player_games`] or a header scan".
    fn accelerated_games_for_player(
        &self,
        _player_key: &str,
        _canonical_name: &str,
    ) -> Option<Vec<usize>> {
        None
    }
}

pub(crate) fn filter_games(games: &[IndexedGame], needle: &str) -> Vec<usize> {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return (0..games.len()).collect();
    }
    games
        .iter()
        .enumerate()
        .filter(|(_, game)| game.matches(&needle))
        .map(|(index, _)| index)
        .collect()
}

pub(crate) fn contains_case_insensitive(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if haystack.is_ascii() && needle_lower.is_ascii() {
        let h = haystack.as_bytes();
        let n = needle_lower.as_bytes();
        if n.len() > h.len() {
            return false;
        }
        return h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
    }
    haystack.to_lowercase().contains(needle_lower)
}

/// A PGN file indexed by headers and byte offsets.
pub struct PgnLibrary {
    path: PathBuf,
    file_len: u64,
    games: Vec<IndexedGame>,
    indexed_in: Duration,
    mmap: Option<Mmap>,
}

impl PgnLibrary {
    pub fn build(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let started = Instant::now();

        let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
        let file_len = file.metadata()?.len();
        // SAFETY: source PGN databases are read-only while indexed; if the file is
        // small or cannot be mapped we fall back cleanly to seek+read.
        let mmap = unsafe { Mmap::map(&file) }.ok();

        let mut reader = Reader::new(BufReader::new(file));
        let mut visitor = HeaderVisitor;
        let mut games = Vec::new();
        let mut strings = StringPool::default();

        loop {
            let start = reader.stream_position();
            let Some(headers) = reader
                .read_game(&mut visitor)
                .with_context(|| format!("indexing {}", path.display()))?
            else {
                break;
            };
            let end = reader.stream_position();

            if end <= start {
                bail!(
                    "indexing made no progress at byte {start} of {}",
                    path.display()
                );
            }
            games.push(IndexedGame::pgn(
                intern_headers(headers, &mut strings),
                start,
                end,
            ));
        }

        Ok(Self {
            path,
            file_len,
            games,
            indexed_in: started.elapsed(),
            mmap,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    pub fn len(&self) -> usize {
        self.games.len()
    }

    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }

    pub fn indexed_in(&self) -> Duration {
        self.indexed_in
    }

    pub fn games(&self) -> &[IndexedGame] {
        &self.games
    }

    pub fn get(&self, index: usize) -> Option<&IndexedGame> {
        self.games.get(index)
    }

    /// Games whose player/event/ECO fields contain `needle` (case-insensitive).
    pub fn filter(&self, needle: &str) -> Vec<usize> {
        filter_games(&self.games, needle)
    }

    /// Parse one game from its indexed byte range.
    pub fn load(&self, index: usize) -> Result<Game> {
        let entry = self
            .games
            .get(index)
            .with_context(|| format!("game index {index} is out of range"))?;
        let GameLocation::Pgn { start, end } = entry.location else {
            bail!("game {index} is not a PGN game");
        };
        let len = end.saturating_sub(start);
        if len == 0 {
            bail!("game {index} has an empty byte range");
        }

        let mut file =
            File::open(&self.path).with_context(|| format!("opening {}", self.path.display()))?;
        file.seek(SeekFrom::Start(start))
            .with_context(|| format!("seeking to byte {start}"))?;

        let mut bytes = Vec::with_capacity(len.min(1 << 20) as usize);
        file.take(len)
            .read_to_end(&mut bytes)
            .with_context(|| format!("reading game {index} at bytes {start}..{end}"))?;

        let mut parsed = domain::parse_pgn(&bytes)
            .with_context(|| format!("parsing game {index} at bytes {start}..{end}"))?;
        if parsed.is_empty() {
            bail!("byte range {start}..{end} for game {index} contained no PGN game");
        }
        Ok(parsed.remove(0))
    }
}

impl GameLibrary for PgnLibrary {
    fn path(&self) -> &Path {
        PgnLibrary::path(self)
    }

    fn file_len(&self) -> u64 {
        PgnLibrary::file_len(self)
    }

    fn len(&self) -> usize {
        PgnLibrary::len(self)
    }

    fn indexed_in(&self) -> Duration {
        PgnLibrary::indexed_in(self)
    }

    fn games(&self) -> &[IndexedGame] {
        PgnLibrary::games(self)
    }

    fn load(&self, index: usize) -> Result<Game> {
        PgnLibrary::load(self, index)
    }

    fn source_meta(&self) -> Result<SourceMeta> {
        SourceMeta::read(&self.path)
    }

    fn filter(&self, needle: &str) -> Vec<usize> {
        filter_games(self.games(), needle)
    }

    fn as_pgn(&self) -> Option<&PgnLibrary> {
        Some(self)
    }

    fn scan_mainline_positions(
        &self,
        index: usize,
        max_ply: u32,
        visitor: &mut dyn FnMut(u32, &Chess) -> bool,
    ) -> Result<bool> {
        let entry = self
            .games
            .get(index)
            .with_context(|| format!("game index {index} is out of range"))?;
        let GameLocation::Pgn { start, end } = entry.location else {
            bail!("game {index} is not a PGN game");
        };
        let len = end.saturating_sub(start);
        if len == 0 {
            return Ok(true);
        }

        let owned;
        let slice: &[u8] = if let Some(mmap) = &self.mmap
            && let (Ok(s), Ok(e)) = (usize::try_from(start), usize::try_from(end))
            && let Some(bytes) = mmap.get(s..e)
        {
            bytes
        } else {
            let mut file = File::open(&self.path)
                .with_context(|| format!("opening {}", self.path.display()))?;
            file.seek(SeekFrom::Start(start))
                .with_context(|| format!("seeking to byte {start}"))?;
            let mut bytes = Vec::with_capacity(len.min(1 << 20) as usize);
            file.take(len)
                .read_to_end(&mut bytes)
                .with_context(|| format!("reading game {index} at bytes {start}..{end}"))?;
            owned = bytes;
            &owned
        };

        let mut scan = PgnMainlineScanVisitor { max_ply, visitor };
        let _ = Reader::new(Cursor::new(slice))
            .read_game(&mut scan)
            .with_context(|| format!("scanning PGN game {index}"))?;
        Ok(true)
    }
}

#[derive(Default)]
struct ScanTags {
    fen: Option<String>,
    is_960: bool,
}

struct MainlineScanState {
    pos: Chess,
    ply: u32,
    var_depth: usize,
    stopped: bool,
}

struct PgnMainlineScanVisitor<'a> {
    max_ply: u32,
    visitor: &'a mut dyn FnMut(u32, &Chess) -> bool,
}

impl Visitor for PgnMainlineScanVisitor<'_> {
    type Tags = ScanTags;
    type Movetext = MainlineScanState;
    type Output = ();

    fn begin_tags(&mut self) -> ControlFlow<(), ScanTags> {
        ControlFlow::Continue(ScanTags::default())
    }

    fn tag(&mut self, tags: &mut ScanTags, name: &str, value: &str) -> ControlFlow<()> {
        if name.eq_ignore_ascii_case("FEN") {
            tags.fen = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("Variant") {
            let lower = value.to_ascii_lowercase();
            tags.is_960 = lower.contains("chess960") || lower.contains("fischer");
        }
        ControlFlow::Continue(())
    }

    fn begin_movetext(&mut self, tags: ScanTags) -> ControlFlow<(), MainlineScanState> {
        let mode = if tags.is_960 {
            CastlingMode::Chess960
        } else {
            CastlingMode::Standard
        };
        let pos = tags
            .fen
            .as_deref()
            .and_then(|v| Fen::from_ascii(v.trim().as_bytes()).ok())
            .and_then(|fen| fen.into_position(mode).ok())
            .unwrap_or_default();
        if !(self.visitor)(0, &pos) || self.max_ply == 0 {
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(MainlineScanState {
            pos,
            ply: 0,
            var_depth: 0,
            stopped: false,
        })
    }

    fn san(&mut self, mt: &mut MainlineScanState, san_plus: SanPlus) -> ControlFlow<()> {
        if mt.stopped || mt.var_depth > 0 {
            return ControlFlow::Continue(());
        }
        if matches!(san_plus.san, San::Null) {
            match mt.pos.clone().swap_turn() {
                Ok(after) => mt.pos = after,
                Err(_) => {
                    mt.stopped = true;
                    return ControlFlow::Break(());
                }
            }
        } else {
            let Ok(mv) = san_plus.san.to_move(&mt.pos) else {
                mt.stopped = true;
                return ControlFlow::Break(());
            };
            let Ok(after) = mt.pos.clone().play(mv) else {
                mt.stopped = true;
                return ControlFlow::Break(());
            };
            mt.pos = after;
        }
        mt.ply += 1;
        if !(self.visitor)(mt.ply, &mt.pos) || mt.ply >= self.max_ply {
            mt.stopped = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    fn begin_variation(&mut self, mt: &mut MainlineScanState) -> ControlFlow<()> {
        mt.var_depth += 1;
        ControlFlow::Continue(())
    }

    fn end_variation(&mut self, mt: &mut MainlineScanState) -> ControlFlow<()> {
        mt.var_depth = mt.var_depth.saturating_sub(1);
        ControlFlow::Continue(())
    }

    fn end_game(&mut self, _mt: MainlineScanState) {}
}

/// Collects tag pairs and throws the movetext away.
struct HeaderVisitor;

impl Visitor for HeaderVisitor {
    type Tags = Vec<(String, String)>;
    type Movetext = Vec<(String, String)>;
    type Output = Vec<(String, String)>;

    fn begin_tags(&mut self) -> ControlFlow<Vec<(String, String)>, Vec<(String, String)>> {
        ControlFlow::Continue(Vec::new())
    }

    fn tag(
        &mut self,
        tags: &mut Vec<(String, String)>,
        name: &str,
        value: &str,
    ) -> ControlFlow<Vec<(String, String)>> {
        tags.push((name.to_string(), value.to_string()));
        ControlFlow::Continue(())
    }

    fn begin_movetext(
        &mut self,
        tags: Vec<(String, String)>,
    ) -> ControlFlow<Vec<(String, String)>, Vec<(String, String)>> {
        ControlFlow::Continue(tags)
    }

    fn wants_movetext(&self) -> bool {
        false
    }

    fn end_game(&mut self, headers: Vec<(String, String)>) -> Vec<(String, String)> {
        headers
    }
}

/// Find the primary supported database file inside `dir`.
pub fn find_database_in_dir(dir: &Path) -> Result<PathBuf> {
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    let mut candidates_2cbh = Vec::new();
    let mut candidates_cbh = Vec::new();
    let mut candidates_si4 = Vec::new();
    let mut candidates_pgn = Vec::new();

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            match ext.as_str() {
                "2cbh" => candidates_2cbh.push(path),
                "cbh" => candidates_cbh.push(path),
                "si4" | "si3" => candidates_si4.push(path),
                "pgn" => candidates_pgn.push(path),
                _ => {}
            }
        }
    }
    candidates_2cbh.sort();
    candidates_cbh.sort();
    candidates_si4.sort();
    candidates_pgn.sort();

    if let Some(target) = candidates_2cbh.into_iter().next() {
        return Ok(target);
    }
    if let Some(target) = candidates_cbh.into_iter().next() {
        return Ok(target);
    }
    if let Some(target) = candidates_si4.into_iter().next() {
        return Ok(target);
    }
    if let Some(target) = candidates_pgn.into_iter().next() {
        return Ok(target);
    }

    bail!(
        "No supported chess database (.2cbh, .cbh, .si4, .pgn) found in directory {}",
        dir.display()
    )
}

/// Open any supported chess database (`.2cbh`, `.cbh`, `.cbv`, `.si4`/`.si3`, `.pgn`).
pub fn open_library(path: &Path) -> Result<Arc<dyn GameLibrary>> {
    let started = Instant::now();
    let resolved = if path.is_dir() {
        find_database_in_dir(path)?
    } else {
        path.to_path_buf()
    };
    let extension = resolved
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let result = if extension == "si4" || extension == "si3" {
        crate::scid::ScidLibrary::open(&resolved)
            .map(|library| Arc::new(library) as Arc<dyn GameLibrary>)
    } else if extension == "cbv" {
        crate::cbh::CbhLibrary::open_cbv(&resolved)
            .map(|library| Arc::new(library) as Arc<dyn GameLibrary>)
    } else if extension == "cbh" {
        crate::cbh::CbhLibrary::open(&resolved)
            .map(|library| Arc::new(library) as Arc<dyn GameLibrary>)
    } else if matches!(extension.as_str(), "2cbh" | "2cbg" | "2cba" | "2lid") {
        crate::twocbh::TwoCbhLibrary::open(&resolved)
            .map(|library| Arc::new(library) as Arc<dyn GameLibrary>)
    } else {
        PgnLibrary::build(resolved).map(|library| Arc::new(library) as Arc<dyn GameLibrary>)
    };
    let elapsed = started.elapsed();
    match &result {
        Ok(library) => tracing::info!(
            "open_library {} ({} games) took {:.2}s",
            path.display(),
            library.len(),
            elapsed.as_secs_f64()
        ),
        Err(err) => tracing::debug!(
            "open_library {} failed in {:.2}s: {err:#}",
            path.display(),
            elapsed.as_secs_f64()
        ),
    }
    if elapsed.as_millis() > 100 {
        tracing::warn!(
            "slow open_library {} took {:.2}s",
            path.display(),
            elapsed.as_secs_f64()
        );
    }
    result
}
