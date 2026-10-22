//! On-disk position index and opening-tree statistics.
//!
//! The index is built from the **mainline** of every PGN game. Each entry is
//! keyed by a Zobrist hash of the position *before* a move, plus the move that
//! was played. Records are sorted by `(position, move)` and memory-mapped for
//! lookup, so an index can be much larger than the process heap.
//!
//! Position totals are the sum of the outgoing move records. A game is counted
//! at most once per position: if a game repeats a position, indexing stops at
//! the repetition.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use memmap2::{Mmap, MmapOptions};
use pgn_reader::{RawTag, Reader, SanPlus, Visitor};
use shakmaty::fen::Fen;
use shakmaty::san::San;
use shakmaty::uci::UciMove;
use shakmaty::zobrist::Zobrist64;
use shakmaty::{CastlingMode, Chess, EnPassantMode, Move, Position, Role, Square};

use crate::library::{GameLibrary, SourceMeta};

const MAGIC: &[u8; 8] = b"BRETTPOS";
const VERSION: u32 = 1;
const HEADER_LEN: usize = 80;
const EDGE_SIZE: usize = 48;
const NO_PROMOTION: u32 = 7;

/// Build options for a position index.
#[derive(Clone, Copy, Debug)]
pub struct BuildOptions {
    /// Stop following a game after this many plies. `u32::MAX` means no limit.
    pub max_plies: u32,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            max_plies: u32::MAX,
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

/// A memory-mapped, read-only position index.
pub struct PositionIndex {
    mmap: Mmap,
    header: IndexHeader,
    source: PathBuf,
}

impl PositionIndex {
    /// Load an index file, validating its header and length.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path).with_context(|| format!("opening index {}", path.display()))?;
        // SAFETY: the file is opened read-only and the mapping is not exposed
        // mutably. The index writer replaces the file atomically.
        let mmap = unsafe { MmapOptions::new().map(&file) }
            .with_context(|| format!("mapping index {}", path.display()))?;
        let header = IndexHeader::parse(&mmap)
            .with_context(|| format!("reading index header from {}", path.display()))?;
        let expected = HEADER_LEN
            .checked_add(
                usize::try_from(header.edge_count)
                    .context("index edge count does not fit in memory")?
                    .checked_mul(EDGE_SIZE)
                    .context("index length overflow")?,
            )
            .context("index length overflow")?;
        if mmap.len() < expected {
            bail!(
                "index {} is truncated: {} bytes, expected at least {}",
                path.display(),
                mmap.len(),
                expected
            );
        }
        Ok(Self {
            mmap,
            header,
            source: path.to_path_buf(),
        })
    }

    /// Open the cached index for `source`, if it exists and is current.
    pub fn open_cached(source: impl AsRef<Path>) -> Result<Option<Self>> {
        let source = source.as_ref();
        let meta = SourceMeta::read(source)?;
        let path = cache_path(source);
        if !path.exists() {
            return Ok(None);
        }
        match Self::open(&path) {
            Ok(index) if index.source_matches(&meta) => Ok(Some(index)),
            Ok(_) => Ok(None),
            Err(err) => {
                tracing::warn!(
                    "ignoring stale or invalid index {}: {err:#}",
                    path.display()
                );
                Ok(None)
            }
        }
    }

    /// Open the cached index for a database, if it exists and is current.
    pub fn open_cached_for(library: &dyn GameLibrary) -> Result<Option<Self>> {
        let meta = library.source_meta()?;
        let path = cache_path(library.path());
        if !path.exists() {
            return Ok(None);
        }
        match Self::open(&path) {
            Ok(index) if index.source_matches(&meta) => Ok(Some(index)),
            Ok(_) => Ok(None),
            Err(err) => {
                tracing::warn!(
                    "ignoring stale or invalid index {}: {err:#}",
                    path.display()
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
    /// fast path when possible and a generic loader for SCID.
    pub fn build_cached_for(
        library: Arc<dyn GameLibrary>,
        options: BuildOptions,
        progress: impl FnMut(BuildProgress),
    ) -> Result<Self> {
        let path = cache_path(library.path());
        if let Some(pgn) = library.as_pgn() {
            Self::build_to(pgn.path(), &path, options, progress)?;
        } else {
            Self::build_library_to(library.as_ref(), &path, options, progress)?;
        }
        Self::open(&path)
    }

    /// Build an index from games loaded through the common database
    /// interface. Used for SCID, where the streaming PGN reader cannot be
    /// applied directly.
    pub fn build_library_to(
        library: &dyn GameLibrary,
        index_path: impl AsRef<Path>,
        options: BuildOptions,
        mut progress: impl FnMut(BuildProgress),
    ) -> Result<BuildReport> {
        let index_path = index_path.as_ref();
        let started = Instant::now();
        let meta = library.source_meta()?;
        let mut records: HashMap<(u64, u32), EdgeRecord> = HashMap::new();
        let mut games = 0usize;
        let mut last_report = 0usize;

        for index in 0..library.len() {
            match library.load(index) {
                Ok(game) => {
                    let game_meta = GameMeta::from_tags(&game.headers);
                    let mut seen = HashSet::new();
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
                        if !seen.insert(key) {
                            break;
                        }
                        let record = records.entry((key, code)).or_default();
                        record.key = key;
                        record.mv = code;
                        game_meta.add_to(record);
                        node = child;
                        plies += 1;
                        if plies >= options.max_plies {
                            break;
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!("position index skipped game {index}: {err:#}");
                }
            }

            games += 1;
            if games.saturating_sub(last_report) >= 1000 {
                progress(BuildProgress {
                    games,
                    edges: records.len(),
                    elapsed: started.elapsed(),
                });
                last_report = games;
            }
        }

        let mut records: Vec<EdgeRecord> = records.into_values().collect();
        records.sort_unstable_by(|a, b| a.key.cmp(&b.key).then(a.mv.cmp(&b.mv)));
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
            max_plies: options.max_plies,
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
        progress(BuildProgress {
            games,
            edges: records.len(),
            elapsed,
        });
        Ok(BuildReport {
            games,
            edges: records.len(),
            positions,
            elapsed,
            bytes,
        })
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
        let mut visitor = IndexVisitor::new(options, started, &mut progress);
        while reader
            .read_game(&mut visitor)
            .with_context(|| format!("indexing {}", source.display()))?
            .is_some()
        {}
        let (records, games) = visitor.finish();
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
            max_plies: options.max_plies,
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
        progress(BuildProgress {
            games,
            edges: records.len(),
            elapsed,
        });
        Ok(BuildReport {
            games,
            edges: records.len(),
            positions,
            elapsed,
            bytes,
        })
    }

    pub fn path(&self) -> &Path {
        &self.source
    }

    pub fn game_count(&self) -> usize {
        self.header.game_count as usize
    }

    pub fn edge_count(&self) -> usize {
        self.header.edge_count as usize
    }

    pub fn position_count(&self) -> usize {
        self.header.position_count as usize
    }

    pub fn build_time(&self) -> Duration {
        Duration::from_millis(self.header.build_millis)
    }

    pub fn file_len(&self) -> u64 {
        self.mmap.len() as u64
    }

    pub fn empty(&self) -> bool {
        self.header.edge_count == 0
    }

    /// Move records from `key`, in stored `(move code)` order.
    pub fn moves(&self, key: u64) -> Vec<MoveStat> {
        let start = self.lower_bound(key);
        let end = self.upper_bound(key);
        (start..end)
            .map(|index| {
                let record = self.edge_at(index);
                MoveStat {
                    key: record.key,
                    mv: record.mv,
                    games: record.games,
                    white: record.white,
                    draw: record.draw,
                    black: record.black,
                    elo_sum: record.elo_sum,
                    elo_count: record.elo_count,
                    first_date: record.first_date,
                    last_date: record.last_date,
                }
            })
            .collect()
    }

    /// Aggregated statistics for a position, or `None` if no indexed game
    /// reached it with a move available.
    pub fn stats(&self, key: u64) -> Option<PositionStats> {
        let start = self.lower_bound(key);
        let end = self.upper_bound(key);
        if start == end {
            return None;
        }
        let mut stats = PositionStats::default();
        for index in start..end {
            let record = self.edge_at(index);
            stats.games += record.games;
            stats.white += record.white;
            stats.draw += record.draw;
            stats.black += record.black;
            stats.elo_sum += record.elo_sum;
            stats.elo_count += record.elo_count;
            if record.first_date != 0
                && (stats.first_date == 0 || record.first_date < stats.first_date)
            {
                stats.first_date = record.first_date;
            }
            if record.last_date > stats.last_date {
                stats.last_date = record.last_date;
            }
        }
        Some(stats)
    }

    fn source_matches(&self, meta: &SourceMeta) -> bool {
        self.header.source_len == meta.len
            && self.header.source_mtime_secs == meta.mtime_secs
            && self.header.source_mtime_nanos == meta.mtime_nanos
    }

    fn edge_at(&self, index: usize) -> EdgeRecord {
        let start = HEADER_LEN + index * EDGE_SIZE;
        read_edge(&self.mmap[start..start + EDGE_SIZE])
    }

    fn lower_bound(&self, key: u64) -> usize {
        let mut lo = 0;
        let mut hi = self.edge_count();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.edge_at(mid).key < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn upper_bound(&self, key: u64) -> usize {
        let mut lo = 0;
        let mut hi = self.edge_count();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.edge_at(mid).key <= key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }
}

/// Stable Zobrist key used by the index. The same position always maps to the
/// same key across runs and index rebuilds.
pub fn position_key(pos: &Chess) -> u64 {
    pos.zobrist_hash::<Zobrist64>(EnPassantMode::Legal).0
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
        shakmaty::File::G
    } else {
        shakmaty::File::C
    };
    Square::from_coords(file, king.rank())
}

#[derive(Clone, Copy, Debug, Default)]
struct EdgeRecord {
    key: u64,
    mv: u32,
    games: u32,
    white: u32,
    draw: u32,
    black: u32,
    elo_sum: u64,
    elo_count: u32,
    first_date: u32,
    last_date: u32,
}

#[derive(Clone, Copy, Debug, Default)]
struct IndexHeader {
    source_len: u64,
    source_mtime_secs: u64,
    source_mtime_nanos: u32,
    max_plies: u32,
    edge_count: u64,
    game_count: u64,
    position_count: u64,
    build_millis: u64,
}

impl IndexHeader {
    fn to_bytes(self) -> [u8; HEADER_LEN] {
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

    fn parse(bytes: &[u8]) -> Result<Self> {
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
    let canonical = fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in canonical.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    cache_dir().join(format!("{hash:016x}.brettpos"))
}

fn cache_dir() -> PathBuf {
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

fn write_edge(bytes: &mut [u8; EDGE_SIZE], r: &EdgeRecord) {
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
}

fn read_edge(bytes: &[u8]) -> EdgeRecord {
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
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct GameMeta {
    outcome: Outcome,
    white_elo: Option<u32>,
    black_elo: Option<u32>,
    date: u32,
}

#[derive(Clone, Copy, Debug, Default)]
enum Outcome {
    White,
    Draw,
    Black,
    #[default]
    Unknown,
}

impl GameMeta {
    fn from_tags(tags: &[(String, String)]) -> Self {
        let mut meta = Self::default();
        for (key, value) in tags {
            if key.eq_ignore_ascii_case("Result") {
                meta.outcome = match value.as_str() {
                    "1-0" => Outcome::White,
                    "0-1" => Outcome::Black,
                    "1/2-1/2" => Outcome::Draw,
                    _ => Outcome::Unknown,
                };
            } else if key.eq_ignore_ascii_case("WhiteElo") {
                meta.white_elo = parse_elo(value);
            } else if key.eq_ignore_ascii_case("BlackElo") {
                meta.black_elo = parse_elo(value);
            } else if key.eq_ignore_ascii_case("Date") {
                meta.date = parse_date(value).unwrap_or(0);
            }
        }
        meta
    }

    fn add_to(&self, record: &mut EdgeRecord) {
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
    seen: HashSet<u64>,
    active: bool,
    plies: u32,
    moves: usize,
}

impl GameState {
    fn new(tags: &[(String, String)]) -> Self {
        let pos = tags
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("FEN"))
            .and_then(|(_, value)| Fen::from_ascii(value.as_bytes()).ok())
            .and_then(|fen| fen.into_position(CastlingMode::Standard).ok())
            .unwrap_or_default();
        Self {
            pos,
            meta: GameMeta::from_tags(tags),
            seen: HashSet::new(),
            active: true,
            plies: 0,
            moves: 0,
        }
    }
}

struct IndexVisitor<'a, F> {
    records: HashMap<(u64, u32), EdgeRecord>,
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
    fn new(options: BuildOptions, started: Instant, progress: &'a mut F) -> Self {
        Self {
            records: HashMap::new(),
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
            edges: self.records.len(),
            elapsed: self.started.elapsed(),
        });
        self.last_report = self.games;
    }

    fn finish(self) -> (Vec<EdgeRecord>, usize) {
        let mut records: Vec<EdgeRecord> = self.records.into_values().collect();
        records.sort_unstable_by(|a, b| a.key.cmp(&b.key).then(a.mv.cmp(&b.mv)));
        (records, self.games)
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
        name: &[u8],
        value: RawTag<'_>,
    ) -> ControlFlow<()> {
        tags.push((
            String::from_utf8_lossy(name).into_owned(),
            value.decode_utf8_lossy().into_owned(),
        ));
        ControlFlow::Continue(())
    }

    fn begin_movetext(&mut self, tags: Vec<(String, String)>) -> ControlFlow<(), GameState> {
        ControlFlow::Continue(GameState::new(&tags))
    }

    fn san(&mut self, state: &mut GameState, san_plus: SanPlus) -> ControlFlow<()> {
        if !state.active {
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
        if !state.seen.insert(key) {
            state.active = false;
            return ControlFlow::Continue(());
        }
        let after = match state.pos.clone().play(mv) {
            Ok(after) => after,
            Err(_) => {
                state.active = false;
                return ControlFlow::Continue(());
            }
        };
        let record = self.records.entry((key, code)).or_default();
        record.key = key;
        record.mv = code;
        state.meta.add_to(record);
        state.pos = after;
        state.plies += 1;
        state.moves += 1;
        if state.plies >= self.options.max_plies {
            state.active = false;
        }
        ControlFlow::Continue(())
    }

    fn end_game(&mut self, _state: GameState) {
        self.games += 1;
        if self.games.saturating_sub(self.last_report) >= 1000 {
            self.report();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position_key_is_stable() {
        assert_eq!(position_key(&Chess::default()), 0x463b_9618_1691_fc9c);
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
    fn dates_and_elo_are_best_effort() {
        assert_eq!(parse_date("2024.03.07"), Some(2024_0307));
        assert_eq!(parse_date("2024.??.??"), None);
        assert_eq!(parse_date("????.??.??"), None);
        assert_eq!(parse_elo("2750"), Some(2750));
        assert_eq!(parse_elo("?"), None);
    }
}
