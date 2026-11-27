//! Native reader for ChessBase's current multi-file database format.
//!
//! A 2CBH database is a set of files sharing one base name:
//!
//! | File | Contents |
//! | --- | --- |
//! | `.2cbh` | 192-byte header records, one per game or text slot |
//! | `.2cbg` | Move records |
//! | `.2cba` | Annotation records (not decoded yet) |
//! | `.2lid` | Inline player, event, and source names |
//!
//! The container, header fields, name blocks, move-token table, standard
//! moves, Chess960 starts, castling, promotions, en passant, and nested
//! variations are decoded natively.  Annotation items are intentionally left
//! opaque until their per-item structure is mapped; no comment or NAG is
//! fabricated.
//!
//! [`TwoCbhFile`] is the low-level, targeted reader.  It validates the
//! container and loads one record at a time, so the full 11.7-million-game
//! Mega database can be inspected without indexing it.  [`TwoCbhLibrary`]
//! adds the game list used by the application and refuses to index databases
//! above [`MAX_INDEXED_GAMES`].

pub mod accelerator;
pub mod fast_index;
mod moves;
mod tokens;
pub mod tournaments;
pub mod writer;

pub use accelerator::TwoCbhAccelerator;
pub use tournaments::{MegaTournamentCategory, MegaTournamentEntry, parse_mega_tournament_index};

#[cfg(test)]
mod tests;

pub use writer::{
    ALL_EXTENSIONS, CORE_EXTENSIONS, DERIVED_EXTENSIONS, TwoCbhWriteOptions, TwoCbhWriteReport,
    core_companion_files, existing_companion_files, has_unsupported_annotations,
    merge_updates_into_2cbh, write_database, write_database_with_options,
};

use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use memmap2::Mmap;

use crate::domain::Game;
use crate::library::{GameLibrary, IndexedGame, SharedStr, SourceMeta, StringPool};

const RECORD_SIZE: usize = 192;
const CBG_HEADER: u64 = 12;
const CBG_MAGIC: [u8; 8] = [0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11];
const LID_FIRST: usize = 0xB8;
const LID_STRIDE: usize = 0x108A;
const LID_EVENT: usize = 0x400;
const LID_SOURCE: usize = 0x860;
pub(crate) const START_ID_BASE: u32 = 0xFC40;
const SEARCH_CACHE_MAX_GAMES: usize = 1_000_000;

/// Upper safety bound on 2CBH slots to prevent integer overflow or runaway allocation.
/// Mega Database 2026 has ~11,743,084 records and is indexed via lightweight/virtual slots.
pub const MAX_INDEXED_GAMES: usize = 50_000_000;

/// One game slot's location and state.
#[derive(Clone, Debug)]
struct Entry {
    id: u32,
    deleted: bool,
}

/// Per-node annotations stored in an optional `<stem>.brettann` gzip-JSON
/// sidecar when writing Brett personal `.2cbh` databases.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BrettNodeAnnotation {
    pub path: Vec<u16>,
    pub comment: String,
    pub nags: Vec<u8>,
    pub clock_centis: Option<u64>,
    pub eval_cp: Option<i32>,
    pub eval_mate: Option<i32>,
    pub shapes: Vec<String>,
}

impl BrettNodeAnnotation {
    fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        if !self.path.is_empty() {
            obj.insert(
                "p".to_string(),
                serde_json::Value::Array(self.path.iter().map(|&v| serde_json::json!(v)).collect()),
            );
        }
        if !self.comment.is_empty() {
            obj.insert("c".to_string(), serde_json::json!(self.comment));
        }
        if !self.nags.is_empty() {
            obj.insert(
                "n".to_string(),
                serde_json::Value::Array(self.nags.iter().map(|&v| serde_json::json!(v)).collect()),
            );
        }
        if let Some(clk) = self.clock_centis {
            obj.insert("k".to_string(), serde_json::json!(clk));
        }
        if let Some(cp) = self.eval_cp {
            obj.insert("ec".to_string(), serde_json::json!(cp));
        }
        if let Some(mate) = self.eval_mate {
            obj.insert("em".to_string(), serde_json::json!(mate));
        }
        if !self.shapes.is_empty() {
            obj.insert("s".to_string(), serde_json::json!(self.shapes));
        }
        serde_json::Value::Object(obj)
    }

    fn from_json(v: &serde_json::Value) -> Option<Self> {
        let obj = v.as_object()?;
        let path = obj
            .get("p")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_u64().and_then(|u| u16::try_from(u).ok()))
                    .collect()
            })
            .unwrap_or_default();
        let comment = obj
            .get("c")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let nags = obj
            .get("n")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_u64().and_then(|u| u8::try_from(u).ok()))
                    .collect()
            })
            .unwrap_or_default();
        let clock_centis = obj.get("k").and_then(serde_json::Value::as_u64);
        let eval_cp = obj
            .get("ec")
            .and_then(serde_json::Value::as_i64)
            .and_then(|x| i32::try_from(x).ok());
        let eval_mate = obj
            .get("em")
            .and_then(serde_json::Value::as_i64)
            .and_then(|x| i32::try_from(x).ok());
        let shapes = obj
            .get("s")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            path,
            comment,
            nags,
            clock_centis,
            eval_cp,
            eval_mate,
            shapes,
        })
    }
}

pub(crate) type BrettAnnotationMap = std::collections::HashMap<u32, Vec<BrettNodeAnnotation>>;

pub(crate) fn serialize_brett_annotations(map: &BrettAnnotationMap) -> Vec<u8> {
    let mut root = serde_json::Map::new();
    for (&id, anns) in map {
        let items: Vec<serde_json::Value> = anns.iter().map(BrettNodeAnnotation::to_json).collect();
        root.insert(id.to_string(), serde_json::Value::Array(items));
    }
    serde_json::to_vec(&serde_json::Value::Object(root)).unwrap_or_default()
}

pub(crate) fn collect_brett_annotations(game: &Game) -> Vec<BrettNodeAnnotation> {
    fn visit(
        nodes: &[crate::domain::Node],
        node_idx: usize,
        path: &mut Vec<u16>,
        out: &mut Vec<BrettNodeAnnotation>,
    ) {
        let Some(node) = nodes.get(node_idx) else {
            return;
        };
        if !node.comment.is_empty()
            || !node.nags.is_empty()
            || node.clock.is_some()
            || node.eval.is_some()
            || !node.shapes.is_empty()
        {
            out.push(BrettNodeAnnotation {
                path: path.clone(),
                comment: node.comment.clone(),
                nags: node.nags.clone(),
                clock_centis: node.clock.map(|c| c.centis),
                eval_cp: node.eval.and_then(|e| e.cp),
                eval_mate: node.eval.and_then(|e| e.mate),
                shapes: node.shapes.iter().map(|s| s.token()).collect(),
            });
        }
        for (child_pos, &child_idx) in node.children.iter().enumerate() {
            if let Ok(step) = u16::try_from(child_pos) {
                path.push(step);
                visit(nodes, child_idx, path, out);
                path.pop();
            }
        }
    }
    let mut out = Vec::new();
    let mut path = Vec::new();
    visit(&game.nodes, 0, &mut path, &mut out);
    out
}

fn load_brettann_sidecar(path: &Path) -> Option<Arc<BrettAnnotationMap>> {
    if !path.is_file() {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let val: serde_json::Value = serde_json::from_reader(decoder).ok()?;
    let obj = val.as_object()?;
    let mut map = BrettAnnotationMap::new();
    for (k, v) in obj {
        let Ok(id) = k.parse::<u32>() else {
            continue;
        };
        let Some(arr) = v.as_array() else {
            continue;
        };
        let items: Vec<BrettNodeAnnotation> = arr
            .iter()
            .filter_map(BrettNodeAnnotation::from_json)
            .collect();
        if !items.is_empty() {
            map.insert(id, items);
        }
    }
    Some(Arc::new(map))
}

fn apply_brett_annotations(game: &mut Game, anns: &[BrettNodeAnnotation]) {
    for ann in anns {
        let mut cur = 0usize;
        let mut valid = true;
        for &step in &ann.path {
            if let Some(&next) = game
                .nodes
                .get(cur)
                .and_then(|n| n.children.get(usize::from(step)))
            {
                cur = next;
            } else {
                valid = false;
                break;
            }
        }
        if !valid {
            continue;
        }
        let node = &mut game.nodes[cur];
        if !ann.comment.is_empty() {
            node.comment = ann.comment.clone();
        }
        if !ann.nags.is_empty() {
            node.nags = ann.nags.clone();
        }
        if let Some(centis) = ann.clock_centis {
            node.clock = Some(crate::domain::Clock { centis });
        }
        if ann.eval_cp.is_some() || ann.eval_mate.is_some() {
            node.eval = Some(crate::domain::PgnEval {
                cp: ann.eval_cp,
                mate: ann.eval_mate,
            });
        }
        for token in &ann.shapes {
            if let Some(shape) = crate::domain::Shape::parse_token(token)
                && !node.shapes.contains(&shape)
            {
                node.shapes.push(shape);
            }
        }
    }
}

/// A validated 2CBH container with targeted record access.
pub struct TwoCbhFile {
    cbh_path: PathBuf,
    cbg_path: PathBuf,
    cba_path: Option<PathBuf>,
    lid_path: PathBuf,
    cbh: Mmap,
    cbg: Mmap,
    count: u32,
    brett_annotations: Option<Arc<BrettAnnotationMap>>,
}

impl TwoCbhFile {
    /// Open and validate the container.  The path may name the `.2cbh`, a
    /// companion file, or the database's bare base name.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let supplied = path.as_ref();
        let stem = match supplied.extension().and_then(|ext| ext.to_str()) {
            Some(ext)
                if ext.eq_ignore_ascii_case("2cbh")
                    || ext.eq_ignore_ascii_case("2cbg")
                    || ext.eq_ignore_ascii_case("2cba")
                    || ext.eq_ignore_ascii_case("2lid") =>
            {
                supplied.with_extension("")
            }
            _ => supplied.to_path_buf(),
        };
        let cbh_path = sibling(&stem, ".2cbh");
        if !cbh_path.is_file() {
            bail!("2CBH header file {} is missing", cbh_path.display());
        }
        let cbg_path = sibling(&stem, ".2cbg");
        if !cbg_path.is_file() {
            bail!(
                "2CBH move file {} is missing (expected beside {})",
                cbg_path.display(),
                cbh_path.display()
            );
        }
        let cba_path = {
            let path = sibling(&stem, ".2cba");
            path.is_file().then_some(path)
        };
        let lid_path = sibling(&stem, ".2lid");

        let cbh = map_file(&cbh_path, "2CBH headers")?;
        if cbh.len() < RECORD_SIZE || cbh.len() % RECORD_SIZE != 0 {
            bail!(
                "{} is not a 2CBH header file: {} bytes is not 192 plus whole records",
                cbh_path.display(),
                cbh.len()
            );
        }
        let record_size = usize::from(le_u16(&cbh[0x0a..0x0c]));
        if record_size != RECORD_SIZE {
            bail!(
                "{} states a {}-byte record; 2CBH uses 192",
                cbh_path.display(),
                record_size
            );
        }
        let generation = cbh[8];
        if generation != 0x26 {
            bail!(
                "{} uses 2CBH generation {generation:#04x}; only 0x26 is decoded",
                cbh_path.display()
            );
        }
        let count = le_u32(&cbh[0x10..0x14]);
        if count == 0 || count as usize != cbh.len() / RECORD_SIZE {
            bail!(
                "{} states {count} records but contains {}",
                cbh_path.display(),
                cbh.len() / RECORD_SIZE
            );
        }

        let cbg = map_file(&cbg_path, "2CBH moves")?;
        validate_companion(&cbg_path, &cbg, "moves")?;
        if let Some(path) = &cba_path {
            let cba = map_file(path, "2CBH annotations")?;
            validate_companion(path, &cba, "annotations")?;
        }
        let brett_annotations = load_brettann_sidecar(&sibling(&stem, ".brettann"));

        Ok(Self {
            cbh_path,
            cbg_path,
            cba_path,
            lid_path,
            cbh,
            cbg,
            count,
            brett_annotations,
        })
    }

    /// The canonical `.2cbh` path.
    pub fn path(&self) -> &Path {
        &self.cbh_path
    }

    /// Number of slots, including text records and record zero.
    pub fn slot_count(&self) -> usize {
        self.count as usize
    }

    /// Number of game slots (record zero and text slots excluded).
    pub fn game_count(&self) -> usize {
        (1..self.count).filter(|&id| self.is_game(id)).count()
    }

    /// The 192-byte header record for a one-based id.
    pub fn record(&self, id: u32) -> Result<&[u8]> {
        if id == 0 || id >= self.count {
            bail!("2CBH record {id} is outside 1..{}", self.count);
        }
        let at = id as usize * RECORD_SIZE;
        self.cbh
            .get(at..at + RECORD_SIZE)
            .with_context(|| format!("2CBH record {id} is truncated"))
    }

    /// Whether a record slot is a game (rather than text or an unused slot).
    pub fn is_game(&self, id: u32) -> bool {
        self.record(id)
            .is_ok_and(|record| record[0] & 1 != 0 && record[0] & 2 == 0)
    }

    /// Whether a record slot is a text record (Mega index, reports, …).
    pub fn is_text(&self, id: u32) -> bool {
        self.record(id).is_ok_and(|record| record[0] & 2 != 0)
    }

    /// Event/title ids of a text record: `event_id` at `0x10` and `title_id`
    /// at `0x28`, per the verified 2CBH layout.
    pub fn text_ids(&self, id: u32) -> Result<(u32, u32)> {
        let record = self.record(id)?;
        if record[0] & 2 == 0 {
            anyhow::bail!("2CBH record {id} is not a text record");
        }
        Ok((le_u32(&record[0x10..0x14]), le_u32(&record[0x28..0x2c])))
    }

    /// Event (place, title) names for a text record.
    pub fn text_event(&self, id: u32) -> Result<(String, String)> {
        let (event_id, _) = self.text_ids(id)?;
        let names = NameBase::open(&self.lid_path)?;
        Ok(names.event(event_id))
    }

    /// Title string for a text record (`.2lid` single-string block at
    /// `0xE82`: `le_u32` length then bytes, shared UTF-8/CP1252 fallback).
    pub fn text_title(&self, id: u32) -> Result<String> {
        let (_, title_id) = self.text_ids(id)?;
        let names = NameBase::open(&self.lid_path)?;
        Ok(names.text_title(title_id))
    }

    /// Parsed guiding-text document for a text record.
    pub fn guiding_document(&self, id: u32) -> Result<crate::guiding::GuidingTextDocument> {
        let html = self.text_html(id)?;
        Ok(crate::guiding::parse_guiding_html(&html))
    }

    /// All one-based text record ids.
    pub fn text_record_ids(&self) -> Vec<u32> {
        (1..self.count).filter(|&id| self.is_text(id)).collect()
    }

    /// Decode a text record (such as the Mega tournament index in slot 1) as
    /// HTML. The `.2cbg` payload starts with the usual magic, a length word
    /// `a` at header bytes 8..12, and the content at `offset + 24`. The
    /// English HTML block is located by searching for `<!DOCTYPE` or `<html`
    /// and decoded with the shared UTF-8/Windows-1252 fallback. All bounds
    /// are validated; this never panics.
    pub fn text_html(&self, id: u32) -> Result<String> {
        let record = self.record(id)?;
        let offset = le_u64(&record[8..16]) as usize;
        let header = self
            .cbg
            .get(offset..offset.saturating_add(24))
            .with_context(|| format!("2CBG text record {id} header is truncated at {offset}"))?;
        if header.len() < 24 || header[..8] != CBG_MAGIC {
            bail!("2CBG text record {id} at {offset} has the wrong magic");
        }
        let a = le_u32(&header[8..12]) as usize;
        let content_start = offset
            .checked_add(24)
            .context("2CBG text offset overflow")?;
        let content_end = content_start
            .checked_add(a)
            .context("2CBG text size overflow")?;
        let bytes = self.cbg.get(content_start..content_end).with_context(|| {
            format!(
                "2CBG text record {id} runs past the end of {}",
                self.cbg_path.display()
            )
        })?;
        let lower = bytes
            .iter()
            .map(|byte| byte.to_ascii_lowercase())
            .collect::<Vec<u8>>();
        let mut start = 0usize;
        let mut found = false;
        for needle in [b"<!doctype".as_slice(), b"<html".as_slice()] {
            if let Some(at) = find_subslice(&lower, needle)
                && (!found || at < start)
            {
                start = at;
                found = true;
            }
        }
        let slice = if found { &bytes[start..] } else { bytes };
        Ok(decode_text(slice))
    }

    /// Read and validate one `.2cbg` record.
    pub fn content(&self, id: u32) -> Result<Content<'_>> {
        let record = self.record(id)?;
        let offset = le_u64(&record[8..16]);
        if offset < CBG_HEADER {
            bail!("2CBG record {id} points before the file header at {offset}");
        }
        let header = self
            .cbg
            .get(offset as usize..offset as usize + 34)
            .with_context(|| format!("2CBG record {id} header is truncated at {offset}"))?;
        if header[..8] != CBG_MAGIC {
            bail!("2CBG record {id} at {offset} has the wrong magic");
        }
        let a = le_u32(&header[8..12]) as usize;
        let b = le_u32(&header[12..16]) as usize;
        let total = 34usize
            .checked_add(a)
            .and_then(|size| size.checked_add(b))
            .context("2CBG record size overflow")?;
        let end = (offset as usize)
            .checked_add(total)
            .context("2CBG record offset overflow")?;
        if end > self.cbg.len() {
            bail!(
                "2CBG record {id} runs past the end of {}",
                self.cbg_path.display()
            );
        }
        if a < 2 {
            bail!("2CBG record {id} has a {a}-byte content area");
        }
        let content_start = offset as usize + 24;
        let content_end = content_start + a;
        if self.cbg.get(content_end..content_end + 2) != Some(&[0xff, 0xff]) {
            bail!("2CBG record {id} has no content terminator");
        }
        // The trailing `ffff` is the last token, not just a sentinel: it
        // terminates the final line.  `a` counts the kind word and the tokens
        // before that final terminator.
        let trailer = content_end + 2 + b;
        let stored_total = self
            .cbg
            .get(trailer..trailer + 8)
            .map(le_u64)
            .with_context(|| format!("2CBG record {id} trailer is truncated"))?;
        if stored_total != total as u64 {
            bail!("2CBG record {id} trailer is damaged");
        }
        let kind = le_u16(&self.cbg[content_start..content_start + 2]);
        let bytes = &self.cbg[content_start + 2..content_end + 2];
        if !bytes.len().is_multiple_of(2) {
            bail!("2CBG record {id} has an odd token-stream length");
        }
        Ok(Content { kind, bytes })
    }

    /// The `.2cba` offset from the game's header record.
    pub fn annotations_offset(&self, id: u32) -> Result<u64> {
        let record = self.record(id)?;
        Ok(le_u64(&record[16..24]))
    }

    /// Load one game without building a library index.  This is the path used
    /// for targeted reads from a database too large to index.
    pub fn load_game(&self, id: u32) -> Result<Game> {
        let record = self.record(id)?;
        let names = NameBase::open(&self.lid_path)?;
        let mut strings = StringPool::default();
        let headers = base_headers(record, &names, &mut strings)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let deleted = record[0] & 0x80 != 0;
        load_game(self, id, headers, deleted)
    }
}

/// The decoded content area of one `.2cbg` record.
pub struct Content<'a> {
    kind: u16,
    bytes: &'a [u8],
}

impl Content<'_> {
    /// Encoding kind: 1 for standard chess, 2 for Chess960.
    pub fn kind(&self) -> u16 {
        self.kind
    }

    /// Raw little-endian `.2cbg` token bytes (after the kind word, ending in `ff ff`).
    pub(crate) fn raw_bytes(&self) -> &[u8] {
        self.bytes
    }

    /// Little-endian token words, after the kind word.
    pub fn words(&self) -> Vec<u16> {
        self.bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect()
    }
}

/// An indexed 2CBH database.
pub struct TwoCbhLibrary {
    path: PathBuf,
    file: TwoCbhFile,
    names: Arc<NameBase>,
    entries: Vec<Entry>,
    games: Vec<IndexedGame>,
    file_len: u64,
    indexed_in: Duration,
    accelerator: Option<TwoCbhAccelerator>,
    tournament_cache: OnceLock<Option<Vec<MegaTournamentCategory>>>,
}

impl TwoCbhLibrary {
    /// Open a 2CBH database and index its game headers. The `.2lgd`
    /// sidecar is memory-mapped best-effort only; it is never built here so
    /// a 2.2 GB build cannot block the UI thread. Callers build it in the
    /// background via [`TwoCbhAccelerator::build_to`] + [`Self::attach_accelerator`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let started = Instant::now();
        let t_file = Instant::now();
        let file = TwoCbhFile::open(path)?;
        tracing::info!(
            "2CBH container open took {:.2}s",
            t_file.elapsed().as_secs_f64()
        );
        let slots = file.slot_count().saturating_sub(1);
        if slots > MAX_INDEXED_GAMES {
            bail!(
                "{} contains {slots} 2CBH slots; Brett indexes at most {MAX_INDEXED_GAMES}.",
                file.path().display()
            );
        }
        let t_names = Instant::now();
        let names = Arc::new(NameBase::open(&file.lid_path)?);
        tracing::info!(
            "2CBH names (.2lid) open took {:.2}s",
            t_names.elapsed().as_secs_f64()
        );
        let total = file.slot_count().saturating_sub(1);
        // Databases up to 250k games pre-build eager AST/strings; larger collections
        // (such as Mega Database with 11.75M+ games) use lazy headers on demand.
        let build_eager = total <= 250_000;
        let build_search = total <= SEARCH_CACHE_MAX_GAMES;
        let mut entries = Vec::with_capacity(total.min(12_000_000));
        let mut games = if build_eager {
            Vec::with_capacity(total)
        } else {
            Vec::new()
        };
        let mut strings = StringPool::default();
        let t_entries = Instant::now();
        for id in 1..file.count {
            let record = file.record(id)?;
            let kind = record[0];
            if kind & 1 == 0 || kind & 2 != 0 {
                continue;
            }
            entries.push(Entry {
                id,
                deleted: kind & 0x80 != 0,
            });
            if build_eager {
                let headers = base_headers(record, &names, &mut strings);
                games.push(IndexedGame::cbh(
                    headers,
                    id,
                    le_u64(&record[8..16]),
                    le_u64(&record[16..24]),
                    kind & 0x80 != 0,
                    build_search,
                ));
            }
        }

        let entries_elapsed = t_entries.elapsed();
        tracing::info!(
            "2CBH entries scan ({} entries) took {:.2}s",
            entries.len(),
            entries_elapsed.as_secs_f64()
        );
        if entries_elapsed.as_millis() > 100 {
            tracing::warn!(
                "slow 2CBH entries scan took {:.2}s",
                entries_elapsed.as_secs_f64()
            );
        }

        let mut file_len = file.cbh.len() as u64 + file.cbg.len() as u64;
        for path in file.cba_path.iter().chain(std::iter::once(&file.lid_path)) {
            file_len += fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        }

        // Best-effort `.2lgd` accelerator: memory-map when present. Never
        // build here — the caller builds in the background.
        let t_lgd = Instant::now();
        let lgd_path = TwoCbhAccelerator::path_for(&file.cbh_path);
        let accelerator = TwoCbhAccelerator::open(&lgd_path).ok();
        let lgd_elapsed = t_lgd.elapsed();
        if lgd_elapsed.as_millis() > 100 {
            tracing::warn!("slow 2LGD attach took {:.2}s", lgd_elapsed.as_secs_f64());
        } else {
            tracing::info!(
                "2LGD attach took {:.2}s (present={})",
                lgd_elapsed.as_secs_f64(),
                accelerator.is_some()
            );
        }
        let total_open = started.elapsed();
        if total_open.as_millis() > 100 {
            tracing::warn!(
                "slow TwoCbhLibrary::open took {:.2}s",
                total_open.as_secs_f64()
            );
        } else {
            tracing::info!("TwoCbhLibrary::open took {:.2}s", total_open.as_secs_f64());
        }

        Ok(Self {
            path: file.cbh_path.clone(),
            file,
            names,
            entries,
            games,
            file_len,
            indexed_in: started.elapsed(),
            accelerator,
            tournament_cache: OnceLock::new(),
        })
    }

    /// Companion `.2lgd` path for this database.
    pub fn lgd_path(&self) -> PathBuf {
        TwoCbhAccelerator::path_for(&self.file.cbh_path)
    }

    /// Attach a background-built accelerator.
    pub fn attach_accelerator(&mut self, acc: TwoCbhAccelerator) {
        self.accelerator = Some(acc);
    }

    /// Entity count bound from the `.2lid` (avoids scanning 4.3M overflow blocks).
    fn entity_bound(&self) -> usize {
        match &self.accelerator {
            Some(acc) => self.names.entity_count().min(acc.block_count()),
            None => 0,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    pub fn len(&self) -> usize {
        if !self.games.is_empty() {
            self.games.len()
        } else {
            self.entries.len()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
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

    pub fn filter(&self, needle: &str) -> Vec<usize> {
        if !self.games.is_empty() {
            crate::library::filter_games(&self.games, needle)
        } else {
            let needle = needle.trim().to_lowercase();
            if needle.is_empty() {
                return (0..self.entries.len()).collect();
            }
            (0..self.entries.len())
                .filter(|&index| {
                    let (white, black) = self.game_players(index);
                    white.to_lowercase().contains(&needle) || black.to_lowercase().contains(&needle)
                })
                .collect()
        }
    }

    /// Decode one game on demand.  A damaged move stream is reported in
    /// [`Game::errors`] while the readable part is returned; a damaged
    /// container record is an error.
    pub fn load(&self, index: usize) -> Result<Game> {
        let entry = self
            .entries
            .get(index)
            .with_context(|| format!("2CBH game index {index} is out of range"))?;
        let headers = if let Some(indexed) = self.games.get(index) {
            indexed
                .headers()
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect()
        } else {
            let record = self.file.record(entry.id)?;
            let mut strings = StringPool::default();
            base_headers(record, &self.names, &mut strings)
                .into_iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect()
        };
        load_game(&self.file, entry.id, headers, entry.deleted)
    }

    pub fn game_headers(&self, index: usize) -> Vec<(SharedStr, SharedStr)> {
        if let Some(indexed) = self.games.get(index) {
            return indexed.headers().to_vec();
        }
        let Some(entry) = self.entries.get(index) else {
            return Vec::new();
        };
        let Ok(record) = self.file.record(entry.id) else {
            return Vec::new();
        };
        let mut strings = StringPool::default();
        base_headers(record, &self.names, &mut strings)
    }

    pub fn game_players(&self, index: usize) -> (String, String) {
        if let Some(game) = self.games.get(index) {
            return (game.white().to_string(), game.black().to_string());
        }
        let Some(entry) = self.entries.get(index) else {
            return ("?".to_string(), "?".to_string());
        };
        let Ok(record) = self.file.record(entry.id) else {
            return ("?".to_string(), "?".to_string());
        };
        let white = self.names.player(le_u32(&record[0x18..0x1c]));
        let black = self.names.player(le_u32(&record[0x20..0x24]));
        (white.name, black.name)
    }

    /// Tournament index categories from the slot-1 text record, if present.
    /// Returns `None` when slot 1 is not a text record or has no entries.
    /// Parsed once and cached (the 1.07 MB Slot-1 HTML is not re-parsed).
    pub fn tournament_categories(&self) -> Option<Vec<MegaTournamentCategory>> {
        self.tournament_cache
            .get_or_init(|| {
                if !self.file.is_text(1) {
                    return None;
                }
                let html = self.file.text_html(1).ok()?;
                let categories = parse_mega_tournament_index(&html);
                if categories.is_empty() {
                    None
                } else {
                    Some(categories)
                }
            })
            .clone()
    }

    /// All one-based text record ids in this database.
    pub fn text_record_ids(&self) -> Vec<u32> {
        self.file.text_record_ids()
    }

    /// Title of a text record, if present.
    pub fn text_title(&self, id: u32) -> Option<String> {
        self.file.text_title(id).ok()
    }

    /// Raw HTML of a text record, if present.
    pub fn text_html(&self, id: u32) -> Option<String> {
        self.file.text_html(id).ok()
    }

    /// Text record whose event matches `(event, site)`, if any. Compares
    /// against the record's `.2lid` event names.
    pub fn find_text_for_event(&self, event: &str, site: &str) -> Option<u32> {
        let want_event = event.trim().to_lowercase();
        let want_site = site.trim().to_lowercase();
        if want_event.is_empty() || want_event == "?" {
            return None;
        }
        for id in self.text_record_ids() {
            if id == 1 {
                continue;
            }
            let Ok((place, title)) = self.file.text_event(id) else {
                continue;
            };
            if title.trim().to_lowercase() == want_event
                && (want_site.is_empty() || place.trim().to_lowercase() == want_site)
            {
                return Some(id);
            }
        }
        None
    }

    /// Whether a one-based `.2cbh` record id holds a game.
    pub fn is_game_record(&self, record_id: u32) -> bool {
        self.file.is_game(record_id)
    }

    /// The one-based `.2cbh` record id backing a game index.
    pub fn record_id_for_index(&self, index: usize) -> Option<u32> {
        self.entries.get(index).map(|entry| entry.id)
    }

    /// The game index backing a one-based `.2cbh` record id, if it is a game.
    /// `entries` is sorted by record id, so this is a binary search.
    pub fn game_index_for_record(&self, record_id: u32) -> Option<usize> {
        self.entries
            .binary_search_by_key(&record_id, |entry| entry.id)
            .ok()
    }

    /// The loaded `.2lgd` accelerator, if one could be read or built.
    pub fn accelerator(&self) -> Option<&TwoCbhAccelerator> {
        self.accelerator.as_ref()
    }

    /// One-based `.2cbh` record ids for one `(Event, Site)` pair, resolved
    /// through the `.2lgd` accelerator when present (O(1) posting-list reads
    /// after the entity-id match) and by header scan otherwise.
    pub fn event_record_ids(&self, event: &str, site: &str) -> Vec<u32> {
        let want_event = event.trim().to_lowercase();
        let want_site = site.trim().to_lowercase();
        if want_event.is_empty() || want_event == "?" {
            return Vec::new();
        }
        if let Some(acc) = &self.accelerator {
            let bound = self.entity_bound();
            let mut out = Vec::new();
            for entity in 0..bound as u32 {
                let (place, title) = self.names.event(entity);
                if title.trim().to_lowercase() != want_event {
                    continue;
                }
                if !want_site.is_empty() && place.trim().to_lowercase() != want_site {
                    continue;
                }
                if let Ok(ids) = acc.event_games(entity) {
                    out.extend(ids);
                }
            }
            if !out.is_empty() {
                out.sort_unstable();
                out.dedup();
                return out;
            }
        }
        // Fallback: scan headers.
        let mut out = Vec::new();
        for index in 0..self.len() {
            let headers = self.game_headers(index);
            let header = |key: &str| {
                headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(key))
                    .map(|(_, value)| value.to_string())
                    .unwrap_or_default()
            };
            if header("Event").trim().to_lowercase() == want_event
                && (want_site.is_empty() || header("Site").trim().to_lowercase() == want_site)
                && let Some(id) = self.record_id_for_index(index)
            {
                out.push(id);
            }
        }
        out
    }

    /// Targeted per-player game indices. Filters the `.2lid` name store first
    /// and only decodes `.2lgd` posting lists for matching entities, so a
    /// multi-million-game Mega database answers in milliseconds.
    pub fn accelerated_games_for_player(
        &self,
        player_key: &str,
        canonical_name: &str,
    ) -> Option<Vec<usize>> {
        let acc = self.accelerator.as_ref()?;
        let bound = self.entity_bound() as u32;
        let mut exact = Vec::new();
        let mut loose = Vec::new();
        for entity in 0..bound {
            let player = self.names.player(entity);
            let Some(key) = crate::players::player_key(&player.name) else {
                continue;
            };
            if key == player_key {
                exact.push(entity);
            } else if crate::players::matches_truncated_or_enumerated_name(
                canonical_name,
                &player.name,
            ) {
                loose.push(entity);
            }
        }
        let primary = if exact.is_empty() { &loose } else { &exact };
        let mut indices = Vec::new();
        for &entity in primary {
            let Ok(ids) = acc.player_games(entity) else {
                continue;
            };
            for record_id in ids {
                if let Some(index) = self.game_index_for_record(record_id) {
                    indices.push(index);
                }
            }
        }
        if indices.is_empty() {
            for &entity in &loose {
                let Ok(ids) = acc.player_games(entity) else {
                    continue;
                };
                for record_id in ids {
                    if let Some(index) = self.game_index_for_record(record_id) {
                        indices.push(index);
                    }
                }
            }
        }
        indices.sort_unstable();
        indices.dedup();
        Some(indices)
    }

    /// Targeted substring player search using `.2lid` + `.2lgd` without scanning
    /// every game record.
    pub fn accelerated_games_matching_player(&self, needle_lower: &str) -> Option<Vec<usize>> {
        let needle = needle_lower.trim();
        if needle.is_empty() {
            return None;
        }
        let acc = self.accelerator.as_ref()?;
        let bound = self.entity_bound() as u32;
        let mut indices = Vec::new();
        for entity in 0..bound {
            let player = self.names.player(entity);
            if !crate::library::contains_case_insensitive(&player.name, needle) {
                continue;
            }
            let Ok(ids) = acc.player_games(entity) else {
                continue;
            };
            for record_id in ids {
                if let Some(index) = self.game_index_for_record(record_id) {
                    indices.push(index);
                }
            }
        }
        indices.sort_unstable();
        indices.dedup();
        Some(indices)
    }

    /// Per-player game lists resolved through the `.2lgd` accelerator.
    /// Each entity id with a non-empty player posting list contributes one
    /// entry named from the `.2lid`; record ids map to game indices.
    pub fn accelerated_players(&self) -> Option<Vec<crate::library::AcceleratedPlayer>> {
        struct EntityAgg {
            indices: Vec<usize>,
            white_games: u32,
            black_games: u32,
            wins: u32,
            draws: u32,
            losses: u32,
            classical_elo_max: u32,
            any_elo_max: u32,
            elo_sum: u64,
            elo_count: u32,
            latest_otb_elo: Option<u16>,
            latest_otb_date: u32,
            first_year: u16,
            last_year: u16,
        }

        let mut event_cache = vec![0u8; self.names.entity_count()];
        let is_classical_event = |event_id: u32, cache: &mut [u8]| -> bool {
            let idx = event_id as usize;
            if let Some(&cached) = cache.get(idx)
                && cached != 0
            {
                return cached == 1;
            }
            let (place, title) = self.names.event(event_id);
            let classical = !crate::players::is_online_game(&title, &place)
                && !crate::players::is_non_classical_event(&title);
            if let Some(slot) = cache.get_mut(idx) {
                *slot = if classical { 1 } else { 2 };
            }
            classical
        };

        if self.games.is_empty() && !self.entries.is_empty() {
            let bound = self.names.entity_count();
            if bound > 0 {
                let mut by_entity: Vec<Option<EntityAgg>> = (0..bound).map(|_| None).collect();
                for (index, entry) in self.entries.iter().enumerate() {
                    let Ok(record) = self.file.record(entry.id) else {
                        continue;
                    };
                    if record.len() < 0xc0 {
                        continue;
                    }
                    let white = le_u32(&record[0x18..0x1c]) as usize;
                    let black = le_u32(&record[0x20..0x24]) as usize;
                    let event_id = le_u32(&record[0x28..0x2c]);
                    let is_classical = is_classical_event(event_id, &mut event_cache);
                    let result = (le_u32(&record[0x58..0x5c]) & 0xffff) as u16;
                    let white_elo = le_u16(&record[0x60..0x62]) as u32;
                    let black_elo = le_u16(&record[0x70..0x72]) as u32;
                    let (year, date_num) = decode_packed_date(le_u32(&record[0xbc..0xc0]));

                    let mut update =
                        |entity: usize, is_white: bool, own_elo: u32, push_idx: bool| {
                            let Some(slot) = by_entity.get_mut(entity) else {
                                return;
                            };
                            let agg = slot.get_or_insert_with(|| EntityAgg {
                                indices: Vec::new(),
                                white_games: 0,
                                black_games: 0,
                                wins: 0,
                                draws: 0,
                                losses: 0,
                                classical_elo_max: 0,
                                any_elo_max: 0,
                                elo_sum: 0,
                                elo_count: 0,
                                latest_otb_elo: None,
                                latest_otb_date: 0,
                                first_year: 0,
                                last_year: 0,
                            });
                            if push_idx {
                                agg.indices.push(index);
                            }
                            if is_white {
                                agg.white_games += 1;
                            } else {
                                agg.black_games += 1;
                            }
                            if result == 1 {
                                agg.draws += 1;
                            } else if (is_white && result == 2) || (!is_white && result == 0) {
                                agg.wins += 1;
                            } else if (is_white && result == 0) || (!is_white && result == 2) {
                                agg.losses += 1;
                            }
                            if own_elo > 0 {
                                agg.elo_sum += u64::from(own_elo);
                                agg.elo_count += 1;
                                agg.any_elo_max = agg.any_elo_max.max(own_elo);
                                if is_classical {
                                    agg.classical_elo_max = agg.classical_elo_max.max(own_elo);
                                    if date_num >= agg.latest_otb_date {
                                        agg.latest_otb_elo = Some(own_elo as u16);
                                        agg.latest_otb_date = date_num;
                                    }
                                }
                            }
                            if year > 0 {
                                if agg.first_year == 0 || year < agg.first_year {
                                    agg.first_year = year;
                                }
                                agg.last_year = agg.last_year.max(year);
                            }
                        };

                    if white == black {
                        update(white, true, white_elo, true);
                    } else {
                        update(white, true, white_elo, true);
                        update(black, false, black_elo, true);
                    }
                }
                let mut out = Vec::new();
                for (entity, slot) in by_entity.into_iter().enumerate() {
                    let Some(agg) = slot else {
                        continue;
                    };
                    if agg.indices.is_empty() {
                        continue;
                    }
                    let player = self.names.player(entity as u32);
                    if crate::players::player_key(&player.name).is_none() {
                        continue;
                    }
                    out.push(crate::library::AcceleratedPlayer {
                        name: player.name,
                        fide_id: player.fide.map(|f| f.to_string()),
                        indices: agg.indices,
                        white_games: agg.white_games,
                        black_games: agg.black_games,
                        wins: agg.wins,
                        draws: agg.draws,
                        losses: agg.losses,
                        elo_max: if agg.classical_elo_max > 0 {
                            agg.classical_elo_max
                        } else {
                            agg.any_elo_max
                        },
                        elo_sum: agg.elo_sum,
                        elo_count: agg.elo_count,
                        latest_otb_elo: agg.latest_otb_elo,
                        latest_otb_date: agg.latest_otb_date,
                        first_year: agg.first_year,
                        last_year: agg.last_year,
                    });
                }
                if !out.is_empty() {
                    return Some(out);
                }
            }
        }
        let acc = self.accelerator.as_ref()?;
        let bound = self.entity_bound();
        let mut out = Vec::new();
        for entity in 0..bound as u32 {
            let Ok(ids) = acc.player_games(entity) else {
                continue;
            };
            if ids.is_empty() {
                continue;
            }
            let player = self.names.player(entity);
            if crate::players::player_key(&player.name).is_none() {
                continue;
            }
            let mut indices = Vec::with_capacity(ids.len());
            let mut white_games = 0u32;
            let mut black_games = 0u32;
            let mut wins = 0u32;
            let mut draws = 0u32;
            let mut losses = 0u32;
            let mut elo_sum = 0u64;
            let mut elo_count = 0u32;
            let mut classical_elo_max = 0u32;
            let mut any_elo_max = 0u32;
            let mut latest_otb_elo = None;
            let mut latest_otb_date = 0u32;
            let mut first_year = 0u16;
            let mut last_year = 0u16;
            for record_id in ids {
                let Some(index) = self.game_index_for_record(record_id) else {
                    continue;
                };
                indices.push(index);
                let Ok(record) = self.file.record(record_id) else {
                    continue;
                };
                if record.len() < 0xc0 {
                    continue;
                }
                let white = le_u32(&record[0x18..0x1c]);
                let black = le_u32(&record[0x20..0x24]);
                let event_id = le_u32(&record[0x28..0x2c]);
                let is_classical = is_classical_event(event_id, &mut event_cache);
                let played_white = white == entity;
                let played_black = black == entity;
                if played_white {
                    white_games += 1;
                }
                if played_black {
                    black_games += 1;
                }

                // Result is from White's perspective: 2 = White win,
                // 1 = draw, 0 = Black win. Translate to our player's outcome.
                let packed = le_u32(&record[0x58..0x5c]);
                let result = (packed & 0xffff) as u16;
                if played_white || played_black {
                    let white_win = result == 2;
                    let black_win = result == 0;
                    let our_win = (played_white && white_win) || (played_black && black_win);
                    let our_loss = (played_white && black_win) || (played_black && white_win);
                    if result == 1 {
                        draws += 1;
                    } else if our_win {
                        wins += 1;
                    } else if our_loss {
                        losses += 1;
                    }
                }

                // Own and opponent Elo. Only count a rating we actually have.
                let white_elo = le_u16(&record[0x60..0x62]) as u32;
                let black_elo = le_u16(&record[0x70..0x72]) as u32;
                let own_elo = if played_white && !played_black {
                    white_elo
                } else if played_black && !played_white {
                    black_elo
                } else {
                    0
                };
                let (year, date_num) = decode_packed_date(le_u32(&record[0xbc..0xc0]));
                if own_elo > 0 {
                    elo_sum += u64::from(own_elo);
                    elo_count += 1;
                    any_elo_max = any_elo_max.max(own_elo);
                    if is_classical {
                        classical_elo_max = classical_elo_max.max(own_elo);
                        if date_num >= latest_otb_date {
                            latest_otb_elo = Some(own_elo as u16);
                            latest_otb_date = date_num;
                        }
                    }
                }

                // Game date → first/last year.
                if year > 0 {
                    if first_year == 0 || year < first_year {
                        first_year = year;
                    }
                    last_year = last_year.max(year);
                }
            }
            if indices.is_empty() {
                continue;
            }
            indices.sort_unstable();
            indices.dedup();
            out.push(crate::library::AcceleratedPlayer {
                name: player.name,
                fide_id: player.fide.map(|f| f.to_string()),
                indices,
                white_games,
                black_games,
                wins,
                draws,
                losses,
                elo_max: if classical_elo_max > 0 {
                    classical_elo_max
                } else {
                    any_elo_max
                },
                elo_sum,
                elo_count,
                latest_otb_elo,
                latest_otb_date,
                first_year,
                last_year,
            });
        }
        if out.is_empty() { None } else { Some(out) }
    }
}

impl GameLibrary for TwoCbhLibrary {
    fn path(&self) -> &Path {
        TwoCbhLibrary::path(self)
    }

    fn file_len(&self) -> u64 {
        TwoCbhLibrary::file_len(self)
    }

    fn len(&self) -> usize {
        TwoCbhLibrary::len(self)
    }

    fn indexed_in(&self) -> Duration {
        TwoCbhLibrary::indexed_in(self)
    }

    fn games(&self) -> &[IndexedGame] {
        TwoCbhLibrary::games(self)
    }

    fn load(&self, index: usize) -> Result<Game> {
        TwoCbhLibrary::load(self, index)
    }

    fn game_headers(&self, index: usize) -> Vec<(SharedStr, SharedStr)> {
        TwoCbhLibrary::game_headers(self, index)
    }

    fn game_players(&self, index: usize) -> (String, String) {
        TwoCbhLibrary::game_players(self, index)
    }

    fn game_record_meta(&self, index: usize) -> crate::library::GameRecordMeta {
        let Some(entry) = self.entries.get(index) else {
            return crate::library::GameRecordMeta::default();
        };
        let Ok(record) = self.file.record(entry.id) else {
            return crate::library::GameRecordMeta {
                deleted: entry.deleted,
                ..Default::default()
            };
        };
        let white_elo = le_u16(&record[0x60..0x62]);
        let black_elo = le_u16(&record[0x70..0x72]);
        let (year, date_num) = decode_packed_date(le_u32(&record[0xbc..0xc0]));
        let raw_res = le_u32(&record[0x58..0x5c]) & 0xffff;
        let result = match raw_res {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 3,
        };
        let raw_eco = le_u16(&record[0x80..0x82]);
        let eco_code = if (128..=64127).contains(&raw_eco) {
            let code = raw_eco / 128 - 1;
            if code <= 499 { code + 1 } else { 0 }
        } else {
            0
        };
        let medals = le_u16(&record[0x82..0x84]);
        let vcs_flags = le_u32(&record[0x84..0x88]);
        let cba_off = le_u64(&record[0x10..0x18]);
        crate::library::GameRecordMeta {
            white_elo,
            black_elo,
            year,
            date_num,
            result,
            eco_code,
            medals,
            vcs_flags,
            annotated: cba_off > 12 || (vcs_flags & 0x0e) != 0 || medals != 0,
            deleted: entry.deleted,
        }
    }

    fn accelerated_games_matching_player(&self, needle_lower: &str) -> Option<Vec<usize>> {
        TwoCbhLibrary::accelerated_games_matching_player(self, needle_lower)
    }

    fn scan_mainline_positions(
        &self,
        index: usize,
        max_ply: u32,
        visitor: &mut dyn FnMut(u32, &crate::chess::Chess) -> bool,
    ) -> Result<bool> {
        let entry = self
            .entries
            .get(index)
            .with_context(|| format!("2CBH game index {index} is out of range"))?;
        let content = self.file.content(entry.id)?;
        let raw = content.raw_bytes();
        match content.kind {
            1 => {
                if raw.len() >= 2 && raw[0] == 0xfc && raw[1] == 0xff {
                    let start_pos = crate::chess::Chess::default();
                    fast_index::scan_2cbh_mainline_positions(
                        &start_pos,
                        &raw[2..],
                        max_ply,
                        visitor,
                    )?;
                    Ok(true)
                } else if raw.len() >= 2 && raw[0] == 0xfb && raw[1] == 0xff {
                    let words = content.words();
                    let Ok((start_pos, rest)) = moves::decode_custom_start(&words) else {
                        return Ok(false);
                    };
                    let consumed_bytes = (words.len() - rest.len()) * 2;
                    fast_index::scan_2cbh_mainline_positions(
                        &start_pos,
                        &raw[consumed_bytes..],
                        max_ply,
                        visitor,
                    )?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            2 => {
                if raw.len() < 6 {
                    return Ok(false);
                }
                let m0 = u16::from_le_bytes([raw[0], raw[1]]);
                let start_id = u16::from_le_bytes([raw[2], raw[3]]);
                let m1 = u16::from_le_bytes([raw[4], raw[5]]);
                if m0 != 0xfffb || m1 != 0xfffc {
                    return Ok(false);
                }
                let start_pos = moves::start_position_960(start_id)?;
                fast_index::scan_2cbh_mainline_positions(&start_pos, &raw[6..], max_ply, visitor)?;
                Ok(true)
            }
            kind => bail!("2CBH game {} uses unknown encoding kind {kind}", entry.id),
        }
    }

    fn stream_mainline(
        &self,
        index: usize,
        max_plies: u32,
        consumer: &mut dyn FnMut(u64, u32),
    ) -> Result<bool> {
        let entry = self
            .entries
            .get(index)
            .with_context(|| format!("2CBH game index {index} is out of range"))?;
        let content = self.file.content(entry.id)?;
        let words = content.words();
        let (start_pos, chess960, move_tokens) = match content.kind {
            1 => {
                // Custom-start records (e.g. game 503255 with words
                // [0x1, 0xfffb, …]) and any non-standard marker are not
                // indexable from the default start: skip cleanly.
                if words.first() != Some(&0xfffc) {
                    tracing::debug!(
                        "2CBH game {} has non-standard start marker {:?}; skipping position index",
                        entry.id,
                        words.first().copied()
                    );
                    return Ok(false);
                }
                (crate::chess::Chess::default(), false, &words[1..])
            }
            2 => {
                // Chess960 games are skipped by the position index (standard
                // start key space only); not an error.
                tracing::debug!(
                    "2CBH game {} is Chess960; skipping position index",
                    entry.id
                );
                return Ok(false);
            }
            kind => bail!("2CBH game {} uses unknown encoding kind {kind}", entry.id),
        };
        fast_index::stream_2cbh_mainline(&start_pos, move_tokens, chess960, max_plies, consumer)?;
        Ok(true)
    }

    fn source_meta(&self) -> Result<SourceMeta> {
        let mut parts = vec![
            SourceMeta::read(&self.file.cbh_path)?,
            SourceMeta::read(&self.file.cbg_path)?,
        ];
        if let Some(path) = &self.file.cba_path {
            parts.push(SourceMeta::read(path)?);
        }
        if self.file.lid_path.is_file() {
            parts.push(SourceMeta::read(&self.file.lid_path)?);
        }
        Ok(SourceMeta::combine(&parts))
    }

    fn filter(&self, needle: &str) -> Vec<usize> {
        crate::library::filter_games(&self.games, needle)
    }

    fn tournament_categories(&self) -> Option<Vec<MegaTournamentCategory>> {
        TwoCbhLibrary::tournament_categories(self)
    }

    fn source_record_id(&self, index: usize) -> Option<u32> {
        TwoCbhLibrary::record_id_for_index(self, index)
    }

    fn source_index_for_record(&self, record_id: u32) -> Option<usize> {
        TwoCbhLibrary::game_index_for_record(self, record_id)
    }

    fn is_game_record(&self, record_id: u32) -> bool {
        TwoCbhLibrary::is_game_record(self, record_id)
    }

    fn guiding_text_ids(&self) -> Vec<u32> {
        TwoCbhLibrary::text_record_ids(self)
    }

    fn guiding_text_title(&self, id: u32) -> Option<String> {
        TwoCbhLibrary::text_title(self, id)
    }

    fn guiding_text_html(&self, id: u32) -> Option<Vec<String>> {
        TwoCbhLibrary::text_html(self, id).map(|html| vec![html])
    }

    fn find_guiding_text(&self, event: &str, site: &str) -> Option<u32> {
        TwoCbhLibrary::find_text_for_event(self, event, site)
    }

    fn event_game_indices(
        &self,
        event: &str,
        site: &str,
        _year: Option<u32>,
    ) -> Option<Vec<usize>> {
        let accelerator = self.accelerator.as_ref()?;
        let mut indices = Vec::new();
        for record_id in self.event_record_ids(event, site) {
            if let Some(index) = self.game_index_for_record(record_id) {
                indices.push(index);
            }
        }
        if indices.is_empty() {
            return None;
        }
        // Touch the accelerator so a present-but-unusable sidecar still
        // counts as wired rather than silently scanned.
        let _ = accelerator.block_count();
        indices.sort_unstable();
        indices.dedup();
        Some(indices)
    }

    fn accelerated_player_games(&self) -> Option<Vec<crate::library::AcceleratedPlayer>> {
        TwoCbhLibrary::accelerated_players(self)
    }

    fn accelerated_games_for_player(
        &self,
        player_key: &str,
        canonical_name: &str,
    ) -> Option<Vec<usize>> {
        TwoCbhLibrary::accelerated_games_for_player(self, player_key, canonical_name)
    }
}

fn load_game(
    file: &TwoCbhFile,
    id: u32,
    headers: Vec<(String, String)>,
    deleted: bool,
) -> Result<Game> {
    let content = file.content(id)?;
    let words = content.words();
    let record_start = start_id_from_record(file.record(id)?);
    let (chess960, start_id, custom_start, move_tokens) = match content.kind {
        1 => {
            if words.first() == Some(&0xfffc) {
                (false, None, None, &words[1..])
            } else if words.first() == Some(&0xfffb) {
                let (pos, rest) = moves::decode_custom_start(&words)
                    .with_context(|| format!("decoding 2CBH game {id} custom start"))?;
                (false, None, Some(pos), rest)
            } else {
                bail!("2CBH game {id} has no standard start marker");
            }
        }
        2 => {
            if words.first() != Some(&0xfffb) || words.get(2) != Some(&0xfffc) {
                bail!("2CBH game {id} has a malformed Chess960 start marker");
            }
            let start_id = *words.get(1).context("Chess960 start id is missing")?;
            if let Some(record_start) = record_start
                && record_start != start_id
            {
                bail!(
                    "2CBH game {id} says Chess960 start {start_id} in its move record \
                     but {record_start} in its header"
                );
            }
            (true, Some(start_id), None, &words[3..])
        }
        kind => bail!("2CBH game {id} uses unknown encoding kind {kind}"),
    };

    let mut game = Game {
        headers,
        ..Game::default()
    };
    if let Some(start_id) = start_id {
        let pos = moves::start_position_960(start_id)
            .with_context(|| format!("2CBH game {id} Chess960 start {start_id}"))?;
        let fen = moves::start_fen(&pos);
        game.nodes[0].pos = pos;
        game.set_header("SetUp", "1".to_string());
        game.set_header("FEN", fen);
        game.set_header("Variant", "Chess960".to_string());
    } else if let Some(pos) = custom_start {
        let fen = moves::start_fen(&pos);
        game.nodes[0].pos = pos;
        game.set_header("SetUp", "1".to_string());
        game.set_header("FEN", fen);
    }
    if deleted {
        game.errors
            .push("This game is marked deleted in the ChessBase database.".to_string());
    }

    let mut decoded = true;
    if let Err(err) = moves::decode_moves(&mut game, move_tokens, chess960) {
        decoded = false;
        game.errors
            .push(format!("2CBH game {id} move stream: {err:#}"));
    }
    if decoded && game.nodes.len() == 1 {
        game.set_header("PlyCount", "0".to_string());
    }
    if decoded
        && let Some(map) = &file.brett_annotations
        && let Some(anns) = map.get(&id)
    {
        apply_brett_annotations(&mut game, anns);
    }
    Ok(game)
}

struct Player {
    name: String,
    fide: Option<u32>,
}

/// The inline `.2lid` name blocks.
pub(crate) struct NameBase {
    bytes: Option<Mmap>,
    first_offset: usize,
    stride: usize,
    event_offset: usize,
    source_offset: usize,
}

impl NameBase {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self {
                bytes: None,
                first_offset: LID_FIRST,
                stride: LID_STRIDE,
                event_offset: LID_EVENT,
                source_offset: LID_SOURCE,
            });
        }
        let bytes = map_file(path, "2CBH names")?;
        if bytes.len() < 8 {
            bail!("{} is shorter than a 2CBH name header", path.display());
        }

        let first = be_u32(&bytes[0..4]) as usize;
        let num_sections = be_u32(&bytes[4..8]) as usize;
        let (first_offset, stride, event_offset, source_offset) = if first >= 8
            && (1..=32).contains(&num_sections)
            && bytes.len() >= 8 + num_sections * 20
        {
            let mut running_offset = 0usize;
            let mut event_off = LID_EVENT;
            let mut source_off = LID_SOURCE;
            let mut total_stride = 0usize;
            for i in 0..num_sections {
                let sec_desc = 8 + i * 20;
                let sec_size = be_u32(&bytes[sec_desc..sec_desc + 4]) as usize;
                if i == 1 {
                    event_off = running_offset;
                } else if i == 2 {
                    source_off = running_offset;
                }
                running_offset = running_offset.saturating_add(sec_size);
                total_stride = total_stride.saturating_add(sec_size);
            }
            (
                if first <= bytes.len() {
                    first
                } else {
                    LID_FIRST
                },
                if total_stride > 0 {
                    total_stride
                } else {
                    LID_STRIDE
                },
                event_off,
                source_off,
            )
        } else {
            (LID_FIRST, LID_STRIDE, LID_EVENT, LID_SOURCE)
        };

        Ok(Self {
            bytes: Some(bytes),
            first_offset,
            stride,
            event_offset,
            source_offset,
        })
    }

    fn player(&self, id: u32) -> Player {
        let Some(block) = self.block(id, 0) else {
            return Player {
                name: "?".to_string(),
                fide: None,
            };
        };
        let Some((texts, tail)) = two_strings(block) else {
            return Player {
                name: "?".to_string(),
                fide: None,
            };
        };
        let last = decode_text(texts[0]);
        let first = decode_text(texts[1]);
        let name = player_name(&last, &first);
        let fide = block
            .get(tail + 16..tail + 20)
            .map(le_u32)
            .filter(|&fide| fide != 0 && fide != u32::MAX);
        Player { name, fide }
    }

    fn event(&self, id: u32) -> (String, String) {
        let Some(block) = self.block(id, self.event_offset) else {
            return ("?".to_string(), "?".to_string());
        };
        let Some((texts, _)) = two_strings(block) else {
            return ("?".to_string(), "?".to_string());
        };
        let place = non_empty(decode_text(texts[0]));
        let title = non_empty(decode_text(texts[1]));
        (place, title)
    }

    fn source(&self, id: u32) -> Option<(String, String)> {
        let block = self.block(id, self.source_offset)?;
        let (texts, _) = two_strings(block)?;
        let title = non_empty(decode_text(texts[0]));
        let publisher = non_empty(decode_text(texts[1]));
        (title != "?" || publisher != "?").then_some((title, publisher))
    }

    /// Number of `.2lid` entity slots (bounds directory scans; overflow
    /// `.2lgd` blocks beyond this are never entity ids).
    pub(crate) fn entity_count(&self) -> usize {
        let Some(bytes) = self.bytes.as_ref() else {
            return 0;
        };
        if self.stride == 0 || bytes.len() <= self.first_offset {
            return 0;
        }
        (bytes.len() - self.first_offset).div_ceil(self.stride)
    }

    /// Single-string title block used by text records (offset `0xE82`):
    /// `le_u32` length followed by bytes.
    pub(crate) fn text_title(&self, id: u32) -> String {
        const TEXT_TITLE_OFFSET: usize = 0xE82;
        const MAX_TITLE: usize = 1 << 20;
        let Some(bytes) = self.bytes.as_ref() else {
            return "?".to_string();
        };
        let Ok(id_usize) = usize::try_from(id) else {
            return "?".to_string();
        };
        let base = self
            .first_offset
            .saturating_add(id_usize.saturating_mul(self.stride))
            .saturating_add(TEXT_TITLE_OFFSET);
        let len = bytes.get(base..base + 4).map(le_u32).unwrap_or(0) as usize;
        if len == 0 || len > MAX_TITLE {
            return "?".to_string();
        }
        let text = bytes.get(base + 4..base + 4 + len).unwrap_or(&[]);
        non_empty(decode_text(text))
    }

    fn block(&self, id: u32, offset: usize) -> Option<&[u8]> {
        let bytes = self.bytes.as_ref()?;
        let base = self
            .first_offset
            .checked_add(usize::try_from(id).ok()?.checked_mul(self.stride)?)?
            .checked_add(offset)?;
        let length = usize::try_from(le_u32(bytes.get(base..base + 4)?)).ok()?;
        if length < 4 {
            return None;
        }
        bytes.get(base..base.checked_add(length)?)
    }
}

fn two_strings(block: &[u8]) -> Option<([&[u8]; 2], usize)> {
    let mut at = 4usize;
    let mut texts = [&[][..]; 2];
    for slot in &mut texts {
        let length = usize::try_from(le_u32(block.get(at..at + 4)?)).ok()?;
        at += 4;
        *slot = block.get(at..at.checked_add(length)?)?;
        at += length;
    }
    Some((texts, at))
}

fn base_headers(
    record: &[u8],
    names: &NameBase,
    strings: &mut StringPool,
) -> Vec<(SharedStr, SharedStr)> {
    let white = names.player(le_u32(&record[0x18..0x1c]));
    let black = names.player(le_u32(&record[0x20..0x24]));
    let (site, event) = names.event(le_u32(&record[0x28..0x2c]));
    let source = names.source(le_u32(&record[0x38..0x3c]));
    let packed = le_u32(&record[0x58..0x5c]);
    let round = (packed >> 16) as u16;
    let result = (packed & 0xffff) as u16;
    let subround = le_u32(&record[0x5c..0x60]);
    let white_elo = le_u16(&record[0x60..0x62]);
    let black_elo = le_u16(&record[0x70..0x72]);
    let eco = le_u16(&record[0x80..0x82]);
    let medals = le_u16(&record[0x82..0x84]);
    let game_id = le_u64(&record[0x98..0xa0]);
    let beauty = le_u64(&record[0xb0..0xb8]);
    let date = le_u32(&record[0xbc..0xc0]);

    let mut headers = Vec::with_capacity(18);
    let mut push = |key: &str, value: String| {
        headers.push((strings.intern(key), strings.intern(value)));
    };
    push("Event", event);
    push("Site", site);
    push("Date", date_string(date));
    push("Round", round_string(round, subround));
    push("White", white.name);
    push("Black", black.name);
    push("Result", result_string(result).to_string());
    if white_elo > 0 {
        push("WhiteElo", white_elo.to_string());
    }
    if black_elo > 0 {
        push("BlackElo", black_elo.to_string());
    }
    if let Some(eco_str) = crate::cbh::eco_string(eco) {
        push("ECO", eco_str);
    }
    if let Some(fide) = white.fide {
        push("WhiteFideId", fide.to_string());
    }
    if let Some(fide) = black.fide {
        push("BlackFideId", fide.to_string());
    }
    if game_id > 0 {
        push("GameId", game_id.to_string());
    }
    if beauty > 0 {
        push("Beauty", beauty.to_string());
    }
    if let Some((title, publisher)) = source {
        push("SourceTitle", title);
        push("Source", publisher);
    }
    if medals != 0 {
        push(
            crate::library::medals::PGN_TAG,
            crate::library::medals::format_short(medals),
        );
    }
    headers
}

fn player_name(last: &str, first: &str) -> String {
    if last.is_empty() && first.is_empty() {
        return "?".to_string();
    }
    let mut name = last.to_string();
    if !first.is_empty() {
        name.push_str(", ");
        name.push_str(first);
        if (1..=2).contains(&first.len())
            && first
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            name.push('.');
        }
    }
    name
}

fn non_empty(text: String) -> String {
    if text.is_empty() {
        "?".to_string()
    } else {
        text
    }
}

fn result_string(result: u16) -> &'static str {
    match result {
        0 => "0-1",
        1 => "1/2-1/2",
        2 => "1-0",
        _ => "*",
    }
}

fn date_string(date: u32) -> String {
    let year = (date >> 9) & 0xfff;
    let month = (date >> 5) & 15;
    let day = date & 31;
    let year = if year == 0 {
        "????".to_string()
    } else {
        format!("{year:04}")
    };
    let month = if month == 0 {
        "??".to_string()
    } else {
        format!("{month:02}")
    };
    let day = if day == 0 {
        "??".to_string()
    } else {
        format!("{day:02}")
    };
    format!("{year}.{month}.{day}")
}

fn round_string(round: u16, subround: u32) -> String {
    match (round, subround) {
        (0, 0) => "?".to_string(),
        (0, sub) => format!("?.{sub}"),
        (round, 0) => round.to_string(),
        (round, sub) => format!("{round}.{sub}"),
    }
}

fn validate_companion(path: &Path, bytes: &[u8], kind: &str) -> Result<()> {
    if bytes.len() < CBG_HEADER as usize {
        bail!("{} is shorter than a 2CBH {kind} header", path.display());
    }
    let header = usize::from(le_u16(&bytes[8..10]));
    if header != CBG_HEADER as usize {
        bail!(
            "{} states a {header}-byte 2CBH {kind} header; expected {CBG_HEADER}",
            path.display()
        );
    }
    Ok(())
}

fn map_file(path: &Path, kind: &str) -> Result<Mmap> {
    let file =
        File::open(path).with_context(|| format!("opening 2CBH {kind} file {}", path.display()))?;
    // SAFETY: the map is read-only and the database is treated as immutable
    // while it is open, matching how the application opens source databases.
    unsafe { Mmap::map(&file) }
        .with_context(|| format!("mapping 2CBH {kind} file {}", path.display()))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}
fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Resolve a companion file case-insensitively, like ChessBase's own file set.
fn sibling(stem: &Path, ext: &str) -> PathBuf {
    let mut name: OsString = stem.as_os_str().to_owned();
    name.push(ext);
    let exact = PathBuf::from(name);
    if exact.symlink_metadata().is_ok() {
        return exact;
    }
    let (Some(dir), Some(want)) = (exact.parent(), exact.file_name()) else {
        return exact;
    };
    let want = want.to_string_lossy().to_ascii_lowercase();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().to_ascii_lowercase() == want {
                return entry.path();
            }
        }
    }
    exact
}

const CP1252_HIGH: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}',
    '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
];

pub(crate) fn decode_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes
            .iter()
            .map(|&byte| match byte {
                0x80..=0x9f => CP1252_HIGH[(byte - 0x80) as usize],
                _ => byte as char,
            })
            .collect(),
    }
}

fn decode_packed_date(raw: u32) -> (u16, u32) {
    let year = ((raw >> 9) & 0xfff) as u16;
    if year == 0 {
        return (0, 0);
    }
    let month = (raw >> 5) & 15;
    let day = raw & 31;
    (year, u32::from(year) * 10_000 + month * 100 + day)
}

fn start_id_from_record(record: &[u8]) -> Option<u16> {
    let value = le_u32(&record[0x80..0x84]);
    (START_ID_BASE..=u32::from(u16::MAX))
        .contains(&value)
        .then(|| (value - START_ID_BASE) as u16)
}
