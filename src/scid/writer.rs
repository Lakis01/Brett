//! Native writer for SCID4 databases (`.si4`/`.sn4`/`.sg4`).
//!
//! The format is documented by the SCID project. This is an independent
//! implementation written from the published on-disk layout; no SCID source
//! code is linked or copied. Games are written one at a time to a temporary
//! game file, with a compact provisional index beside it, so exporting a large
//! session does not require holding every record in memory.
//!
//! The writer emits the modern 400 layout: a 182-byte index header, 47-byte
//! index entries, a 36-byte namebase header with front-coded names, and game
//! records containing extra tags, an optional start FEN, the compact move
//! stream, NAGs, variations, and trailing comment strings.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use shakmaty::fen::Fen;
use shakmaty::uci::UciMove;
use shakmaty::{
    Chess, Color, EnPassantMode, File as ChessFile, Move, Position, Rank, Role, Square,
};

use crate::domain::{Clock, Game, Node};
use crate::scid::PieceLists;

const INDEX_MAGIC: &[u8; 8] = b"Scid.si\0";
const NAME_MAGIC: &[u8; 8] = b"Scid.sn\0";
const INDEX_VERSION: u16 = 400;
const INDEX_HEADER_LEN: usize = 182;
const ENTRY_LEN: usize = 47;
const NAME_HEADER_LEN: usize = 36;
const MAX_GAME_LEN: usize = 131_071;
const MAX_GAMES: usize = 16_777_214;
const MAX_ELO: u16 = 4_000;
const MAX_NAME_LEN: usize = 255;
const MAX_CUSTOM_TAG_LEN: usize = 240;
const MAX_TAG_VALUE_LEN: usize = 254;
const PENDING_ENTRY_LEN: usize = 57;
const HPSIG_SIZE: usize = 9;

const NAME_LIMITS: [u32; 4] = [1_048_575, 524_287, 524_287, 262_143];

const COMMON_TAGS: [&str; 10] = [
    "WhiteCountry",
    "BlackCountry",
    "Annotator",
    "PlyCount",
    "EventDate",
    "Opening",
    "Variation",
    "Setup",
    "Source",
    "SetUp",
];

/// Result of a completed SCID export.
#[derive(Clone, Debug, Default)]
pub struct ScidWriteReport {
    pub games: usize,
    pub bytes: u64,
}

/// Write a SCID4 triple at `path` (which should end in `.si4`).
///
/// The iterator is consumed in order. Each item is one game; returning an
/// error aborts the export and removes the temporary files. `description`
/// is stored in the index header (SCID truncates it to 107 bytes).
pub fn write_database<I>(path: &Path, description: &str, games: I) -> Result<ScidWriteReport>
where
    I: IntoIterator<Item = Result<Game>>,
{
    let mut writer = ScidWriter::create(path, description)?;
    for game in games {
        writer.add_game(&game?)?;
    }
    writer.finish()
}

struct ScidWriter {
    target: PathBuf,
    name_target: PathBuf,
    game_target: PathBuf,
    name_tmp: PathBuf,
    game_tmp: PathBuf,
    index_tmp: PathBuf,
    game_file: Option<BufWriter<File>>,
    entries_file: Option<BufWriter<File>>,
    names: [NameTable; 4],
    games: usize,
    game_offset: u64,
    description: String,
    guard: TempGuard,
}

impl ScidWriter {
    fn create(path: &Path, description: &str) -> Result<Self> {
        if !path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("si4"))
        {
            bail!(
                "SCID export target must end in .si4 (got {})",
                path.display()
            );
        }
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            bail!("export folder {} does not exist", parent.display());
        }

        let target = path.to_path_buf();
        let name_target = target.with_extension("sn4");
        let game_target = target.with_extension("sg4");
        let unique = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        );
        let name_tmp = temp_path(&name_target, &unique);
        let game_tmp = temp_path(&game_target, &unique);
        let index_tmp = temp_path(&target, &unique);
        let entries_tmp = temp_path(&target.with_extension("entries"), &unique);

        let game_file = File::create(&game_tmp)
            .with_context(|| format!("creating temporary SCID game file {}", game_tmp.display()))?;
        let entries_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&entries_tmp)
            .with_context(|| {
                format!(
                    "creating temporary SCID index entries {}",
                    entries_tmp.display()
                )
            })?;
        let guard = TempGuard {
            paths: vec![
                name_tmp.clone(),
                game_tmp.clone(),
                index_tmp.clone(),
                entries_tmp.clone(),
            ],
            committed: false,
        };

        Ok(Self {
            target,
            name_target,
            game_target,
            name_tmp,
            game_tmp,
            index_tmp,
            game_file: Some(BufWriter::new(game_file)),
            entries_file: Some(BufWriter::new(entries_file)),
            names: std::array::from_fn(|_| NameTable::default()),
            games: 0,
            game_offset: 0,
            description: description.to_string(),
            guard,
        })
    }

    fn add_game(&mut self, game: &Game) -> Result<()> {
        if self.games >= MAX_GAMES {
            bail!("SCID databases cannot hold more than {MAX_GAMES} games");
        }
        let prepared = self.prepare_game(game)?;
        if prepared.record.len() > MAX_GAME_LEN {
            bail!(
                "game {} - {} needs {} bytes, but one SCID record can hold at most {}",
                game.white(),
                game.black(),
                prepared.record.len(),
                MAX_GAME_LEN
            );
        }
        if self.game_offset > u64::from(u32::MAX) {
            bail!("SCID game file would exceed the 4 GiB offset limit");
        }
        let offset = self.game_offset as u32;
        let length = prepared.record.len() as u32;

        let game_file = self
            .game_file
            .as_mut()
            .context("SCID game file is already closed")?;
        game_file
            .write_all(&prepared.record)
            .with_context(|| format!("writing game {} - {}", game.white(), game.black()))?;
        self.game_offset += prepared.record.len() as u64;

        let pending = PendingEntry {
            offset,
            length,
            ..prepared.entry
        };
        let entries = self
            .entries_file
            .as_mut()
            .context("SCID provisional index is already closed")?;
        write_pending_entry(entries, &pending)?;
        self.games += 1;
        Ok(())
    }

    fn prepare_game(&mut self, game: &Game) -> Result<PreparedGame> {
        let root = game
            .nodes
            .first()
            .context("cannot write a game without a root node")?;
        if let Some(variant) = game.header("Variant") {
            let lower = variant.to_ascii_lowercase();
            if lower.contains("960") || lower.contains("chess960") {
                bail!("Chess960 games cannot be written to SCID yet");
            }
            if !matches!(
                lower.trim(),
                "" | "standard" | "chess" | "normal" | "classical" | "classic"
            ) {
                bail!("variant {:?} cannot be written to SCID", variant);
            }
        }

        let standard_fen = Fen::from_position(&Chess::default(), EnPassantMode::Legal).to_string();
        let root_fen = Fen::from_position(&root.pos, EnPassantMode::Legal).to_string();
        let custom_start = root_fen != standard_fen;

        let mut promotions = false;
        let mut underpromotions = false;
        for node_id in game.mainline() {
            if let Some(ply) = &game.nodes[node_id].mv {
                let uci = ply.uci.as_bytes();
                if uci.len() == 5 {
                    promotions = true;
                    if uci[4] != b'q' {
                        underpromotions = true;
                    }
                }
            }
        }

        let mut record = Vec::new();
        encode_tags(&mut record, game);
        let flags =
            u8::from(custom_start) | (u8::from(promotions) << 1) | (u8::from(underpromotions) << 2);
        record.push(flags);
        if custom_start {
            record.extend_from_slice(root_fen.as_bytes());
            record.push(0);
        }

        let lists = if custom_start {
            PieceLists::from_fen(&root.pos)
        } else {
            PieceLists::standard()
        };
        let mut comments = Vec::new();
        let mut counts = Counts::default();
        MoveEncoder {
            game,
            out: &mut record,
            comments: &mut comments,
            counts: &mut counts,
        }
        .encode_line(0, &root.pos, lists, 0)?;
        for comment in comments {
            record.extend_from_slice(comment.as_bytes());
            record.push(0);
        }

        let (home_pawn, num_halfmoves) = home_pawn_data(game, custom_start)?;
        let final_matsig = material_signature(game.final_position());
        let result = result_code(game.header("Result").unwrap_or("*"));
        let dates = date_field(game);
        let eco = game.header("ECO").map(eco_code).unwrap_or(0);

        let white = self.names[0].intern(game.header("White").unwrap_or("?"), NAME_LIMITS[0])?;
        let black = self.names[0].intern(game.header("Black").unwrap_or("?"), NAME_LIMITS[0])?;
        let event = self.names[1].intern(game.header("Event").unwrap_or("?"), NAME_LIMITS[1])?;
        let site = self.names[2].intern(game.header("Site").unwrap_or("?"), NAME_LIMITS[2])?;
        let round = self.names[3].intern(&round_name(game), NAME_LIMITS[3])?;
        let white_elo = parse_elo(game.header("WhiteElo"));
        let black_elo = parse_elo(game.header("BlackElo"));
        let var_counts = (u16::from(result) << 12)
            | (encode_count(counts.nags) << 8)
            | (encode_count(counts.comments) << 4)
            | encode_count(counts.variations);
        let entry_flags = u16::from(custom_start)
            | (u16::from(promotions) << 1)
            | (u16::from(underpromotions) << 2);

        Ok(PreparedGame {
            record,
            entry: PendingEntry {
                offset: 0,
                length: 0,
                flags: entry_flags,
                white,
                black,
                event,
                site,
                round,
                var_counts,
                eco,
                dates,
                white_elo,
                black_elo,
                final_matsig,
                num_halfmoves,
                home_pawn,
            },
        })
    }

    fn finish(mut self) -> Result<ScidWriteReport> {
        let games = self.games;
        let entries_file = self
            .entries_file
            .take()
            .context("SCID provisional index is already closed")?;
        let mut entries_file = entries_file
            .into_inner()
            .map_err(|err| err.into_error())
            .context("flushing the SCID provisional index")?;
        entries_file
            .flush()
            .context("flushing the SCID provisional index")?;

        let name_order: Vec<Vec<u32>> = self
            .names
            .iter()
            .map(NameTable::sorted_order)
            .collect::<Vec<_>>();
        let final_ids: Vec<Vec<u32>> = name_order
            .iter()
            .enumerate()
            .map(|(kind, order)| {
                let mut ids = vec![0u32; self.names[kind].names.len()];
                for (rank, &intern) in order.iter().enumerate() {
                    ids[intern as usize] = rank as u32;
                }
                ids
            })
            .collect();

        let name_len = write_namebase(&self.name_tmp, &self.names, &name_order)
            .context("writing the SCID namebase")?;

        let index_len = write_index(
            &self.index_tmp,
            &mut entries_file,
            &final_ids,
            games,
            &self.description,
        )
        .context("writing the SCID index")?;

        drop(entries_file);
        let game_file = self
            .game_file
            .take()
            .context("SCID game file is already closed")?;
        let game_file = game_file
            .into_inner()
            .map_err(|err| err.into_error())
            .context("flushing the SCID game file")?;
        game_file.sync_all().context("syncing the SCID game file")?;
        drop(game_file);

        // A SCID database is three files, so the triple cannot be replaced
        // with one atomic rename. Move the old files aside first, install the
        // new companions, and put the index in place last. If anything fails,
        // remove the partial result and restore the old triple.
        let unique = format!(
            "backup-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        );
        let moves = [
            (self.game_tmp.clone(), self.game_target.clone()),
            (self.name_tmp.clone(), self.name_target.clone()),
            (self.index_tmp.clone(), self.target.clone()),
        ];
        let mut backups: Vec<(PathBuf, PathBuf)> = Vec::new();
        for (_, target) in &moves {
            if target.exists() {
                let backup = temp_path(target, &unique);
                if let Err(err) = fs::rename(target, &backup) {
                    restore_backups(&backups);
                    return Err(err).with_context(|| {
                        format!("moving the old database file {} aside", target.display())
                    });
                }
                backups.push((backup, target.clone()));
            }
        }

        let mut installed: Vec<PathBuf> = Vec::new();
        let mut failure = None;
        for (temporary, target) in &moves {
            if let Err(err) = fs::rename(temporary, target) {
                failure = Some((err, target.clone()));
                break;
            }
            installed.push(target.clone());
        }
        if let Some((err, target)) = failure {
            for path in &installed {
                let _ = fs::remove_file(path);
            }
            restore_backups(&backups);
            return Err(err)
                .with_context(|| format!("moving the completed file to {}", target.display()));
        }
        for (backup, _) in &backups {
            let _ = fs::remove_file(backup);
        }
        self.guard.committed = true;

        Ok(ScidWriteReport {
            games,
            bytes: (index_len + name_len) as u64 + self.game_offset,
        })
    }
}

fn restore_backups(backups: &[(PathBuf, PathBuf)]) {
    for (backup, target) in backups {
        let _ = fs::rename(backup, target);
    }
}

impl Drop for ScidWriter {
    fn drop(&mut self) {
        // The guard owns cleanup; dropping the writers first flushes their
        // buffers so the guard can remove the files cleanly.
        self.game_file.take();
        self.entries_file.take();
    }
}

struct PreparedGame {
    record: Vec<u8>,
    entry: PendingEntry,
}

struct TempGuard {
    paths: Vec<PathBuf>,
    committed: bool,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for path in &self.paths {
            let _ = fs::remove_file(path);
        }
    }
}

fn temp_path(target: &Path, unique: &str) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "scid".to_string());
    target.with_file_name(format!(".{name}.{unique}.tmp"))
}

#[derive(Default)]
struct NameTable {
    names: Vec<String>,
    lookup: HashMap<String, u32>,
    frequencies: Vec<u32>,
}

impl NameTable {
    fn intern(&mut self, raw: &str, limit: u32) -> Result<u32> {
        let name = normalize_name(raw);
        if let Some(&id) = self.lookup.get(&name) {
            self.frequencies[id as usize] += 1;
            return Ok(id);
        }
        let id = self.names.len() as u32;
        if id >= limit {
            bail!("SCID namebase is full");
        }
        self.lookup.insert(name.clone(), id);
        self.names.push(name);
        self.frequencies.push(1);
        Ok(id)
    }

    fn sorted_order(&self) -> Vec<u32> {
        let mut order: Vec<u32> = (0..self.names.len() as u32).collect();
        order.sort_by(|&left, &right| {
            self.names[left as usize]
                .as_bytes()
                .cmp(self.names[right as usize].as_bytes())
        });
        order
    }
}

fn normalize_name(raw: &str) -> String {
    let mut name = raw.replace('\0', " ");
    if name.is_empty() {
        name.push('?');
    }
    truncate_utf8(&mut name, MAX_NAME_LEN);
    name
}

fn truncate_utf8(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

#[derive(Default)]
struct Counts {
    comments: usize,
    nags: usize,
    variations: usize,
}

#[derive(Clone, Copy)]
struct PendingEntry {
    offset: u32,
    length: u32,
    flags: u16,
    white: u32,
    black: u32,
    event: u32,
    site: u32,
    round: u32,
    var_counts: u16,
    eco: u16,
    dates: u32,
    white_elo: u16,
    black_elo: u16,
    final_matsig: u32,
    num_halfmoves: u16,
    home_pawn: [u8; HPSIG_SIZE],
}

fn write_pending_entry(writer: &mut impl Write, entry: &PendingEntry) -> Result<()> {
    writer.write_all(&entry.offset.to_be_bytes())?;
    writer.write_all(&entry.length.to_be_bytes())?;
    writer.write_all(&entry.flags.to_be_bytes())?;
    writer.write_all(&entry.white.to_be_bytes())?;
    writer.write_all(&entry.black.to_be_bytes())?;
    writer.write_all(&entry.event.to_be_bytes())?;
    writer.write_all(&entry.site.to_be_bytes())?;
    writer.write_all(&entry.round.to_be_bytes())?;
    writer.write_all(&entry.var_counts.to_be_bytes())?;
    writer.write_all(&entry.eco.to_be_bytes())?;
    writer.write_all(&entry.dates.to_be_bytes())?;
    writer.write_all(&entry.white_elo.to_be_bytes())?;
    writer.write_all(&entry.black_elo.to_be_bytes())?;
    writer.write_all(&entry.final_matsig.to_be_bytes())?;
    writer.write_all(&entry.num_halfmoves.to_be_bytes())?;
    writer.write_all(&entry.home_pawn)?;
    Ok(())
}

fn read_pending_entry(reader: &mut impl Read) -> Result<Option<PendingEntry>> {
    let mut buf = [0u8; PENDING_ENTRY_LEN];
    let mut read = 0usize;
    while read < buf.len() {
        match reader.read(&mut buf[read..]) {
            Ok(0) => break,
            Ok(count) => read += count,
            Err(err) => return Err(err.into()),
        }
    }
    if read == 0 {
        return Ok(None);
    }
    if read != buf.len() {
        bail!("the provisional SCID index is truncated");
    }
    let mut cursor = 0usize;
    Ok(Some(PendingEntry {
        offset: take_u32(&buf, &mut cursor),
        length: take_u32(&buf, &mut cursor),
        flags: take_u16(&buf, &mut cursor),
        white: take_u32(&buf, &mut cursor),
        black: take_u32(&buf, &mut cursor),
        event: take_u32(&buf, &mut cursor),
        site: take_u32(&buf, &mut cursor),
        round: take_u32(&buf, &mut cursor),
        var_counts: take_u16(&buf, &mut cursor),
        eco: take_u16(&buf, &mut cursor),
        dates: take_u32(&buf, &mut cursor),
        white_elo: take_u16(&buf, &mut cursor),
        black_elo: take_u16(&buf, &mut cursor),
        final_matsig: take_u32(&buf, &mut cursor),
        num_halfmoves: take_u16(&buf, &mut cursor),
        home_pawn: {
            let mut home = [0u8; HPSIG_SIZE];
            home.copy_from_slice(&buf[cursor..cursor + HPSIG_SIZE]);
            home
        },
    }))
}

fn take_u16(buf: &[u8], cursor: &mut usize) -> u16 {
    let value = u16::from_be_bytes([buf[*cursor], buf[*cursor + 1]]);
    *cursor += 2;
    value
}

fn take_u32(buf: &[u8], cursor: &mut usize) -> u32 {
    let value = u32::from_be_bytes([
        buf[*cursor],
        buf[*cursor + 1],
        buf[*cursor + 2],
        buf[*cursor + 3],
    ]);
    *cursor += 4;
    value
}

fn write_namebase(path: &Path, names: &[NameTable; 4], order: &[Vec<u32>]) -> Result<usize> {
    let mut file = BufWriter::new(
        File::create(path).with_context(|| format!("creating SCID namebase {}", path.display()))?,
    );
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as u32)
        .unwrap_or(0);
    file.write_all(NAME_MAGIC)?;
    file.write_all(&timestamp.to_be_bytes())?;
    for table in names {
        write_u24(&mut file, table.names.len() as u32)?;
    }
    for table in names {
        write_u24(
            &mut file,
            table.frequencies.iter().copied().max().unwrap_or(0),
        )?;
    }

    let mut bytes = NAME_HEADER_LEN;
    for (kind, table) in names.iter().enumerate() {
        let count = table.names.len();
        if count == 0 {
            continue;
        }
        let id_width = if count >= 65_536 { 3 } else { 2 };
        let max_frequency = table.frequencies.iter().copied().max().unwrap_or(0);
        let frequency_width = if max_frequency >= 65_536 {
            3
        } else if max_frequency >= 256 {
            2
        } else {
            1
        };
        let mut previous = String::new();
        for (rank, &intern) in order[kind].iter().enumerate() {
            let name = &table.names[intern as usize];
            write_width(&mut file, rank as u32, id_width)?;
            write_width(
                &mut file,
                table.frequencies[intern as usize],
                frequency_width,
            )?;
            let length = name.len().min(MAX_NAME_LEN);
            file.write_all(&[length as u8])?;
            let prefix = if rank == 0 {
                0
            } else {
                common_prefix(previous.as_bytes(), name.as_bytes())
            };
            if rank > 0 {
                file.write_all(&[prefix as u8])?;
            }
            file.write_all(&name.as_bytes()[prefix..length])?;
            bytes +=
                id_width + frequency_width + 1 + if rank > 0 { 1 } else { 0 } + length - prefix;
            previous = name.clone();
        }
    }
    let file = file
        .into_inner()
        .map_err(|err| err.into_error())
        .context("flushing the SCID namebase")?;
    file.sync_all().context("syncing the SCID namebase")?;
    Ok(bytes)
}

fn write_index(
    path: &Path,
    entries: &mut File,
    final_ids: &[Vec<u32>],
    games: usize,
    description: &str,
) -> Result<usize> {
    let mut file = BufWriter::new(
        File::create(path).with_context(|| format!("creating SCID index {}", path.display()))?,
    );
    file.write_all(INDEX_MAGIC)?;
    file.write_all(&INDEX_VERSION.to_be_bytes())?;
    file.write_all(&0u32.to_be_bytes())?;
    write_u24(&mut file, games as u32)?;
    write_u24(&mut file, 2)?;
    let mut description_bytes = description.as_bytes().to_vec();
    truncate_utf8_bytes(&mut description_bytes, 107);
    file.write_all(&description_bytes)?;
    file.write_all(&vec![0u8; 108 - description_bytes.len()])?;
    file.write_all(&[0u8; 54])?;

    entries
        .seek(std::io::SeekFrom::Start(0))
        .context("rewinding the provisional SCID index")?;
    let mut reader = BufReader::new(entries);
    let mut written = 0usize;
    while let Some(mut entry) = read_pending_entry(&mut reader)? {
        entry.white = *final_ids[0]
            .get(entry.white as usize)
            .context("SCID player name id is out of range")?;
        entry.black = *final_ids[0]
            .get(entry.black as usize)
            .context("SCID player name id is out of range")?;
        entry.event = *final_ids[1]
            .get(entry.event as usize)
            .context("SCID event name id is out of range")?;
        entry.site = *final_ids[2]
            .get(entry.site as usize)
            .context("SCID site name id is out of range")?;
        entry.round = *final_ids[3]
            .get(entry.round as usize)
            .context("SCID round name id is out of range")?;
        write_index_entry(&mut file, &entry)?;
        written += 1;
    }
    if written != games {
        bail!("the provisional SCID index has {written} entries but {games} games were written");
    }
    let file = file
        .into_inner()
        .map_err(|err| err.into_error())
        .context("flushing the SCID index")?;
    file.sync_all().context("syncing the SCID index")?;
    Ok(INDEX_HEADER_LEN + written * ENTRY_LEN)
}

fn write_index_entry(writer: &mut impl Write, entry: &PendingEntry) -> Result<()> {
    if entry.length > MAX_GAME_LEN as u32 {
        bail!("SCID record length {} is too large", entry.length);
    }
    let length_high = ((entry.length >> 16) as u8) << 7;
    let white_black_high = (((entry.white >> 16) as u8) << 4) | ((entry.black >> 16) as u8);
    let event_site_round_high = (((entry.event >> 16) as u8) << 5)
        | (((entry.site >> 16) as u8) << 2)
        | ((entry.round >> 16) as u8);

    writer.write_all(&entry.offset.to_be_bytes())?;
    writer.write_all(&((entry.length & 0xFFFF) as u16).to_be_bytes())?;
    writer.write_all(&[length_high])?;
    writer.write_all(&entry.flags.to_be_bytes())?;
    writer.write_all(&[white_black_high])?;
    writer.write_all(&((entry.white & 0xFFFF) as u16).to_be_bytes())?;
    writer.write_all(&((entry.black & 0xFFFF) as u16).to_be_bytes())?;
    writer.write_all(&[event_site_round_high])?;
    writer.write_all(&((entry.event & 0xFFFF) as u16).to_be_bytes())?;
    writer.write_all(&((entry.site & 0xFFFF) as u16).to_be_bytes())?;
    writer.write_all(&((entry.round & 0xFFFF) as u16).to_be_bytes())?;
    writer.write_all(&entry.var_counts.to_be_bytes())?;
    writer.write_all(&entry.eco.to_be_bytes())?;
    writer.write_all(&entry.dates.to_be_bytes())?;
    writer.write_all(&entry.white_elo.to_be_bytes())?;
    writer.write_all(&entry.black_elo.to_be_bytes())?;
    writer.write_all(&entry.final_matsig.to_be_bytes())?;
    writer.write_all(&[(entry.num_halfmoves & 0xFF) as u8])?;
    writer.write_all(&[entry.home_pawn[0] | (((entry.num_halfmoves >> 8) as u8) << 6)])?;
    writer.write_all(&entry.home_pawn[1..])?;
    Ok(())
}

fn write_u24(writer: &mut impl Write, value: u32) -> Result<()> {
    writer.write_all(&[
        ((value >> 16) & 0xFF) as u8,
        ((value >> 8) & 0xFF) as u8,
        (value & 0xFF) as u8,
    ])?;
    Ok(())
}

fn write_width(writer: &mut impl Write, value: u32, width: usize) -> Result<()> {
    match width {
        1 => writer.write_all(&[value as u8])?,
        2 => writer.write_all(&(value as u16).to_be_bytes())?,
        3 => write_u24(writer, value)?,
        _ => unreachable!(),
    }
    Ok(())
}

fn common_prefix(left: &[u8], right: &[u8]) -> usize {
    left.iter()
        .zip(right.iter())
        .take_while(|(left, right)| left == right)
        .count()
        .min(MAX_NAME_LEN)
}

fn truncate_utf8_bytes(text: &mut Vec<u8>, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && (text[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    text.truncate(end);
}

fn encode_tags(out: &mut Vec<u8>, game: &Game) {
    let mut seen = HashSet::new();
    for (name, value) in &game.headers {
        if !seen.insert(name.to_ascii_lowercase()) {
            continue;
        }
        if skip_header(name) {
            continue;
        }
        let name = sanitize_tag_name(name);
        if name.is_empty() {
            continue;
        }
        let mut value = value.replace('\0', " ");
        truncate_utf8(&mut value, MAX_TAG_VALUE_LEN);
        if name.len() > MAX_CUSTOM_TAG_LEN {
            continue;
        }
        if let Some(index) = COMMON_TAGS
            .iter()
            .position(|common| common.eq_ignore_ascii_case(&name))
        {
            out.push(241 + index as u8);
        } else {
            out.push(name.len() as u8);
            out.extend_from_slice(name.as_bytes());
        }
        out.push(value.len() as u8);
        out.extend_from_slice(value.as_bytes());
    }
    out.push(0);
}

fn skip_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "event"
            | "site"
            | "date"
            | "round"
            | "white"
            | "black"
            | "result"
            | "eco"
            | "whiteelo"
            | "blackelo"
            | "eventdate"
            | "plycount"
            | "fen"
            | "setup"
            | "scidflags"
            | "variant"
            | "board"
            | "whiteratingtype"
            | "blackratingtype"
    )
}

fn sanitize_tag_name(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .take(MAX_CUSTOM_TAG_LEN)
        .collect()
}

fn round_name(game: &Game) -> String {
    let round = game.header("Round").unwrap_or("?").trim();
    let board = game
        .header("Board")
        .map(str::trim)
        .filter(|board| !board.is_empty());
    match board {
        Some(board) => {
            let base = round
                .split_once('.')
                .map(|(round, _)| round)
                .unwrap_or(round);
            if base.is_empty() || base == "?" {
                format!("?.{board}")
            } else {
                format!("{base}.{board}")
            }
        }
        None => {
            if round.is_empty() {
                "?".to_string()
            } else {
                round.to_string()
            }
        }
    }
}

struct MoveEncoder<'a> {
    game: &'a Game,
    out: &'a mut Vec<u8>,
    comments: &'a mut Vec<String>,
    counts: &'a mut Counts,
}

impl MoveEncoder<'_> {
    fn encode_line(
        &mut self,
        start: usize,
        start_pos: &Chess,
        mut lists: PieceLists,
        depth: usize,
    ) -> Result<()> {
        let mut current = start;
        let mut pos = start_pos.clone();
        if depth == 0
            && let Some(comment) = comment_text(&self.game.nodes[current])
        {
            self.out.push(12);
            self.comments.push(comment);
            self.counts.comments += 1;
        }

        loop {
            let children = &self.game.nodes[current].children;
            let Some(&main) = children.first() else {
                break;
            };
            self.encode_node_move(main, &pos, &mut lists)?;

            for &variation in &children[1..] {
                self.out.push(13);
                self.counts.variations += 1;
                self.encode_variation(variation, &pos, lists.clone(), depth + 1)?;
            }

            current = main;
            pos = self.game.nodes[main].pos.clone();
        }

        self.out.push(if depth == 0 { 15 } else { 14 });
        Ok(())
    }

    /// Encode a variation's first move and its continuation. The caller writes
    /// the variation-start token; the matching end token comes from `encode_line`.
    fn encode_variation(
        &mut self,
        first: usize,
        pos_before: &Chess,
        mut lists: PieceLists,
        depth: usize,
    ) -> Result<()> {
        self.encode_node_move(first, pos_before, &mut lists)?;
        let after = self.game.nodes[first].pos.clone();
        self.encode_line(first, &after, lists, depth)
    }

    fn encode_node_move(
        &mut self,
        node_id: usize,
        pos_before: &Chess,
        lists: &mut PieceLists,
    ) -> Result<()> {
        let node = self
            .game
            .nodes
            .get(node_id)
            .with_context(|| format!("SCID node {node_id} is missing"))?;
        let ply = node
            .mv
            .as_ref()
            .with_context(|| format!("SCID node {node_id} is missing its move"))?;

        if ply.uci == "0000" || ply.san == "--" {
            self.out.push(0);
        } else {
            let uci = UciMove::from_ascii(ply.uci.as_bytes())
                .with_context(|| format!("invalid UCI move {:?}", ply.uci))?;
            let mv = uci
                .to_move(pos_before)
                .with_context(|| format!("illegal UCI move {:?}", ply.uci))?;
            encode_move(lists, mv, self.out)?;
            lists
                .apply(pos_before, mv)
                .with_context(|| format!("updating the SCID piece list for {:?}", ply.uci))?;
        }

        for nag in &node.nags {
            if *nag != 0 {
                self.out.push(11);
                self.out.push(*nag);
                self.counts.nags += 1;
            }
        }
        if let Some(comment) = comment_text(node) {
            self.out.push(12);
            self.comments.push(comment);
            self.counts.comments += 1;
        }
        Ok(())
    }
}

fn comment_text(node: &Node) -> Option<String> {
    let mut text = node
        .comment
        .replace('\0', " ")
        .replace('{', "(")
        .replace('}', ")");
    if let Some(clock) = node.clock {
        if !text.trim().is_empty() {
            text.push(' ');
        }
        text.push_str("[%clk ");
        text.push_str(&clock_text(clock));
        text.push(']');
    }
    (!text.trim().is_empty()).then_some(text)
}

fn clock_text(clock: Clock) -> String {
    let total = clock.centis;
    let hours = total / 360_000;
    let minutes = (total % 360_000) / 6_000;
    let seconds = (total % 6_000) / 100;
    let centis = total % 100;
    let fraction = if centis > 0 {
        format!(".{centis:02}")
    } else {
        String::new()
    };
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}{fraction}")
    } else {
        format!("{minutes}:{seconds:02}{fraction}")
    }
}

fn encode_move(lists: &PieceLists, mv: Move, out: &mut Vec<u8>) -> Result<()> {
    let Some(from) = mv.from() else {
        bail!("piece drops cannot be stored in a SCID game");
    };
    if let Move::Castle { king, rook } = mv {
        let slot = lists
            .slot(king)
            .with_context(|| format!("SCID piece list has no king on {king}"))?;
        if slot != 0 {
            bail!("SCID king is not in piece-list slot 0");
        }
        let kingside = rook.file() > king.file();
        out.push(if kingside { 10 } else { 9 });
        return Ok(());
    }

    let slot = lists
        .slot(from)
        .with_context(|| format!("SCID piece list has no piece on {from}"))?;
    if slot > 15 {
        bail!("SCID piece-list slot {slot} does not fit in a move byte");
    }
    let role = mv.role();
    let to = mv.to();
    let mut second = None;
    let code = match role {
        Role::Pawn => {
            let diff = (to.to_u32() as i32 - from.to_u32() as i32).abs();
            let mut code = if diff == 16 {
                15
            } else {
                match diff {
                    7 => 0,
                    8 => 1,
                    9 => 2,
                    _ => bail!("invalid pawn move {from} -> {to}"),
                }
            };
            if let Some(promotion) = mv.promotion() {
                let value = match promotion {
                    Role::Queen => 2,
                    Role::Rook => 3,
                    Role::Bishop => 4,
                    Role::Knight => 5,
                    _ => bail!("invalid promotion piece {promotion:?}"),
                };
                code += 3 * (value - 1);
            }
            code
        }
        Role::Knight => {
            let diff = to.to_u32() as i32 - from.to_u32() as i32;
            match diff {
                -17 => 1,
                -15 => 2,
                -10 => 3,
                -6 => 4,
                6 => 5,
                10 => 6,
                15 => 7,
                17 => 8,
                _ => bail!("invalid knight move {from} -> {to}"),
            }
        }
        Role::Bishop => {
            let rank_diff = to.rank().to_u32() as i32 - from.rank().to_u32() as i32;
            let file_diff = to.file().to_u32() as i32 - from.file().to_u32() as i32;
            let mut code = to.file().to_u32() as u8;
            if rank_diff * file_diff < 0 {
                code += 8;
            }
            code
        }
        Role::Rook => {
            if from.rank() == to.rank() {
                to.file().to_u32() as u8
            } else if from.file() == to.file() {
                8 + to.rank().to_u32() as u8
            } else {
                bail!("invalid rook move {from} -> {to}");
            }
        }
        Role::Queen => {
            if from.rank() == to.rank() {
                to.file().to_u32() as u8
            } else if from.file() == to.file() {
                8 + to.rank().to_u32() as u8
            } else {
                second = Some(64 + to.to_u32() as u8);
                from.file().to_u32() as u8
            }
        }
        Role::King => {
            let diff = to.to_u32() as i32 - from.to_u32() as i32;
            match diff {
                -9 => 1,
                -8 => 2,
                -7 => 3,
                -1 => 4,
                1 => 5,
                7 => 6,
                8 => 7,
                9 => 8,
                _ => bail!("invalid king move {from} -> {to}"),
            }
        }
    };
    out.push(((slot as u8) << 4) | code);
    if let Some(byte) = second {
        out.push(byte);
    }
    Ok(())
}

fn home_pawn_data(game: &Game, custom_start: bool) -> Result<([u8; HPSIG_SIZE], u16)> {
    let mut data = [0u8; HPSIG_SIZE];
    // SCID stores only the low ten bits of the half-move count.
    let num_halfmoves = game.mainline_len().min(1023) as u16;
    if custom_start {
        return Ok((data, num_halfmoves));
    }
    let mut mask = 0xFFFFu16;
    let mut changes = Vec::new();
    for node in game.mainline() {
        let next = home_pawn_mask(&game.nodes[node].pos);
        if next != mask {
            let diff = mask & !next;
            if diff == 0 || !diff.is_power_of_two() {
                bail!("home-pawn signature changed by more than one pawn in one move");
            }
            changes.push(diff.trailing_zeros() as u8);
            mask = next;
        }
    }
    if changes.len() > 16 {
        bail!("home-pawn signature has more than 16 changes");
    }
    data[0] = changes.len() as u8;
    for (index, &change) in changes.iter().enumerate() {
        if index % 2 == 0 {
            data[1 + index / 2] = change << 4;
        } else {
            data[1 + index / 2] |= change;
        }
    }
    Ok((data, num_halfmoves))
}

fn home_pawn_mask(pos: &Chess) -> u16 {
    let mut mask = 0u16;
    for file in 0..8u32 {
        let file = ChessFile::new(file);
        if let Some(piece) = pos
            .board()
            .piece_at(Square::from_coords(file, Rank::Second))
            && piece.color == Color::White
            && piece.role == Role::Pawn
        {
            mask |= 1 << (15 - file.to_u32());
        }
        if let Some(piece) = pos
            .board()
            .piece_at(Square::from_coords(file, Rank::Seventh))
            && piece.color == Color::Black
            && piece.role == Role::Pawn
        {
            mask |= 1 << (7 - file.to_u32());
        }
    }
    mask
}

fn material_signature(pos: &Chess) -> u32 {
    let mut white_pawns = 0u32;
    let mut white_knights = 0u32;
    let mut white_bishops = 0u32;
    let mut white_rooks = 0u32;
    let mut white_queens = 0u32;
    let mut black_pawns = 0u32;
    let mut black_knights = 0u32;
    let mut black_bishops = 0u32;
    let mut black_rooks = 0u32;
    let mut black_queens = 0u32;
    for square in 0..64 {
        let Some(piece) = pos.board().piece_at(Square::new(square)) else {
            continue;
        };
        match (piece.color, piece.role) {
            (Color::White, Role::Pawn) => white_pawns += 1,
            (Color::White, Role::Knight) => white_knights += 1,
            (Color::White, Role::Bishop) => white_bishops += 1,
            (Color::White, Role::Rook) => white_rooks += 1,
            (Color::White, Role::Queen) => white_queens += 1,
            (Color::Black, Role::Pawn) => black_pawns += 1,
            (Color::Black, Role::Knight) => black_knights += 1,
            (Color::Black, Role::Bishop) => black_bishops += 1,
            (Color::Black, Role::Rook) => black_rooks += 1,
            (Color::Black, Role::Queen) => black_queens += 1,
            _ => {}
        }
    }
    (white_pawns.min(15) << 12)
        | (white_knights.min(3) << 16)
        | (white_bishops.min(3) << 18)
        | (white_rooks.min(3) << 20)
        | (white_queens.min(3) << 22)
        | black_pawns.min(15)
        | (black_knights.min(3) << 4)
        | (black_bishops.min(3) << 6)
        | (black_rooks.min(3) << 8)
        | (black_queens.min(3) << 10)
}

fn result_code(result: &str) -> u8 {
    let result = result.trim();
    if result.starts_with("1-0") {
        1
    } else if result.starts_with("0-1") {
        2
    } else if result.starts_with("1/2") {
        3
    } else {
        0
    }
}

fn date_field(game: &Game) -> u32 {
    let date = parse_date(game.header("Date").unwrap_or(""));
    let event = parse_date(game.header("EventDate").unwrap_or(""));
    let date_year = (date >> 9) & 0x7FF;
    let event_year = (event >> 9) & 0x7FF;
    let coded_event =
        if event == 0 || date_year == 0 || event_year == 0 || event_year.abs_diff(date_year) > 3 {
            0
        } else {
            let delta = (event_year as i32 + 4 - date_year as i32) & 7;
            ((delta as u32) << 9) | (event & 0x1FF)
        };
    (coded_event << 20) | (date & 0x000F_FFFF)
}

fn parse_date(text: &str) -> u32 {
    let mut parts = text.trim().split('.');
    let year = parse_date_part(parts.next(), 2047);
    let month = parse_date_part(parts.next(), 12);
    let day = parse_date_part(parts.next(), 31);
    (year << 9) | (month << 5) | day
}

fn parse_date_part(text: Option<&str>, max: u32) -> u32 {
    let Some(text) = text else {
        return 0;
    };
    let digits: String = text.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    let value = digits.parse::<u32>().unwrap_or(0);
    if value > max { 0 } else { value }
}

fn parse_elo(text: Option<&str>) -> u16 {
    let Some(text) = text else {
        return 0;
    };
    let digits: String = text
        .trim()
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse::<u16>().unwrap_or(0).min(MAX_ELO)
}

fn eco_code(text: &str) -> u16 {
    let bytes = text.trim().as_bytes();
    let Some(&first) = bytes.first() else {
        return 0;
    };
    let mut code = match first {
        b'A'..=b'E' => u32::from(first - b'A') * 13_100,
        b'a'..=b'e' => u32::from(first - b'a') * 13_100,
        _ => return 0,
    };
    if bytes.len() == 1 {
        return (code + 1) as u16;
    }
    let Some(&second) = bytes.get(1) else {
        return 0;
    };
    if !second.is_ascii_digit() {
        return 0;
    }
    code += u32::from(second - b'0') * 1_310;
    if bytes.len() == 2 {
        return (code + 1) as u16;
    }
    let Some(&third) = bytes.get(2) else {
        return 0;
    };
    if !third.is_ascii_digit() {
        return 0;
    }
    code += u32::from(third - b'0') * 131;
    if let Some(&extension) = bytes.get(3)
        && extension.is_ascii_lowercase()
    {
        code += 1;
        code += u32::from(extension - b'a') * 5;
        if let Some(&digit) = bytes.get(4)
            && (b'1'..=b'4').contains(&digit)
        {
            code += u32::from(digit - b'0');
        }
    }
    (code + 1).min(u32::from(u16::MAX)) as u16
}

fn encode_count(value: usize) -> u16 {
    match value {
        0..=10 => value as u16,
        11..=12 => 10,
        13..=17 => 11,
        18..=24 => 12,
        25..=34 => 13,
        35..=44 => 14,
        _ => 15,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::parse_pgn;
    use crate::scid::ScidLibrary;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir()
                .join(format!("brett-scid-writer-{}-{unique}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> PathBuf {
            self.0.join("out.si4")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn round_trip(pgn: &str) -> (Game, ScidLibrary, TempDir) {
        let dir = TempDir::new();
        let path = dir.path();
        let games = parse_pgn(pgn.as_bytes()).unwrap();
        assert!(!games.is_empty());
        let report =
            write_database(&path, "Brett writer test", games.iter().cloned().map(Ok)).unwrap();
        assert_eq!(report.games, games.len());
        let library = ScidLibrary::open(&path).unwrap();
        let loaded = library.load(0).unwrap();
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        (games.into_iter().next().unwrap(), library, dir)
    }

    fn sans(game: &Game) -> Vec<String> {
        game.mainline()
            .into_iter()
            .map(|node| game.nodes[node].san().unwrap_or("--").to_string())
            .collect()
    }

    #[test]
    fn round_trips_headers_comments_nags_and_variations() {
        let (original, library, _dir) = round_trip(
            r#"[Event "Writer Test"]
[Site "Test Site"]
[Date "2026.10.27"]
[Round "1"]
[White "Alice"]
[Black "Bob"]
[Result "1-0"]
[WhiteElo "2500"]
[BlackElo "2400"]
[ECO "C41"]
[Opening "Philidor Defense"]
[Annotator "Brett"]
[TimeControl "600+5"]
[WhiteTitle "GM"]
[BlackTitle "IM"]

1. e4 e5 {King pawn.} 2. Nf3 (2. Bc4 Nf6) 2... d6 $1 3. d4 1-0
"#,
        );
        let loaded = library.load(0).unwrap();
        assert_eq!(loaded.header("White"), Some("Alice"));
        assert_eq!(loaded.header("Black"), Some("Bob"));
        assert_eq!(loaded.header("WhiteElo"), Some("2500"));
        assert_eq!(loaded.header("BlackElo"), Some("2400"));
        assert_eq!(loaded.header("ECO"), Some("C41"));
        assert_eq!(loaded.header("Opening"), Some("Philidor Defense"));
        assert_eq!(loaded.header("WhiteTitle"), Some("GM"));
        assert_eq!(sans(&loaded), sans(&original));
        assert!(
            loaded
                .nodes
                .iter()
                .any(|node| node.comment.contains("King pawn."))
        );
        assert!(loaded.nodes.iter().any(|node| node.nags.contains(&1)));
        let branch = loaded
            .nodes
            .iter()
            .position(|node| node.san() == Some("e5"))
            .expect("the branch point is missing");
        assert_eq!(loaded.nodes[branch].children.len(), 2);
        assert!(loaded.nodes.iter().any(|node| node.san() == Some("Bc4")));
    }

    #[test]
    fn round_trips_castling_en_passant_and_promotions() {
        let (original, library, _dir) = round_trip(
            r#"[Event "Tactics"]
[Site "Test"]
[Date "2026.10.27"]
[Round "1"]
[White "A"]
[Black "B"]
[Result "*"]

1. e4 e5 2. Nf3 Nc6 3. Bc4 Bc5 4. O-O Nf6 5. d3 O-O 6. Nc3 d6
7. Bg5 Be6 8. Nd5 Bxd5 9. exd5 Nb8 10. d4 exd4 11. Nxd4 h6
12. Bh4 g5 13. Bg3 Nbd7 14. Re1 Re8 15. Qd2 Ne5 16. Bxe5 dxe5
17. dxe5 Qxd2 18. Nf5 Qxc2 19. e6 Qxb2 20. exf7+ Kxf7 21. Ne7 *
"#,
        );
        let loaded = library.load(0).unwrap();
        assert_eq!(sans(&loaded), sans(&original));

        let (original, library, _dir) = round_trip(
            r#"[Event "En passant"]
[Site "Test"]
[Date "2026.10.27"]
[Round "1"]
[White "A"]
[Black "B"]
[Result "*"]

1. e4 a6 2. e5 d5 3. exd6 *
"#,
        );
        let loaded = library.load(0).unwrap();
        assert_eq!(sans(&loaded), sans(&original));

        let (original, library, _dir) = round_trip(
            r#"[Event "Underpromotion"]
[Site "Test"]
[Date "2026.10.27"]
[Round "1"]
[White "A"]
[Black "B"]
[Result "*"]
[SetUp "1"]
[FEN "8/P7/8/8/8/8/7k/4K3 w - - 0 1"]

1. a8=N *
"#,
        );
        let loaded = library.load(0).unwrap();
        assert_eq!(sans(&loaded), sans(&original));
        assert_eq!(loaded.header("FEN"), Some("8/P7/8/8/8/8/7k/4K3 w - - 0 1"));
    }

    #[test]
    fn round_trips_null_moves_and_queen_diagonals() {
        let (original, library, _dir) = round_trip(
            r#"[Event "Null and queen"]
[Site "Test"]
[Date "2026.10.27"]
[Round "1"]
[White "A"]
[Black "B"]
[Result "*"]

1. -- e5 2. Nf3 Nc6 3. e4 Nf6 4. Qe2 Bc5 5. Qc4 d6 6. d3 Be6
7. Qa4 Bd7 8. Qb3 Na5 9. Qb5 Nc6 10. Bg5 Be7 11. Bxf6 Bxf6 *
"#,
        );
        let loaded = library.load(0).unwrap();
        assert_eq!(sans(&loaded), sans(&original));
        assert!(sans(&loaded).iter().any(|san| san == "--"));
    }

    #[test]
    fn preserves_clock_comments() {
        let (original, library, _dir) = round_trip(
            r#"[Event "Clocks"]
[Site "Test"]
[Date "2026.10.27"]
[Round "1"]
[White "A"]
[Black "B"]
[Result "*"]

1. e4 {[%clk 0:03:00]} e5 {[%clk 0:02:58.4]} 2. Nf3 *
"#,
        );
        let loaded = library.load(0).unwrap();
        assert_eq!(sans(&loaded), sans(&original));
        let clocks: Vec<_> = loaded.nodes.iter().filter_map(|node| node.clock).collect();
        assert_eq!(clocks.len(), 2);
        assert_eq!(clocks[0].centis, 18_000);
        assert_eq!(clocks[1].centis, 17_840);
    }

    #[test]
    fn writes_an_empty_database() {
        let dir = TempDir::new();
        let path = dir.path();
        let report = write_database(&path, "Empty", std::iter::empty::<Result<Game>>()).unwrap();
        assert_eq!(report.games, 0);
        let library = ScidLibrary::open(&path).unwrap();
        assert_eq!(library.len(), 0);
    }

    #[test]
    fn rejects_a_target_that_is_not_si4() {
        let dir = TempDir::new();
        let error = write_database(
            &dir.0.join("out.si3"),
            "Bad",
            std::iter::empty::<Result<Game>>(),
        )
        .unwrap_err();
        assert!(error.to_string().contains(".si4"));
    }

    #[test]
    fn namebase_ids_are_dense_and_sorted() {
        let dir = TempDir::new();
        let path = dir.path();
        let games = parse_pgn(
            br#"[Event "Zeta"]
[Site "B"]
[Date "2026.10.27"]
[Round "1"]
[White "Zoe"]
[Black "Adam"]
[Result "*"]

1. e4 *
"#,
        )
        .unwrap();
        write_database(&path, "Names", games.into_iter().map(Ok)).unwrap();
        let bytes = fs::read(path.with_extension("sn4")).unwrap();
        assert_eq!(&bytes[0..8], NAME_MAGIC);
        // Players are the first name list. The first two entries should be
        // Adam and Zoe in alphabetical order, with ids 0 and 1.
        let mut cursor = NAME_HEADER_LEN;
        let mut names = Vec::new();
        for index in 0..2 {
            let id = u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]) as usize;
            cursor += 2;
            cursor += 1; // frequency
            let length = bytes[cursor] as usize;
            cursor += 1;
            let prefix = if index == 0 {
                0
            } else {
                let prefix = bytes[cursor] as usize;
                cursor += 1;
                prefix
            };
            let tail = &bytes[cursor..cursor + length - prefix];
            cursor += length - prefix;
            let mut name = names
                .last()
                .map(|name: &Vec<u8>| name[..prefix].to_vec())
                .unwrap_or_default();
            name.extend_from_slice(tail);
            assert_eq!(id, index);
            names.push(name);
        }
        assert_eq!(names, [b"Adam".to_vec(), b"Zoe".to_vec()]);
    }

    #[test]
    fn rejects_chess960() {
        let dir = TempDir::new();
        let path = dir.path();
        let games = parse_pgn(
            br#"[Event "960"]
[Site "Test"]
[Date "2026.10.27"]
[Round "1"]
[White "A"]
[Black "B"]
[Result "*"]
[Variant "Chess960"]
[FEN "bbqnnrkr/pppppppp/8/8/8/8/PPPPPPPP/BBQNNRKR w KQkq - 0 1"]

1. e4 *
"#,
        )
        .unwrap();
        let error = write_database(&path, "960", games.into_iter().map(Ok)).unwrap_err();
        assert!(error.to_string().contains("Chess960"));
    }
}
