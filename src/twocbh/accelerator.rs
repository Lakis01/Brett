//! Reverse-engineered `.2lgd` classic sidecar reader and builder.
//!
//! Verified binary layout (do not guess):
//!
//! - File size is `12 + block_count * 512`.
//! - Header (12 bytes): `version` little-endian `u32` (`0x100`),
//!   `block_count` little-endian `u32`, reserved little-endian `u32` (0).
//! - Entity directory: `block_id == entity_id` (1-based `.2lid` id). Read 512
//!   bytes at `12 + entity_id * 512`. Bytes `0..256` hold four role slots with
//!   `role = Player 1, Event 2, Source 3, Annotator 4`:
//!   `start = le_u64(blk[role * 8 .. +8])`,
//!   `end = le_u64(blk[80 + role * 8 .. +8])`,
//!   `count = le_u32(blk[160 + role * 4 .. +4])`.
//! - `RANGE_FLAG = 1 << 61`, `ID_MASK = (1 << 61) - 1`.
//! - If `count == 0` or `start == u64::MAX` the list is empty.
//! - If `start & RANGE_FLAG != 0` the entry is an inline range covering
//!   `(start & ID_MASK) ..= (end & ID_MASK)` as `u32` record ids.
//! - Otherwise it is a linked list: `cur = start`, loop reading 512 bytes at
//!   `12 + cur * 512` with `next = le_u64(b[256..264])`,
//!   `n = le_u64(b[264..272])` as `usize` (at most 30) and
//!   `vals = b[272 .. 272 + n * 8]`; iterate with `i = 0`: if
//!   `vals[i] & RANGE_FLAG != 0` expand
//!   `(vals[i] & MASK) ..= (vals[i + 1] & MASK)` and `i += 2`, else push
//!   `vals[i]` as `u32` and `i += 1`. Stop when `cur == end` or
//!   `cur == u64::MAX`; otherwise `cur = next`. Loops are guarded with a
//!   visited set and a cap of `block_count + 1` blocks.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use memmap2::Mmap;

use super::TwoCbhFile;

pub const LGD_VERSION: u32 = 0x100;
pub const LGD_HEADER_LEN: usize = 12;
pub const LGD_BLOCK_LEN: usize = 512;
pub const LGD_MAX_VALUES_PER_BLOCK: usize = 30;
pub const RANGE_FLAG: u64 = 1 << 61;
pub const INLINE_PAIR_FLAG: u64 = 1 << 62;
pub const ID_MASK: u64 = (1 << 61) - 1;

/// Role ids used by the `.2lgd` directory slots.
pub const ROLE_PLAYER: usize = 1;
pub const ROLE_EVENT: usize = 2;
pub const ROLE_SOURCE: usize = 3;
pub const ROLE_ANNOTATOR: usize = 4;

/// Offsets of `.2cbh` record fields holding `.2lid` entity ids.
const OFF_WHITE: usize = 0x18;
const OFF_BLACK: usize = 0x20;
const OFF_EVENT: usize = 0x28;
const OFF_ANNOTATOR: usize = 0x30;
const OFF_SOURCE: usize = 0x38;

/// One decoded directory entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DirEntry {
    start: u64,
    end: u64,
    count: u32,
}

/// `(start, end, count)` of an inline directory entry.
type InlineEntry = (u64, u64, u32);
/// `(first_block, last_block, count, overflow_chunks)` of a chained entry.
type ChainedEntry = (u64, u64, u32, Vec<Vec<u64>>);

/// A validated `.2lgd` sidecar backed by a memory map (zero heap copy).
#[derive(Clone, Debug)]
pub struct TwoCbhAccelerator {
    path: PathBuf,
    backing: Arc<Backing>,
    block_count: usize,
}

#[derive(Debug)]
enum Backing {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl Backing {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Mapped(mmap) => mmap,
            Self::Owned(vec) => vec,
        }
    }
}

impl TwoCbhAccelerator {
    /// Companion `.2lgd` path for a `.2cbh` path or bare stem.
    pub fn path_for(cbh_path: &Path) -> PathBuf {
        let stem = match cbh_path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("2cbh") => cbh_path.with_extension(""),
            _ => cbh_path.to_path_buf(),
        };
        let mut name = stem.as_os_str().to_owned();
        name.push(".2lgd");
        PathBuf::from(name)
    }

    /// Open and validate an existing sidecar via `mmap` (no 2.2 GB heap
    /// copy); falls back to an owned read only when mapping fails.
    pub fn open(path: &Path) -> Result<Self> {
        let started = Instant::now();
        let mapped = File::open(path)
            .with_context(|| format!("opening 2LGD sidecar {}", path.display()))
            .and_then(|file| {
                // SAFETY: read-only map; the sidecar is treated as immutable
                // while open.
                unsafe { Mmap::map(&file) }
                    .with_context(|| format!("mapping 2LGD sidecar {}", path.display()))
            });
        let backing = match mapped {
            Ok(mmap) => {
                tracing::debug!(
                    "2LGD mmap {} ({:.1} MiB) in {:.2}s",
                    path.display(),
                    mmap.len() as f64 / (1024.0 * 1024.0),
                    started.elapsed().as_secs_f64()
                );
                Backing::Mapped(mmap)
            }
            Err(err) => {
                let not_found = err.chain().any(|cause| {
                    cause
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|io_err| io_err.kind() == std::io::ErrorKind::NotFound)
                });
                if not_found {
                    tracing::debug!("2LGD sidecar not present at {}; falling back to read", path.display());
                } else {
                    tracing::warn!(
                        "2LGD mmap failed for {} ({err:#}); falling back to read",
                        path.display()
                    );
                }
                let bytes = fs::read(path)
                    .with_context(|| format!("reading 2LGD sidecar {}", path.display()))?;
                return Self::from_bytes(path.to_path_buf(), bytes);
            }
        };
        let elapsed = started.elapsed();
        if elapsed.as_millis() > 100 {
            tracing::warn!(
                "slow 2LGD open {} took {:.2}s",
                path.display(),
                elapsed.as_secs_f64()
            );
        } else {
            tracing::info!(
                "2LGD open {} took {:.2}s",
                path.display(),
                elapsed.as_secs_f64()
            );
        }
        Self::from_backing(path.to_path_buf(), backing)
    }

    fn from_bytes(path: PathBuf, bytes: Vec<u8>) -> Result<Self> {
        Self::from_backing(path, Backing::Owned(bytes))
    }

    fn from_backing(path: PathBuf, backing: Backing) -> Result<Self> {
        let bytes = backing.as_slice();
        if bytes.len() < LGD_HEADER_LEN {
            bail!("{} is shorter than a 2LGD header", path.display());
        }
        let version = le_u32(&bytes[0..4]);
        if version != LGD_VERSION {
            bail!(
                "{} uses 2LGD version {version:#x}; only 0x100 is decoded",
                path.display()
            );
        }
        let block_count = le_u32(&bytes[4..8]) as usize;
        let reserved = le_u32(&bytes[8..12]);
        if reserved != 0 {
            bail!("{} has reserved value {reserved}", path.display());
        }
        if bytes.len() != LGD_HEADER_LEN + block_count * LGD_BLOCK_LEN {
            bail!(
                "{} is {} bytes but 12 + {block_count}*512 must hold",
                path.display(),
                bytes.len()
            );
        }
        Ok(Self {
            path,
            backing: Arc::new(backing),
            block_count,
        })
    }

    fn bytes(&self) -> &[u8] {
        self.backing.as_slice()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn block_count(&self) -> usize {
        self.block_count
    }

    fn block(&self, id: u64) -> Result<&[u8]> {
        let id = id as usize;
        if id >= self.block_count {
            bail!("2LGD block {id} is outside 0..{}", self.block_count);
        }
        let at = LGD_HEADER_LEN + id * LGD_BLOCK_LEN;
        self.bytes()
            .get(at..at + LGD_BLOCK_LEN)
            .with_context(|| format!("2LGD block {id} is truncated"))
    }

    fn entry(&self, entity_id: u32, role: usize) -> Result<DirEntry> {
        if !(1..=4).contains(&role) {
            bail!("2LGD role {role} is outside 1..=4");
        }
        let block = self.block(u64::from(entity_id))?;
        let count_u64 = le_u64(&block[160 + role * 8..160 + role * 8 + 8]);
        let count = if count_u64 != 0 {
            u32::try_from(count_u64).unwrap_or(u32::MAX)
        } else {
            le_u32(&block[160 + role * 4..160 + role * 4 + 4])
        };
        Ok(DirEntry {
            start: le_u64(&block[role * 8..role * 8 + 8]),
            end: le_u64(&block[80 + role * 8..80 + role * 8 + 8]),
            count,
        })
    }

    /// Decode the posting list for one `(entity_id, role)` pair as game
    /// record ids.
    pub fn games_for(&self, entity_id: u32, role: usize) -> Result<Vec<u32>> {
        if entity_id as usize >= self.block_count {
            return Ok(Vec::new());
        }
        let entry = self.entry(entity_id, role)?;
        if entry.count == 0 || entry.start == u64::MAX {
            return Ok(Vec::new());
        }
        if entry.start & INLINE_PAIR_FLAG != 0 {
            let first = (entry.start & ID_MASK) as u32;
            if entry.count == 1 || entry.end == u64::MAX {
                return Ok(vec![first]);
            }
            let second = (entry.end & ID_MASK) as u32;
            return Ok(vec![first, second]);
        }
        if entry.start & RANGE_FLAG != 0 {
            let first = (entry.start & ID_MASK) as u32;
            let last = (entry.end & ID_MASK) as u32;
            if last < first {
                bail!("2LGD inline range {first}..={last} is inverted");
            }
            if u64::from(entry.count) != u64::from(last - first) + 1 {
                bail!(
                    "2LGD inline range {first}..={last} disagrees with count {}",
                    entry.count
                );
            }
            return Ok((first..=last).collect());
        }
        let mut out = Vec::with_capacity(entry.count as usize);
        let mut seen = HashSet::new();
        let mut cur = entry.start;
        for _ in 0..self.block_count + 1 {
            if !seen.insert(cur) {
                bail!("2LGD posting list loops at block {cur}");
            }
            let block = self.block(cur)?;
            let next = le_u64(&block[256..264]);
            let n = le_u64(&block[264..272]) as usize;
            if n > LGD_MAX_VALUES_PER_BLOCK {
                bail!("2LGD block {cur} holds {n} values, at most 30");
            }
            let mut vals = Vec::with_capacity(n);
            for i in 0..n {
                let at = 272 + i * 8;
                vals.push(le_u64(&block[at..at + 8]));
            }
            let mut i = 0;
            while i < vals.len() {
                if vals[i] & RANGE_FLAG != 0 {
                    let Some(&second) = vals.get(i + 1) else {
                        bail!("2LGD block {cur} ends mid-range");
                    };
                    let first = (vals[i] & ID_MASK) as u32;
                    let last = (second & ID_MASK) as u32;
                    if last < first {
                        bail!("2LGD range {first}..={last} is inverted");
                    }
                    out.extend(first..=last);
                    i += 2;
                } else {
                    out.push(vals[i] as u32);
                    i += 1;
                }
            }
            if cur == entry.end || cur == u64::MAX {
                break;
            }
            if next == u64::MAX {
                break;
            }
            cur = next;
        }
        if out.len() > entry.count as usize {
            bail!(
                "2LGD posting list decoded {} ids but count is {}",
                out.len(),
                entry.count
            );
        }
        Ok(out)
    }

    pub fn player_games(&self, entity_id: u32) -> Result<Vec<u32>> {
        self.games_for(entity_id, ROLE_PLAYER)
    }

    pub fn event_games(&self, entity_id: u32) -> Result<Vec<u32>> {
        self.games_for(entity_id, ROLE_EVENT)
    }

    pub fn source_games(&self, entity_id: u32) -> Result<Vec<u32>> {
        self.games_for(entity_id, ROLE_SOURCE)
    }

    pub fn annotator_games(&self, entity_id: u32) -> Result<Vec<u32>> {
        self.games_for(entity_id, ROLE_ANNOTATOR)
    }

    /// Build a sidecar for `cbh` with one scan of `1..slot_count` and write
    /// it to `path` using exactly the layout above.
    pub fn build_to(cbh: &TwoCbhFile, path: &Path) -> Result<()> {
        let build_started = Instant::now();
        let mut player: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut event: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut source: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut annotator: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut max_entity = 0u32;

        let entity_limit = super::NameBase::open(&cbh.lid_path)
            .ok()
            .map(|n| n.entity_count() as u32)
            .filter(|&c| c > 0);
        let valid_entity = |record: &[u8], off: usize| -> Option<u32> {
            let lo = le_u32(&record[off..off + 4]);
            let hi = le_u32(&record[off + 4..off + 8]);
            if hi != 0 || lo == u32::MAX || entity_limit.is_some_and(|limit| lo >= limit) {
                None
            } else {
                Some(lo)
            }
        };

        for id in 1..cbh.slot_count() as u32 {
            let Ok(record) = cbh.record(id) else {
                continue;
            };
            if record[0] & 1 == 0 || record[0] & 2 != 0 || record[0] & 0x80 != 0 {
                continue;
            }
            if record.len() < OFF_SOURCE + 8 {
                continue;
            }
            let white = valid_entity(record, OFF_WHITE);
            let black = valid_entity(record, OFF_BLACK);
            let event_id = valid_entity(record, OFF_EVENT);
            let annot_id = valid_entity(record, OFF_ANNOTATOR);
            let source_id = valid_entity(record, OFF_SOURCE);
            for entity in [white, black, event_id, annot_id, source_id].into_iter().flatten() {
                max_entity = max_entity.max(entity);
            }
            if let Some(w) = white {
                player.entry(w).or_default().push(id);
            }
            if let Some(b) = black
                && Some(b) != white
            {
                player.entry(b).or_default().push(id);
            }
            if let Some(e) = event_id {
                event.entry(e).or_default().push(id);
            }
            if let Some(s) = source_id {
                source.entry(s).or_default().push(id);
            }
            if let Some(a) = annot_id {
                annotator.entry(a).or_default().push(id);
            }
        }

        // Compress every posting list to RLE tokens. A run of two or more
        // consecutive ids becomes two flagged words; singletons stay raw.
        let compress = |ids: &[u32]| -> Vec<u64> {
            let mut tokens = Vec::new();
            let mut run_start = 0usize;
            let mut i = 1usize;
            while i <= ids.len() {
                let continues = i < ids.len() && ids[i] == ids[i - 1] + 1;
                if continues {
                    i += 1;
                    continue;
                }
                let run = &ids[run_start..i];
                if run.len() >= 2 {
                    tokens.push(RANGE_FLAG | u64::from(run[0]));
                    tokens.push(RANGE_FLAG | u64::from(run[run.len() - 1]));
                } else if let &[single] = run {
                    tokens.push(u64::from(single));
                }
                run_start = i;
                i += 1;
            }
            tokens
        };

        // Directory blocks cover 0..=max_entity; overflow blocks follow.
        let dir_blocks = max_entity as usize + 1;
        let mut overflow_needed = 0usize;
        let mut inline: BTreeMap<(u32, usize), InlineEntry> = BTreeMap::new();
        let mut chained: BTreeMap<(u32, usize), ChainedEntry> = BTreeMap::new();

        let plan = |entity: u32,
                    role: usize,
                    ids: &[u32],
                    inline: &mut BTreeMap<(u32, usize), InlineEntry>,
                    chained: &mut BTreeMap<(u32, usize), ChainedEntry>,
                    overflow_needed: &mut usize| {
            if ids.is_empty() {
                return;
            }
            let tokens = compress(ids);
            // A single contiguous run (one or two flagged words) fits inline.
            let is_single_range = tokens.len() == 2
                && tokens.iter().all(|token| token & RANGE_FLAG != 0)
                || tokens.len() == 1 && ids.len() == 1;
            if is_single_range {
                let (first, last) = if tokens.len() == 2 {
                    (tokens[0], tokens[1] & ID_MASK)
                } else {
                    (RANGE_FLAG | tokens[0], tokens[0])
                };
                inline.insert((entity, role), (first, last, ids.len() as u32));
            } else {
                let chunks: Vec<Vec<u64>> = tokens
                    .chunks(LGD_MAX_VALUES_PER_BLOCK)
                    .map(|chunk| chunk.to_vec())
                    .collect();
                *overflow_needed += chunks.len();
                chained.insert((entity, role), (0, 0, ids.len() as u32, chunks));
            }
        };

        for (entity, ids) in &player {
            plan(
                *entity,
                ROLE_PLAYER,
                ids,
                &mut inline,
                &mut chained,
                &mut overflow_needed,
            );
        }
        for (entity, ids) in &event {
            plan(
                *entity,
                ROLE_EVENT,
                ids,
                &mut inline,
                &mut chained,
                &mut overflow_needed,
            );
        }
        for (entity, ids) in &source {
            plan(
                *entity,
                ROLE_SOURCE,
                ids,
                &mut inline,
                &mut chained,
                &mut overflow_needed,
            );
        }
        for (entity, ids) in &annotator {
            plan(
                *entity,
                ROLE_ANNOTATOR,
                ids,
                &mut inline,
                &mut chained,
                &mut overflow_needed,
            );
        }

        let block_count = dir_blocks + overflow_needed;
        let mut file = vec![0u8; LGD_HEADER_LEN + block_count * LGD_BLOCK_LEN];
        file[0..4].copy_from_slice(&LGD_VERSION.to_le_bytes());
        file[4..8].copy_from_slice(&(block_count as u32).to_le_bytes());

        // Default every directory slot to empty (start = MAX, count = 0).
        for entity in 0..dir_blocks {
            let at = LGD_HEADER_LEN + entity * LGD_BLOCK_LEN;
            for role in 1..=4 {
                file[at + role * 8..at + role * 8 + 8].copy_from_slice(&u64::MAX.to_le_bytes());
                file[at + 80 + role * 8..at + 80 + role * 8 + 8]
                    .copy_from_slice(&u64::MAX.to_le_bytes());
            }
        }

        for ((entity, role), (start, end, count)) in &inline {
            let at = LGD_HEADER_LEN + (*entity as usize) * LGD_BLOCK_LEN;
            file[at + role * 8..at + role * 8 + 8].copy_from_slice(&start.to_le_bytes());
            file[at + 80 + role * 8..at + 80 + role * 8 + 8].copy_from_slice(&end.to_le_bytes());
            file[at + 160 + role * 8..at + 160 + role * 8 + 8]
                .copy_from_slice(&u64::from(*count).to_le_bytes());
        }

        let mut next_block = dir_blocks;
        let mut chained_keys: Vec<(u32, usize)> = chained.keys().copied().collect();
        chained_keys.sort();
        for key in chained_keys {
            let (entity, role) = key;
            let (_, _, count, chunks) = chained.remove(&key).expect("planned list");
            let first = next_block as u64;
            let last = (next_block + chunks.len() - 1) as u64;
            let at = LGD_HEADER_LEN + (entity as usize) * LGD_BLOCK_LEN;
            file[at + role * 8..at + role * 8 + 8].copy_from_slice(&first.to_le_bytes());
            file[at + 80 + role * 8..at + 80 + role * 8 + 8].copy_from_slice(&last.to_le_bytes());
            file[at + 160 + role * 8..at + 160 + role * 8 + 8]
                .copy_from_slice(&u64::from(count).to_le_bytes());
            for (i, chunk) in chunks.iter().enumerate() {
                let block_id = next_block + i;
                let bat = LGD_HEADER_LEN + block_id * LGD_BLOCK_LEN;
                let next = if i + 1 < chunks.len() {
                    (block_id + 1) as u64
                } else {
                    u64::MAX
                };
                file[bat + 256..bat + 264].copy_from_slice(&next.to_le_bytes());
                file[bat + 264..bat + 272].copy_from_slice(&(chunk.len() as u64).to_le_bytes());
                for (j, value) in chunk.iter().enumerate() {
                    let vat = bat + 272 + j * 8;
                    file[vat..vat + 8].copy_from_slice(&value.to_le_bytes());
                }
            }
            next_block += chunks.len();
        }

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(path, &file)
            .with_context(|| format!("writing 2LGD sidecar {}", path.display()))?;
        let elapsed = build_started.elapsed();
        tracing::info!(
            "2LGD build_to {} took {:.2}s ({block_count} blocks)",
            path.display(),
            elapsed.as_secs_f64()
        );
        if elapsed.as_millis() > 100 {
            tracing::warn!(
                "slow 2LGD build_to {} took {:.2}s",
                path.display(),
                elapsed.as_secs_f64()
            );
        }
        Ok(())
    }
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::parse_pgn;

    fn test_game(white: &str, black: &str, event: &str) -> crate::domain::Game {
        let pgn = format!(
            "[Event \"{event}\"]\n[Site \"Testville\"]\n[Date \"2025.01.01\"]\n[Round \"1\"]\n\
             [White \"{white}\"]\n[Black \"{black}\"]\n[Result \"1-0\"]\n\n1. e4 e5 1-0\n"
        );
        parse_pgn(pgn.as_bytes()).unwrap().remove(0)
    }

    fn temp_stem(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "brett-2lgd-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.join("test.2cbh"), dir)
    }

    fn entity_of(record: &[u8], off: usize) -> u32 {
        u32::from_le_bytes([
            record[off],
            record[off + 1],
            record[off + 2],
            record[off + 3],
        ])
    }

    #[test]
    fn round_trips_inline_range_and_multiblock_rle() {
        let (path, _dir) = temp_stem("roundtrip");
        // Gukesh-like synthetic lists: alternate two whites so each player's
        // posting list is fragmented (35 non-consecutive ids -> 2 blocks),
        // while the shared event list stays one dense inline range.
        let mut games = Vec::new();
        for i in 0..70 {
            let white = if i % 2 == 0 {
                "Gukesh, D"
            } else {
                "Erigaisi, A"
            };
            games.push(Ok(test_game(white, "Opponent, O", "Candidates Debacle")));
        }
        crate::twocbh::writer::write_database_with_options(
            &path,
            crate::twocbh::writer::TwoCbhWriteOptions {
                allow_annotation_loss: true,
            },
            games,
        )
        .unwrap();
        let cbh = TwoCbhFile::open(&path).unwrap();
        let rec1 = cbh.record(1).unwrap().to_vec();
        let gukesh = entity_of(&rec1, OFF_WHITE);
        let event = entity_of(&rec1, OFF_EVENT);
        let source = entity_of(&rec1, OFF_SOURCE);

        let sidecar = TwoCbhAccelerator::path_for(&path);
        TwoCbhAccelerator::build_to(&cbh, &sidecar).unwrap();
        // Exact sizing: 12 + block_count * 512.
        let bytes = std::fs::read(&sidecar).unwrap();
        let block_count = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        assert_eq!(bytes.len(), LGD_HEADER_LEN + block_count * LGD_BLOCK_LEN);

        let acc = TwoCbhAccelerator::open(&sidecar).unwrap();
        // Fragmented player list decodes exactly (multi-block, no ranges).
        let gukesh_games = acc.player_games(gukesh).unwrap();
        let expected: Vec<u32> = (1..=70).step_by(2).collect();
        assert_eq!(gukesh_games, expected);
        // Dense event list decodes as an inline range.
        let event_games = acc.event_games(event).unwrap();
        assert_eq!(event_games, (1..=70).collect::<Vec<_>>());
        // Source role decodes too, and every id is a game record.
        let source_games = acc.source_games(source).unwrap();
        assert_eq!(source_games.len(), 70);
        for id in gukesh_games.iter().chain(event_games.iter()) {
            assert!(cbh.is_game(*id), "record {id} is not a game");
        }
        // Unknown entity is empty, not an error.
        assert!(acc.player_games(999_999).unwrap().is_empty());
    }

    #[test]
    fn linked_list_with_ranges_decodes() {
        // Hand-built file: entity 1, player role -> two overflow blocks.
        let block_count = 4u32;
        let mut file = vec![0u8; LGD_HEADER_LEN + 4 * LGD_BLOCK_LEN];
        file[0..4].copy_from_slice(&LGD_VERSION.to_le_bytes());
        file[4..8].copy_from_slice(&block_count.to_le_bytes());
        for entity in 0..4 {
            let at = LGD_HEADER_LEN + entity * LGD_BLOCK_LEN;
            for role in 1..=4 {
                file[at + role * 8..at + role * 8 + 8].copy_from_slice(&u64::MAX.to_le_bytes());
                file[at + 80 + role * 8..at + 80 + role * 8 + 8]
                    .copy_from_slice(&u64::MAX.to_le_bytes());
            }
        }
        // Directory: entity 1 player role -> blocks 2..=3, count 35.
        let at = LGD_HEADER_LEN + LGD_BLOCK_LEN;
        file[at + 8..at + 16].copy_from_slice(&2u64.to_le_bytes());
        file[at + 88..at + 96].copy_from_slice(&3u64.to_le_bytes());
        file[at + 164..at + 168].copy_from_slice(&35u32.to_le_bytes());
        // Block 2: next=3, n=2: [5, RANGE|10..20].
        let b2 = LGD_HEADER_LEN + 2 * LGD_BLOCK_LEN;
        file[b2 + 256..b2 + 264].copy_from_slice(&3u64.to_le_bytes());
        file[b2 + 264..b2 + 272].copy_from_slice(&3u64.to_le_bytes());
        file[b2 + 272..b2 + 280].copy_from_slice(&5u64.to_le_bytes());
        file[b2 + 280..b2 + 288].copy_from_slice(&(RANGE_FLAG | 10).to_le_bytes());
        file[b2 + 288..b2 + 296].copy_from_slice(&(RANGE_FLAG | 20).to_le_bytes());
        // Block 3: next=MAX, n=1: [42].
        let b3 = LGD_HEADER_LEN + 3 * LGD_BLOCK_LEN;
        file[b3 + 256..b3 + 264].copy_from_slice(&u64::MAX.to_le_bytes());
        file[b3 + 264..b3 + 272].copy_from_slice(&1u64.to_le_bytes());
        file[b3 + 272..b3 + 280].copy_from_slice(&42u64.to_le_bytes());
        let dir = std::env::temp_dir().join(format!(
            "brett-2lgd-{}-{}.2lgd",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&dir, &file).unwrap();
        let acc = TwoCbhAccelerator::open(&dir).unwrap();
        let games = acc.player_games(1).unwrap();
        let mut expected = vec![5];
        expected.extend(10..=20);
        expected.push(42);
        assert_eq!(games, expected);
        // Empty slot: entity 3 has no postings.
        assert!(acc.event_games(3).unwrap().is_empty());
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn open_uses_mmap_backing_and_clones_cheaply() {
        let (path, _dir) = temp_stem("mmap-open");
        let games = vec![Ok(test_game("Alpha, A", "Beta, B", "Mmap Open"))];
        crate::twocbh::writer::write_database_with_options(
            &path,
            crate::twocbh::writer::TwoCbhWriteOptions {
                allow_annotation_loss: true,
            },
            games,
        )
        .unwrap();
        let cbh = TwoCbhFile::open(&path).unwrap();
        let sidecar = TwoCbhAccelerator::path_for(&path);
        TwoCbhAccelerator::build_to(&cbh, &sidecar).unwrap();
        let acc = TwoCbhAccelerator::open(&sidecar).unwrap();
        assert!(acc.block_count() > 0);
        // Mmap-backed: byte length matches the file size exactly without a
        // heap copy per open, and clones share the mapping.
        let len = std::fs::metadata(&sidecar).unwrap().len() as usize;
        assert_eq!(acc.bytes().len(), len);
        let cloned = acc.clone();
        assert_eq!(cloned.bytes().len(), len);
        assert_eq!(cloned.block_count(), acc.block_count());
    }

    #[test]
    fn build_to_ignores_nonzero_high_word_in_annotator_slot() {
        let (path, dir) = temp_stem("high-annot");
        let games = vec![Ok(test_game("Alpha, A", "Beta, B", "High Annot Test"))];
        crate::twocbh::writer::write_database_with_options(
            &path,
            crate::twocbh::writer::TwoCbhWriteOptions {
                allow_annotation_loss: true,
            },
            games,
        )
        .unwrap();

        // Simulate a ChessBase 64-bit packed value at OFF_ANNOTATOR (`0x30..0x38`).
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            file.seek(SeekFrom::Start(192 + OFF_ANNOTATOR as u64)).unwrap();
            file.write_all(&0x8a27_2176u32.to_le_bytes()).unwrap();
            file.write_all(&0x0007_f181u32.to_le_bytes()).unwrap();
        }

        let cbh = TwoCbhFile::open(&path).unwrap();
        let sidecar = TwoCbhAccelerator::path_for(&path);
        TwoCbhAccelerator::build_to(&cbh, &sidecar).unwrap();
        let acc = TwoCbhAccelerator::open(&sidecar).unwrap();
        assert!(acc.block_count() < 16);
        let _ = std::fs::remove_dir_all(dir);
    }
}
