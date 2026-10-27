//! Native reader for classic ChessBase databases (`.cbh` + `.cbg` and their
//! companion files).
//!
//! The classic format is the multi-file database ChessBase used before CBH2:
//! a fixed 46-byte header per game (`.cbh`), a move record per game (`.cbg`),
//! optional annotations (`.cba`), and small name/title files for players,
//! tournaments, annotators and sources.  The format is read-only here: Brett
//! indexes the headers when a database is opened and decodes a game's moves,
//! variations, comments and NAGs only when it is displayed.
//!
//! The implementation is written from the published on-disk layout and the
//! independent format work in the MIT-licensed `cbvault` project.  It does
//! not link to or copy a converter.

mod annotations;
mod moves;
mod tables;

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::domain::Game;
use crate::library::{GameLibrary, IndexedGame, SharedStr, SourceMeta, StringPool};

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
}

/// A classic multi-file database opened through [`GameLibrary`].
pub struct CbhLibrary {
    /// The path shown to the user.  For a `.cbv` archive this is the archive
    /// itself; for a classic database it is the `.cbh`.
    path: PathBuf,
    cbh_path: PathBuf,
    cbg_path: PathBuf,
    cba_path: Option<PathBuf>,
    entity_paths: Vec<PathBuf>,
    wide: Option<Wide>,
    file_len: u64,
    entries: Vec<Entry>,
    games: Vec<IndexedGame>,
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
        let mut strings = StringPool::default();
        for index in 0..total {
            let id = u32::try_from(index + 1).context("ChessBase record count is too large")?;
            let at = (index + 1) * RECORD_SIZE;
            let record = &cbh[at..at + RECORD_SIZE];
            let flags = record[0];
            // Bit 0 marks a game/text record; bit 1 marks a guiding text.  A
            // zero bit is an unused slot past the last game in some
            // databases.  Deleted games keep bit 0 and are still readable.
            if flags & 1 == 0 || flags & 2 != 0 {
                continue;
            }
            let moves_offset = be_u32(&record[1..5]);
            let annotations_offset = be_u32(&record[5..9]);
            let deleted = flags & 0x80 != 0;
            let headers = base_headers(record, &entities, &mut strings);
            entries.push(Entry {
                id,
                moves_offset,
                annotations_offset,
                deleted,
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

        Ok(Self {
            path: cbh_path.clone(),
            cbh_path,
            cbg_path,
            cba_path,
            entity_paths,
            wide,
            file_len,
            entries,
            games,
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

fn eco_string(field: u16) -> Option<String> {
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
}

#[cfg(test)]
mod tests;
