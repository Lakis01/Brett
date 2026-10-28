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
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::ops::{ControlFlow, Deref};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::domain::{self, Game};
use crate::pgn::reader::{Reader, Visitor};

/// Metadata used to decide whether a cached position index is still current.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

    /// Fast path for the streaming PGN position-index builder.
    fn as_pgn(&self) -> Option<&PgnLibrary> {
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

/// A PGN file indexed by headers and byte offsets.
pub struct PgnLibrary {
    path: PathBuf,
    file_len: u64,
    games: Vec<IndexedGame>,
    indexed_in: Duration,
}

impl PgnLibrary {
    pub fn build(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let started = Instant::now();

        let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
        let file_len = file.metadata()?.len();

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
