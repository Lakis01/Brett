//! On-disk position index and opening-tree statistics.
//!
//! The index is built from the **mainline** of every PGN game. Each entry is
//! keyed by Brett's stable `cozy-chess`-based hash of the position *before* a
//! move, plus the move that was played. Records are sorted by `(position, move)`
//! and memory-mapped for lookup, so an index can be much larger than the
//! process heap. On-disk `VERSION` is `3`; older caches are ignored.
//!
//! Position totals are the sum of the outgoing move records. A game is counted
//! at most once per position: if a game repeats a position, indexing stops at
//! the repetition.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::chess::{
    CastlingMode, Chess, Fen, File as ChessFile, Move, Position, Role, San, SanPlus, Square,
    UciMove,
};
use crate::library::{GameLibrary, SharedStr, SourceMeta};
use crate::pgn::reader::{Reader, Visitor};

pub mod chunked_builder;
pub mod compactor;
pub mod segment;
pub mod segmented_index;

pub(crate) const MAGIC: &[u8; 8] = b"BRETTPOS";
pub(crate) const VERSION: u32 = 3;
pub(crate) const HEADER_LEN: usize = 80;
pub(crate) const EDGE_SIZE: usize = 120;
const NO_PROMOTION: u32 = 7;

/// Number of quarterly date buckets retained per `(position, move)` edge
/// (16 quarters = 48 months).
pub const TREND_BUCKETS: usize = 16;

/// Default maximum number of unique `(position, move)` edges kept in memory
/// before spilling a sorted external-sort run to disk.
pub const DEFAULT_MAX_IN_MEMORY_EDGES: usize = 500_000;

/// Convert a `YYYYMMDD` integer date into a monotonic quarter index
/// (`year * 4 + (month - 1) / 3`).
pub fn date_to_quarter(date: u32) -> Option<u32> {
    if date == 0 {
        return None;
    }
    let year = date / 10_000;
    let month = (date / 100) % 100;
    if year == 0 || !(1..=12).contains(&month) {
        return None;
    }
    Some(year * 4 + (month - 1) / 3)
}

/// Human-readable label for a quarter index (for example `"2026 Q1"`).
pub fn quarter_label(quarter: u32) -> String {
    if quarter == 0 {
        return "—".to_string();
    }
    let year = quarter / 4;
    let q = (quarter % 4) + 1;
    format!("{year} Q{q}")
}

/// Rating window used when building a reference index.
///
/// The filter uses the mean of the available WhiteElo and BlackElo tags. Games
/// with no rating are excluded. The application applies any rating inflation
/// offset before constructing the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RatingFilter {
    pub min: u32,
    pub max: u32,
}

impl RatingFilter {
    pub fn around(target: u32, half_width: u32) -> Self {
        let half_width = half_width.min(2_000);
        Self {
            min: target.saturating_sub(half_width),
            max: target.saturating_add(half_width),
        }
    }

    fn accepts(&self, meta: &GameMeta) -> bool {
        meta.average_elo()
            .is_some_and(|elo| (self.min..=self.max).contains(&elo))
    }
}

/// Build options for a position index.
#[derive(Clone, Copy, Debug)]
pub struct BuildOptions {
    /// Stop following a game after this many plies. `u32::MAX` means no limit.
    pub max_plies: u32,
    /// Optional rating window for a reference corpus. Games outside the window
    /// are not indexed. `None` indexes every game.
    pub rating_filter: Option<RatingFilter>,
    /// Maximum number of unique `(position, move)` edges held in memory before
    /// spilling a sorted run file to disk during external-sort indexing.
    pub max_in_memory_edges: usize,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            max_plies: u32::MAX,
            rating_filter: None,
            max_in_memory_edges: DEFAULT_MAX_IN_MEMORY_EDGES,
        }
    }
}

/// Progress reported while a position index is being built.
#[derive(Clone, Copy, Debug, Default)]
pub struct BuildProgress {
    pub games: usize,
    pub edges: usize,
    pub elapsed: Duration,
}

/// Summary of a completed index build.
#[derive(Clone, Copy, Debug, Default)]
pub struct BuildReport {
    pub games: usize,
    pub edges: usize,
    pub positions: usize,
    pub elapsed: Duration,
    pub bytes: u64,
}

/// Aggregated statistics for one move from a position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MoveStat {
    pub key: u64,
    pub mv: u32,
    pub games: u32,
    pub white: u32,
    pub draw: u32,
    pub black: u32,
    pub elo_sum: u64,
    pub elo_count: u32,
    pub first_date: u32,
    pub last_date: u32,
    /// Quarter index corresponding to `buckets[TREND_BUCKETS - 1]`, or `0` if
    /// no dated game played this move. When returned by [`PositionIndex::moves`]
    /// or [`crate::recent::RecentTree::moves`], all moves from the same
    /// position are aligned to the position's latest quarter.
    pub anchor_quarter: u32,
    /// Dated games older than the 16-quarter window ending at `anchor_quarter`.
    pub older_dated: u32,
    /// Quarterly game counts for `[anchor_quarter - 15 ..= anchor_quarter]`.
    pub buckets: [u32; TREND_BUCKETS],
}

impl MoveStat {
    /// White's score in the games where this move was played (0.0–1.0).
    pub fn score_white(&self) -> f32 {
        if self.games == 0 {
            return 0.0;
        }
        (self.white as f32 + self.draw as f32 * 0.5) / self.games as f32
    }

    pub fn elo_avg(&self) -> Option<f64> {
        (self.elo_count > 0).then(|| self.elo_sum as f64 / self.elo_count as f64)
    }

    /// Decode the stored move against the position it belongs to.
    pub fn to_move(&self, pos: &Chess) -> Option<Move> {
        decode_move(pos, self.mv)
    }

    /// Render the move as SAN in the given position.
    pub fn san(&self, pos: &Chess) -> Option<String> {
        let mv = self.to_move(pos)?;
        Some(SanPlus::from_move(pos.clone(), mv).to_string())
    }
}

/// Aggregated statistics for a position (the sum of its outgoing moves).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PositionStats {
    pub games: u32,
    pub white: u32,
    pub draw: u32,
    pub black: u32,
    pub elo_sum: u64,
    pub elo_count: u32,
    pub first_date: u32,
    pub last_date: u32,
}

impl PositionStats {
    pub fn score_white(&self) -> f32 {
        if self.games == 0 {
            return 0.0;
        }
        (self.white as f32 + self.draw as f32 * 0.5) / self.games as f32
    }

    pub fn elo_avg(&self) -> Option<f64> {
        (self.elo_count > 0).then(|| self.elo_sum as f64 / self.elo_count as f64)
    }
}

pub use chunked_builder::{ChunkedBuildOptions, DEFAULT_CHUNK_SIZE, build_chunked_parallel};
pub use compactor::compact_segments;
pub use segment::IndexSegment;
pub use segmented_index::SegmentedIndex;

/// A memory-mapped, read-only position index.
///
/// Supports single monolithic index files (`.brettpos`) as well as tiered
/// multi-segment indices (`.bpx`) for progressive live availability.
#[derive(Clone)]
pub struct PositionIndex {
    inner: SegmentedIndex,
}

impl PositionIndex {
    /// Load an index file, validating its header and length.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let segment = IndexSegment::open(path)?;
        let mut segments = vec![segment];
        let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if let Some(parent) = path.parent()
            && let Ok(entries) = fs::read_dir(parent)
        {
            for entry in entries.flatten() {
                let p = entry.path();
                if p != path
                    && p.extension().is_some_and(|ext| ext == "bpx")
                    && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
                    && stem.starts_with(file_stem)
                    && let Ok(seg) = IndexSegment::open(&p)
                {
                    segments.push(seg);
                }
            }
        }
        let inner = SegmentedIndex::from_segments(path, segments);
        Ok(Self { inner })
    }

    pub fn from_segmented(inner: SegmentedIndex) -> Self {
        Self { inner }
    }

    pub fn segmented(&self) -> &SegmentedIndex {
        &self.inner
    }

    pub fn segments(&self) -> Arc<std::sync::RwLock<Vec<IndexSegment>>> {
        self.inner.segments_arc()
    }

    pub fn add_segment(&self, segment: IndexSegment) {
        self.inner.add_segment(segment);
    }

    pub fn set_segments(&self, segments: Vec<IndexSegment>) {
        self.inner.set_segments(segments);
    }

    pub fn segment_count(&self) -> usize {
        self.inner.segment_count()
    }

    pub fn append_delta_segment(
        &self,
        games: &[crate::domain::Game],
        meta: SourceMeta,
        delta_path: impl AsRef<Path>,
        options: BuildOptions,
    ) -> Result<BuildReport> {
        self.inner
            .append_delta_segment(games, meta, delta_path, options)
    }

    pub fn compact(&self, meta: SourceMeta, max_plies: u32) -> Result<BuildReport> {
        compact_segments(&self.inner, self.path(), meta, max_plies)
    }

    /// Open the cached index for `source`, if it exists and is current.
    pub fn open_cached(source: impl AsRef<Path>) -> Result<Option<Self>> {
        let source = source.as_ref();
        let meta = SourceMeta::read(source)?;
        let sidecar = source.with_extension("brettpos");
        if let Some(index) = Self::open_cached_at(source, &sidecar, &meta)? {
            return Ok(Some(index));
        }
        Self::open_cached_at(source, &cache_path(source), &meta)
    }

    /// Open the cached index for a database, if it exists and is current.
    pub fn open_cached_for(library: &dyn GameLibrary) -> Result<Option<Self>> {
        let meta = library.source_meta()?;
        let sidecar = library.path().with_extension("brettpos");
        if let Some(index) = Self::open_cached_at(library.path(), &sidecar, &meta)? {
            return Ok(Some(index));
        }
        let path = cache_path(library.path());
        Self::open_cached_at(library.path(), &path, &meta)
    }

    /// Open a cached index at an explicit path, for callers that key the cache
    /// by more than the source (for example a reference rating window).
    pub fn open_cached_for_at(
        library: &dyn GameLibrary,
        index_path: impl AsRef<Path>,
    ) -> Result<Option<Self>> {
        let meta = library.source_meta()?;
        Self::open_cached_at(library.path(), index_path.as_ref(), &meta)
    }

    fn open_cached_at(source: &Path, index_path: &Path, meta: &SourceMeta) -> Result<Option<Self>> {
        if !index_path.exists() {
            return Ok(None);
        }
        match Self::open(index_path) {
            Ok(index) if index.source_matches(meta) => Ok(Some(index)),
            Ok(_) => Ok(None),
            Err(err) => {
                tracing::warn!(
                    "ignoring stale or invalid index {} for {}: {err:#}",
                    index_path.display(),
                    source.display()
                );
                Ok(None)
            }
        }
    }

    /// Build an index into the cache and open it.
    pub fn build_cached(
        source: impl AsRef<Path>,
        options: BuildOptions,
        progress: impl FnMut(BuildProgress),
    ) -> Result<Self> {
        let source = source.as_ref();
        let path = cache_path(source);
        Self::build_to(source, &path, options, progress)?;
        Self::open(&path)
    }

    /// Build a cache index for any [`GameLibrary`], using the streaming PGN
    /// fast path when possible and a generic loader for SCID and classic CBH.
    pub fn build_cached_for(
        library: Arc<dyn GameLibrary>,
        options: BuildOptions,
        progress: impl FnMut(BuildProgress) + Send,
    ) -> Result<Self> {
        let path = cache_path(library.path());
        Self::build_cached_for_at(library, path, options, progress)
    }

    /// Build a cache index at an explicit path using parallel chunked streaming
    /// and background compaction.
    pub fn build_cached_for_at(
        library: Arc<dyn GameLibrary>,
        index_path: impl AsRef<Path>,
        options: BuildOptions,
        progress: impl FnMut(BuildProgress) + Send,
    ) -> Result<Self> {
        let index_path = index_path.as_ref();
        let meta = library.source_meta()?;
        let chunk_options = ChunkedBuildOptions {
            build_options: options,
            chunk_size: DEFAULT_CHUNK_SIZE,
        };
        let live_index = SegmentedIndex::new(index_path);
        let progress_mutex = std::sync::Mutex::new(progress);
        build_chunked_parallel(
            library.as_ref(),
            index_path,
            chunk_options,
            Some(&live_index),
            |p| {
                if let Ok(mut guard) = progress_mutex.lock() {
                    guard(p);
                }
            },
        )?;
        compact_segments(&live_index, index_path, meta, options.max_plies)?;
        Self::open(index_path)
    }

    /// Build an index from games loaded through the common database
    /// interface. Used for SCID and classic CBH, where the streaming PGN
    /// reader cannot be applied directly.
    pub fn build_library_to(
        library: &dyn GameLibrary,
        index_path: impl AsRef<Path>,
        options: BuildOptions,
        mut progress: impl FnMut(BuildProgress),
    ) -> Result<BuildReport> {
        let index_path = index_path.as_ref();
        let started = Instant::now();
        let meta = library.source_meta()?;
        let mut accumulator = EdgeAccumulator::new(index_path, options.max_in_memory_edges);
        let mut games = 0usize;
        let mut last_report = 0usize;

        for index in 0..library.len() {
            if let Some(entry) = library.get(index) {
                if entry.cbh_deleted() {
                    continue;
                }
                if let Some(filter) = options.rating_filter {
                    let pre_meta = GameMeta::from_shared_tags(entry.headers());
                    if !filter.accepts(&pre_meta) {
                        continue;
                    }
                }
            }

            let included = match library.load(index) {
                Ok(game) => {
                    if Self::accumulate_domain_game(&game, options, &mut accumulator) {
                        accumulator.maybe_spill()?;
                        true
                    } else {
                        false
                    }
                }
                Err(err) => {
                    tracing::warn!("position index skipped game {index}: {err:#}");
                    false
                }
            };

            if included {
                games += 1;
                if games.saturating_sub(last_report) >= 1000 {
                    progress(BuildProgress {
                        games,
                        edges: accumulator.in_memory_len(),
                        elapsed: started.elapsed(),
                    });
                    last_report = games;
                }
            }
        }

        let report =
            accumulator.finish_to_index(index_path, meta, options.max_plies, games, started)?;
        let rate = if report.elapsed.as_secs_f64() > 0.0 {
            report.games as f64 / report.elapsed.as_secs_f64()
        } else {
            0.0
        };
        tracing::info!(
            "position index (library) built: {} games, {} edges in {:.2}s ({rate:.0} games/s)",
            report.games,
            report.edges,
            report.elapsed.as_secs_f64()
        );
        progress(BuildProgress {
            games: report.games,
            edges: report.edges,
            elapsed: report.elapsed,
        });
        Ok(report)
    }

    /// Build an index file at `index_path`.
    pub fn build_to(
        source: impl AsRef<Path>,
        index_path: impl AsRef<Path>,
        options: BuildOptions,
        mut progress: impl FnMut(BuildProgress),
    ) -> Result<BuildReport> {
        let source = source.as_ref();
        let index_path = index_path.as_ref();
        let started = Instant::now();
        let meta = SourceMeta::read(source)?;

        let file = File::open(source).with_context(|| format!("opening {}", source.display()))?;
        let mut reader = Reader::new(BufReader::new(file));
        let mut visitor = IndexVisitor::new(index_path, options, started, &mut progress);
        while reader
            .read_game(&mut visitor)
            .with_context(|| format!("indexing {}", source.display()))?
            .is_some()
        {
            visitor.accumulator.maybe_spill()?;
        }
        let (accumulator, games) = visitor.finish();
        let report =
            accumulator.finish_to_index(index_path, meta, options.max_plies, games, started)?;
        progress(BuildProgress {
            games: report.games,
            edges: report.edges,
            elapsed: report.elapsed,
        });
        Ok(report)
    }

    /// Incrementally merge additional in-memory [`crate::domain::Game`]s into
    /// this index and write the updated `.brettpos` file to `index_path`.
    pub fn merge_games_to(
        &self,
        games: &[crate::domain::Game],
        meta: SourceMeta,
        index_path: impl AsRef<Path>,
        options: BuildOptions,
    ) -> Result<BuildReport> {
        let index_path = index_path.as_ref();
        let started = Instant::now();
        let mut accumulator = EdgeAccumulator::new(index_path, options.max_in_memory_edges);
        let mut added_games = 0usize;
        for game in games {
            if Self::accumulate_domain_game(game, options, &mut accumulator) {
                added_games += 1;
                accumulator.maybe_spill()?;
            }
        }
        let total_games = self.game_count().saturating_add(added_games);
        accumulator.finish_merged_with_existing(
            self,
            index_path,
            meta,
            options.max_plies,
            total_games,
            started,
        )
    }

    /// Merge multiple existing [`PositionIndex`] files into a single combined
    /// index at `index_path` using a streaming k-way merge without rebuilding
    /// from the underlying databases.
    pub fn merge_indices_to(
        indices: &[&PositionIndex],
        meta: SourceMeta,
        index_path: impl AsRef<Path>,
        max_plies: u32,
    ) -> Result<BuildReport> {
        let index_path = index_path.as_ref();
        let started = Instant::now();
        let total_games: usize = indices.iter().map(|idx| idx.game_count()).sum();
        let sources: Vec<MergeSource<'_>> = indices
            .iter()
            .filter(|idx| !idx.empty())
            .map(|idx| MergeSource::Index {
                index: idx,
                pos: 0,
                end: idx.edge_count(),
            })
            .collect();
        write_merged_sources(index_path, meta, max_plies, total_games, started, sources)
    }

    fn accumulate_domain_game(
        game: &crate::domain::Game,
        options: BuildOptions,
        accumulator: &mut EdgeAccumulator,
    ) -> bool {
        let game_meta = GameMeta::from_tags(&game.headers);
        if options
            .rating_filter
            .is_some_and(|filter| !filter.accepts(&game_meta))
        {
            return false;
        }
        let mut seen_edges: HashSet<(u64, u32)> = HashSet::new();
        let mut node = 0usize;
        let mut plies = 0u32;
        while let Some(&child) = game.nodes[node].children.first() {
            let parent = &game.nodes[node];
            let Some(ply) = game.nodes[child].mv.as_ref() else {
                break;
            };
            if ply.uci == "0000" {
                break;
            }
            let Ok(ucimove) = UciMove::from_ascii(ply.uci.as_bytes()) else {
                break;
            };
            let Ok(mv) = ucimove.to_move(&parent.pos) else {
                break;
            };
            let Some(code) = encode_move(mv) else {
                break;
            };
            let key = position_key(&parent.pos);
            if seen_edges.insert((key, code)) {
                accumulator.add_move(key, code, &game_meta);
            }
            node = child;
            plies += 1;
            if plies >= options.max_plies {
                break;
            }
        }
        true
    }

    pub fn path(&self) -> &Path {
        self.inner.path()
    }

    pub fn game_count(&self) -> usize {
        self.inner.game_count()
    }

    pub fn edge_count(&self) -> usize {
        self.inner.edge_count()
    }

    pub fn position_count(&self) -> usize {
        self.inner.position_count()
    }

    pub fn build_time(&self) -> Duration {
        let segs = self.inner.segments_arc();
        let segs = segs.read().expect("segments lock poisoned");
        segs.first()
            .map(|s| Duration::from_millis(s.header().build_millis))
            .unwrap_or_default()
    }

    pub fn file_len(&self) -> u64 {
        self.inner.file_len()
    }

    pub fn empty(&self) -> bool {
        self.inner.empty()
    }

    /// Move records from `key`, in stored `(move code)` order, with quarterly
    /// buckets aligned to the latest dated quarter at `key`.
    pub fn moves(&self, key: u64) -> Vec<MoveStat> {
        self.inner.moves(key)
    }

    /// Aggregated statistics for a position, or `None` if no indexed game
    /// reached it with a move available.
    pub fn stats(&self, key: u64) -> Option<PositionStats> {
        self.inner.stats(key)
    }

    /// Latest game date recorded in this index (as `YYYYMMDD`, or `0` if empty or undated).
    pub fn latest_date(&self) -> u32 {
        let root_date = self
            .stats(position_key(&crate::chess::Chess::default()))
            .map(|s| s.last_date)
            .unwrap_or(0);
        if root_date > 0 {
            return root_date;
        }
        let segs = self.inner.segments_arc();
        let segs = segs.read().expect("segments lock poisoned");
        let mut max_date = 0;
        for seg in segs.iter() {
            for i in 0..seg.edge_count() {
                let d = seg.edge_at(i).last_date;
                if d > max_date {
                    max_date = d;
                }
            }
        }
        max_date
    }

    fn source_matches(&self, meta: &SourceMeta) -> bool {
        let segs = self.inner.segments_arc();
        let segs = segs.read().expect("segments lock poisoned");
        if let Some(base) = segs.first() {
            let h = base.header();
            h.source_len == meta.len
                && h.source_mtime_secs == meta.mtime_secs
                && h.source_mtime_nanos == meta.mtime_nanos
        } else {
            false
        }
    }

    pub(crate) fn edge_at(&self, index: usize) -> EdgeRecord {
        let segs = self.inner.segments_arc();
        let segs = segs.read().expect("segments lock poisoned");
        let mut offset = index;
        for seg in segs.iter() {
            if offset < seg.edge_count() {
                return seg.edge_at(offset);
            }
            offset -= seg.edge_count();
        }
        panic!(
            "edge_at out of bounds: index {index} >= total {}",
            self.edge_count()
        );
    }
}

/// Stable hash key used by the index. The same position always maps to the
/// same key across runs and index rebuilds.
pub fn position_key(pos: &Chess) -> u64 {
    pos.position_hash()
}

/// Encode a legal chess move as 16 bits of squares plus a promotion code.
pub fn encode_move(mv: Move) -> Option<u32> {
    let (from, to, promotion) = match mv {
        Move::Normal {
            from,
            to,
            promotion,
            ..
        } => (from, to, promotion),
        Move::EnPassant { from, to } => (from, to, None),
        Move::Castle { king, rook } => (king, castle_destination(king, rook), None),
        Move::Put { .. } => return None,
    };
    let promotion = match promotion {
        None => NO_PROMOTION,
        Some(Role::Queen) => 0,
        Some(Role::Rook) => 1,
        Some(Role::Bishop) => 2,
        Some(Role::Knight) => 3,
        Some(_) => return None,
    };
    Some(from.to_u32() | (to.to_u32() << 6) | (promotion << 12))
}

/// Decode a stored move code against `pos` by finding the matching legal move.
pub fn decode_move(pos: &Chess, code: u32) -> Option<Move> {
    let from = Square::new(code & 0x3F);
    let to = Square::new((code >> 6) & 0x3F);
    let promotion = match (code >> 12) & 0x7 {
        0 => Some(Role::Queen),
        1 => Some(Role::Rook),
        2 => Some(Role::Bishop),
        3 => Some(Role::Knight),
        NO_PROMOTION => None,
        _ => return None,
    };
    pos.legal_moves().iter().copied().find(|mv| {
        mv.from() == Some(from)
            && move_destination(*mv) == to
            && mv.promotion() == promotion
            && !matches!(mv, Move::Put { .. })
    })
}

fn move_destination(mv: Move) -> Square {
    match mv {
        Move::Castle { king, rook } => castle_destination(king, rook),
        _ => mv.to(),
    }
}

fn castle_destination(king: Square, rook: Square) -> Square {
    let file = if rook.file() > king.file() {
        ChessFile::G
    } else {
        ChessFile::C
    };
    Square::from_coords(file, king.rank())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EdgeRecord {
    pub(crate) key: u64,
    pub(crate) mv: u32,
    pub(crate) games: u32,
    pub(crate) white: u32,
    pub(crate) draw: u32,
    pub(crate) black: u32,
    pub(crate) elo_sum: u64,
    pub(crate) elo_count: u32,
    pub(crate) first_date: u32,
    pub(crate) last_date: u32,
    pub(crate) anchor_quarter: u32,
    pub(crate) older_dated: u32,
    pub(crate) buckets: [u32; TREND_BUCKETS],
}

impl EdgeRecord {
    pub(crate) fn advance_anchor(&mut self, new_q: u32) {
        if new_q == 0 {
            return;
        }
        if self.anchor_quarter == 0 {
            self.anchor_quarter = new_q;
            return;
        }
        if new_q <= self.anchor_quarter {
            return;
        }
        let shift = (new_q - self.anchor_quarter) as usize;
        if shift >= TREND_BUCKETS {
            let moved: u32 = self.buckets.iter().sum();
            self.older_dated = self.older_dated.saturating_add(moved);
            self.buckets = [0; TREND_BUCKETS];
        } else {
            let moved: u32 = self.buckets[..shift].iter().sum();
            self.older_dated = self.older_dated.saturating_add(moved);
            self.buckets.copy_within(shift..TREND_BUCKETS, 0);
            self.buckets[TREND_BUCKETS - shift..].fill(0);
        }
        self.anchor_quarter = new_q;
    }

    pub(crate) fn add_quarter(&mut self, q: u32) {
        if q == 0 {
            return;
        }
        if self.anchor_quarter == 0 {
            self.anchor_quarter = q;
            self.buckets[TREND_BUCKETS - 1] = self.buckets[TREND_BUCKETS - 1].saturating_add(1);
        } else if q > self.anchor_quarter {
            self.advance_anchor(q);
            self.buckets[TREND_BUCKETS - 1] = self.buckets[TREND_BUCKETS - 1].saturating_add(1);
        } else {
            let age = (self.anchor_quarter - q) as usize;
            if age < TREND_BUCKETS {
                let idx = TREND_BUCKETS - 1 - age;
                self.buckets[idx] = self.buckets[idx].saturating_add(1);
            } else {
                self.older_dated = self.older_dated.saturating_add(1);
            }
        }
    }

    pub(crate) fn merge_from(&mut self, other: &Self) {
        self.games = self.games.saturating_add(other.games);
        self.white = self.white.saturating_add(other.white);
        self.draw = self.draw.saturating_add(other.draw);
        self.black = self.black.saturating_add(other.black);
        self.elo_sum = self.elo_sum.saturating_add(other.elo_sum);
        self.elo_count = self.elo_count.saturating_add(other.elo_count);
        if other.first_date != 0 && (self.first_date == 0 || other.first_date < self.first_date) {
            self.first_date = other.first_date;
        }
        if other.last_date > self.last_date {
            self.last_date = other.last_date;
        }
        if other.anchor_quarter == 0 {
            return;
        }
        let target_anchor = self.anchor_quarter.max(other.anchor_quarter);
        self.advance_anchor(target_anchor);
        let mut rhs = *other;
        rhs.advance_anchor(target_anchor);
        self.older_dated = self.older_dated.saturating_add(rhs.older_dated);
        for (dst, src) in self.buckets.iter_mut().zip(rhs.buckets.iter()) {
            *dst = dst.saturating_add(*src);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct IndexHeader {
    pub(crate) source_len: u64,
    pub(crate) source_mtime_secs: u64,
    pub(crate) source_mtime_nanos: u32,
    pub(crate) max_plies: u32,
    pub(crate) edge_count: u64,
    pub(crate) game_count: u64,
    pub(crate) position_count: u64,
    pub(crate) build_millis: u64,
}

impl IndexHeader {
    pub(crate) fn to_bytes(self) -> [u8; HEADER_LEN] {
        let mut bytes = [0u8; HEADER_LEN];
        bytes[0..8].copy_from_slice(MAGIC);
        bytes[8..12].copy_from_slice(&VERSION.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.source_len.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.source_mtime_secs.to_le_bytes());
        bytes[32..36].copy_from_slice(&self.source_mtime_nanos.to_le_bytes());
        bytes[36..40].copy_from_slice(&self.max_plies.to_le_bytes());
        bytes[40..48].copy_from_slice(&self.edge_count.to_le_bytes());
        bytes[48..56].copy_from_slice(&self.game_count.to_le_bytes());
        bytes[56..64].copy_from_slice(&self.position_count.to_le_bytes());
        bytes[64..72].copy_from_slice(&self.build_millis.to_le_bytes());
        bytes
    }

    pub(crate) fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_LEN {
            bail!("index header is shorter than {HEADER_LEN} bytes");
        }
        if &bytes[0..8] != MAGIC {
            bail!("not a Brett position index");
        }
        let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        if version != VERSION {
            bail!("unsupported position index version {version} (expected {VERSION})");
        }
        Ok(Self {
            source_len: u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            source_mtime_secs: u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
            source_mtime_nanos: u32::from_le_bytes(bytes[32..36].try_into().unwrap()),
            max_plies: u32::from_le_bytes(bytes[36..40].try_into().unwrap()),
            edge_count: u64::from_le_bytes(bytes[40..48].try_into().unwrap()),
            game_count: u64::from_le_bytes(bytes[48..56].try_into().unwrap()),
            position_count: u64::from_le_bytes(bytes[56..64].try_into().unwrap()),
            build_millis: u64::from_le_bytes(bytes[64..72].try_into().unwrap()),
        })
    }
}

/// Cache location for the index of `source`. The index is never written next
/// to a database or PGN file.
pub fn cache_path(source: &Path) -> PathBuf {
    cache_path_tagged(source, None)
}

/// Cache location for a reference index. The path includes the rating window,
/// so a filtered reference cache can never be mistaken for the player index or
/// for another player's rating band.
pub fn reference_cache_path(source: &Path, filter: Option<RatingFilter>) -> PathBuf {
    let tag = match filter {
        Some(filter) => format!("reference-{}-{}", filter.min, filter.max),
        None => "reference-all".to_string(),
    };
    cache_path_tagged(source, Some(&tag))
}

fn cache_path_tagged(source: &Path, tag: Option<&str>) -> PathBuf {
    let canonical = fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in canonical.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    if let Some(tag) = tag {
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x100_0000_01b3);
        for byte in tag.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    cache_dir().join(format!("{hash:016x}.brettpos"))
}

fn cache_dir() -> PathBuf {
    #[cfg(test)]
    {
        return std::env::temp_dir().join(format!("brett-test-index-{}", std::process::id()));
    }
    #[cfg(not(test))]
    {
        if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
            return PathBuf::from(dir).join("brett").join("index");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".cache")
                .join("brett")
                .join("index");
        }
        std::env::temp_dir().join("brett-index")
    }
}

fn temp_path(index_path: &Path) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = index_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "index".to_string());
    index_path.with_file_name(format!("{name}.tmp-{}-{unique}", std::process::id()))
}

fn write_index(path: &Path, header: &IndexHeader, records: &[EdgeRecord]) -> Result<()> {
    let file = File::create(path).with_context(|| format!("creating index {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(&header.to_bytes())
        .context("writing index header")?;
    let mut bytes = [0u8; EDGE_SIZE];
    for record in records {
        write_edge(&mut bytes, record);
        writer.write_all(&bytes).context("writing index record")?;
    }
    writer.flush().context("flushing index")?;
    writer
        .into_inner()
        .context("finishing index file")?
        .sync_all()
        .context("syncing index")?;
    Ok(())
}

pub(crate) fn write_edge(bytes: &mut [u8; EDGE_SIZE], r: &EdgeRecord) {
    bytes[0..8].copy_from_slice(&r.key.to_le_bytes());
    bytes[8..12].copy_from_slice(&r.mv.to_le_bytes());
    bytes[12..16].copy_from_slice(&r.games.to_le_bytes());
    bytes[16..20].copy_from_slice(&r.white.to_le_bytes());
    bytes[20..24].copy_from_slice(&r.draw.to_le_bytes());
    bytes[24..28].copy_from_slice(&r.black.to_le_bytes());
    bytes[28..36].copy_from_slice(&r.elo_sum.to_le_bytes());
    bytes[36..40].copy_from_slice(&r.elo_count.to_le_bytes());
    bytes[40..44].copy_from_slice(&r.first_date.to_le_bytes());
    bytes[44..48].copy_from_slice(&r.last_date.to_le_bytes());
    bytes[48..52].copy_from_slice(&r.anchor_quarter.to_le_bytes());
    bytes[52..56].copy_from_slice(&r.older_dated.to_le_bytes());
    for (i, &count) in r.buckets.iter().enumerate() {
        let start = 56 + i * 4;
        bytes[start..start + 4].copy_from_slice(&count.to_le_bytes());
    }
}

pub(crate) fn read_edge(bytes: &[u8]) -> EdgeRecord {
    let mut buckets = [0u32; TREND_BUCKETS];
    for (i, slot) in buckets.iter_mut().enumerate() {
        let start = 56 + i * 4;
        *slot = u32::from_le_bytes(bytes[start..start + 4].try_into().unwrap());
    }
    EdgeRecord {
        key: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
        mv: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        games: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
        white: u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
        draw: u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
        black: u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
        elo_sum: u64::from_le_bytes(bytes[28..36].try_into().unwrap()),
        elo_count: u32::from_le_bytes(bytes[36..40].try_into().unwrap()),
        first_date: u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
        last_date: u32::from_le_bytes(bytes[44..48].try_into().unwrap()),
        anchor_quarter: u32::from_le_bytes(bytes[48..52].try_into().unwrap()),
        older_dated: u32::from_le_bytes(bytes[52..56].try_into().unwrap()),
        buckets,
    }
}

/// Bounded-memory accumulator that spills sorted run files when `records.len()`
/// reaches `max_in_memory_edges` and merges them via a streaming k-way merge.
struct EdgeAccumulator {
    records: HashMap<(u64, u32), EdgeRecord>,
    max_in_memory_edges: usize,
    base_path: PathBuf,
    run_paths: Vec<PathBuf>,
}

impl EdgeAccumulator {
    fn new(index_path: &Path, max_in_memory_edges: usize) -> Self {
        Self {
            records: HashMap::new(),
            max_in_memory_edges: max_in_memory_edges.max(1),
            base_path: index_path.to_path_buf(),
            run_paths: Vec::new(),
        }
    }

    fn in_memory_len(&self) -> usize {
        self.records.len()
    }

    fn add_move(&mut self, key: u64, mv: u32, meta: &GameMeta) {
        let record = self.records.entry((key, mv)).or_default();
        record.key = key;
        record.mv = mv;
        meta.add_to(record);
    }

    fn maybe_spill(&mut self) -> Result<()> {
        if self.records.len() >= self.max_in_memory_edges {
            self.spill_run()?;
        }
        Ok(())
    }

    fn spill_run(&mut self) -> Result<()> {
        if self.records.is_empty() {
            return Ok(());
        }
        if let Some(parent) = self
            .base_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating index directory {}", parent.display()))?;
        }
        let mut sorted: Vec<EdgeRecord> = self.records.drain().map(|(_, v)| v).collect();
        sorted.sort_unstable_by(|a, b| a.key.cmp(&b.key).then(a.mv.cmp(&b.mv)));

        let run_idx = self.run_paths.len();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = self
            .base_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "index".to_string());
        let run_path = self.base_path.with_file_name(format!(
            "{name}.run-{run_idx}-{}-{unique}.tmp",
            std::process::id()
        ));
        let file = File::create(&run_path)
            .with_context(|| format!("creating external-sort run {}", run_path.display()))?;
        let mut writer = BufWriter::new(file);
        let mut bytes = [0u8; EDGE_SIZE];
        for record in &sorted {
            write_edge(&mut bytes, record);
            writer
                .write_all(&bytes)
                .with_context(|| format!("writing external-sort run {}", run_path.display()))?;
        }
        writer
            .flush()
            .with_context(|| format!("flushing external-sort run {}", run_path.display()))?;
        self.run_paths.push(run_path);
        Ok(())
    }

    fn into_sorted_vec(mut self) -> Vec<EdgeRecord> {
        let mut records: Vec<EdgeRecord> = self.records.drain().map(|(_, v)| v).collect();
        records.sort_unstable_by(|a, b| a.key.cmp(&b.key).then(a.mv.cmp(&b.mv)));
        records
    }

    fn finish_to_index(
        mut self,
        index_path: &Path,
        meta: SourceMeta,
        max_plies: u32,
        games: usize,
        started: Instant,
    ) -> Result<BuildReport> {
        if self.run_paths.is_empty() {
            let records = self.into_sorted_vec();
            let elapsed = started.elapsed();
            let positions = records
                .windows(2)
                .filter(|pair| pair[0].key != pair[1].key)
                .count()
                + usize::from(!records.is_empty());
            let header = IndexHeader {
                source_len: meta.len,
                source_mtime_secs: meta.mtime_secs,
                source_mtime_nanos: meta.mtime_nanos,
                max_plies,
                edge_count: records.len() as u64,
                game_count: games as u64,
                position_count: positions as u64,
                build_millis: elapsed.as_millis().min(u64::MAX as u128) as u64,
            };
            if let Some(parent) = index_path
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
            {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating index directory {}", parent.display()))?;
            }
            let tmp = temp_path(index_path);
            let write_result = write_index(&tmp, &header, &records);
            if let Err(err) = write_result {
                let _ = fs::remove_file(&tmp);
                return Err(err);
            }
            fs::rename(&tmp, index_path).with_context(|| {
                format!(
                    "installing index {} -> {}",
                    tmp.display(),
                    index_path.display()
                )
            })?;
            let bytes = HEADER_LEN as u64 + records.len() as u64 * EDGE_SIZE as u64;
            return Ok(BuildReport {
                games,
                edges: records.len(),
                positions,
                elapsed,
                bytes,
            });
        }

        if !self.records.is_empty() {
            self.spill_run()?;
        }
        let mut sources = Vec::with_capacity(self.run_paths.len());
        for path in &self.run_paths {
            let file = File::open(path)
                .with_context(|| format!("opening external-sort run {}", path.display()))?;
            sources.push(MergeSource::RunFile(BufReader::new(file)));
        }
        write_merged_sources(index_path, meta, max_plies, games, started, sources)
    }

    fn finish_merged_with_existing(
        mut self,
        existing: &PositionIndex,
        index_path: &Path,
        meta: SourceMeta,
        max_plies: u32,
        total_games: usize,
        started: Instant,
    ) -> Result<BuildReport> {
        let mut sources: Vec<MergeSource<'_>> = Vec::new();
        if !existing.empty() {
            sources.push(MergeSource::Index {
                index: existing,
                pos: 0,
                end: existing.edge_count(),
            });
        }
        if self.run_paths.is_empty() {
            let mut records: Vec<EdgeRecord> = self.records.drain().map(|(_, v)| v).collect();
            records.sort_unstable_by(|a, b| a.key.cmp(&b.key).then(a.mv.cmp(&b.mv)));
            if !records.is_empty() {
                sources.push(MergeSource::Memory {
                    records: records.into_iter(),
                });
            }
        } else {
            if !self.records.is_empty() {
                self.spill_run()?;
            }
            for path in &self.run_paths {
                let file = File::open(path)
                    .with_context(|| format!("opening external-sort run {}", path.display()))?;
                sources.push(MergeSource::RunFile(BufReader::new(file)));
            }
        }
        write_merged_sources(index_path, meta, max_plies, total_games, started, sources)
    }
}

impl Drop for EdgeAccumulator {
    fn drop(&mut self) {
        for path in &self.run_paths {
            let _ = fs::remove_file(path);
        }
    }
}

pub(crate) enum MergeSource<'a> {
    RunFile(BufReader<File>),
    Index {
        index: &'a PositionIndex,
        pos: usize,
        end: usize,
    },
    Segment {
        segment: &'a IndexSegment,
        pos: usize,
        end: usize,
    },
    Memory {
        records: std::vec::IntoIter<EdgeRecord>,
    },
}

impl MergeSource<'_> {
    fn next_record(&mut self) -> Result<Option<EdgeRecord>> {
        match self {
            Self::RunFile(reader) => {
                let mut buf = [0u8; EDGE_SIZE];
                match reader.read_exact(&mut buf) {
                    Ok(()) => Ok(Some(read_edge(&buf))),
                    Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
                    Err(err) => Err(err).context("reading external-sort run record"),
                }
            }
            Self::Index { index, pos, end } => {
                if *pos >= *end {
                    Ok(None)
                } else {
                    let record = index.edge_at(*pos);
                    *pos += 1;
                    Ok(Some(record))
                }
            }
            Self::Segment { segment, pos, end } => {
                if *pos >= *end {
                    Ok(None)
                } else {
                    let record = segment.edge_at(*pos);
                    *pos += 1;
                    Ok(Some(record))
                }
            }
            Self::Memory { records } => Ok(records.next()),
        }
    }
}

#[derive(Clone, Copy)]
struct HeapItem {
    record: EdgeRecord,
    source_idx: usize,
}

impl PartialEq for HeapItem {
    fn eq(&self, other: &Self) -> bool {
        self.record.key == other.record.key
            && self.record.mv == other.record.mv
            && self.source_idx == other.source_idx
    }
}

impl Eq for HeapItem {}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .record
            .key
            .cmp(&self.record.key)
            .then_with(|| other.record.mv.cmp(&self.record.mv))
            .then_with(|| other.source_idx.cmp(&self.source_idx))
    }
}

pub(crate) fn write_merged_sources(
    index_path: &Path,
    meta: SourceMeta,
    max_plies: u32,
    games: usize,
    started: Instant,
    mut sources: Vec<MergeSource<'_>>,
) -> Result<BuildReport> {
    if let Some(parent) = index_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating index directory {}", parent.display()))?;
    }
    let tmp = temp_path(index_path);
    let result = (|| -> Result<(usize, usize, Duration)> {
        let file =
            File::create(&tmp).with_context(|| format!("creating index {}", tmp.display()))?;
        let mut writer = BufWriter::new(file);
        writer
            .write_all(&[0u8; HEADER_LEN])
            .context("writing placeholder index header")?;

        let mut heap = BinaryHeap::with_capacity(sources.len());
        for (source_idx, source) in sources.iter_mut().enumerate() {
            if let Some(record) = source.next_record()? {
                heap.push(HeapItem { record, source_idx });
            }
        }

        let mut edges = 0usize;
        let mut positions = 0usize;
        let mut last_key: Option<u64> = None;
        let mut bytes = [0u8; EDGE_SIZE];

        while let Some(HeapItem {
            mut record,
            source_idx,
        }) = heap.pop()
        {
            if let Some(next) = sources[source_idx].next_record()? {
                heap.push(HeapItem {
                    record: next,
                    source_idx,
                });
            }
            while let Some(&top) = heap.peek() {
                if top.record.key != record.key || top.record.mv != record.mv {
                    break;
                }
                let dup = heap.pop().unwrap();
                record.merge_from(&dup.record);
                if let Some(next) = sources[dup.source_idx].next_record()? {
                    heap.push(HeapItem {
                        record: next,
                        source_idx: dup.source_idx,
                    });
                }
            }
            if last_key != Some(record.key) {
                positions += 1;
                last_key = Some(record.key);
            }
            edges += 1;
            write_edge(&mut bytes, &record);
            writer.write_all(&bytes).context("writing merged edge")?;
        }

        writer.flush().context("flushing merged index")?;
        let mut file = writer.into_inner().context("finishing merged index")?;
        let elapsed = started.elapsed();
        let header = IndexHeader {
            source_len: meta.len,
            source_mtime_secs: meta.mtime_secs,
            source_mtime_nanos: meta.mtime_nanos,
            max_plies,
            edge_count: edges as u64,
            game_count: games as u64,
            position_count: positions as u64,
            build_millis: elapsed.as_millis().min(u64::MAX as u128) as u64,
        };
        file.seek(SeekFrom::Start(0))
            .context("seeking to index header")?;
        file.write_all(&header.to_bytes())
            .context("writing final index header")?;
        file.sync_all().context("syncing merged index")?;
        Ok((edges, positions, elapsed))
    })();

    let (edges, positions, elapsed) = match result {
        Ok(vals) => vals,
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
    };
    fs::rename(&tmp, index_path).with_context(|| {
        format!(
            "installing index {} -> {}",
            tmp.display(),
            index_path.display()
        )
    })?;
    let bytes = HEADER_LEN as u64 + edges as u64 * EDGE_SIZE as u64;
    Ok(BuildReport {
        games,
        edges,
        positions,
        elapsed,
        bytes,
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct GameMeta {
    pub(crate) outcome: Outcome,
    pub(crate) white_elo: Option<u32>,
    pub(crate) black_elo: Option<u32>,
    pub(crate) date: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) enum Outcome {
    White,
    Draw,
    Black,
    #[default]
    Unknown,
}

impl GameMeta {
    pub(crate) fn average_elo(&self) -> Option<u32> {
        match (self.white_elo, self.black_elo) {
            (Some(white), Some(black)) => Some(((u64::from(white) + u64::from(black)) / 2) as u32),
            (Some(elo), None) | (None, Some(elo)) => Some(elo),
            (None, None) => None,
        }
    }

    pub(crate) fn from_tags(tags: &[(String, String)]) -> Self {
        let mut meta = Self::default();
        for (key, value) in tags {
            meta.observe_tag(key, value);
        }
        meta
    }

    pub(crate) fn from_shared_tags(tags: &[(SharedStr, SharedStr)]) -> Self {
        let mut meta = Self::default();
        for (key, value) in tags {
            meta.observe_tag(key.as_str(), value.as_str());
        }
        meta
    }

    pub(crate) fn observe_tag(&mut self, key: &str, value: &str) {
        if key.eq_ignore_ascii_case("Result") {
            self.outcome = match value {
                "1-0" => Outcome::White,
                "0-1" => Outcome::Black,
                "1/2-1/2" => Outcome::Draw,
                _ => Outcome::Unknown,
            };
        } else if key.eq_ignore_ascii_case("WhiteElo") {
            self.white_elo = parse_elo(value);
        } else if key.eq_ignore_ascii_case("BlackElo") {
            self.black_elo = parse_elo(value);
        } else if key.eq_ignore_ascii_case("Date") {
            self.date = parse_date(value).unwrap_or(0);
        }
    }

    pub(crate) fn add_to(&self, record: &mut EdgeRecord) {
        record.games += 1;
        match self.outcome {
            Outcome::White => record.white += 1,
            Outcome::Draw => record.draw += 1,
            Outcome::Black => record.black += 1,
            Outcome::Unknown => {}
        }
        if let Some(elo) = self.white_elo {
            record.elo_sum += u64::from(elo);
            record.elo_count += 1;
        }
        if let Some(elo) = self.black_elo {
            record.elo_sum += u64::from(elo);
            record.elo_count += 1;
        }
        if self.date != 0 {
            if record.first_date == 0 || self.date < record.first_date {
                record.first_date = self.date;
            }
            if self.date > record.last_date {
                record.last_date = self.date;
            }
            if let Some(q) = date_to_quarter(self.date) {
                record.add_quarter(q);
            }
        }
    }
}

fn parse_elo(value: &str) -> Option<u32> {
    value.trim().parse().ok().filter(|elo| *elo > 0)
}

fn parse_date(value: &str) -> Option<u32> {
    let mut parts = value.split('.');
    let year = parts.next()?.parse::<u32>().ok()?;
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    if year == 0 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(year * 10_000 + month * 100 + day)
}

struct GameState {
    pos: Chess,
    meta: GameMeta,
    seen_edges: HashSet<(u64, u32)>,
    active: bool,
    included: bool,
    plies: u32,
    variation_depth: usize,
}

impl GameState {
    fn new(tags: &[(String, String)], options: BuildOptions) -> Self {
        let pos = tags
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("FEN"))
            .and_then(|(_, value)| Fen::from_ascii(value.as_bytes()).ok())
            .and_then(|fen| fen.into_position(CastlingMode::Standard).ok())
            .unwrap_or_default();
        let meta = GameMeta::from_tags(tags);
        let included = options
            .rating_filter
            .is_none_or(|filter| filter.accepts(&meta));
        Self {
            pos,
            meta,
            seen_edges: HashSet::new(),
            active: included,
            included,
            plies: 0,
            variation_depth: 0,
        }
    }
}

struct IndexVisitor<'a, F> {
    accumulator: EdgeAccumulator,
    options: BuildOptions,
    started: Instant,
    progress: &'a mut F,
    games: usize,
    last_report: usize,
}

impl<'a, F> IndexVisitor<'a, F>
where
    F: FnMut(BuildProgress),
{
    fn new(
        index_path: &Path,
        options: BuildOptions,
        started: Instant,
        progress: &'a mut F,
    ) -> Self {
        Self {
            accumulator: EdgeAccumulator::new(index_path, options.max_in_memory_edges),
            options,
            started,
            progress,
            games: 0,
            last_report: 0,
        }
    }

    fn report(&mut self) {
        (self.progress)(BuildProgress {
            games: self.games,
            edges: self.accumulator.in_memory_len(),
            elapsed: self.started.elapsed(),
        });
        self.last_report = self.games;
    }

    fn finish(self) -> (EdgeAccumulator, usize) {
        (self.accumulator, self.games)
    }
}

impl<F> Visitor for IndexVisitor<'_, F>
where
    F: FnMut(BuildProgress),
{
    type Tags = Vec<(String, String)>;
    type Movetext = GameState;
    type Output = ();

    fn begin_tags(&mut self) -> ControlFlow<(), Vec<(String, String)>> {
        ControlFlow::Continue(Vec::new())
    }

    fn tag(
        &mut self,
        tags: &mut Vec<(String, String)>,
        name: &str,
        value: &str,
    ) -> ControlFlow<()> {
        tags.push((name.to_string(), value.to_string()));
        ControlFlow::Continue(())
    }

    fn begin_movetext(&mut self, tags: Vec<(String, String)>) -> ControlFlow<(), GameState> {
        ControlFlow::Continue(GameState::new(&tags, self.options))
    }

    fn begin_variation(&mut self, state: &mut GameState) -> ControlFlow<()> {
        state.variation_depth += 1;
        ControlFlow::Continue(())
    }

    fn end_variation(&mut self, state: &mut GameState) -> ControlFlow<()> {
        state.variation_depth = state.variation_depth.saturating_sub(1);
        ControlFlow::Continue(())
    }

    fn san(&mut self, state: &mut GameState, san_plus: SanPlus) -> ControlFlow<()> {
        if !state.active || state.variation_depth > 0 {
            return ControlFlow::Continue(());
        }
        if matches!(san_plus.san, San::Null) {
            state.active = false;
            return ControlFlow::Continue(());
        }
        let mv = match san_plus.san.to_move(&state.pos) {
            Ok(mv) => mv,
            Err(_) => {
                state.active = false;
                return ControlFlow::Continue(());
            }
        };
        let Some(code) = encode_move(mv) else {
            state.active = false;
            return ControlFlow::Continue(());
        };
        let key = position_key(&state.pos);
        let after = match state.pos.clone().play(mv) {
            Ok(after) => after,
            Err(_) => {
                state.active = false;
                return ControlFlow::Continue(());
            }
        };
        if state.seen_edges.insert((key, code)) {
            self.accumulator.add_move(key, code, &state.meta);
        }
        state.pos = after;
        state.plies += 1;
        if state.plies >= self.options.max_plies {
            state.active = false;
        }
        ControlFlow::Continue(())
    }

    fn end_game(&mut self, state: GameState) {
        if state.included {
            self.games += 1;
            if self.games.saturating_sub(self.last_report) >= 1000 {
                self.report();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position_key_is_stable() {
        assert_eq!(position_key(&Chess::default()), 0x33ab_452b_8705_6e0f);
    }

    #[test]
    fn zobrist_key_and_move_codec_round_trip() {
        position_key_is_stable();
        let pos = Chess::default();
        for mv in pos.legal_moves() {
            let code = encode_move(mv).expect("chess move is encodable");
            assert_eq!(decode_move(&pos, code), Some(mv));
        }
    }

    #[test]
    fn castling_round_trips() {
        let pos: Chess = "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1"
            .parse::<Fen>()
            .unwrap()
            .into_position(CastlingMode::Standard)
            .unwrap();
        for mv in pos.legal_moves() {
            let code = encode_move(mv).unwrap();
            assert_eq!(decode_move(&pos, code), Some(mv));
        }
    }

    #[test]
    fn rating_filtered_reference_counts_selected_games() {
        let pgn = r#"
[Event "High"]
[White "A"]
[Black "B"]
[WhiteElo "2500"]
[BlackElo "2540"]
[Result "1-0"]

1. e4 e5 1-0

[Event "Low"]
[White "C"]
[Black "D"]
[WhiteElo "1500"]
[BlackElo "1520"]
[Result "0-1"]

1. d4 d5 0-1

[Event "Unrated"]
[White "E"]
[Black "F"]
[Result "1/2-1/2"]

1. c4 c5 1/2-1/2
"#;
        let dir = std::env::temp_dir().join(format!(
            "brett-position-filter-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pgn_path = dir.join("reference.pgn");
        let index_path = dir.join("reference.brettpos");
        std::fs::write(&pgn_path, pgn).unwrap();

        PositionIndex::build_to(
            &pgn_path,
            &index_path,
            BuildOptions {
                rating_filter: Some(RatingFilter::around(2520, 200)),
                ..BuildOptions::default()
            },
            |_| {},
        )
        .unwrap();
        let index = PositionIndex::open(&index_path).unwrap();
        assert_eq!(index.game_count(), 1);
        let moves = index.moves(position_key(&Chess::default()));
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].games, 1);
        assert_ne!(
            cache_path(&pgn_path),
            reference_cache_path(&pgn_path, Some(RatingFilter::around(2520, 200)))
        );
        assert_ne!(
            reference_cache_path(&pgn_path, Some(RatingFilter::around(2520, 200))),
            reference_cache_path(&pgn_path, Some(RatingFilter::around(2520, 300)))
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dates_and_elo_are_best_effort() {
        assert_eq!(parse_date("2024.03.07"), Some(2024_0307));
        assert_eq!(parse_date("2024.??.??"), None);
        assert_eq!(parse_date("????.??.??"), None);
        assert_eq!(parse_elo("2750"), Some(2750));
        assert_eq!(parse_elo("?"), None);
        assert_eq!(date_to_quarter(2026_0115), Some(2026 * 4));
        assert_eq!(date_to_quarter(2026_1029), Some(2026 * 4 + 3));
        assert_eq!(quarter_label(2026 * 4 + 3), "2026 Q4");
    }

    #[test]
    fn external_sort_multi_run_matches_single_pass_and_incremental_merge() {
        let pgn = r#"
[Event "G1"]
[Date "2022.02.10"]
[White "A"]
[Black "B"]
[WhiteElo "2500"]
[BlackElo "2480"]
[Result "1-0"]

1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 1-0

[Event "G2"]
[Date "2025.06.15"]
[White "C"]
[Black "D"]
[WhiteElo "2600"]
[BlackElo "2550"]
[Result "1/2-1/2"]

1. e4 e5 2. Nf3 Nf6 3. Nxe5 d6 1/2-1/2

[Event "G3"]
[Date "2026.08.20"]
[White "E"]
[Black "F"]
[WhiteElo "2420"]
[BlackElo "2440"]
[Result "0-1"]

1. e4 c5 2. Nf3 d6 3. d4 cxd4 0-1

[Event "G4"]
[Date "2020.01.05"]
[White "G"]
[Black "H"]
[WhiteElo "2380"]
[BlackElo "2400"]
[Result "1-0"]

1. d4 Nf6 2. c4 e6 3. Nc3 Bb4 1-0
"#;
        let dir = std::env::temp_dir().join(format!(
            "brett-extsort-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pgn_path = dir.join("games.pgn");
        let single_path = dir.join("single.brettpos");
        let multi_path = dir.join("multi.brettpos");
        let inc_path = dir.join("incremental.brettpos");
        std::fs::write(&pgn_path, pgn).unwrap();

        PositionIndex::build_to(&pgn_path, &single_path, BuildOptions::default(), |_| {}).unwrap();
        PositionIndex::build_to(
            &pgn_path,
            &multi_path,
            BuildOptions {
                max_in_memory_edges: 2,
                ..BuildOptions::default()
            },
            |_| {},
        )
        .unwrap();

        let single = PositionIndex::open(&single_path).unwrap();
        let multi = PositionIndex::open(&multi_path).unwrap();
        assert_eq!(single.game_count(), multi.game_count());
        assert_eq!(single.edge_count(), multi.edge_count());
        assert_eq!(single.position_count(), multi.position_count());

        // Compare every edge record byte-for-byte (skipping the 80-byte header
        // whose build_millis timestamp may differ).
        let single_bytes = std::fs::read(&single_path).unwrap();
        let multi_bytes = std::fs::read(&multi_path).unwrap();
        assert_eq!(&single_bytes[HEADER_LEN..], &multi_bytes[HEADER_LEN..]);

        // Now verify incremental merge: build first 2 games, then merge games 3..4.
        let parsed = crate::domain::parse_pgn(pgn.as_bytes()).unwrap();
        assert_eq!(parsed.len(), 4);
        let first_half_pgn = dir.join("half.pgn");
        let half_index_path = dir.join("half.brettpos");
        let split_pos = pgn.find("[Event \"G3\"]").unwrap();
        std::fs::write(&first_half_pgn, &pgn[..split_pos]).unwrap();
        PositionIndex::build_to(
            &first_half_pgn,
            &half_index_path,
            BuildOptions::default(),
            |_| {},
        )
        .unwrap();
        let half_index = PositionIndex::open(&half_index_path).unwrap();
        let meta = SourceMeta::read(&pgn_path).unwrap();
        half_index
            .merge_games_to(&parsed[2..], meta, &inc_path, BuildOptions::default())
            .unwrap();
        let inc_bytes = std::fs::read(&inc_path).unwrap();
        assert_eq!(&single_bytes[HEADER_LEN..], &inc_bytes[HEADER_LEN..]);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn position_index_reports_latest_date() {
        let pgn = r#"
[Event "Old"]
[Date "2024.01.15"]
[White "A"]
[Black "B"]
[Result "1-0"]

1. e4 e5 1-0

[Event "New"]
[Date "2026.05.20"]
[White "C"]
[Black "D"]
[Result "0-1"]

1. d4 d5 0-1
"#;
        let dir = std::env::temp_dir().join(format!(
            "brett-position-latest-date-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pgn_path = dir.join("games.pgn");
        let index_path = dir.join("games.brettpos");
        std::fs::write(&pgn_path, pgn).unwrap();

        PositionIndex::build_to(&pgn_path, &index_path, BuildOptions::default(), |_| {}).unwrap();
        let index = PositionIndex::open(&index_path).unwrap();
        assert_eq!(index.latest_date(), 20260520);

        std::fs::remove_dir_all(dir).unwrap();
    }
}
