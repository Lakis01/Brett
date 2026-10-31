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

mod moves;
mod tokens;

#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
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
const START_ID_BASE: u32 = 0xFC40;
const SEARCH_CACHE_MAX_GAMES: usize = 1_000_000;

/// Above this many slots, indexing the whole database is not useful in the
/// current in-memory game-list model.  The full Mega Database 2026 has
/// 11,743,084 records; it is still readable through [`TwoCbhFile`].
pub const MAX_INDEXED_GAMES: usize = 2_000_000;

/// One game slot's location and state.
#[derive(Clone, Debug)]
struct Entry {
    id: u32,
    deleted: bool,
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

        Ok(Self {
            cbh_path,
            cbg_path,
            cba_path,
            lid_path,
            cbh,
            cbg,
            count,
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
    entries: Vec<Entry>,
    games: Vec<IndexedGame>,
    file_len: u64,
    indexed_in: Duration,
}

impl TwoCbhLibrary {
    /// Open a 2CBH database and index its game headers.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let started = Instant::now();
        let file = TwoCbhFile::open(path)?;
        // Check the slot count before scanning every header.  This keeps a
        // full-Mega open attempt from walking 11.7 million records just to
        // reject the database, and it is a safe upper bound on the game count.
        let slots = file.slot_count().saturating_sub(1);
        if slots > MAX_INDEXED_GAMES {
            bail!(
                "{} contains {slots} 2CBH slots; Brett indexes at most {MAX_INDEXED_GAMES}. \
                 The file is still readable one record at a time through TwoCbhFile.",
                file.path().display()
            );
        }
        let names = NameBase::open(&file.lid_path)?;
        let total = file.slot_count().saturating_sub(1);
        let build_search = total <= SEARCH_CACHE_MAX_GAMES;
        let mut entries = Vec::new();
        let mut games = Vec::new();
        let mut strings = StringPool::default();
        for id in 1..file.count {
            let record = file.record(id)?;
            let kind = record[0];
            if kind & 1 == 0 || kind & 2 != 0 {
                continue;
            }
            let headers = base_headers(record, &names, &mut strings);
            entries.push(Entry {
                id,
                deleted: kind & 0x80 != 0,
            });
            games.push(IndexedGame::cbh(
                headers,
                id,
                le_u64(&record[8..16]),
                le_u64(&record[16..24]),
                kind & 0x80 != 0,
                build_search,
            ));
        }

        let mut file_len = file.cbh.len() as u64 + file.cbg.len() as u64;
        for path in file.cba_path.iter().chain(std::iter::once(&file.lid_path)) {
            file_len += fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        }

        Ok(Self {
            path: file.cbh_path.clone(),
            file,
            entries,
            games,
            file_len,
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

    pub fn filter(&self, needle: &str) -> Vec<usize> {
        crate::library::filter_games(&self.games, needle)
    }

    /// Decode one game on demand.  A damaged move stream is reported in
    /// [`Game::errors`] while the readable part is returned; a damaged
    /// container record is an error.
    pub fn load(&self, index: usize) -> Result<Game> {
        let entry = self
            .entries
            .get(index)
            .with_context(|| format!("2CBH game index {index} is out of range"))?;
        let headers = self.games[index]
            .headers()
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        load_game(&self.file, entry.id, headers, entry.deleted)
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
    let (chess960, start_id, move_tokens) = match content.kind {
        1 => {
            if words.first() != Some(&0xfffc) {
                bail!("2CBH game {id} has no standard start marker");
            }
            (false, None, &words[1..])
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
            (true, Some(start_id), &words[3..])
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
    Ok(game)
}

struct Player {
    name: String,
    fide: Option<u32>,
}

/// The inline `.2lid` name blocks.
struct NameBase {
    bytes: Option<Mmap>,
}

impl NameBase {
    fn open(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self { bytes: None });
        }
        let bytes = map_file(path, "2CBH names")?;
        if bytes.len() < LID_FIRST {
            bail!("{} is shorter than a 2CBH name header", path.display());
        }
        Ok(Self { bytes: Some(bytes) })
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
        let Some(block) = self.block(id, LID_EVENT) else {
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
        let block = self.block(id, LID_SOURCE)?;
        let (texts, _) = two_strings(block)?;
        let title = non_empty(decode_text(texts[0]));
        let publisher = non_empty(decode_text(texts[1]));
        (title != "?" || publisher != "?").then_some((title, publisher))
    }

    fn block(&self, id: u32, offset: usize) -> Option<&[u8]> {
        let bytes = self.bytes.as_ref()?;
        let base = LID_FIRST
            .checked_add(usize::try_from(id).ok()?.checked_mul(LID_STRIDE)?)?
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
    let white_elo = le_u32(&record[0x60..0x64]);
    let black_elo = le_u32(&record[0x70..0x74]);
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

fn decode_text(bytes: &[u8]) -> String {
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

fn start_id_from_record(record: &[u8]) -> Option<u16> {
    let value = le_u32(&record[0x80..0x84]);
    (START_ID_BASE..=u32::from(u16::MAX))
        .contains(&value)
        .then(|| (value - START_ID_BASE) as u16)
}
