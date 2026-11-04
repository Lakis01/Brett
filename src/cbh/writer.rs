//! Native writer for classic ChessBase databases (`.cbh`, `.cbg`, `.cba`,
//! `.cbp`, `.cbt`, `.cbc`, `.cbs`, and optional `.cbj`).
//!
//! This is an independent implementation written from the published classic
//! on-disk layout and the MIT-licensed `cbvault` format specification. No
//! GPL converter code is linked or copied.
//!
//! Games are streamed to temporary `.cbh`, `.cbg`, and `.cba` files while
//! player, tournament, annotator, and source names are interned in memory.
//! On completion, the existing companion files at the target stem are moved
//! aside to backup paths and replaced atomically, rolling back on error.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::chess::{
    CastlingMode, CastlingSide, Chess, Color, EnPassantMode, Fen, Move, Position, Role, SanPlus,
    UciMove,
};
use crate::domain::{Game, PgnEval, Ply};

use super::moves::{PieceLists, from_stored_square, square, to_stored_square};
use super::{ENTITY_MAGIC, MAX_ANNOTATION_RECORD, MAX_MOVE_RECORD, RECORD_SIZE, sibling, tables};

/// The seven mandatory files of a classic ChessBase database.
pub const CORE_EXTENSIONS: [&str; 7] = ["cbh", "cbg", "cba", "cbp", "cbt", "cbc", "cbs"];

/// All companion extensions that belong to a classic ChessBase stem (core
/// files, optional wide index, and derived ChessBase index/booster files).
pub const ALL_EXTENSIONS: [&str; 19] = [
    "cbh", "cbg", "cba", "cbp", "cbt", "cbc", "cbs", "cbj", "cbe", "cbl", "cbtt", "flags", "cbm",
    "cit", "cib", "cit2", "cib2", "cbb", "cbgi",
];

const CBG_FILE_HEADER_LEN: usize = 26;
const CBA_FILE_HEADER_LEN: usize = 26;
const CBJ_HEADER_LEN: usize = 32;
const CBJ_RECORD_LEN: usize = 0x26;
const CBA_HEAD_LEN: usize = 14;
const CBA_ITEM_HEAD_LEN: usize = 6;
const CBA_MARK: [u8; 4] = [1, 0, 0x0e, 0x0e];
const MAX_ENTITY_ID: usize = 0x00ff_ffff;
const WIDE_THRESHOLD: u64 = 1u64 << 32;

/// Summary of a completed classic CBH export.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CbhWriteReport {
    pub games: usize,
    pub bytes: u64,
}

/// Options controlling how move streams and wide offsets are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CbhWriteOptions {
    /// Explicit `.cbg` move encoding mode (`0`, `4`, `5`, or `10`). When
    /// `None`, standard games use mode `0` and Chess960 games use mode `10`.
    pub mode: Option<u8>,
    /// Use one-byte `PieceLists` codes in compact modes (`0`, `4`, `10`) when
    /// a move is representable, falling back to the `235` two-byte escape for
    /// promotions and fourth-or-later pieces of a kind.
    pub use_compact_codes: bool,
    /// Always emit the `.cbj` 64-bit offset file even when `.cbg` and `.cba`
    /// are smaller than 4 GiB.
    pub force_wide: bool,
}

impl Default for CbhWriteOptions {
    fn default() -> Self {
        Self {
            mode: None,
            use_compact_codes: true,
            force_wide: false,
        }
    }
}

/// Return the core companion file paths (`.cbh`, `.cbg`, `.cba`, `.cbp`,
/// `.cbt`, `.cbc`, `.cbs`) for a `.cbh` path or bare stem.
pub fn core_companion_files(path: &Path) -> [PathBuf; 7] {
    let stem = normalize_stem(path);
    CORE_EXTENSIONS.map(|ext| stem.with_extension(ext))
}

/// Return all existing classic CBH companion files sharing `path`'s stem.
pub fn existing_companion_files(path: &Path) -> Vec<PathBuf> {
    let stem = normalize_stem(path);
    let mut out = Vec::new();
    for ext in ALL_EXTENSIONS {
        let candidate = sibling(&stem, &format!(".{ext}"));
        if candidate.exists() && !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// Write a classic ChessBase database at `path` (which must end in `.cbh`).
pub fn write_database<I>(path: &Path, games: I) -> Result<CbhWriteReport>
where
    I: IntoIterator<Item = Result<Game>>,
{
    write_database_with_options(path, CbhWriteOptions::default(), games)
}

/// Write a classic ChessBase database at `path` with explicit encoding options.
pub fn write_database_with_options<I>(
    path: &Path,
    options: CbhWriteOptions,
    games: I,
) -> Result<CbhWriteReport>
where
    I: IntoIterator<Item = Result<Game>>,
{
    let mut writer = CbhWriter::create(path, options)?;
    for game in games {
        writer.add_game(&game?)?;
    }
    writer.finish()
}

fn normalize_stem(path: &Path) -> PathBuf {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext)
            if ALL_EXTENSIONS
                .iter()
                .any(|candidate| ext.eq_ignore_ascii_case(candidate)) =>
        {
            path.with_extension("")
        }
        _ => path.to_path_buf(),
    }
}

struct CbhWriter {
    stem: PathBuf,
    options: CbhWriteOptions,
    unique: String,
    cbh_tmp: PathBuf,
    cbg_tmp: PathBuf,
    cba_tmp: PathBuf,
    cbp_tmp: PathBuf,
    cbt_tmp: PathBuf,
    cbc_tmp: PathBuf,
    cbs_tmp: PathBuf,
    cbj_tmp: PathBuf,
    cbh_file: Option<BufWriter<File>>,
    cbg_file: Option<BufWriter<File>>,
    cba_file: Option<BufWriter<File>>,
    cbg_offset: u64,
    cba_offset: u64,
    offsets: Vec<(u64, u64)>,
    players: EntityInterner<([u8; 30], [u8; 20])>,
    tournaments: EntityInterner<([u8; 40], [u8; 30], i32)>,
    annotators: EntityInterner<[u8; 45]>,
    sources: EntityInterner<[u8; 25]>,
    games: u32,
    guard: TempGuard,
}

impl CbhWriter {
    fn create(path: &Path, options: CbhWriteOptions) -> Result<Self> {
        if !path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cbh"))
        {
            bail!(
                "classic CBH export target must end in .cbh (got {})",
                path.display()
            );
        }
        if let Some(mode) = options.mode
            && !matches!(mode, 0 | 4 | 5 | 10)
        {
            bail!("unsupported classic CBH move encoding mode {mode} (expected 0, 4, 5, or 10)");
        }
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            bail!("export folder {} does not exist", parent.display());
        }

        let stem = path.with_extension("");
        let unique = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        );
        let cbh_tmp = temp_path(&stem.with_extension("cbh"), &unique);
        let cbg_tmp = temp_path(&stem.with_extension("cbg"), &unique);
        let cba_tmp = temp_path(&stem.with_extension("cba"), &unique);
        let cbp_tmp = temp_path(&stem.with_extension("cbp"), &unique);
        let cbt_tmp = temp_path(&stem.with_extension("cbt"), &unique);
        let cbc_tmp = temp_path(&stem.with_extension("cbc"), &unique);
        let cbs_tmp = temp_path(&stem.with_extension("cbs"), &unique);
        let cbj_tmp = temp_path(&stem.with_extension("cbj"), &unique);

        let guard = TempGuard {
            paths: vec![
                cbh_tmp.clone(),
                cbg_tmp.clone(),
                cba_tmp.clone(),
                cbp_tmp.clone(),
                cbt_tmp.clone(),
                cbc_tmp.clone(),
                cbs_tmp.clone(),
                cbj_tmp.clone(),
            ],
            committed: false,
        };

        let mut cbh_file = BufWriter::new(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&cbh_tmp)
                .with_context(|| format!("creating temporary CBH header {}", cbh_tmp.display()))?,
        );
        // Reserve record 0 (46-byte file header).
        cbh_file.write_all(&file_header_record(0))?;

        let mut cbg_file = BufWriter::new(
            File::create(&cbg_tmp)
                .with_context(|| format!("creating temporary CBG moves {}", cbg_tmp.display()))?,
        );
        cbg_file.write_all(&[0u8; CBG_FILE_HEADER_LEN])?;

        let mut cba_file = BufWriter::new(File::create(&cba_tmp).with_context(|| {
            format!("creating temporary CBA annotations {}", cba_tmp.display())
        })?);
        cba_file.write_all(&[0u8; CBA_FILE_HEADER_LEN])?;

        // Record 0 in .cbc and .cbs is a blank name so games without an
        // Annotator or Source tag resolve to `None` when read back.
        let mut annotators = EntityInterner::default();
        annotators.intern([0u8; 45], vec![0u8; 45])?;
        let mut sources = EntityInterner::default();
        sources.intern([0u8; 25], vec![0u8; 25])?;

        Ok(Self {
            stem,
            options,
            unique,
            cbh_tmp,
            cbg_tmp,
            cba_tmp,
            cbp_tmp,
            cbt_tmp,
            cbc_tmp,
            cbs_tmp,
            cbj_tmp,
            cbh_file: Some(cbh_file),
            cbg_file: Some(cbg_file),
            cba_file: Some(cba_file),
            cbg_offset: CBG_FILE_HEADER_LEN as u64,
            cba_offset: CBA_FILE_HEADER_LEN as u64,
            offsets: Vec::new(),
            players: EntityInterner::default(),
            tournaments: EntityInterner::default(),
            annotators,
            sources,
            games: 0,
            guard,
        })
    }

    fn add_game(&mut self, game: &Game) -> Result<()> {
        let id = self
            .games
            .checked_add(1)
            .filter(|&next| next < u32::MAX)
            .context("classic CBH database record count overflow")?;

        let EncodedMoves {
            flags: cbg_flags,
            body: move_body,
            positions: node_positions,
        } = encode_game_moves(game, self.options).with_context(|| {
            format!(
                "encoding moves for game {id} ({} - {})",
                game.white(),
                game.black()
            )
        })?;
        let move_record_size = 4usize
            .checked_add(move_body.len())
            .context("CBG record size overflow")?;
        if move_record_size > MAX_MOVE_RECORD {
            bail!(
                "game {id} ({} - {}) move record is {move_record_size} bytes, exceeding the {MAX_MOVE_RECORD}-byte classic CBH limit",
                game.white(),
                game.black()
            );
        }

        let moves_offset = self.cbg_offset;
        let cbg = self
            .cbg_file
            .as_mut()
            .context("CBG temporary file is already closed")?;
        let move_head = [
            cbg_flags,
            (move_record_size >> 16) as u8,
            (move_record_size >> 8) as u8,
            move_record_size as u8,
        ];
        cbg.write_all(&move_head)?;
        cbg.write_all(&move_body)?;
        self.cbg_offset = self
            .cbg_offset
            .checked_add(move_record_size as u64)
            .context("CBG file offset overflow")?;

        let annotations_offset = match encode_game_annotations(id, game, &node_positions)? {
            Some(record) => {
                if record.len() > MAX_ANNOTATION_RECORD {
                    bail!(
                        "game {id} ({} - {}) annotation record is {} bytes, exceeding the {MAX_ANNOTATION_RECORD}-byte classic CBH limit",
                        game.white(),
                        game.black(),
                        record.len()
                    );
                }
                let at = self.cba_offset;
                let cba = self
                    .cba_file
                    .as_mut()
                    .context("CBA temporary file is already closed")?;
                cba.write_all(&record)?;
                self.cba_offset = self
                    .cba_offset
                    .checked_add(record.len() as u64)
                    .context("CBA file offset overflow")?;
                at
            }
            None => 0,
        };

        let white_id = self.intern_player(game.white())?;
        let black_id = self.intern_player(game.black())?;
        let tournament_id = self.intern_tournament(
            game.header("Event").unwrap_or("?"),
            game.header("Site").unwrap_or("?"),
            game.header("EventDate"),
        )?;
        let annotator_id = self.intern_annotator(game.header("Annotator"))?;
        let source_id = self.intern_source(game.header("Source"))?;

        let deleted = game
            .header("Deleted")
            .is_some_and(|v| v.trim() == "1" || v.trim().eq_ignore_ascii_case("true"))
            || game
                .errors
                .iter()
                .any(|err| err.contains("marked deleted in the ChessBase database"));
        let cbh_flags = if deleted { 0x81 } else { 0x01 };
        let date = encode_date(game.header("Date"));
        let result = encode_result(game.result());
        let (round, subround) = encode_round(game.header("Round"), game.header("Board"));
        let white_elo = parse_elo(game.header("WhiteElo"));
        let black_elo = parse_elo(game.header("BlackElo"));
        let eco = encode_eco(game.header("ECO"));
        let mainline_moves = game.mainline_len().div_ceil(2).min(255) as u8;

        let mut header = [0u8; RECORD_SIZE];
        header[0] = cbh_flags;
        header[1..5].copy_from_slice(&(moves_offset as u32).to_be_bytes());
        header[5..9].copy_from_slice(&(annotations_offset as u32).to_be_bytes());
        put_u24(&mut header[9..12], white_id);
        put_u24(&mut header[12..15], black_id);
        put_u24(&mut header[15..18], tournament_id);
        put_u24(&mut header[18..21], annotator_id);
        put_u24(&mut header[21..24], source_id);
        put_u24(&mut header[24..27], date);
        header[27] = result;
        header[29] = round;
        header[30] = subround;
        header[31..33].copy_from_slice(&white_elo.to_be_bytes());
        header[33..35].copy_from_slice(&black_elo.to_be_bytes());
        header[35..37].copy_from_slice(&eco.to_be_bytes());
        header[45] = mainline_moves;

        let cbh = self
            .cbh_file
            .as_mut()
            .context("CBH temporary file is already closed")?;
        cbh.write_all(&header)?;

        self.offsets.push((moves_offset, annotations_offset));
        self.games = id;
        Ok(())
    }

    fn intern_player(&mut self, raw: &str) -> Result<u32> {
        let trimmed = raw.trim();
        let (last_str, first_str) = if trimmed.is_empty() || trimmed == "?" {
            ("", "")
        } else if let Some((last, first)) = trimmed.split_once(',') {
            (last.trim(), first.trim())
        } else {
            (trimmed, "")
        };
        let last_bytes = fixed_utf8::<30>(last_str);
        let first_bytes = fixed_utf8::<20>(first_str);
        let mut payload = Vec::with_capacity(50);
        payload.extend_from_slice(&last_bytes);
        payload.extend_from_slice(&first_bytes);
        self.players.intern((last_bytes, first_bytes), payload)
    }

    fn intern_tournament(
        &mut self,
        event: &str,
        site: &str,
        event_date: Option<&str>,
    ) -> Result<u32> {
        let title_str = clean_placeholder(event);
        let place_str = clean_placeholder(site);
        let title_bytes = fixed_utf8::<40>(title_str);
        let place_bytes = fixed_utf8::<30>(place_str);
        let start_date = encode_date(event_date) as i32;
        let mut payload = Vec::with_capacity(74);
        payload.extend_from_slice(&title_bytes);
        payload.extend_from_slice(&place_bytes);
        payload.extend_from_slice(&start_date.to_le_bytes());
        self.tournaments
            .intern((title_bytes, place_bytes, start_date), payload)
    }

    fn intern_annotator(&mut self, annotator: Option<&str>) -> Result<u32> {
        let text = annotator.map(clean_placeholder).unwrap_or("");
        if text.is_empty() {
            return Ok(0);
        }
        let bytes = fixed_utf8::<45>(text);
        self.annotators.intern(bytes, bytes.to_vec())
    }

    fn intern_source(&mut self, source: Option<&str>) -> Result<u32> {
        let text = source.map(clean_placeholder).unwrap_or("");
        if text.is_empty() {
            return Ok(0);
        }
        let bytes = fixed_utf8::<25>(text);
        self.sources.intern(bytes, bytes.to_vec())
    }

    fn finish(mut self) -> Result<CbhWriteReport> {
        let mut cbh_writer = self
            .cbh_file
            .take()
            .context("CBH temporary file is already closed")?;
        cbh_writer.flush()?;
        let mut cbh_file = cbh_writer
            .into_inner()
            .map_err(|err| anyhow::anyhow!("flushing temporary CBH file: {err}"))?;
        cbh_file.seek(SeekFrom::Start(0))?;
        cbh_file.write_all(&file_header_record(self.games))?;
        cbh_file.flush()?;
        drop(cbh_file);

        let mut cbg_writer = self
            .cbg_file
            .take()
            .context("CBG temporary file is already closed")?;
        cbg_writer.flush()?;
        drop(cbg_writer);

        let mut cba_writer = self
            .cba_file
            .take()
            .context("CBA temporary file is already closed")?;
        cba_writer.flush()?;
        drop(cba_writer);

        write_entity_file(&self.cbp_tmp, 50, &self.players.records)?;
        write_entity_file(&self.cbt_tmp, 74, &self.tournaments.records)?;
        write_entity_file(&self.cbc_tmp, 45, &self.annotators.records)?;
        write_entity_file(&self.cbs_tmp, 25, &self.sources.records)?;

        let need_wide = self.options.force_wide
            || self.cbg_offset >= WIDE_THRESHOLD
            || self.cba_offset >= WIDE_THRESHOLD;
        if need_wide {
            write_wide_file(&self.cbj_tmp, &self.offsets)?;
        }

        let mut staged = vec![
            (&self.cbg_tmp, self.stem.with_extension("cbg")),
            (&self.cba_tmp, self.stem.with_extension("cba")),
            (&self.cbp_tmp, self.stem.with_extension("cbp")),
            (&self.cbt_tmp, self.stem.with_extension("cbt")),
            (&self.cbc_tmp, self.stem.with_extension("cbc")),
            (&self.cbs_tmp, self.stem.with_extension("cbs")),
        ];
        if need_wide {
            staged.push((&self.cbj_tmp, self.stem.with_extension("cbj")));
        }
        // Install .cbh last so readers never observe a new header before its
        // companion files are in place.
        staged.push((&self.cbh_tmp, self.stem.with_extension("cbh")));

        let mut total_bytes = 0u64;
        for (tmp, _) in &staged {
            total_bytes += fs::metadata(tmp).map(|m| m.len()).unwrap_or(0);
        }

        replace_companion_set(&self.stem, &staged, &self.unique)?;
        self.guard.committed = true;

        Ok(CbhWriteReport {
            games: self.games as usize,
            bytes: total_bytes,
        })
    }
}

fn file_header_record(games: u32) -> [u8; RECORD_SIZE] {
    let mut header = [0u8; RECORD_SIZE];
    header[0] = 0;
    header[2] = 0x2c;
    header[3..5].copy_from_slice(&(RECORD_SIZE as u16).to_be_bytes());
    header[5] = 0;
    header[6..10].copy_from_slice(&games.saturating_add(1).to_be_bytes());
    header
}

struct EntityInterner<K> {
    ids: HashMap<K, u32>,
    records: Vec<Vec<u8>>,
}

impl<K> Default for EntityInterner<K> {
    fn default() -> Self {
        Self {
            ids: HashMap::new(),
            records: Vec::new(),
        }
    }
}

impl<K: Eq + std::hash::Hash> EntityInterner<K> {
    fn intern(&mut self, key: K, payload: Vec<u8>) -> Result<u32> {
        if let Some(&id) = self.ids.get(&key) {
            return Ok(id);
        }
        if self.records.len() > MAX_ENTITY_ID {
            bail!("classic CBH entity table cannot hold more than {MAX_ENTITY_ID} entries");
        }
        let id = self.records.len() as u32;
        self.records.push(payload);
        self.ids.insert(key, id);
        Ok(id)
    }
}

fn write_entity_file(path: &Path, data_size: usize, records: &[Vec<u8>]) -> Result<()> {
    let file = File::create(path)
        .with_context(|| format!("creating temporary CBH entity file {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    let mut header = [0u8; 28];
    header[8..12].copy_from_slice(&ENTITY_MAGIC.to_le_bytes());
    header[12..16].copy_from_slice(&(data_size as i32).to_le_bytes());
    header[24..28].copy_from_slice(&0i32.to_le_bytes());
    writer.write_all(&header)?;
    for record in records {
        debug_assert_eq!(record.len(), data_size);
        writer.write_all(&0i32.to_le_bytes())?;
        writer.write_all(&0i32.to_le_bytes())?;
        writer.write_all(&[0u8])?;
        writer.write_all(record)?;
    }
    writer.flush()?;
    Ok(())
}

fn write_wide_file(path: &Path, offsets: &[(u64, u64)]) -> Result<()> {
    let file = File::create(path)
        .with_context(|| format!("creating temporary CBJ wide offsets {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    let mut header = [0u8; CBJ_HEADER_LEN];
    header[4..8].copy_from_slice(&(CBJ_RECORD_LEN as i32).to_le_bytes());
    header[8..12].copy_from_slice(&(offsets.len() as i32).to_le_bytes());
    writer.write_all(&header)?;
    for &(moves_offset, annotations_offset) in offsets {
        let mut record = [0u8; CBJ_RECORD_LEN];
        record[0x0c..0x14].copy_from_slice(&(annotations_offset as i64).to_be_bytes());
        record[0x1e..0x26].copy_from_slice(&(moves_offset as i64).to_be_bytes());
        writer.write_all(&record)?;
    }
    writer.flush()?;
    Ok(())
}

fn replace_companion_set(stem: &Path, staged: &[(&PathBuf, PathBuf)], unique: &str) -> Result<()> {
    let mut backups: Vec<(PathBuf, PathBuf)> = Vec::new();
    for ext in ALL_EXTENSIONS {
        let existing = sibling(stem, &format!(".{ext}"));
        if existing.exists() {
            let backup = temp_path(&existing, &format!("bak-{unique}"));
            if let Err(err) = fs::rename(&existing, &backup) {
                restore_backups(&backups);
                return Err(err).with_context(|| {
                    format!(
                        "moving existing CBH file {} aside before replacement",
                        existing.display()
                    )
                });
            }
            backups.push((existing, backup));
        }
    }

    let mut installed: Vec<PathBuf> = Vec::new();
    for (tmp, target) in staged {
        if let Err(err) = fs::rename(tmp, target) {
            for placed in &installed {
                let _ = fs::remove_file(placed);
            }
            restore_backups(&backups);
            return Err(err).with_context(|| {
                format!(
                    "installing new CBH file {} from {}",
                    target.display(),
                    tmp.display()
                )
            });
        }
        installed.push(target.clone());
    }

    for (_, backup) in backups {
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

fn restore_backups(backups: &[(PathBuf, PathBuf)]) {
    for (original, backup) in backups.iter().rev() {
        let _ = fs::rename(backup, original);
    }
}

struct TempGuard {
    paths: Vec<PathBuf>,
    committed: bool,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if !self.committed {
            for path in &self.paths {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn temp_path(target: &Path, unique: &str) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("cbh-export");
    target.with_file_name(format!(".{name}.{unique}.tmp"))
}

fn clean_placeholder(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed == "?" { "" } else { trimmed }
}

fn fixed_utf8<const N: usize>(text: &str) -> [u8; N] {
    let mut out = [0u8; N];
    let limit = text.floor_char_boundary(N);
    out[..limit].copy_from_slice(&text.as_bytes()[..limit]);
    out
}

fn put_u24(target: &mut [u8], value: u32) {
    target[0] = (value >> 16) as u8;
    target[1] = (value >> 8) as u8;
    target[2] = value as u8;
}

fn encode_date(raw: Option<&str>) -> u32 {
    let Some(raw) = raw else {
        return 0;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "?" {
        return 0;
    }
    let mut parts = trimmed.split(['.', '-', '/']);
    let year = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&y| (1..=4095).contains(&y))
        .unwrap_or(0);
    let month = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&m| (1..=12).contains(&m))
        .unwrap_or(0);
    let day = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&d| (1..=31).contains(&d))
        .unwrap_or(0);
    (year << 9) | (month << 5) | day
}

fn encode_result(result: &str) -> u8 {
    match result.trim() {
        "0-1" => 0,
        "1/2-1/2" => 1,
        "1-0" => 2,
        "0-0" => 7,
        _ => 3,
    }
}

fn encode_round(round: Option<&str>, board: Option<&str>) -> (u8, u8) {
    let parse_part = |s: &str| -> u8 {
        let s = s.trim();
        if s.is_empty() || s == "?" {
            return 0;
        }
        s.parse::<u16>()
            .ok()
            .map(|n| n.min(255) as u8)
            .unwrap_or_else(|| {
                let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
                digits.parse::<u16>().map(|n| n.min(255) as u8).unwrap_or(0)
            })
    };
    let raw = round.unwrap_or("").trim();
    if raw.is_empty() || raw == "?" {
        let sub = board.map(parse_part).unwrap_or(0);
        return (0, sub);
    }
    if let Some((r, sub)) = raw.split_once(['.', '/', '(']) {
        let sub = sub.trim_end_matches(')');
        return (parse_part(r), parse_part(sub));
    }
    let main = parse_part(raw);
    let sub = board.map(parse_part).unwrap_or(0);
    (main, sub)
}

fn parse_elo(raw: Option<&str>) -> u16 {
    raw.and_then(|s| s.trim().parse::<u16>().ok()).unwrap_or(0)
}

pub(crate) fn encode_eco(raw: Option<&str>) -> u16 {
    let Some(raw) = raw.map(str::trim) else {
        return 0;
    };
    let bytes = raw.as_bytes();
    if bytes.len() < 3 {
        return 0;
    }
    let letter = bytes[0].to_ascii_uppercase();
    if !(b'A'..=b'E').contains(&letter) {
        return 0;
    }
    let Ok(number) = std::str::from_utf8(&bytes[1..3])
        .unwrap_or("")
        .parse::<u16>()
    else {
        return 0;
    };
    if number > 99 {
        return 0;
    }
    let code = u16::from(letter - b'A') * 100 + number;
    (code + 1) * 128
}

struct EncodedMoves {
    flags: u8,
    body: Vec<u8>,
    positions: Vec<(usize, i32)>,
}

/// Encode the `.cbg` flags, optional explicit start + move stream bytes, and
/// return the map from `Game::nodes` index to stored depth-first move position
/// (`-1` for the root node `0`, `0..` for move nodes).
fn encode_game_moves(game: &Game, options: CbhWriteOptions) -> Result<EncodedMoves> {
    let root = game
        .nodes
        .first()
        .context("cannot encode a game without a root node")?;

    let variant_header = game.header("Variant").unwrap_or("").trim();
    let variant_lower = variant_header.to_ascii_lowercase();
    let is_chess960_variant = variant_lower.contains("960") || variant_lower.contains("fischer");
    if !variant_lower.is_empty()
        && !is_chess960_variant
        && !matches!(
            variant_lower.as_str(),
            "standard" | "chess" | "normal" | "classical" | "classic"
        )
    {
        bail!("variant {variant_header:?} cannot be written to classic CBH");
    }

    let chess960 = is_chess960_variant
        || root.pos.castles().mode() == CastlingMode::Chess960
        || options.mode == Some(10);
    let mode = match options.mode {
        Some(m) => m,
        None if chess960 => 10,
        None => 0,
    };
    if chess960 && mode != 10 {
        bail!("Chess960 games require CBH move encoding mode 10 (got mode {mode})");
    }

    let standard_fen = Fen::from_position(&Chess::default(), EnPassantMode::Legal).to_string();
    let root_fen = Fen::from_position(&root.pos, EnPassantMode::Legal).to_string();
    let fen_zero_fullmoves = game
        .header("FEN")
        .is_some_and(|fen| fen.trim().ends_with(" 0"));
    let explicit_start = mode == 10
        || chess960
        || root_fen != standard_fen
        || fen_zero_fullmoves
        || game.header("SetUp").is_some_and(|v| v.trim() == "1");

    let mut body = Vec::new();
    if explicit_start {
        body.extend_from_slice(&encode_start_position(
            &root.pos,
            mode == 10,
            fen_zero_fullmoves,
        )?);
    }

    let mut positions = Vec::with_capacity(game.nodes.len());
    positions.push((0usize, -1i32));

    if !root.children.is_empty() {
        let mut encoder =
            MoveStreamEncoder::new(mode, chess960, options.use_compact_codes, &root.pos)?;
        encoder.walk_children(game, 0, &mut positions)?;
        body.extend_from_slice(&encoder.stream);
    }

    let flags = (mode & 0x3f) | if explicit_start { 0x40 } else { 0 };
    Ok(EncodedMoves {
        flags,
        body,
        positions,
    })
}

fn encode_start_position(
    pos: &Chess,
    chess960_extra: bool,
    fen_zero_fullmoves: bool,
) -> Result<Vec<u8>> {
    let mut bits = Vec::with_capacity(192);
    for cb in 0..64u8 {
        let sq = from_stored_square(cb);
        match pos.board().piece_at(sq) {
            Some(piece) => {
                bits.push(true);
                bits.push(piece.color == Color::Black);
                let code: u8 = match piece.role {
                    Role::King => 1,
                    Role::Queen => 2,
                    Role::Knight => 3,
                    Role::Bishop => 4,
                    Role::Rook => 5,
                    Role::Pawn => 6,
                };
                bits.push((code >> 2) & 1 != 0);
                bits.push((code >> 1) & 1 != 0);
                bits.push(code & 1 != 0);
            }
            None => bits.push(false),
        }
    }
    if bits.len() > 192 {
        bail!(
            "start position has too many pieces for a 24-byte classic CBH board ({} bits)",
            bits.len()
        );
    }

    let mut board_bytes = [0u8; 24];
    for (index, bit) in bits.into_iter().enumerate() {
        if bit {
            board_bytes[index / 8] |= 0x80 >> (index % 8);
        }
    }

    let ep_nibble = pos
        .ep_square(EnPassantMode::Legal)
        .map(|sq| (sq.file().to_u32() as u8) + 1)
        .unwrap_or(0);
    let turn_and_ep = (if pos.turn() == Color::Black { 0x10 } else { 0 }) | (ep_nibble & 0x0f);

    let mut castling = 0u8;
    if pos.castles().has(Color::White, CastlingSide::QueenSide) {
        castling |= 1;
    }
    if pos.castles().has(Color::White, CastlingSide::KingSide) {
        castling |= 2;
    }
    if pos.castles().has(Color::Black, CastlingSide::QueenSide) {
        castling |= 4;
    }
    if pos.castles().has(Color::Black, CastlingSide::KingSide) {
        castling |= 8;
    }

    let move_number = if fen_zero_fullmoves {
        0
    } else {
        pos.fullmoves().get().min(255) as u8
    };

    let mut out = Vec::with_capacity(if chess960_extra { 36 } else { 28 });
    out.extend_from_slice(&[0u8, turn_and_ep, castling, move_number]);
    out.extend_from_slice(&board_bytes);

    if chess960_extra {
        let wk = pos
            .board()
            .king_of(Color::White)
            .map(to_stored_square)
            .unwrap_or(0xff);
        let bk = pos
            .board()
            .king_of(Color::Black)
            .map(to_stored_square)
            .unwrap_or(0xff);
        let w_oo = pos
            .castles()
            .rook(Color::White, CastlingSide::KingSide)
            .map(to_stored_square)
            .unwrap_or(0xff);
        let w_ooo = pos
            .castles()
            .rook(Color::White, CastlingSide::QueenSide)
            .map(to_stored_square)
            .unwrap_or(0xff);
        let b_oo = pos
            .castles()
            .rook(Color::Black, CastlingSide::KingSide)
            .map(to_stored_square)
            .unwrap_or(0xff);
        let b_ooo = pos
            .castles()
            .rook(Color::Black, CastlingSide::QueenSide)
            .map(to_stored_square)
            .unwrap_or(0xff);
        out.extend_from_slice(&[wk, bk, w_oo, w_ooo, b_oo, b_ooo, 0x02, 0x06]);
    }

    Ok(out)
}

struct MoveStreamEncoder {
    inv: [u8; 256],
    pre: bool,
    simple: bool,
    chess960: bool,
    use_compact_codes: bool,
    pieces: PieceLists,
    stream: Vec<u8>,
    total: u32,
}

impl MoveStreamEncoder {
    fn new(mode: u8, chess960: bool, use_compact_codes: bool, start_pos: &Chess) -> Result<Self> {
        let (table, pre, simple) = match mode {
            0 => (&tables::MODE_0, true, false),
            4 => (&tables::MODE_4, false, false),
            5 => (&tables::MODE_5, false, true),
            10 => (&tables::MODE_10, false, false),
            other => bail!("unsupported CBH move encoding mode {other}"),
        };
        let mut inv = [0u8; 256];
        for (index, &canonical) in table.iter().enumerate() {
            inv[canonical as usize] = index as u8;
        }
        let pieces = PieceLists::scan(start_pos).context("scanning start position for CBH")?;
        Ok(Self {
            inv,
            pre,
            simple,
            chess960,
            use_compact_codes,
            pieces,
            stream: Vec::new(),
            total: 0,
        })
    }

    fn encode_byte(&self, canonical: u8) -> u8 {
        let n = self.total as u8;
        if self.pre {
            self.inv[canonical as usize].wrapping_add(n)
        } else {
            self.inv[canonical.wrapping_add(n) as usize]
        }
    }

    fn push_byte(&mut self, canonical: u8) {
        let byte = self.encode_byte(canonical);
        self.stream.push(byte);
    }

    fn walk_children(
        &mut self,
        game: &Game,
        parent: usize,
        positions: &mut Vec<(usize, i32)>,
    ) -> Result<()> {
        let children = &game.nodes[parent].children;
        for (idx, &child) in children.iter().enumerate() {
            let has_next_sibling = idx + 1 < children.len();
            let is_leaf = game.nodes[child].children.is_empty();
            let saved_pieces = self.pieces;

            let stored_pos = i32::try_from(self.total).context("too many moves in CBH game")?;
            positions.push((child, stored_pos));

            let before = &game.nodes[parent].pos;
            let ply = game.nodes[child]
                .mv
                .as_ref()
                .with_context(|| format!("node {child} has no move ply"))?;
            let resolved = resolve_node_move(before, ply)?;

            if self.simple {
                let mut word = match resolved {
                    Some(mv) => square_move_word(before, mv, self.chess960)?,
                    None => 0,
                };
                if has_next_sibling {
                    word |= 0x8000;
                }
                if is_leaf {
                    word |= 0x4000;
                }
                self.push_byte((word >> 8) as u8);
                self.push_byte(word as u8);
                if let Some(mv) = resolved {
                    self.pieces.update(before, mv)?;
                }
                self.total += 1;
            } else {
                if has_next_sibling {
                    self.push_byte(254);
                }
                match resolved {
                    None => {
                        if self.use_compact_codes {
                            self.push_byte(0);
                        } else {
                            self.push_byte(235);
                            self.push_byte(0);
                            self.push_byte(0);
                        }
                        self.total += 1;
                    }
                    Some(mv) => {
                        let compact = if self.use_compact_codes {
                            self.pieces.compact_code(before, mv)
                        } else {
                            None
                        };
                        if let Some(code) = compact {
                            self.push_byte(code);
                        } else {
                            let word = square_move_word(before, mv, self.chess960)?;
                            self.push_byte(235);
                            self.push_byte((word >> 8) as u8);
                            self.push_byte(word as u8);
                        }
                        self.pieces.update(before, mv)?;
                        self.total += 1;
                    }
                }
                if is_leaf {
                    self.push_byte(255);
                }
            }

            if !is_leaf {
                self.walk_children(game, child, positions)?;
            }
            if has_next_sibling {
                self.pieces = saved_pieces;
            }
        }
        Ok(())
    }
}

fn resolve_node_move(before: &Chess, ply: &Ply) -> Result<Option<Move>> {
    if ply.uci == "0000" || ply.san == "--" {
        return Ok(None);
    }
    if let Ok(uci) = UciMove::from_ascii(ply.uci.as_bytes())
        && let Ok(mv) = uci.to_move(before)
    {
        return Ok(Some(mv));
    }
    if let Ok(san_plus) = SanPlus::from_ascii(ply.san.as_bytes())
        && let Ok(mv) = san_plus.san.to_move(before)
    {
        return Ok(Some(mv));
    }
    bail!(
        "cannot resolve move {} (UCI {}) in position {}",
        ply.san,
        ply.uci,
        Fen::from_position(before, EnPassantMode::Legal)
    )
}

fn square_move_word(before: &Chess, mv: Move, chess960: bool) -> Result<u16> {
    let us = before.turn();
    let back = if us == Color::White { 0 } else { 7 };
    match mv {
        Move::Castle { king, rook } => {
            let dest_file = if rook > king { 6 } else { 2 };
            let to_cb = u16::from(to_stored_square(square(dest_file, back)));
            let from_cb = if chess960 {
                to_cb
            } else {
                u16::from(to_stored_square(square(4, back)))
            };
            Ok(from_cb | (to_cb << 6))
        }
        Move::EnPassant { from, to } => {
            let from_cb = u16::from(to_stored_square(from));
            let to_cb = u16::from(to_stored_square(to));
            Ok(from_cb | (to_cb << 6))
        }
        Move::Normal {
            from,
            to,
            promotion,
            ..
        } => {
            let from_cb = u16::from(to_stored_square(from));
            let to_cb = u16::from(to_stored_square(to));
            let promo_bits: u16 = match promotion {
                None | Some(Role::Queen) => 0,
                Some(Role::Rook) => 1,
                Some(Role::Bishop) => 2,
                Some(Role::Knight) => 3,
                Some(other) => bail!("cannot encode promotion to {other:?} in classic CBH"),
            };
            Ok(from_cb | (to_cb << 6) | (promo_bits << 12))
        }
        Move::Put { .. } => bail!("piece drops cannot be encoded in classic CBH"),
    }
}

/// Encode all annotations of `game` into a `.cba` record, or `Ok(None)` if
/// the game has no comments, NAGs, evaluations, or drawing/time/medal tokens.
fn encode_game_annotations(
    game_id: u32,
    game: &Game,
    node_positions: &[(usize, i32)],
) -> Result<Option<Vec<u8>>> {
    let mut items = Vec::new();
    let mut item_count = 0u32;

    for &(node_id, position) in node_positions {
        let node = &game.nodes[node_id];
        let extracted = extract_comment_tokens(&node.comment);

        // 1. Engine evaluations (`0x21`): structured `node.eval` first if no
        // raw `[%eval score,depth]` token was present in `node.comment`.
        if extracted.evaluations.is_empty() {
            if let Some(eval) = node.eval {
                let payload = encode_pgn_eval(eval);
                push_annotation_item(&mut items, position, 0x21, &payload)?;
                item_count += 1;
            }
        } else {
            for payload in &extracted.evaluations {
                push_annotation_item(&mut items, position, 0x21, payload)?;
                item_count += 1;
            }
        }

        // 2. Time spent (`0x07`, `[%emt ...]`).
        for payload in &extracted.times {
            push_annotation_item(&mut items, position, 0x07, payload)?;
            item_count += 1;
        }

        // 3. Coloured squares (`0x04`, `[%csl ...]`).
        let mut squares = extracted.squares;
        let mut arrows = extracted.arrows;
        for shape in &node.shapes {
            match *shape {
                crate::domain::Shape::Square { color, square } => {
                    if let (Some(color), Some(code)) = (
                        drawing_color_code(color.code() as u8),
                        drawing_square_code(square.to_string().as_bytes()),
                    ) {
                        squares.extend_from_slice(&[color, code]);
                    }
                }
                crate::domain::Shape::Arrow { color, from, to } => {
                    if let (Some(color), Some(from), Some(to)) = (
                        drawing_color_code(color.code() as u8),
                        drawing_square_code(from.to_string().as_bytes()),
                        drawing_square_code(to.to_string().as_bytes()),
                    ) {
                        arrows.extend_from_slice(&[color, from, to]);
                    }
                }
            }
        }
        if !squares.is_empty() {
            push_annotation_item(&mut items, position, 0x04, &squares)?;
            item_count += 1;
        }

        // 4. Coloured arrows (`0x05`, `[%cal ...]`).
        if !arrows.is_empty() {
            push_annotation_item(&mut items, position, 0x05, &arrows)?;
            item_count += 1;
        }

        // 5. Medals (`0x22`, `[%mdl ...]`).
        for payload in &extracted.medals {
            push_annotation_item(&mut items, position, 0x22, payload)?;
            item_count += 1;
        }

        // 6. Human text comment (`0x02` after move).
        if !extracted.text.is_empty() {
            let mut payload = Vec::with_capacity(2 + extracted.text.len());
            payload.extend_from_slice(&[0u8, 0u8]);
            payload.extend_from_slice(extracted.text.as_bytes());
            push_annotation_item(&mut items, position, 0x02, &payload)?;
            item_count += 1;
        }

        // 7. NAGs (`0x03`, move/position/prefix triples).
        if position >= 0 {
            let non_zero: Vec<u8> = node.nags.iter().copied().filter(|&n| n != 0).collect();
            for chunk in non_zero.chunks(3) {
                let payload = match *chunk {
                    [n0] => [n0, 0, 0],
                    [n0, n1] => [n0, n1, 0],
                    [n0, n1, n2] => [n1, n2, n0],
                    _ => unreachable!(),
                };
                push_annotation_item(&mut items, position, 0x03, &payload)?;
                item_count += 1;
            }
        }
    }

    if item_count == 0 {
        return Ok(None);
    }

    let total_size = CBA_HEAD_LEN
        .checked_add(items.len())
        .context("CBA record size overflow")?;
    let mut record = Vec::with_capacity(total_size);
    let mut head = [0u8; CBA_HEAD_LEN];
    put_u24(&mut head[0..3], game_id);
    head[3..7].copy_from_slice(&CBA_MARK);
    put_u24(&mut head[7..10], item_count + 1);
    head[10..14].copy_from_slice(&(total_size as u32).to_be_bytes());
    record.extend_from_slice(&head);
    record.extend_from_slice(&items);
    Ok(Some(record))
}

fn push_annotation_item(out: &mut Vec<u8>, position: i32, kind: u8, payload: &[u8]) -> Result<()> {
    let size = CBA_ITEM_HEAD_LEN
        .checked_add(payload.len())
        .filter(|&s| s <= usize::from(u16::MAX))
        .context("classic CBH annotation item exceeds 65,535 bytes")?;
    let mut head = [0u8; CBA_ITEM_HEAD_LEN];
    put_u24(&mut head[0..3], (position as u32) & 0x00ff_ffff);
    head[3] = kind;
    head[4..6].copy_from_slice(&(size as u16).to_be_bytes());
    out.extend_from_slice(&head);
    out.extend_from_slice(payload);
    Ok(())
}

fn encode_pgn_eval(eval: PgnEval) -> [u8; 6] {
    let (value, kind): (i16, i16) = if let Some(plies) = eval.mate {
        if plies > 0 {
            (32767 - plies.clamp(1, 1000) as i16, 1)
        } else {
            (-32767 - plies.clamp(-1000, -1) as i16, 1)
        }
    } else {
        let cp = eval.cp.unwrap_or(0);
        (cp.clamp(-30_000, 30_000) as i16, 0)
    };
    let mut out = [0u8; 6];
    out[0..2].copy_from_slice(&value.to_le_bytes());
    out[2..4].copy_from_slice(&kind.to_le_bytes());
    out[4..6].copy_from_slice(&0i16.to_le_bytes());
    out
}

#[derive(Default)]
struct ExtractedTokens {
    evaluations: Vec<[u8; 6]>,
    times: Vec<[u8; 4]>,
    squares: Vec<u8>,
    arrows: Vec<u8>,
    medals: Vec<[u8; 4]>,
    text: String,
}

fn extract_comment_tokens(comment: &str) -> ExtractedTokens {
    let mut out = ExtractedTokens::default();
    let mut human = String::new();
    let mut rest = comment;

    while let Some(start) = rest.find("[%") {
        human.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find(']') else {
            human.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let token_body = &after_open[..end];
        let whole_token = &rest[start..start + 2 + end + 1];
        if !parse_single_token(token_body, &mut out) {
            human.push_str(whole_token);
        }
        rest = &after_open[end + 1..];
    }
    human.push_str(rest);
    out.text = human.split_whitespace().collect::<Vec<_>>().join(" ");
    out
}

fn parse_single_token(body: &str, out: &mut ExtractedTokens) -> bool {
    let mut parts = body.split_whitespace();
    let Some(kind) = parts.next() else {
        return false;
    };
    let arg = parts.next().unwrap_or("");
    if parts.next().is_some() {
        return false;
    }

    if kind.eq_ignore_ascii_case("eval") {
        if let Some(eval_bytes) = parse_cbh_eval_token(arg) {
            out.evaluations.push(eval_bytes);
            return true;
        }
    } else if kind.eq_ignore_ascii_case("emt") {
        if let Some(time_bytes) = parse_emt_token(arg) {
            out.times.push(time_bytes);
            return true;
        }
    } else if kind.eq_ignore_ascii_case("csl") {
        if let Some(squares) = parse_csl_token(arg) {
            out.squares.extend_from_slice(&squares);
            return true;
        }
    } else if kind.eq_ignore_ascii_case("cal") {
        if let Some(arrows) = parse_cal_token(arg) {
            out.arrows.extend_from_slice(&arrows);
            return true;
        }
    } else if kind.eq_ignore_ascii_case("mdl")
        && let Ok(medal) = arg.parse::<u32>()
    {
        out.medals.push(medal.to_be_bytes());
        return true;
    }
    false
}

fn parse_cbh_eval_token(arg: &str) -> Option<[u8; 6]> {
    if let Some((score_str, depth_str)) = arg.split_once(',') {
        let score = score_str.trim().parse::<i16>().ok()?;
        let depth = depth_str.trim().parse::<i16>().ok()?;
        let mut out = [0u8; 6];
        out[0..2].copy_from_slice(&score.to_le_bytes());
        out[2..4].copy_from_slice(&0i16.to_le_bytes());
        out[4..6].copy_from_slice(&depth.to_le_bytes());
        return Some(out);
    }
    PgnEval::parse(arg).map(encode_pgn_eval)
}

fn parse_emt_token(arg: &str) -> Option<[u8; 4]> {
    let mut parts = arg.split(':');
    let hours = parts.next()?.parse::<u8>().ok()?;
    let minutes = parts.next()?.parse::<u8>().ok()?;
    let sec_part = parts.next()?;
    if parts.next().is_some() || minutes >= 60 {
        return None;
    }
    let (seconds, hundredths) = match sec_part.split_once('.') {
        Some((s, h)) => (s.parse::<u8>().ok()?, h.parse::<u8>().ok()?),
        None => (sec_part.parse::<u8>().ok()?, 0),
    };
    if seconds >= 60 || hundredths >= 100 {
        return None;
    }
    Some([hours, minutes, seconds, hundredths])
}

fn parse_csl_token(arg: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for item in arg.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let bytes = item.as_bytes();
        if bytes.len() != 3 {
            return None;
        }
        let color = drawing_color_code(bytes[0])?;
        let sq = drawing_square_code(&bytes[1..3])?;
        out.extend_from_slice(&[color, sq]);
    }
    (!out.is_empty()).then_some(out)
}

fn parse_cal_token(arg: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for item in arg.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let bytes = item.as_bytes();
        if bytes.len() != 5 {
            return None;
        }
        let color = drawing_color_code(bytes[0])?;
        let from = drawing_square_code(&bytes[1..3])?;
        let to = drawing_square_code(&bytes[3..5])?;
        out.extend_from_slice(&[color, from, to]);
    }
    (!out.is_empty()).then_some(out)
}

fn drawing_color_code(ch: u8) -> Option<u8> {
    match ch.to_ascii_uppercase() {
        b'G' => Some(2),
        b'Y' => Some(3),
        b'R' => Some(4),
        b'B' => Some(7),
        _ => None,
    }
}

/// Drawing payloads number squares 1..=64, file-major (`a1 = 1`, `b1 = 9`).
fn drawing_square_code(bytes: &[u8]) -> Option<u8> {
    if bytes.len() != 2 {
        return None;
    }
    let file = bytes[0].to_ascii_lowercase();
    let rank = bytes[1];
    if !(b'a'..=b'h').contains(&file) || !(b'1'..=b'8').contains(&rank) {
        return None;
    }
    Some((file - b'a') * 8 + (rank - b'1') + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cbh::CbhLibrary;
    use crate::domain::parse_pgn;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("brett-cbh-writer-{}-{unique}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn verify_with_cbvault(cbh_path: &Path, expected_games: usize) {
        let headers = cbvault_format::cbh::Headers::open(cbh_path).unwrap();
        assert_eq!(headers.records() as usize, expected_games);
        let base = cbh_path.with_extension("");
        let entities = cbvault_format::cbh::Entities::open(&base).unwrap();
        let annotations = cbvault_format::cbh::Annotations::open(&base).unwrap();
        let wide = cbvault_format::cbh::Wide::open(&base).ok();
        let cbg_path = cbh_path.with_extension("cbg");
        let cbg_file = cbvault_format::file::DbFile::open(cbg_path).unwrap();
        if expected_games > 0 {
            let batch = cbvault_format::cbh::Batch::open(
                &headers,
                &cbg_file,
                wide.as_ref(),
                1,
                expected_games as u32,
            )
            .unwrap();
            let mut move_scratch = Vec::new();
            let mut ann_scratch = Vec::new();
            for id in 1..=expected_games as u32 {
                let header = batch.record(id).unwrap();
                let _moves = batch.moves_of(&header, &mut move_scratch).unwrap();
                let _anns = annotations
                    .of(&header, wide.as_ref(), &mut ann_scratch)
                    .unwrap();
                let _white = entities.player(header.white()).unwrap();
                let _black = entities.player(header.black()).unwrap();
                let _tournament = entities.tournament(header.tournament()).unwrap();
            }
        }
    }

    #[test]
    fn round_trips_headers_variations_comments_nags_and_tokens_across_modes() {
        let pgn = br#"[Event "Candidates"]
[Site "Toronto"]
[Date "2024.04.15"]
[Round "7.2"]
[White "Gukesh, D."]
[Black "Caruana, Fabiano"]
[Result "1/2-1/2"]
[WhiteElo "2743"]
[BlackElo "2803"]
[ECO "C50"]
[Annotator "Analyst, Ann"]
[Source "Brett Test"]

{Opening survey.}
1. e4 {[%eval +0.45] [%emt 0:01:12] King pawn.} e5 {[%mdl 32]}
2. Nf3 {Develop.} Nc6 $1 $5
( 2... Nf6 3. Nxe5 d6 4. Nf3 Nxe4 )
3. Bc4 ( 3. Bb5 {[%csl Ge5][%cal Gh8h7] Spanish.} a6 4. Ba4 Nf6 5. O-O )
3... Bc5 4. O-O Nf6 1/2-1/2
"#;
        let games = parse_pgn(pgn).unwrap();
        assert_eq!(games.len(), 1);

        for (mode, use_compact) in [
            (Some(0), true),
            (Some(0), false),
            (Some(4), true),
            (Some(4), false),
            (Some(5), false),
        ] {
            let dir = TempDir::new();
            let path = dir.path().join("roundtrip.cbh");
            let report = write_database_with_options(
                &path,
                CbhWriteOptions {
                    mode,
                    use_compact_codes: use_compact,
                    force_wide: false,
                },
                games.iter().cloned().map(Ok),
            )
            .unwrap();
            assert_eq!(report.games, 1);
            assert!(report.bytes > 0);
            for companion in core_companion_files(&path) {
                assert!(companion.is_file(), "missing {}", companion.display());
            }

            verify_with_cbvault(&path, 1);

            let library = CbhLibrary::open(&path).unwrap();
            assert_eq!(library.len(), 1);
            let indexed = library.get(0).unwrap();
            assert_eq!(indexed.event(), "Candidates");
            assert_eq!(indexed.header("Site"), Some("Toronto"));
            assert_eq!(indexed.date(), "2024.04.15");
            assert_eq!(indexed.header("Round"), Some("7.2"));
            assert_eq!(indexed.white(), "Gukesh, D.");
            assert_eq!(indexed.black(), "Caruana, Fabiano");
            assert_eq!(indexed.result(), "1/2-1/2");
            assert_eq!(indexed.header("WhiteElo"), Some("2743"));
            assert_eq!(indexed.header("BlackElo"), Some("2803"));
            assert_eq!(indexed.header("ECO"), Some("C50"));
            assert_eq!(indexed.header("Annotator"), Some("Analyst, Ann"));
            assert_eq!(indexed.header("Source"), Some("Brett Test"));

            let loaded = library.load(0).unwrap();
            assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
            let mainline: Vec<&str> = loaded
                .mainline()
                .iter()
                .map(|&id| loaded.nodes[id].mv.as_ref().unwrap().san.as_str())
                .collect();
            assert_eq!(
                mainline,
                ["e4", "e5", "Nf3", "Nc6", "Bc4", "Bc5", "O-O", "Nf6"]
            );
            assert_eq!(loaded.nodes[0].comment, "Opening survey.");
            assert_eq!(
                loaded.nodes[1].comment,
                "[%eval 45,0] [%emt 0:01:12] King pawn."
            );
            assert_eq!(loaded.nodes[2].comment, "[%mdl 32]");
            assert_eq!(loaded.nodes[3].comment, "Develop.");
            assert_eq!(loaded.nodes[4].nags, vec![1, 5]);
        }
    }

    #[test]
    fn round_trips_promotions_en_passant_null_moves_and_custom_starts() {
        let pgn = br#"[Event "Special Moves"]
[Site "Test"]
[Date "2025.02.03"]
[Round "1"]
[White "Alpha"]
[Black "Beta"]
[Result "1-0"]
[SetUp "1"]
[FEN "r3k2r/pPqp1ppp/8/4pP2/8/8/P1P2PPP/R3K2R w KQkq e6 0 1"]

1. fxe6 ( 1. O-O-O -- 2. Kb1 ) 1... dxe6 2. bxa8=N O-O 3. O-O 1-0
"#;
        let games = parse_pgn(pgn).unwrap();
        assert!(games[0].errors.is_empty(), "{:?}", games[0].errors);
        let dir = TempDir::new();
        let path = dir.path().join("special.cbh");
        write_database(&path, games.iter().cloned().map(Ok)).unwrap();

        verify_with_cbvault(&path, 1);

        let library = CbhLibrary::open(&path).unwrap();
        let loaded = library.load(0).unwrap();
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(loaded.header("SetUp"), Some("1"));
        assert_eq!(
            loaded.header("FEN"),
            Some("r3k2r/pPqp1ppp/8/4pP2/8/8/P1P2PPP/R3K2R w KQkq e6 0 1")
        );
        let mainline: Vec<&str> = loaded
            .mainline()
            .iter()
            .map(|&id| loaded.nodes[id].mv.as_ref().unwrap().san.as_str())
            .collect();
        assert_eq!(mainline, ["fxe6", "dxe6", "bxa8=N", "O-O", "O-O"]);
        let var_start = loaded.nodes[0].children[1];
        assert_eq!(loaded.nodes[var_start].san(), Some("O-O-O"));
        let null_node = loaded.nodes[var_start].children[0];
        assert_eq!(loaded.nodes[null_node].san(), Some("--"));
    }

    #[test]
    fn round_trips_chess960_mode_ten_and_wide_cbj_offsets() {
        let pgn = br#"[Event "Freestyle"]
[Site "Weissenhaus"]
[Date "2025.02.10"]
[Round "1"]
[White "Carlsen, Magnus"]
[Black "Abdusattorov, Nodirbek"]
[Result "1/2-1/2"]
[Variant "Chess960"]
[SetUp "1"]
[FEN "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"]

1. e4 e5 2. Nf3 Nc6 3. Bc4 Bc5 4. O-O Nf6 1/2-1/2

[Event "Empty Game"]
[Site "Nowhere"]
[Date "2013.01.07"]
[Round "?"]
[White "Player One"]
[Black "Player Two"]
[Result "1/2-1/2"]

1/2-1/2
"#;
        let mut games = parse_pgn(pgn).unwrap();
        games[1]
            .errors
            .push("This game is marked deleted in the ChessBase database.".to_string());

        let dir = TempDir::new();
        let path = dir.path().join("freestyle.cbh");
        write_database_with_options(
            &path,
            CbhWriteOptions {
                mode: None,
                use_compact_codes: true,
                force_wide: true,
            },
            games.into_iter().map(Ok),
        )
        .unwrap();

        assert!(path.with_extension("cbj").is_file());
        verify_with_cbvault(&path, 2);

        let wide = cbvault_format::cbh::Wide::open(&path.with_extension("")).unwrap();
        assert_eq!(wide.records(), 2);

        let library = CbhLibrary::open(&path).unwrap();
        assert_eq!(library.len(), 2);

        let first = library.load(0).unwrap();
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert_eq!(first.header("Variant"), Some("Chess960"));
        assert_eq!(first.header("SetUp"), Some("1"));
        assert_eq!(first.mainline_len(), 8);

        assert!(library.get(1).unwrap().cbh_deleted());
        let second = library.load(1).unwrap();
        assert_eq!(second.nodes.len(), 1);
        assert_eq!(second.header("PlyCount"), Some("0"));
    }

    #[test]
    fn replaces_existing_companion_files_and_removes_stale_boosters() {
        let dir = TempDir::new();
        let path = dir.path().join("replace.cbh");
        let stale_booster = dir.path().join("replace.cbb");
        fs::write(&stale_booster, b"stale booster").unwrap();

        let first_pgn = b"[Event \"First\"]\n[White \"One\"]\n[Black \"Two\"]\n[Result \"1-0\"]\n\n1. e4 e5 1-0\n";
        let first_games = parse_pgn(first_pgn).unwrap();
        write_database(&path, first_games.into_iter().map(Ok)).unwrap();
        assert!(
            !stale_booster.exists(),
            "stale .cbb booster should be removed on replace"
        );

        let second_pgn = b"[Event \"Second\"]\n[White \"Three\"]\n[Black \"Four\"]\n[Result \"0-1\"]\n\n1. d4 d5 0-1\n";
        let second_games = parse_pgn(second_pgn).unwrap();
        write_database(&path, second_games.into_iter().map(Ok)).unwrap();

        let library = CbhLibrary::open(&path).unwrap();
        assert_eq!(library.len(), 1);
        assert_eq!(library.get(0).unwrap().event(), "Second");
        assert_eq!(library.get(0).unwrap().white(), "Three");
    }
}
