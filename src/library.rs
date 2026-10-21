//! Streaming PGN database index.
//!
//! A [`PgnLibrary`] keeps only the tag pairs and the byte range of every game.
//! Full move trees are built on demand by [`PgnLibrary::load`], so opening a
//! 50,000-game file costs megabytes instead of gigabytes.
//!
//! The byte ranges are obtained from `pgn-reader`'s [`Seek`] implementation,
//! which reports the logical stream position (excluding buffered lookahead).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use pgn_reader::{RawTag, Reader, Visitor};

use crate::domain::{self, Game};

/// One indexed game: its headers and where its text lives in the file.
#[derive(Clone, Debug)]
pub struct IndexedGame {
    headers: Vec<(String, String)>,
    /// Lowercase concatenation of the searchable header values.
    search: String,
    start: u64,
    end: u64,
}

impl IndexedGame {
    fn new(headers: Vec<(String, String)>, start: u64, end: u64) -> Self {
        let mut search = String::with_capacity(128);
        for key in ["White", "Black", "Event", "Site", "Date", "ECO", "Opening"] {
            if let Some(value) = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v)
            {
                search.push_str(&value.to_lowercase());
                search.push(' ');
            }
        }
        Self {
            headers,
            search,
            start,
            end,
        }
    }

    pub fn headers(&self) -> &[(String, String)] {
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

    pub fn byte_range(&self) -> (u64, u64) {
        (self.start, self.end)
    }

    fn matches(&self, needle_lower: &str) -> bool {
        self.search.contains(needle_lower)
    }
}

/// A PGN file indexed by headers and byte offsets.
pub struct PgnLibrary {
    path: PathBuf,
    file_len: u64,
    games: Vec<IndexedGame>,
    indexed_in: std::time::Duration,
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

        loop {
            let start = reader
                .stream_position()
                .context("querying PGN stream position")?;
            let Some(headers) = reader
                .read_game(&mut visitor)
                .with_context(|| format!("indexing {}", path.display()))?
            else {
                break;
            };
            let end = reader
                .stream_position()
                .context("querying PGN stream position")?;

            if end <= start {
                bail!(
                    "indexing made no progress at byte {start} of {}",
                    path.display()
                );
            }
            games.push(IndexedGame::new(headers, start, end));
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

    pub fn indexed_in(&self) -> std::time::Duration {
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
        let needle = needle.trim().to_lowercase();
        if needle.is_empty() {
            return (0..self.games.len()).collect();
        }
        self.games
            .iter()
            .enumerate()
            .filter(|(_, game)| game.matches(&needle))
            .map(|(index, _)| index)
            .collect()
    }

    /// Parse one game from its indexed byte range.
    pub fn load(&self, index: usize) -> Result<Game> {
        let entry = self
            .games
            .get(index)
            .with_context(|| format!("game index {index} is out of range"))?;
        let (start, end) = entry.byte_range();
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
        name: &[u8],
        value: RawTag<'_>,
    ) -> ControlFlow<Vec<(String, String)>> {
        tags.push((
            String::from_utf8_lossy(name).into_owned(),
            value.decode_utf8_lossy().into_owned(),
        ));
        ControlFlow::Continue(())
    }

    fn begin_movetext(
        &mut self,
        tags: Vec<(String, String)>,
    ) -> ControlFlow<Vec<(String, String)>, Vec<(String, String)>> {
        ControlFlow::Continue(tags)
    }

    fn end_game(&mut self, headers: Vec<(String, String)>) -> Vec<(String, String)> {
        headers
    }
}
