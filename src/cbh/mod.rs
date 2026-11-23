//! Native reader and writer for classic ChessBase databases (`.cbh` + `.cbg`
//! and their companion files).
//!
//! The classic format is the multi-file database ChessBase used before CBH2:
//! a fixed 46-byte header per game (`.cbh`), a move record per game (`.cbg`),
//! optional annotations (`.cba`), and small name/title files for players,
//! tournaments, annotators and sources.  Brett indexes the headers when a
//! database is opened, decodes a game's moves, variations, comments and NAGs
//! on demand when it is displayed, and writes new `.cbh` databases through
//! [`writer`].
//!
//! The implementation is written from the published on-disk layout and the
//! independent format work in the MIT-licensed `cbvault` project.  It does
//! not link to or copy a converter.

mod annotations;
pub mod fast_index;
mod moves;
mod tables;
pub mod writer;

pub use writer::{
    ALL_EXTENSIONS, CORE_EXTENSIONS, CbhWriteOptions, CbhWriteReport, core_companion_files,
    existing_companion_files, write_database, write_database_with_options,
};

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use memmap2::Mmap;

use crate::domain::Game;
use crate::library::{GameLibrary, GameRecordMeta, IndexedGame, SharedStr, SourceMeta, StringPool};

/// Above this size the per-game lowercase search cache costs more memory than
/// it saves. Searches then inspect the interned headers on demand.
const SEARCH_CACHE_MAX_GAMES: usize = 1_000_000;

const RECORD_SIZE: usize = 46;
const ENTITY_MAGIC: i32 = 1_234_567_890;
const ENTITY_DELETED: i32 = -999;
const MAX_ENTITY_DATA: i32 = 64 << 10;
const MAX_MOVE_RECORD: usize = 16 << 20;
const MAX_ANNOTATION_RECORD: usize = 16 << 20;
const MIN_CBG_HEADER: u64 = 10;

/// One indexed classic game.  The offsets are the short (32-bit) values from
/// the `.cbh`; a `.cbj` wide index is consulted lazily for files over 4 GiB.
#[derive(Clone, Debug)]
struct Entry {
    id: u32,
    moves_offset: u32,
    annotations_offset: u32,
    deleted: bool,
    white_elo: u16,
    black_elo: u16,
    year: u16,
    result: u8,
    eco_code: u16,
    medals: u16,
    vcs_flags: u32,
    annotated: bool,
}

/// A classic multi-file database opened through [`GameLibrary`].
pub struct CbhLibrary {
    /// The path shown to the user.  For a `.cbv` archive this is the archive
    /// itself; for a classic database it is the `.cbh`.
    path: PathBuf,
    cbh_path: PathBuf,
    cbg_path: PathBuf,
    cba_path: Option<PathBuf>,
    cbg_mmap: Option<Mmap>,
    entity_paths: Vec<PathBuf>,
    wide: Option<Wide>,
    file_len: u64,
    entries: Vec<Entry>,
    games: Vec<IndexedGame>,
    /// One-based record ids whose flag bit 1 marks a guiding text.
    text_ids: Vec<u32>,
    indexed_in: Duration,
}

impl CbhLibrary {
    /// Open a `.cbh` database.  The path may name the `.cbh` file, a `.cbg`
    /// or `.cba` companion, or the database's bare base name.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let started = Instant::now();
        let supplied = path.as_ref();
        let stem = match supplied.extension().and_then(|e| e.to_str()) {
            Some(ext)
                if ext.eq_ignore_ascii_case("cbh")
                    || ext.eq_ignore_ascii_case("cbg")
                    || ext.eq_ignore_ascii_case("cba") =>
            {
                supplied.with_extension("")
            }
            _ => supplied.to_path_buf(),
        };
        let cbh_path = sibling(&stem, ".cbh");
        if !cbh_path.is_file() {
            bail!(
                "classic ChessBase header file {} is missing",
                cbh_path.display()
            );
        }
        let cbg_path = sibling(&stem, ".cbg");
        if !cbg_path.is_file() {
            bail!(
                "classic ChessBase move file {} is missing (expected beside {})",
                cbg_path.display(),
                cbh_path.display()
            );
        }
        let cba_path = {
            let path = sibling(&stem, ".cba");
            path.is_file().then_some(path)
        };

        let cbh = fs::read(&cbh_path)
            .with_context(|| format!("reading ChessBase headers {}", cbh_path.display()))?;
        if cbh.len() < RECORD_SIZE || cbh.len() % RECORD_SIZE != 0 {
            bail!(
                "{} is not a classic ChessBase header file: {} bytes is not 46 plus whole records",
                cbh_path.display(),
                cbh.len()
            );
        }
        let record_size = u16::from_be_bytes([cbh[3], cbh[4]]) as usize;
        if record_size != RECORD_SIZE {
            bail!(
                "{} states a {}-byte game record; classic ChessBase uses 46",
                cbh_path.display(),
                record_size
            );
        }

        let entities = Entities::open(&stem)?;
        let cbg_len = fs::metadata(&cbg_path)?.len();
        let cba_len = match &cba_path {
            Some(path) => fs::metadata(path)?.len(),
            None => 0,
        };
        let wide = Wide::open_if_needed(&stem, cbg_len, cba_len)?;

        let total = cbh.len() / RECORD_SIZE - 1;
        let build_search = total <= SEARCH_CACHE_MAX_GAMES;
        let mut entries = Vec::with_capacity(total);
        let mut games = Vec::with_capacity(total);
        let mut text_ids = Vec::new();
        let mut strings = StringPool::default();
        for index in 0..total {
            let id = u32::try_from(index + 1).context("ChessBase record count is too large")?;
            let at = (index + 1) * RECORD_SIZE;
            let record = &cbh[at..at + RECORD_SIZE];
            let flags = record[0];
            // Bit 0 marks a game/text record; bit 1 marks a guiding text.  A
            // zero bit is an unused slot past the last game in some
            // databases.  Deleted games keep bit 0 and are still readable.
            if flags & 1 == 0 {
                continue;
            }
            if flags & 2 != 0 {
                text_ids.push(id);
                continue;
            }
            let moves_offset = be_u32(&record[1..5]);
            let annotations_offset = be_u32(&record[5..9]);
            let deleted = flags & 0x80 != 0;
            let date = be_u24(&record[24..27]);
            let year = ((date >> 9) & 0xfff) as u16;
            let result = match record[27] {
                0 | 4 => 0,
                1 | 5 => 1,
                2 | 6 => 2,
                _ => 3,
            };
            let white_elo = be_u16(&record[31..33]);
            let black_elo = be_u16(&record[33..35]);
            let raw_eco = be_u16(&record[35..37]);
            let eco_code = if (128..=64127).contains(&raw_eco) {
                let code = raw_eco / 128 - 1;
                if code <= 499 { code + 1 } else { 0 }
            } else {
                0
            };
            let medals = be_u16(&record[37..39]);
            let vcs_flags = be_u32(&record[39..43]);
            let annotated = annotations_offset != 0 || (vcs_flags & 0x0e) != 0 || medals != 0;
            let headers = base_headers(record, &entities, &mut strings);
            entries.push(Entry {
                id,
                moves_offset,
                annotations_offset,
                deleted,
                white_elo,
                black_elo,
                year,
                result,
                eco_code,
                medals,
                vcs_flags,
                annotated,
            });
            games.push(IndexedGame::cbh(
                headers,
                id,
                u64::from(moves_offset),
                u64::from(annotations_offset),
                deleted,
                build_search,
            ));
        }

        let mut entity_paths = Vec::new();
        for path in [
            &entities.players_path,
            &entities.tournaments_path,
            &entities.annotators_path,
            &entities.sources_path,
        ]
        .into_iter()
        .flatten()
        {
            entity_paths.push(path.clone());
        }
        if let Some(path) = &cba_path {
            entity_paths.push(path.clone());
        }
        if let Some(path) = wide.as_ref().map(|w| &w.path) {
            entity_paths.push(path.clone());
        }

        let mut file_len = fs::metadata(&cbh_path)?.len() + cbg_len;
        for path in &entity_paths {
            file_len += fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        }

        let cbg_mmap = File::open(&cbg_path)
            .ok()
            .and_then(|file| unsafe { Mmap::map(&file) }.ok());

        Ok(Self {
            path: cbh_path.clone(),
            cbh_path,
            cbg_path,
            cba_path,
            cbg_mmap,
            entity_paths,
            wide,
            file_len,
            entries,
            games,
            text_ids,
            indexed_in: started.elapsed(),
        })
    }

    /// Open a `.cbv` archive by materialising its database members into a
    /// cache.  The library keeps the archive path for display and cache keys,
    /// while all reads go to the extracted classic files.
    pub fn open_cbv(archive: impl AsRef<Path>) -> Result<Self> {
        Self::open_cbv_with_progress(archive, |_| {})
    }

    pub fn open_cbv_with_progress(
        archive: impl AsRef<Path>,
        progress: impl FnMut(&str),
    ) -> Result<Self> {
        let archive = archive.as_ref();
        let extracted = crate::cbv::materialize_with_progress(archive, progress)?;
        let mut library = Self::open(&extracted)?;
        library.path = archive.to_path_buf();
        Ok(library)
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

    /// Fast zero-syscall access to one `.cbg` move record (uses mmap when
    /// available, falling back to `read_record` otherwise).
    fn with_cbg_record<T>(
        &self,
        moves_offset: u64,
        id: u32,
        f: impl FnOnce(u8, &[u8]) -> Result<T>,
    ) -> Result<T> {
        if let Some(mmap) = &self.cbg_mmap {
            let off = usize::try_from(moves_offset).context("CBG offset overflow")?;
            if off >= MIN_CBG_HEADER as usize && off + 4 <= mmap.len() {
                let head = &mmap[off..off + 4];
                let size = (usize::from(head[1]) << 16)
                    | (usize::from(head[2]) << 8)
                    | usize::from(head[3]);
                if (4..=MAX_MOVE_RECORD).contains(&size) && off + size <= mmap.len() {
                    return f(head[0], &mmap[off + 4..off + size]);
                }
            }
        }
        let record = read_record(
            &self.cbg_path,
            moves_offset,
            MAX_MOVE_RECORD,
            "move",
            id,
            MIN_CBG_HEADER,
        )?;
        f(record.head[0], &record.bytes)
    }

    /// Decode one game on demand. A damaged move stream or annotation payload
    /// is reported in [`Game::errors`] while the readable part of the game is
    /// returned; a missing or unreadable source file, or a damaged record
    /// header, is an error.
    pub fn load(&self, index: usize) -> Result<Game> {
        let entry = self
            .entries
            .get(index)
            .with_context(|| format!("ChessBase game index {index} is out of range"))?;
        let (moves_offset, annotations_offset) = match &self.wide {
            Some(wide) => wide.offsets(entry.id, (entry.moves_offset, entry.annotations_offset))?,
            None => (
                u64::from(entry.moves_offset),
                u64::from(entry.annotations_offset),
            ),
        };
        let record = read_record(
            &self.cbg_path,
            moves_offset,
            MAX_MOVE_RECORD,
            "move",
            entry.id,
            MIN_CBG_HEADER,
        )?;
        let headers = self.games[index]
            .headers()
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let (start, consumed) = moves::decode_start(record.head[0], &record.bytes)
            .with_context(|| format!("decoding start of ChessBase game {}", entry.id))?;
        let mut game = Game {
            headers,
            ..Game::default()
        };
        game.nodes[0].pos = start.pos.clone();
        if start.custom {
            game.set_header("SetUp", "1".to_string());
            game.set_header("FEN", start.fen.clone());
            if start.chess960 {
                game.set_header("Variant", "Chess960".to_string());
            }
        }
        if entry.deleted {
            game.errors
                .push("This game is marked deleted in the ChessBase database.".to_string());
        }

        let mode = record.head[0] & 0x3f;
        let mut moves_decoded = false;
        if mode == 11 {
            game.errors.push(
                "ChessBase mode 11 move encoding is not supported; only the game header is shown."
                    .to_string(),
            );
        } else if let Err(err) =
            moves::decode_moves(&mut game, mode, &record.bytes[consumed..], start.chess960)
        {
            game.errors
                .push(format!("CBH game {} move stream: {err:#}", entry.id));
        } else {
            moves_decoded = true;
        }

        if annotations_offset != 0 {
            match &self.cba_path {
                Some(path) => match read_record(
                    path,
                    annotations_offset,
                    MAX_ANNOTATION_RECORD,
                    "annotation",
                    entry.id,
                    10,
                ) {
                    Ok(record) => {
                        if let Err(err) = annotations::apply(&record, entry.id, &mut game) {
                            game.errors
                                .push(format!("CBH game {} annotations: {err:#}", entry.id));
                        }
                    }
                    Err(err) => game.errors.push(format!(
                        "CBH game {} annotations could not be read: {err:#}",
                        entry.id
                    )),
                },
                None => game.errors.push(format!(
                    "CBH game {} points at annotations but the .cba file is missing",
                    entry.id
                )),
            }
        }

        if moves_decoded && game.nodes.len() == 1 {
            game.set_header("PlyCount", "0".to_string());
        }

        Ok(game)
    }

    /// One-based text record ids (flag bit 1 set).
    pub fn text_record_ids(&self) -> Vec<u32> {
        self.text_ids.clone()
    }

    /// Tournament id of a text record (`be_u24` at header bytes 7..10).
    pub fn text_tournament_id(&self, record_id: u32) -> Result<u32> {
        let bytes = fs::read(&self.cbh_path)
            .with_context(|| format!("reading {}", self.cbh_path.display()))?;
        let at = record_id as usize * RECORD_SIZE;
        let record = bytes
            .get(at..at + RECORD_SIZE)
            .with_context(|| format!("ChessBase text record {record_id} is truncated"))?;
        if record[0] & 2 == 0 {
            bail!("ChessBase record {record_id} is not a text record");
        }
        Ok(be_u24(&record[7..10]))
    }

    /// Raw guiding-text payload of a text record: `(titles, html_blocks)`.
    pub fn guiding_text(&self, record_id: u32) -> Result<(Vec<String>, Vec<String>)> {
        let bytes = fs::read(&self.cbh_path)
            .with_context(|| format!("reading {}", self.cbh_path.display()))?;
        let at = record_id as usize * RECORD_SIZE;
        let record = bytes
            .get(at..at + RECORD_SIZE)
            .with_context(|| format!("ChessBase text record {record_id} is truncated"))?;
        if record[0] & 2 == 0 {
            bail!("ChessBase record {record_id} is not a text record");
        }
        let short = be_u32(&record[1..5]);
        let offset = match &self.wide {
            Some(wide) => wide.text_offset(record_id, short)?,
            None => u64::from(short),
        };
        let bytes = fs::read(&self.cbg_path)
            .with_context(|| format!("reading {}", self.cbg_path.display()))?;
        parse_classic_guiding_record(&bytes, offset)
    }

    /// Games linked to the player named `name`, via the `.cbp` AVL search
    /// and the record's `game_count`/`first_game_id` tail.
    /// Returns `(entity_id, game_count, first_game_id)`.
    pub fn player_entity_stats(&self, name: &str) -> Option<(u32, u32, u32)> {
        let entities = Entities::open(&self.cbh_path.with_extension("")).ok()?;
        let id = entities.find_player_id(name)?;
        Some((
            id,
            entities.player_game_count(id),
            entities.player_first_game(id),
        ))
    }

    /// Games linked to the tournament named `title`.
    pub fn event_entity_stats(&self, title: &str) -> Option<(u32, u32)> {
        let entities = Entities::open(&self.cbh_path.with_extension("")).ok()?;
        let id = entities.find_tournament_id(title)?;
        Some((id, entities.tournament_game_count(id)))
    }

    /// Text record whose tournament matches `(event, site)`, if any.
    pub fn find_text_for_event(&self, event: &str, site: &str) -> Option<u32> {
        let entities = Entities::open(&self.cbh_path.with_extension("")).ok()?;
        let want_event = event.trim().to_lowercase();
        let want_site = site.trim().to_lowercase();
        if want_event.is_empty() || want_event == "?" {
            return None;
        }
        for &id in &self.text_ids {
            let Ok(tournament_id) = self.text_tournament_id(id) else {
                continue;
            };
            let title = entities.tournament_title(tournament_id).to_lowercase();
            let place = entities.tournament_place(tournament_id).to_lowercase();
            if title.trim() == want_event && (want_site.is_empty() || place.trim() == want_site) {
                return Some(id);
            }
        }
        None
    }
}

impl GameLibrary for CbhLibrary {
    fn path(&self) -> &Path {
        CbhLibrary::path(self)
    }

    fn file_len(&self) -> u64 {
        CbhLibrary::file_len(self)
    }

    fn len(&self) -> usize {
        CbhLibrary::len(self)
    }

    fn indexed_in(&self) -> Duration {
        CbhLibrary::indexed_in(self)
    }

    fn games(&self) -> &[IndexedGame] {
        CbhLibrary::games(self)
    }

    fn load(&self, index: usize) -> Result<Game> {
        CbhLibrary::load(self, index)
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
            .with_context(|| format!("ChessBase game index {index} is out of range"))?;
        let (moves_offset, _) = match &self.wide {
            Some(wide) => wide.offsets(entry.id, (entry.moves_offset, entry.annotations_offset))?,
            None => (
                u64::from(entry.moves_offset),
                u64::from(entry.annotations_offset),
            ),
        };
        self.with_cbg_record(moves_offset, entry.id, |head0, bytes| {
            let (start, consumed) = moves::decode_start(head0, bytes)
                .with_context(|| format!("decoding start of ChessBase game {}", entry.id))?;
            let mode = head0 & 0x3f;
            if mode == 11 {
                return Ok(());
            }
            fast_index::stream_cbh_mainline(
                &start.pos,
                mode,
                &bytes[consumed..],
                start.chess960,
                max_plies,
                consumer,
            )
        })?;
        Ok(true)
    }

    fn game_record_meta(&self, index: usize) -> GameRecordMeta {
        let Some(entry) = self.entries.get(index) else {
            return GameRecordMeta::default();
        };
        GameRecordMeta {
            white_elo: entry.white_elo,
            black_elo: entry.black_elo,
            year: entry.year,
            result: entry.result,
            eco_code: entry.eco_code,
            medals: entry.medals,
            vcs_flags: entry.vcs_flags,
            annotated: entry.annotated,
            deleted: entry.deleted,
        }
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
            .with_context(|| format!("ChessBase game index {index} is out of range"))?;
        let (moves_offset, _) = match &self.wide {
            Some(wide) => wide.offsets(entry.id, (entry.moves_offset, entry.annotations_offset))?,
            None => (
                u64::from(entry.moves_offset),
                u64::from(entry.annotations_offset),
            ),
        };
        self.with_cbg_record(moves_offset, entry.id, |head0, bytes| {
            let (start, consumed) = moves::decode_start(head0, bytes)
                .with_context(|| format!("decoding start of ChessBase game {}", entry.id))?;
            let mode = head0 & 0x3f;
            if mode == 11 {
                let _ = visitor(0, &start.pos);
                return Ok(true);
            }
            fast_index::scan_cbh_mainline_positions(
                &start.pos,
                mode,
                &bytes[consumed..],
                start.chess960,
                max_ply,
                visitor,
            )?;
            Ok(true)
        })
    }

    fn source_meta(&self) -> Result<SourceMeta> {
        let mut parts = vec![SourceMeta::read(&self.cbh_path)?];
        if self.path != self.cbh_path {
            // Include the archive itself so replacing a `.cbv` invalidates the
            // position cache even when an old extraction happens to survive.
            parts.push(SourceMeta::read(&self.path)?);
        }
        parts.push(SourceMeta::read(&self.cbg_path)?);
        for path in &self.entity_paths {
            parts.push(SourceMeta::read(path)?);
        }
        Ok(SourceMeta::combine(&parts))
    }

    fn filter(&self, needle: &str) -> Vec<usize> {
        crate::library::filter_games(&self.games, needle)
    }

    fn guiding_text_ids(&self) -> Vec<u32> {
        CbhLibrary::text_record_ids(self)
    }

    fn guiding_text_title(&self, id: u32) -> Option<String> {
        let tournament = self.text_tournament_id(id).ok()?;
        let stem = self.cbh_path.with_extension("");
        let entities = Entities::open(&stem).ok()?;
        let title = entities.tournament_title(tournament);
        let place = entities.tournament_place(tournament);
        Some(format!("{title} · {place}"))
    }

    fn guiding_text_html(&self, id: u32) -> Option<Vec<String>> {
        let (titles, blocks) = self.guiding_text(id).ok()?;
        let mut html = titles;
        html.extend(blocks);
        (!html.is_empty()).then_some(html)
    }

    fn find_guiding_text(&self, event: &str, site: &str) -> Option<u32> {
        CbhLibrary::find_text_for_event(self, event, site)
    }

    fn event_game_indices(&self, event: &str, site: &str, year: Option<u32>) -> Option<Vec<usize>> {
        // Resolve the tournament through the `.cbt` entity AVL search first;
        // without a matching entity there is nothing to accelerate.
        let stem = self.cbh_path.with_extension("");
        let entities = Entities::open(&stem).ok()?;
        entities.find_tournament_id(event)?;
        let want_site = site.trim().to_lowercase();
        let mut out = Vec::new();
        for (index, game) in self.games.iter().enumerate() {
            if !game.event().eq_ignore_ascii_case(event.trim()) {
                continue;
            }
            if !want_site.is_empty()
                && !game
                    .header("Site")
                    .unwrap_or("?")
                    .eq_ignore_ascii_case(site.trim())
            {
                continue;
            }
            if let Some(year) = year
                && !game.date().starts_with(&year.to_string())
            {
                continue;
            }
            out.push(index);
        }
        if out.is_empty() { None } else { Some(out) }
    }
}

struct CbgRecord {
    head: [u8; 14],
    head_len: usize,
    bytes: Vec<u8>,
}

/// Read a `.cbg`/`.cba` record: a four- or fourteen-byte head selected by the
/// caller, then the body.  `head_len` is 4 for moves, 14 for annotations.
/// The returned bytes exclude the head.
fn read_record(
    path: &Path,
    offset: u64,
    max_size: usize,
    kind: &str,
    id: u32,
    minimum_offset: u64,
) -> Result<CbgRecord> {
    let mut file = File::open(path)
        .with_context(|| format!("opening ChessBase {kind} file {}", path.display()))?;
    let len = file.metadata()?.len();
    if offset < minimum_offset || offset >= len {
        bail!(
            "ChessBase game {id}'s {kind} offset {offset} is outside {} ({} bytes)",
            path.display(),
            len
        );
    }
    file.seek(SeekFrom::Start(offset))
        .with_context(|| format!("seeking to ChessBase {kind} offset {offset}"))?;
    let mut head = [0u8; 14];
    let head_len = if kind == "annotation" { 14 } else { 4 };
    file.read_exact(&mut head[..head_len])
        .with_context(|| format!("reading ChessBase {kind} head at {offset}"))?;
    let size = if head_len == 4 {
        (u32::from(head[1]) << 16 | u32::from(head[2]) << 8 | u32::from(head[3])) as usize
    } else {
        u32::from_be_bytes([head[10], head[11], head[12], head[13]]) as usize
    };
    if size < head_len {
        bail!("ChessBase {kind} record at {offset} has size {size}, shorter than its head");
    }
    if size > max_size {
        bail!(
            "ChessBase {kind} record at {offset} is {size} bytes, over the {max_size}-byte limit"
        );
    }
    let end = offset
        .checked_add(size as u64)
        .context("ChessBase record offset overflow")?;
    if end > len {
        bail!(
            "ChessBase {kind} record at {offset} runs past the end of {} ({} bytes)",
            path.display(),
            len
        );
    }
    let mut body = vec![0u8; size - head_len];
    file.read_exact(&mut body)
        .with_context(|| format!("reading ChessBase {kind} record at {offset}"))?;
    Ok(CbgRecord {
        head,
        head_len,
        bytes: body,
    })
}

fn base_headers(
    record: &[u8],
    entities: &Entities,
    strings: &mut StringPool,
) -> Vec<(SharedStr, SharedStr)> {
    let white_id = be_u24(&record[9..12]);
    let black_id = be_u24(&record[12..15]);
    let tournament_id = be_u24(&record[15..18]);
    let annotator_id = be_u24(&record[18..21]);
    let source_id = be_u24(&record[21..24]);
    let date = be_u24(&record[24..27]);
    let result = match record[27] {
        0 | 4 => "0-1",
        1 | 5 => "1/2-1/2",
        2 | 6 => "1-0",
        _ => "*",
    };
    let round = record[29];
    let subround = record[30];
    let white_elo = be_u16(&record[31..33]);
    let black_elo = be_u16(&record[33..35]);
    let eco = be_u16(&record[35..37]);

    let mut headers = Vec::with_capacity(12);
    headers.push((
        strings.intern("Event"),
        strings.intern(entities.tournament_title(tournament_id)),
    ));
    headers.push((
        strings.intern("Site"),
        strings.intern(entities.tournament_place(tournament_id)),
    ));
    headers.push((strings.intern("Date"), strings.intern(date_string(date))));
    headers.push((
        strings.intern("Round"),
        strings.intern(round_string(round, subround)),
    ));
    headers.push((
        strings.intern("White"),
        strings.intern(entities.player_name(white_id)),
    ));
    headers.push((
        strings.intern("Black"),
        strings.intern(entities.player_name(black_id)),
    ));
    headers.push((strings.intern("Result"), strings.intern(result)));
    if let Some(eco) = eco_string(eco) {
        headers.push((strings.intern("ECO"), strings.intern(eco)));
    }
    if white_elo > 0 {
        headers.push((
            strings.intern("WhiteElo"),
            strings.intern(white_elo.to_string()),
        ));
    }
    if black_elo > 0 {
        headers.push((
            strings.intern("BlackElo"),
            strings.intern(black_elo.to_string()),
        ));
    }
    if let Some(annotator) = entities.annotator_name(annotator_id) {
        headers.push((strings.intern("Annotator"), strings.intern(annotator)));
    }
    if let Some(source) = entities.source_name(source_id) {
        headers.push((strings.intern("Source"), strings.intern(source)));
    }
    headers
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

fn round_string(round: u8, subround: u8) -> String {
    match (round, subround) {
        (0, 0) => "?".to_string(),
        (0, sub) => format!("?.{sub}"),
        (round, 0) => round.to_string(),
        (round, sub) => format!("{round}.{sub}"),
    }
}

pub(crate) fn eco_string(field: u16) -> Option<String> {
    if !(128..=64127).contains(&field) {
        return None;
    }
    let code = field / 128 - 1;
    if code > 499 {
        return None;
    }
    let letter = b'A' + (code / 100) as u8;
    Some(format!("{}{:02}", letter as char, code % 100))
}

/// Parse a classic guiding-text record at `offset` in the `.cbg` bytes.
///
/// Layout: `total_len = be_u32(b[0..4]) & 0x7fffffff`, `b[4..6] == [3, 0]`,
/// `title_count = le_u16(b[6..8])`, then `title_count * (lang le_u16, len
/// le_u16, bytes)`, then `0x01`, `block_count le_u32`, then
/// `(lang u8, html_len le_u32, html bytes)` per block.
pub(crate) fn parse_classic_guiding_record(
    cbg: &[u8],
    offset: u64,
) -> Result<(Vec<String>, Vec<String>)> {
    let offset = offset as usize;
    let head = cbg
        .get(offset..offset.saturating_add(8))
        .with_context(|| format!("ChessBase text record at {offset} is truncated"))?;
    if head.len() < 8 {
        bail!("ChessBase text record at {offset} is truncated");
    }
    let total_len =
        (u32::from_be_bytes([head[0], head[1], head[2], head[3]]) & 0x7fff_ffff) as usize;
    if head[4] != 3 || head[5] != 0 {
        bail!("ChessBase text record at {offset} has the wrong kind");
    }
    let end = offset
        .checked_add(4 + total_len)
        .context("text record size overflow")?;
    let bytes = cbg.get(offset..end).with_context(|| {
        format!("ChessBase text record at {offset} runs past the end of the file")
    })?;
    let mut at = 8usize; // past total_len + [3,0] + title_count
    let title_count = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
    let mut titles = Vec::with_capacity(title_count);
    for _ in 0..title_count {
        let chunk = bytes
            .get(at..at + 4)
            .with_context(|| format!("ChessBase text title at {offset}+{at} is truncated"))?;
        let _lang = u16::from_le_bytes([chunk[0], chunk[1]]);
        let len = u16::from_le_bytes([chunk[2], chunk[3]]) as usize;
        at += 4;
        let text = bytes.get(at..at + len).with_context(|| {
            format!("ChessBase text title at {offset}+{at} runs past the record")
        })?;
        titles.push(decode_text(text));
        at += len;
    }
    let marker = bytes
        .get(at)
        .copied()
        .with_context(|| format!("ChessBase text blocks at {offset}+{at} are truncated"))?;
    if marker != 0x01 {
        bail!("ChessBase text record at {offset} has marker {marker:#04x}, expected 0x01");
    }
    at += 1;
    let chunk = bytes
        .get(at..at + 4)
        .with_context(|| format!("ChessBase text block count at {offset}+{at} is truncated"))?;
    let block_count = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as usize;
    at += 4;
    let mut blocks = Vec::with_capacity(block_count);
    for _ in 0..block_count {
        let lang = bytes
            .get(at)
            .copied()
            .with_context(|| format!("ChessBase text block at {offset}+{at} is truncated"))?;
        let _ = lang;
        at += 1;
        let chunk = bytes.get(at..at + 4).with_context(|| {
            format!("ChessBase text block length at {offset}+{at} is truncated")
        })?;
        let len = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as usize;
        at += 4;
        let html = bytes.get(at..at + len).with_context(|| {
            format!("ChessBase text block at {offset}+{at} runs past the record")
        })?;
        blocks.push(decode_text(html));
        at += len;
    }
    Ok((titles, blocks))
}

fn be_u16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

fn be_u24(bytes: &[u8]) -> u32 {
    (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2])
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Resolve a companion file case-insensitively, like ChessBase's own file
/// set (the stem itself may have any extension).
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

fn decode_text(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    let field = &field[..end];
    match std::str::from_utf8(field) {
        Ok(text) => return text.to_owned(),
        Err(error) if error.error_len().is_none() => {
            let valid = &field[..error.valid_up_to()];
            if valid.iter().any(|&b| b >= 0x80) {
                return String::from_utf8_lossy(valid).into_owned();
            }
        }
        Err(_) => {}
    }
    field
        .iter()
        .map(|&b| match b {
            0x80..=0x9f => CP1252_HIGH[(b - 0x80) as usize],
            _ => b as char,
        })
        .collect()
}

struct EntityFile {
    bytes: Vec<u8>,
    header: usize,
    record: usize,
    count: usize,
}

impl EntityFile {
    fn open(path: &Path, min_data: i32) -> Result<Option<Self>> {
        if !path.is_file() {
            return Ok(None);
        }
        let bytes = fs::read(path)
            .with_context(|| format!("reading ChessBase names {}", path.display()))?;
        if bytes.len() < 28 {
            bail!(
                "{} is shorter than a ChessBase entity header",
                path.display()
            );
        }
        let magic = i32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        if magic != ENTITY_MAGIC {
            bail!("{} has the wrong ChessBase entity magic", path.display());
        }
        let data = i32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        if !(min_data..=MAX_ENTITY_DATA).contains(&data) {
            bail!("{} states an invalid record size {data}", path.display());
        }
        let extra = i32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
        if extra != 0 && extra != 4 {
            bail!("{} has {extra} extra header bytes", path.display());
        }
        let header = 28 + extra as usize;
        let record = 9 + data as usize;
        let count = bytes.len().saturating_sub(header) / record;
        Ok(Some(Self {
            bytes,
            header,
            record,
            count,
        }))
    }

    fn data(&self, id: u32) -> Option<&[u8]> {
        let id = id as usize;
        if id >= self.count {
            return None;
        }
        let at = self.header.checked_add(id.checked_mul(self.record)?)?;
        let record = self.bytes.get(at..at + self.record)?;
        let deleted = i32::from_le_bytes([record[0], record[1], record[2], record[3]]);
        (deleted != ENTITY_DELETED).then_some(&record[9..])
    }

    /// The full entity record (9-byte AVL/prefix plus data) for `id`.
    fn record_bytes(&self, id: u32) -> Option<&[u8]> {
        let id = id as usize;
        if id >= self.count {
            return None;
        }
        let at = self.header.checked_add(id.checked_mul(self.record)?)?;
        let record = self.bytes.get(at..at + self.record)?;
        let deleted = i32::from_le_bytes([record[0], record[1], record[2], record[3]]);
        (deleted != ENTITY_DELETED).then_some(record)
    }

    /// Number of games linked to one entity:
    /// `le_u32(record[record_len - 8 .. -4])`.
    pub fn game_count(&self, id: u32) -> u32 {
        self.record_bytes(id)
            .filter(|record| record.len() == self.record && record.len() >= 8)
            .map(|record| {
                let at = record.len() - 8;
                u32::from_le_bytes([record[at], record[at + 1], record[at + 2], record[at + 3]])
            })
            .unwrap_or(0)
    }

    /// First game id linked to one entity: `le_u32(record[-4..])`.
    pub fn first_game_id(&self, id: u32) -> u32 {
        self.record_bytes(id)
            .filter(|record| record.len() == self.record && record.len() >= 4)
            .map(|record| {
                let at = record.len() - 4;
                u32::from_le_bytes([record[at], record[at + 1], record[at + 2], record[at + 3]])
            })
            .unwrap_or(0)
    }

    /// AVL root entity id, from `header[16..20]` as little-endian `i32`.
    /// Negative values mean the tree is empty.
    pub fn root(&self) -> i32 {
        self.bytes
            .get(16..20)
            .map(|slot| i32::from_le_bytes([slot[0], slot[1], slot[2], slot[3]]))
            .unwrap_or(-1)
    }

    /// AVL left child of one entity record: `i32 record[0..4]`.
    pub fn left(&self, id: u32) -> Option<i32> {
        self.record_bytes(id)
            .map(|record| i32::from_le_bytes([record[0], record[1], record[2], record[3]]))
    }

    /// AVL right child of one entity record: `i32 record[4..8]`.
    pub fn right(&self, id: u32) -> Option<i32> {
        self.record_bytes(id)
            .map(|record| i32::from_le_bytes([record[4], record[5], record[6], record[7]]))
    }

    /// Walk the AVL tree from `root` (`header[16..20]`), comparing each
    /// record's data slice with `cmp`. Returns the matching entity id.
    /// The walk is guarded with a visited set and capped at `count + 1`
    /// steps; a broken tree yields `None` rather than looping.
    pub fn avl_search(&self, mut cmp: impl FnMut(&[u8]) -> std::cmp::Ordering) -> Option<u32> {
        use std::cmp::Ordering;
        let mut current = self.root();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..self.count + 1 {
            if current < 0 {
                return None;
            }
            let id = current as u32;
            if !seen.insert(id) {
                return None;
            }
            let data = self.data(id)?;
            match cmp(data) {
                Ordering::Equal => return Some(id),
                Ordering::Less => current = self.right(id)?,
                Ordering::Greater => current = self.left(id)?,
            }
        }
        None
    }

    fn field(&self, id: u32, start: usize, end: usize) -> Option<String> {
        let data = self.data(id)?;
        Some(decode_text(data.get(start..end)?))
    }
}

struct Entities {
    players: Option<EntityFile>,
    tournaments: Option<EntityFile>,
    annotators: Option<EntityFile>,
    sources: Option<EntityFile>,
    players_path: Option<PathBuf>,
    tournaments_path: Option<PathBuf>,
    annotators_path: Option<PathBuf>,
    sources_path: Option<PathBuf>,
}

impl Entities {
    fn open(stem: &Path) -> Result<Self> {
        let players_path = sibling(stem, ".cbp");
        let tournaments_path = sibling(stem, ".cbt");
        let annotators_path = sibling(stem, ".cbc");
        let sources_path = sibling(stem, ".cbs");
        let players = EntityFile::open(&players_path, 50)?;
        let tournaments = EntityFile::open(&tournaments_path, 0x4a)?;
        let annotators = EntityFile::open(&annotators_path, 45)?;
        let sources = EntityFile::open(&sources_path, 25)?;
        let players_path = players.as_ref().map(|_| players_path);
        let tournaments_path = tournaments.as_ref().map(|_| tournaments_path);
        let annotators_path = annotators.as_ref().map(|_| annotators_path);
        let sources_path = sources.as_ref().map(|_| sources_path);
        Ok(Self {
            players,
            tournaments,
            annotators,
            sources,
            players_path,
            tournaments_path,
            annotators_path,
            sources_path,
        })
    }

    fn player_name(&self, id: u32) -> String {
        let Some(file) = &self.players else {
            return "?".to_string();
        };
        let Some(last) = file.field(id, 0, 30) else {
            return "?".to_string();
        };
        let Some(first) = file.field(id, 30, 50) else {
            return "?".to_string();
        };
        if last.is_empty() && first.is_empty() {
            return "?".to_string();
        }
        let mut name = last;
        if !first.is_empty() {
            name.push_str(", ");
            name.push_str(&first);
            if (1..=2).contains(&first.len())
                && first
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            {
                name.push('.');
            }
        }
        name
    }

    fn tournament_title(&self, id: u32) -> String {
        self.tournaments
            .as_ref()
            .and_then(|file| file.field(id, 0, 40))
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "?".to_string())
    }

    fn tournament_place(&self, id: u32) -> String {
        self.tournaments
            .as_ref()
            .and_then(|file| file.field(id, 40, 70))
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "?".to_string())
    }

    fn annotator_name(&self, id: u32) -> Option<String> {
        self.annotators
            .as_ref()
            .and_then(|file| file.field(id, 0, 45))
            .filter(|text| !text.is_empty())
    }

    /// Entity id of the player named `last, first`, searched through the
    /// `.cbp` AVL tree first with a linear fallback for trees whose key
    /// order differs. Returns `None` when no player file is present.
    pub fn find_player_id(&self, name: &str) -> Option<u32> {
        let file = self.players.as_ref()?;
        let want = name.trim().to_lowercase();
        if let Some(id) = file.avl_search(|data| {
            let last = decode_text(data.get(0..30).unwrap_or(&[])).to_lowercase();
            let first = decode_text(data.get(30..50).unwrap_or(&[])).to_lowercase();
            let full = if first.is_empty() {
                last.clone()
            } else {
                format!("{last}, {first}")
            };
            full.cmp(&want)
        }) {
            return Some(id);
        }
        // Linear fallback: compare the display name.
        (0..file.count as u32).find(|&id| self.player_name(id).to_lowercase() == want)
    }

    /// Tournament entity id whose title matches, via the `.cbt` AVL tree
    /// with a linear fallback.
    pub fn find_tournament_id(&self, title: &str) -> Option<u32> {
        let file = self.tournaments.as_ref()?;
        let want = title.trim().to_lowercase();
        if let Some(id) = file.avl_search(|data| {
            decode_text(data.get(0..40).unwrap_or(&[]))
                .to_lowercase()
                .cmp(&want)
        }) {
            return Some(id);
        }
        (0..file.count as u32).find(|&id| self.tournament_title(id).to_lowercase() == want)
    }

    /// Games linked to one player entity (`le_u32` tail of the `.cbp`
    /// record), or 0 when unknown.
    pub fn player_game_count(&self, id: u32) -> u32 {
        self.players
            .as_ref()
            .map(|file| file.game_count(id))
            .unwrap_or(0)
    }

    /// First game id linked to one player entity.
    pub fn player_first_game(&self, id: u32) -> u32 {
        self.players
            .as_ref()
            .map(|file| file.first_game_id(id))
            .unwrap_or(0)
    }

    /// Games linked to one tournament entity.
    pub fn tournament_game_count(&self, id: u32) -> u32 {
        self.tournaments
            .as_ref()
            .map(|file| file.game_count(id))
            .unwrap_or(0)
    }

    fn source_name(&self, id: u32) -> Option<String> {
        self.sources
            .as_ref()
            .and_then(|file| file.field(id, 0, 25))
            .filter(|text| !text.is_empty())
    }
}

/// The optional `.cbj` file: 64-bit offsets for databases whose `.cbg` or
/// `.cba` is larger than 4 GiB.  Below that the 32-bit `.cbh` offsets are
/// complete, so the file is ignored.
struct Wide {
    path: PathBuf,
    record: u64,
    count: u32,
}

impl Wide {
    fn open_if_needed(stem: &Path, cbg_len: u64, cba_len: u64) -> Result<Option<Self>> {
        if cbg_len < (1u64 << 32) && cba_len < (1u64 << 32) {
            return Ok(None);
        }
        let path = sibling(stem, ".cbj");
        if !path.is_file() {
            bail!(
                "the ChessBase game or annotation file is over 4 GiB but {} is missing",
                path.display()
            );
        }
        let mut file = File::open(&path)
            .with_context(|| format!("opening ChessBase wide offsets {}", path.display()))?;
        let mut header = [0u8; 32];
        file.read_exact(&mut header)?;
        let record = i32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        let count = i32::from_le_bytes([header[8], header[9], header[10], header[11]]);
        if !(0x26..=4096).contains(&record) {
            bail!(
                "{} states an invalid wide record size {record}",
                path.display()
            );
        }
        Ok(Some(Self {
            path,
            record: record as u64,
            count: count.max(0) as u32,
        }))
    }

    fn offsets(&self, id: u32, short: (u32, u32)) -> Result<(u64, u64)> {
        if id == 0 || id > self.count {
            return Ok((u64::from(short.0), u64::from(short.1)));
        }
        let mut file = File::open(&self.path)
            .with_context(|| format!("opening ChessBase wide offsets {}", self.path.display()))?;
        let at = 32 + self.record * u64::from(id - 1);
        file.seek(SeekFrom::Start(at))?;
        let mut record = [0u8; 0x26];
        file.read_exact(&mut record)?;
        let long = |offset: usize| {
            i64::from_be_bytes([
                record[offset],
                record[offset + 1],
                record[offset + 2],
                record[offset + 3],
                record[offset + 4],
                record[offset + 5],
                record[offset + 6],
                record[offset + 7],
            ])
        };
        let moves = long(0x1e);
        let annotations = long(0x0c).max(0);
        let agrees =
            |wide: i64, short: u32| wide >= 0 && wide as u64 & 0xffff_ffff == u64::from(short);
        if !agrees(moves, short.0) || !agrees(annotations, short.1) {
            bail!("the .cbj and .cbh offsets for ChessBase game {id} disagree");
        }
        Ok((moves as u64, annotations as u64))
    }

    fn text_offset(&self, id: u32, short: u32) -> Result<u64> {
        let (moves, _) = self.offsets(id, (short, 0))?;
        Ok(moves)
    }
}

#[cfg(test)]
mod tests;
