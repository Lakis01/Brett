//! Native writer for ChessBase's current multi-file database format
//! (`.2cbh`, `.2cbg`, `.2cba`, and `.2lid`).
//!
//! Games are streamed to temporary files beside the target.  On completion the
//! existing companion files at the target stem are moved aside to backup paths
//! and replaced atomically, rolling back on error.  Derived `.2lcd`/`.2lgd`
//! indexes are removed with the backups so ChessBase rebuilds them.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use memmap2::Mmap;

use crate::chess::{
    CastlingMode, Chess, Color, EnPassantMode, Fen, File as ChessFile, Move, Position, Rank, Role,
    SanPlus, Square, UciMove,
};
use crate::domain::{Game, Ply};

use super::moves;
use super::tokens::table;

use super::{
    CBG_MAGIC, LID_EVENT, LID_FIRST, LID_SOURCE, LID_STRIDE, RECORD_SIZE, START_ID_BASE, be_u32,
    decode_text, le_u32, le_u64, sibling,
};

const LID_PLAYER: usize = 0;

/// The four files that make up a 2CBH database.
pub const CORE_EXTENSIONS: [&str; 4] = ["2cbh", "2cbg", "2cba", "2lid"];

/// ChessBase index files derived from a 2CBH database.
pub const DERIVED_EXTENSIONS: [&str; 2] = ["2lcd", "2lgd"];

/// Every file that belongs to a 2CBH stem (core files, derived indexes, plus Brett annotations sidecar).
pub const ALL_EXTENSIONS: [&str; 7] = ["2cbh", "2cbg", "2cba", "2lid", "2lcd", "2lgd", "brettann"];

const CBG_HEADER: u64 = 12;
const CBG_VERSION: u16 = 0x0500;
const CBA_VERSION: u16 = 0;
const RECORD_ZERO_GENERATION: u8 = 0x26;

/// Summary of a completed 2CBH export.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TwoCbhWriteReport {
    pub games: usize,
    pub bytes: u64,
}

/// Options controlling what a 2CBH export may discard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TwoCbhWriteOptions {
    /// Allow writing games whose comments, NAGs, clocks, or evaluations are
    /// not representable in this container-first pass.
    pub allow_annotation_loss: bool,
    /// Write a lossless gzip-JSON `<stem>.brettann` sidecar alongside `.2cbh`
    /// so comments, NAGs, clocks, evaluations, and board shapes round-trip.
    pub write_brett_annotations_sidecar: bool,
}

/// Whether `game` carries comments, NAGs, clocks, or evaluations that the
/// first 2CBH writer pass does not preserve.
pub fn has_unsupported_annotations(game: &Game) -> bool {
    first_unsupported_annotation(game).is_some()
}

/// The first annotation the writer cannot represent, as its feature name and
/// node index.
fn first_unsupported_annotation(game: &Game) -> Option<(&'static str, usize)> {
    game.nodes.iter().enumerate().find_map(|(node, value)| {
        if !value.comment.trim().is_empty() {
            Some(("comment", node))
        } else if !value.shapes.is_empty() {
            Some(("drawing", node))
        } else if !value.nags.is_empty() {
            Some(("NAG", node))
        } else if value.clock.is_some() {
            Some(("clock", node))
        } else if value.eval.is_some() {
            Some(("evaluation", node))
        } else {
            None
        }
    })
}

/// Return the four core companion paths for a `.2cbh` path or bare stem.
pub fn core_companion_files(path: &Path) -> [PathBuf; 4] {
    let stem = lenient_stem(path);
    CORE_EXTENSIONS.map(|ext| sibling(&stem, &format!(".{ext}")))
}

/// Return every existing 2CBH companion file sharing `path`'s stem.
pub fn existing_companion_files(path: &Path) -> Vec<PathBuf> {
    let stem = lenient_stem(path);
    let mut out = Vec::new();
    for ext in ALL_EXTENSIONS {
        let candidate = sibling(&stem, &format!(".{ext}"));
        if candidate.exists() && !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// Write a 2CBH database at `path`, refusing annotation loss.
pub fn write_database<I>(path: &Path, games: I) -> Result<TwoCbhWriteReport>
where
    I: IntoIterator<Item = Result<Game>>,
{
    write_database_with_options(path, TwoCbhWriteOptions::default(), games)
}

/// Write a 2CBH database at `path` with explicit options.
pub fn write_database_with_options<I>(
    path: &Path,
    options: TwoCbhWriteOptions,
    games: I,
) -> Result<TwoCbhWriteReport>
where
    I: IntoIterator<Item = Result<Game>>,
{
    let stem = normalize_stem(path)?;
    if let Some(parent) = stem.parent()
        && !parent.as_os_str().is_empty()
        && !parent.is_dir()
    {
        bail!("2CBH export directory {} does not exist", parent.display());
    }

    let mut writer = TwoCbhWriter::create(&stem, options)?;
    for (index, game) in games.into_iter().enumerate() {
        let game = game.with_context(|| format!("reading 2CBH game {index}"))?;
        writer.add_game(&game).with_context(|| {
            format!(
                "writing 2CBH game {index} ({} - {})",
                game.white(),
                game.black()
            )
        })?;
    }
    writer.finish()
}

fn normalize_stem(path: &Path) -> Result<PathBuf> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext)
            if ext.eq_ignore_ascii_case("2cbh")
                || ext.eq_ignore_ascii_case("2cbg")
                || ext.eq_ignore_ascii_case("2cba")
                || ext.eq_ignore_ascii_case("2lid") =>
        {
            Ok(path.with_extension(""))
        }
        Some(_) => bail!(
            "2CBH export target must end in .2cbh or name a database stem (got {})",
            path.display()
        ),
        None => Ok(path.to_path_buf()),
    }
}

fn lenient_stem(path: &Path) -> PathBuf {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext)
            if CORE_EXTENSIONS
                .iter()
                .any(|candidate| ext.eq_ignore_ascii_case(candidate)) =>
        {
            path.with_extension("")
        }
        _ => path.to_path_buf(),
    }
}

/// Append an extension without replacing a dotted stem's last component.
fn core_path(stem: &Path, ext: &str) -> PathBuf {
    let mut path = stem.as_os_str().to_owned();
    path.push(ext);
    PathBuf::from(path)
}

/// Frame one `.2cbg` or `.2cba` record at `offset` with the 2CBH padding rule.
fn frame_record(
    content: &[u8],
    terminator: [u8; 2],
    base_padding: u64,
    key: u64,
    offset: u64,
) -> Vec<u8> {
    let a = content.len() as u64;
    let b = base_padding + (8 - ((offset + 34 + a + base_padding) % 8)) % 8;
    let total = 34 + a + b;
    debug_assert!(a <= u64::from(u32::MAX), "2CBH record content is too large");
    debug_assert!(b <= u64::from(u32::MAX), "2CBH record padding is too large");

    // magic (8), A (4), B (4), key (8), content (a), terminator (2), zero
    // padding (b), trailer (8).
    let mut record = Vec::with_capacity(total as usize);
    record.extend_from_slice(&CBG_MAGIC);
    record.extend_from_slice(&(a as u32).to_le_bytes());
    record.extend_from_slice(&(b as u32).to_le_bytes());
    record.extend_from_slice(&key.to_be_bytes());
    record.extend_from_slice(content);
    record.extend_from_slice(&terminator);
    record.resize(record.len() + b as usize, 0);
    record.extend_from_slice(&total.to_le_bytes());
    debug_assert_eq!(record.len() as u64, total);
    record
}

/// Deterministic non-zero opaque key for one `.2cbg` record.
fn record_key(index: u64, content: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in index.to_le_bytes().iter().chain(content) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    if hash == 0 { 1 } else { hash }
}

/// The start position a game is encoded against.
enum Start {
    Standard,
    Chess960(u16),
    Custom(Box<Chess>),
}

#[derive(Debug)]
struct EncodedMoves {
    start_id: Option<u16>,
    custom_start: Option<Vec<u16>>,
    tokens: Vec<u16>,
}

/// Pick the 2CBH start encoding that matches `game`'s root position.
fn detect_start(game: &Game) -> Result<Start> {
    let root = &game.nodes[0].pos;
    let fen = start_fen_prefix(root);
    let variant = game
        .header("Variant")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let variant_960 = variant.contains("960") || variant.contains("fischer");
    let variant_from_pos = variant == "from position" || variant == "fromposition";
    let variant_standard =
        variant.is_empty() || variant == "standard" || variant == "chess" || variant_from_pos;
    if !variant_960 && !variant_standard {
        bail!("2CBH export does not support variant {variant:?}");
    }

    if variant_960 || root.castling_mode().is_chess960() {
        if !root.castling_mode().is_chess960() {
            bail!("2CBH variant tag says Chess960 but the start position uses standard castling");
        }
        for id in 0..960u16 {
            if start_fen_prefix(&moves::start_position_960(id)?) == fen {
                return Ok(Start::Chess960(id));
            }
        }
        bail!("2CBH export start position is not standard chess or Chess960: {fen}");
    }

    if root.castling_mode() != CastlingMode::Standard {
        bail!("2CBH export start position uses unsupported castling mode: {fen}");
    }
    if variant_from_pos || fen != start_fen_prefix(&Chess::default()) || root.fullmoves().get() != 1
    {
        return Ok(Start::Custom(Box::new(root.clone())));
    }
    Ok(Start::Standard)
}

/// The first four FEN fields, ignoring clocks and move numbers.
fn start_fen_prefix(pos: &Chess) -> String {
    Fen::from_position(pos, EnPassantMode::Legal)
        .to_string()
        .split_whitespace()
        .take(4)
        .collect::<Vec<_>>()
        .join(" ")
}

fn encode_game_moves(game: &Game) -> Result<EncodedMoves> {
    let start = detect_start(game)?;
    let (start_id, custom_start) = match start {
        Start::Standard => (None, None),
        Start::Chess960(id) => (Some(id), None),
        Start::Custom(pos) => (None, Some(moves::encode_custom_start(&pos)?)),
    };
    let mut tokens = Vec::new();
    if let Some(&first) = game.nodes[0].children.first() {
        encode_line(game, first, false, start_id, &mut tokens)?;
    }
    Ok(EncodedMoves {
        start_id,
        custom_start,
        tokens,
    })
}

fn encode_move(before: &Chess, mv: Move, chess960_start: Option<u16>) -> Result<u16> {
    if let (Move::Castle { king, rook }, Some(start_id)) = (mv, chess960_start) {
        let color_offset = if before.turn() == Color::Black { 2 } else { 0 };
        let side_offset = if rook.file() > king.file() { 1 } else { 0 };
        return Ok(45_357 + start_id * 4 + color_offset + side_offset);
    }
    let entry = move_entry(before, mv)?;
    table().token_for(entry).with_context(|| {
        format!(
            "cannot encode move {mv} for 2CBH in position {}",
            Fen::from_position(before, EnPassantMode::Legal)
        )
    })
}

/// The eight stored fields for one move, before the token table lookup.
fn move_entry(before: &Chess, mv: Move) -> Result<[u16; 8]> {
    let mover = before.turn();
    let mover_bit = if mover == Color::Black { 8 } else { 0 };
    let captured_bit = if mover == Color::White { 8 } else { 0 };
    match mv {
        Move::Normal {
            role,
            from,
            capture,
            to,
            promotion,
        } => {
            let from = u16::from(moves::to_stored_square(from));
            let to = u16::from(moves::to_stored_square(to));
            let piece = role_code(role) | mover_bit;
            let aux = match capture {
                Some(Role::Pawn) if promotion.is_some() => role_code(Role::Queen) | captured_bit,
                Some(captured) => role_code(captured) | captured_bit,
                None => 0,
            };
            match promotion {
                Some(promoted) => Ok([
                    from,
                    to,
                    piece,
                    aux,
                    to,
                    to,
                    role_code(promoted) | mover_bit,
                    piece,
                ]),
                None => Ok([from, to, piece, aux, 0, 0, 0, 0]),
            }
        }
        Move::EnPassant { from, to } => {
            let from = u16::from(moves::to_stored_square(from));
            let to = u16::from(moves::to_stored_square(to));
            let captured = if mover == Color::White {
                to - 1
            } else {
                to + 1
            };
            let special7 = if mover == Color::White { 14 } else { 6 };
            Ok([
                from,
                to,
                role_code(Role::Pawn) | mover_bit,
                0,
                captured,
                captured,
                0,
                special7,
            ])
        }
        Move::Castle { king, rook } => {
            let back = mover.fold_wb(Rank::First, Rank::Eighth);
            let (king_file, rook_file) = if rook.file() > king.file() {
                (ChessFile::G, ChessFile::F)
            } else {
                (ChessFile::C, ChessFile::D)
            };
            Ok([
                u16::from(moves::to_stored_square(king)),
                u16::from(moves::to_stored_square(Square::from_coords(
                    king_file, back,
                ))),
                role_code(Role::King) | mover_bit,
                0,
                u16::from(moves::to_stored_square(rook)),
                u16::from(moves::to_stored_square(Square::from_coords(
                    rook_file, back,
                ))),
                role_code(Role::Rook) | mover_bit,
                0,
            ])
        }
        Move::Put { role, to } => {
            bail!("2CBH cannot encode the drop {role:?}@{to}")
        }
    }
}

fn role_code(role: Role) -> u16 {
    match role {
        Role::King => 1,
        Role::Queen => 2,
        Role::Knight => 3,
        Role::Bishop => 4,
        Role::Rook => 5,
        Role::Pawn => 6,
    }
}

fn resolve_ply(before: &Chess, ply: &Ply) -> Result<Option<Move>> {
    if ply.uci == "0000" || ply.san == "--" {
        return Ok(None);
    }
    if let Ok(uci) = UciMove::from_ascii(ply.uci.as_bytes())
        && let Ok(mv) = uci.to_move(before)
        && !matches!(mv, Move::Put { .. })
    {
        return Ok(Some(mv));
    }
    if let Ok(san_plus) = SanPlus::from_ascii(ply.san.as_bytes())
        && let Ok(mv) = san_plus.san.to_move(before)
        && !matches!(mv, Move::Put { .. })
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

fn encode_line(
    game: &Game,
    start: usize,
    skip_first_branch: bool,
    chess960_start: Option<u16>,
    tokens: &mut Vec<u16>,
) -> Result<()> {
    let mut current = Some(start);
    let mut branch_points = Vec::new();
    let mut first = true;

    while let Some(node) = current {
        let parent = game.nodes[node].parent.context("move has no parent")?;
        let before = &game.nodes[parent].pos;
        let ply = game.nodes[node].mv.as_ref().context("node has no move")?;
        match resolve_ply(before, ply)? {
            Some(mv) => tokens.push(encode_move(before, mv, chess960_start)?),
            None => tokens.push(0),
        }

        if !(skip_first_branch && first) {
            let children = &game.nodes[parent].children;
            if children.len() > 1 && children[0] == node {
                for _ in 1..children.len() {
                    tokens.push(0xfffd);
                }
                branch_points.push(node);
            }
        }
        first = false;
        current = game.nodes[node].children.first().copied();
    }

    tokens.push(0xffff);
    for branch in branch_points.into_iter().rev() {
        let parent = game.nodes[branch].parent.context("branch has no parent")?;
        for &sibling in game.nodes[parent].children[1..].iter().rev() {
            encode_line(game, sibling, true, chess960_start, tokens)?;
        }
    }
    Ok(())
}

type PlayerKey = (String, String, Option<u32>);
type EventKey = (String, String);
type SourceKey = (String, String);

/// A keyed list of `.2lid` name records and their block ids.
#[derive(Default)]
struct Interner<K> {
    records: Vec<Vec<u8>>,
    ids: HashMap<K, u32>,
}

impl<K: Eq + std::hash::Hash> Interner<K> {
    fn intern(&mut self, key: K, build: impl FnOnce() -> Result<Vec<u8>>) -> Result<u32> {
        if let Some(&id) = self.ids.get(&key) {
            return Ok(id);
        }
        let record = build()?;
        let id = u32::try_from(self.records.len()).context("2LID name table is full")?;
        self.records.push(record);
        self.ids.insert(key, id);
        Ok(id)
    }

    fn id(&self, key: &K) -> Option<u32> {
        self.ids.get(key).copied()
    }

    fn record(&self, id: u32) -> &[u8] {
        &self.records[id as usize]
    }

    fn len(&self) -> u32 {
        self.records.len() as u32
    }
}

/// The inline player, event, and source name blocks written to `.2lid`.
#[derive(Default)]
struct NameTables {
    players: Interner<PlayerKey>,
    events: Interner<EventKey>,
    sources: Interner<SourceKey>,
}

impl NameTables {
    fn intern_player(
        &mut self,
        lid: &mut File,
        name: Option<&str>,
        fide: Option<&str>,
    ) -> Result<u32> {
        let key = player_key(name, fide);
        let id = self.players.intern(key.clone(), || player_record(&key))?;
        write_lid_block(lid, id, LID_PLAYER, self.players.record(id))?;
        Ok(id)
    }

    fn intern_event(
        &mut self,
        lid: &mut File,
        site: Option<&str>,
        event: Option<&str>,
    ) -> Result<u32> {
        let key = event_key(site, event);
        let id = self.events.intern(key.clone(), || event_record(&key))?;
        write_lid_block(lid, id, LID_EVENT, self.events.record(id))?;
        Ok(id)
    }

    fn intern_source(
        &mut self,
        lid: &mut File,
        title: Option<&str>,
        publisher: Option<&str>,
    ) -> Result<u32> {
        let key = source_key(title, publisher);
        let id = self.sources.intern(key.clone(), || source_record(&key))?;
        write_lid_block(lid, id, LID_SOURCE, self.sources.record(id))?;
        Ok(id)
    }

    fn player_id(&self, name: Option<&str>, fide: Option<&str>) -> Result<u32> {
        let key = player_key(name, fide);
        self.players
            .id(&key)
            .with_context(|| format!("2LID player {key:?} was not interned"))
    }

    fn event_id(&self, site: Option<&str>, event: Option<&str>) -> Result<u32> {
        let key = event_key(site, event);
        self.events
            .id(&key)
            .with_context(|| format!("2LID event {key:?} was not interned"))
    }

    fn source_id(&self, title: Option<&str>, publisher: Option<&str>) -> Result<u32> {
        let key = source_key(title, publisher);
        self.sources
            .id(&key)
            .with_context(|| format!("2LID source {key:?} was not interned"))
    }

    fn block_count(&self) -> u32 {
        self.players
            .len()
            .max(self.events.len())
            .max(self.sources.len())
            .max(1)
    }

    fn event_count(&self) -> u32 {
        self.events.len()
    }
}

fn write_lid_block(lid: &mut File, id: u32, offset: usize, record: &[u8]) -> Result<()> {
    let at = LID_FIRST + id as usize * LID_STRIDE + offset;
    lid.seek(SeekFrom::Start(at as u64))?;
    lid.write_all(record)?;
    Ok(())
}

fn clean_name(name: Option<&str>) -> String {
    let name = name.unwrap_or("").trim();
    if name == "?" {
        String::new()
    } else {
        name.to_string()
    }
}

fn split_player_name(name: Option<&str>) -> (String, String) {
    let name = clean_name(name);
    match name.split_once(',') {
        Some((last, first)) => (clean_name(Some(last)), clean_name(Some(first))),
        None => (name, String::new()),
    }
}

fn parse_fide(raw: Option<&str>) -> Option<u32> {
    let raw = raw?.trim();
    if raw.is_empty() || raw == "?" {
        return None;
    }
    raw.parse::<u32>().ok().filter(|&id| id != 0)
}

fn player_key(name: Option<&str>, fide: Option<&str>) -> PlayerKey {
    let (last, first) = split_player_name(name);
    (last, first, parse_fide(fide))
}

fn event_key(site: Option<&str>, event: Option<&str>) -> EventKey {
    (clean_name(site), clean_name(event))
}

fn source_key(title: Option<&str>, publisher: Option<&str>) -> SourceKey {
    (clean_name(title), clean_name(publisher))
}

fn player_record(key: &PlayerKey) -> Result<Vec<u8>> {
    let (last, first, fide) = key;
    let total = 32 + last.len() + first.len();
    if total > 0x400 {
        bail!("2LID player name {last}, {first} is too long ({total} bytes; at most 1024)");
    }
    let mut record = Vec::with_capacity(total);
    record.extend_from_slice(&(total as u32).to_le_bytes());
    record.extend_from_slice(&(last.len() as u32).to_le_bytes());
    record.extend_from_slice(last.as_bytes());
    record.extend_from_slice(&(first.len() as u32).to_le_bytes());
    record.extend_from_slice(first.as_bytes());
    record.extend_from_slice(&[0u8; 8]);
    record.extend_from_slice(&1u32.to_le_bytes());
    record.extend_from_slice(&8u32.to_le_bytes());
    record.extend_from_slice(&fide.unwrap_or(u32::MAX).to_le_bytes());
    debug_assert_eq!(record.len(), total);
    Ok(record)
}

fn event_record(key: &EventKey) -> Result<Vec<u8>> {
    let (place, title) = key;
    let total = 12 + place.len() + title.len() + 104;
    if total > 0x460 {
        bail!("2LID event name {place} - {title} is too long ({total} bytes; at most 1120)");
    }
    let mut record = Vec::with_capacity(total);
    record.extend_from_slice(&(total as u32).to_le_bytes());
    record.extend_from_slice(&(place.len() as u32).to_le_bytes());
    record.extend_from_slice(place.as_bytes());
    record.extend_from_slice(&(title.len() as u32).to_le_bytes());
    record.extend_from_slice(title.as_bytes());
    record.resize(total, 0);
    debug_assert_eq!(record.len(), total);
    Ok(record)
}

fn source_record(key: &SourceKey) -> Result<Vec<u8>> {
    let (title, publisher) = key;
    let total = 20 + title.len() + publisher.len();
    if total > 0x82A {
        bail!("2LID source name {title} - {publisher} is too long ({total} bytes; at most 2090)");
    }
    let mut record = Vec::with_capacity(total);
    record.extend_from_slice(&(total as u32).to_le_bytes());
    record.extend_from_slice(&(title.len() as u32).to_le_bytes());
    record.extend_from_slice(title.as_bytes());
    record.extend_from_slice(&(publisher.len() as u32).to_le_bytes());
    record.extend_from_slice(publisher.as_bytes());
    record.resize(total, 0);
    debug_assert_eq!(record.len(), total);
    Ok(record)
}

fn is_deleted(game: &Game) -> bool {
    game.header("Deleted").is_some_and(|value| {
        let value = value.trim();
        value == "1" || value.eq_ignore_ascii_case("true")
    }) || game
        .errors
        .iter()
        .any(|error| error.contains("marked deleted"))
}

fn encode_date(raw: Option<&str>) -> u32 {
    let Some(raw) = raw else {
        return 0;
    };
    let mut parts = raw.trim().split(['.', '-', '/']);
    let year = parse_date_part(parts.next(), 0xfff);
    let month = parse_date_part(parts.next(), 12);
    let day = parse_date_part(parts.next(), 31);
    (year << 9) | (month << 5) | day
}

fn parse_date_part(raw: Option<&str>, max: u32) -> u32 {
    raw.and_then(|part| part.trim().parse::<u32>().ok())
        .filter(|&value| value <= max)
        .unwrap_or(0)
}

fn encode_result(result: &str) -> u8 {
    match result.trim() {
        "0-1" => 0,
        "1/2-1/2" => 1,
        "1-0" => 2,
        _ => 3,
    }
}

fn encode_round(round: Option<&str>, board: Option<&str>) -> (u16, u32) {
    let round = round
        .map(str::trim)
        .filter(|text| !text.is_empty() && *text != "?");
    let board = board
        .map(str::trim)
        .filter(|text| !text.is_empty() && *text != "?");
    let mut parts = round.unwrap_or("").split(['.', '/', '(']);
    let main = parts.next().and_then(parse_round_part).unwrap_or(0);
    let sub = parts
        .next()
        .and_then(parse_round_part)
        .or_else(|| board.and_then(parse_round_part))
        .unwrap_or(0);
    (main.min(u32::from(u16::MAX)) as u16, sub)
}

fn parse_round_part(part: &str) -> Option<u32> {
    part.trim().trim_end_matches(')').trim().parse().ok()
}

fn parse_elo(raw: Option<&str>) -> u32 {
    raw.and_then(|text| text.trim().parse().ok()).unwrap_or(0)
}

fn parse_u64_header(raw: Option<&str>) -> u64 {
    raw.and_then(|text| text.trim().parse().ok()).unwrap_or(0)
}

struct TwoCbhWriter {
    stem: PathBuf,
    unique: String,
    cbh_tmp: PathBuf,
    cbg_tmp: PathBuf,
    cba_tmp: PathBuf,
    lid_tmp: PathBuf,
    cbh_file: Option<BufWriter<File>>,
    cbg_file: Option<BufWriter<File>>,
    cba_file: Option<BufWriter<File>>,
    lid_file: Option<File>,
    names: NameTables,
    cbg_offset: u64,
    cba_offset: u64,
    games: u32,
    options: TwoCbhWriteOptions,
    brett_annotations: super::BrettAnnotationMap,
    guard: TempGuard,
}

impl TwoCbhWriter {
    fn create(stem: &Path, options: TwoCbhWriteOptions) -> Result<Self> {
        let unique = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        );
        let cbh_tmp = temp_path(&core_path(stem, ".2cbh"), &unique);
        let cbg_tmp = temp_path(&core_path(stem, ".2cbg"), &unique);
        let cba_tmp = temp_path(&core_path(stem, ".2cba"), &unique);
        let lid_tmp = temp_path(&core_path(stem, ".2lid"), &unique);

        let guard = TempGuard {
            paths: vec![
                cbh_tmp.clone(),
                cbg_tmp.clone(),
                cba_tmp.clone(),
                lid_tmp.clone(),
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
                .with_context(|| format!("creating temporary 2CBH header {}", cbh_tmp.display()))?,
        );
        // Reserve record 0; the final record count is patched at finish.
        cbh_file.write_all(&cbh_record_zero(0))?;

        let mut cbg_file = BufWriter::new(
            File::create(&cbg_tmp)
                .with_context(|| format!("creating temporary 2CBG moves {}", cbg_tmp.display()))?,
        );
        cbg_file.write_all(&companion_header(0, CBG_VERSION))?;

        let mut cba_file = BufWriter::new(File::create(&cba_tmp).with_context(|| {
            format!("creating temporary 2CBA annotations {}", cba_tmp.display())
        })?);
        cba_file.write_all(&companion_header(0, CBA_VERSION))?;

        let mut lid_file = File::create(&lid_tmp)
            .with_context(|| format!("creating temporary 2LID names {}", lid_tmp.display()))?;
        lid_file.write_all(&lid_header(1, 0))?;

        Ok(Self {
            stem: stem.to_path_buf(),
            unique,
            cbh_tmp,
            cbg_tmp,
            cba_tmp,
            lid_tmp,
            cbh_file: Some(cbh_file),
            cbg_file: Some(cbg_file),
            cba_file: Some(cba_file),
            lid_file: Some(lid_file),
            names: NameTables::default(),
            cbg_offset: CBG_HEADER,
            cba_offset: CBG_HEADER,
            games: 0,
            options,
            brett_annotations: HashMap::new(),
            guard,
        })
    }

    fn build_cbg_content(
        start_id: Option<u16>,
        custom_start: Option<&[u16]>,
        tokens: &[u16],
    ) -> Vec<u8> {
        let prefix_words = custom_start.map_or(if start_id.is_some() { 3 } else { 1 }, |w| w.len());
        let mut content = Vec::with_capacity((1 + prefix_words + tokens.len()) * 2);
        match (start_id, custom_start) {
            (Some(id), _) => {
                content.extend_from_slice(&2u16.to_le_bytes());
                content.extend_from_slice(&0xfffbu16.to_le_bytes());
                content.extend_from_slice(&id.to_le_bytes());
                content.extend_from_slice(&0xfffcu16.to_le_bytes());
            }
            (None, Some(prefix)) => {
                content.extend_from_slice(&1u16.to_le_bytes());
                for word in prefix {
                    content.extend_from_slice(&word.to_le_bytes());
                }
            }
            (None, None) => {
                content.extend_from_slice(&1u16.to_le_bytes());
                content.extend_from_slice(&0xfffcu16.to_le_bytes());
            }
        }
        for token in tokens {
            content.extend_from_slice(&token.to_le_bytes());
        }
        content
    }

    /// Encode, stream, and index one game.
    fn add_game(&mut self, game: &Game) -> Result<()> {
        let index = self.games;
        if !self.options.allow_annotation_loss
            && !self.options.write_brett_annotations_sidecar
            && let Some((feature, node)) = first_unsupported_annotation(game)
        {
            bail!(
                "game {index} ({white} - {black}) has a {feature} at node {node}; \
                 the first 2CBH writer does not preserve annotations \
                 (pass allow_annotation_loss to override)",
                white = game.white(),
                black = game.black()
            );
        }
        let EncodedMoves {
            start_id,
            custom_start,
            mut tokens,
        } = encode_game_moves(game).with_context(|| {
            format!(
                "encoding 2CBH game {index} ({} - {})",
                game.white(),
                game.black()
            )
        })?;

        let base_padding = if tokens.is_empty() {
            194
        } else {
            assert_eq!(
                tokens.pop(),
                Some(0xffff),
                "2CBH move stream must end with a line terminator"
            );
            98
        };

        let content = Self::build_cbg_content(start_id, custom_start.as_deref(), &tokens);

        let cbg_offset = self.cbg_offset;
        let key = record_key(u64::from(index), &content);
        let record = frame_record(&content, [0xff, 0xff], base_padding, key, cbg_offset);
        self.cbg_file
            .as_mut()
            .context("2CBG temporary file is already closed")?
            .write_all(&record)?;
        self.cbg_offset += record.len() as u64;

        let cba_offset = self.cba_offset;
        let record = frame_record(
            &[0x00, 0x20, 0xff, 0xff],
            [0xff, 0x7f],
            194,
            0x7fff_ffff,
            cba_offset,
        );
        self.cba_file
            .as_mut()
            .context("2CBA temporary file is already closed")?
            .write_all(&record)?;
        self.cba_offset += record.len() as u64;

        {
            let lid = self
                .lid_file
                .as_mut()
                .context("2LID temporary file is already closed")?;
            self.names
                .intern_player(lid, game.header("White"), game.header("WhiteFideId"))?;
            self.names
                .intern_player(lid, game.header("Black"), game.header("BlackFideId"))?;
            self.names
                .intern_event(lid, game.header("Site"), game.header("Event"))?;
            self.names
                .intern_source(lid, game.header("SourceTitle"), game.header("Source"))?;
        }

        let record = self.game_record(game, cbg_offset, cba_offset, start_id)?;
        self.cbh_file
            .as_mut()
            .context("2CBH temporary file is already closed")?
            .write_all(&record)?;
        if self.options.write_brett_annotations_sidecar {
            let anns = super::collect_brett_annotations(game);
            if !anns.is_empty() {
                self.brett_annotations.insert(index + 1, anns);
            }
        }
        self.games = self
            .games
            .checked_add(1)
            .context("2CBH game count overflow")?;
        Ok(())
    }

    /// Build the 192-byte `.2cbh` record for one already-interned game.
    fn game_record(
        &self,
        game: &Game,
        cbg_offset: u64,
        cba_offset: u64,
        start_id: Option<u16>,
    ) -> Result<[u8; RECORD_SIZE]> {
        let white = self
            .names
            .player_id(game.header("White"), game.header("WhiteFideId"))?;
        let black = self
            .names
            .player_id(game.header("Black"), game.header("BlackFideId"))?;
        let event = self
            .names
            .event_id(game.header("Site"), game.header("Event"))?;
        let source = self
            .names
            .source_id(game.header("SourceTitle"), game.header("Source"))?;
        let (round, subround) = encode_round(game.header("Round"), game.header("Board"));

        let mut record = [0u8; RECORD_SIZE];
        record[0] = if is_deleted(game) { 0x81 } else { 0x01 };
        record[1..4].copy_from_slice(&[0, 1, 1]);
        record[0x08..0x10].copy_from_slice(&cbg_offset.to_le_bytes());
        record[0x10..0x18].copy_from_slice(&cba_offset.to_le_bytes());
        record[0x18..0x1c].copy_from_slice(&white.to_le_bytes());
        record[0x20..0x24].copy_from_slice(&black.to_le_bytes());
        record[0x28..0x2c].copy_from_slice(&event.to_le_bytes());
        record[0x30..0x34].copy_from_slice(&2u32.to_le_bytes());
        record[0x38..0x3c].copy_from_slice(&source.to_le_bytes());
        record[0x40..0x50].fill(0xff);
        let packed = (u32::from(round) << 16) | u32::from(encode_result(game.result()));
        record[0x58..0x5c].copy_from_slice(&packed.to_le_bytes());
        record[0x5c..0x60].copy_from_slice(&subround.to_le_bytes());
        record[0x60..0x64].copy_from_slice(&parse_elo(game.header("WhiteElo")).to_le_bytes());
        record[0x70..0x74].copy_from_slice(&parse_elo(game.header("BlackElo")).to_le_bytes());
        if let Some(start_id) = start_id {
            let packed_start = START_ID_BASE as u16 + start_id;
            record[0x80..0x82].copy_from_slice(&packed_start.to_le_bytes());
        } else {
            let eco = crate::cbh::writer::encode_eco(game.header("ECO"));
            if eco > 0 {
                record[0x80..0x82].copy_from_slice(&eco.to_le_bytes());
            }
        }
        let medals = crate::library::medals::from_game(&game);
        if medals != 0 {
            record[0x82..0x84].copy_from_slice(&medals.to_le_bytes());
        }
        record[0x98..0xa0].copy_from_slice(&parse_u64_header(game.header("GameId")).to_le_bytes());
        record[0xb0..0xb8].copy_from_slice(&parse_u64_header(game.header("Beauty")).to_le_bytes());
        record[0xbc..0xc0].copy_from_slice(&encode_date(game.header("Date")).to_le_bytes());
        Ok(record)
    }

    fn finish(mut self) -> Result<TwoCbhWriteReport> {
        let mut cbh_writer = self
            .cbh_file
            .take()
            .context("2CBH header temporary file is already closed")?;
        cbh_writer.flush()?;
        let mut cbh_file = cbh_writer
            .into_inner()
            .map_err(|err| anyhow::anyhow!("flushing temporary 2CBH header: {err}"))?;
        cbh_file.seek(SeekFrom::Start(0))?;
        cbh_file.write_all(&cbh_record_zero(
            self.games
                .checked_add(1)
                .context("2CBH record count overflow")?,
        ))?;
        cbh_file.flush()?;
        drop(cbh_file);

        let mut cbg_writer = self
            .cbg_file
            .take()
            .context("2CBG temporary file is already closed")?;
        cbg_writer.flush()?;
        let mut cbg_file = cbg_writer
            .into_inner()
            .map_err(|err| anyhow::anyhow!("flushing temporary 2CBG moves: {err}"))?;
        cbg_file.seek(SeekFrom::Start(0))?;
        cbg_file.write_all(&companion_header(self.cbg_offset, CBG_VERSION))?;
        cbg_file.flush()?;
        drop(cbg_file);

        let mut cba_writer = self
            .cba_file
            .take()
            .context("2CBA temporary file is already closed")?;
        cba_writer.flush()?;
        let mut cba_file = cba_writer
            .into_inner()
            .map_err(|err| anyhow::anyhow!("flushing temporary 2CBA annotations: {err}"))?;
        cba_file.seek(SeekFrom::Start(0))?;
        cba_file.write_all(&companion_header(self.cba_offset, CBA_VERSION))?;
        cba_file.flush()?;
        drop(cba_file);

        let block_count = self.names.block_count();
        let event_count = self.names.event_count();
        let mut lid_file = self
            .lid_file
            .take()
            .context("2LID temporary file is already closed")?;
        let lid_len = LID_FIRST as u64 + u64::from(block_count) * LID_STRIDE as u64;
        lid_file.set_len(lid_len)?;
        lid_file.seek(SeekFrom::Start(0))?;
        lid_file.write_all(&lid_header(block_count, event_count))?;
        lid_file.flush()?;
        drop(lid_file);

        // Install `.2cbh` last so readers never observe a new header before
        // its companion files are in place.
        let mut staged_owned = vec![
            (self.cbg_tmp.clone(), core_path(&self.stem, ".2cbg")),
            (self.cba_tmp.clone(), core_path(&self.stem, ".2cba")),
            (self.lid_tmp.clone(), core_path(&self.stem, ".2lid")),
            (self.cbh_tmp.clone(), core_path(&self.stem, ".2cbh")),
        ];
        if self.options.write_brett_annotations_sidecar && !self.brett_annotations.is_empty() {
            let ann_tmp = temp_path(&core_path(&self.stem, ".brettann"), &self.unique);
            self.guard.paths.push(ann_tmp.clone());
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            let raw = super::serialize_brett_annotations(&self.brett_annotations);
            encoder.write_all(&raw)?;
            let compressed = encoder.finish()?;
            fs::write(&ann_tmp, &compressed).with_context(|| {
                format!("writing temporary annotation sidecar {}", ann_tmp.display())
            })?;
            staged_owned.insert(3, (ann_tmp, core_path(&self.stem, ".brettann")));
        }
        let staged: Vec<(&PathBuf, PathBuf)> = staged_owned
            .iter()
            .map(|(tmp, dst)| (tmp, dst.clone()))
            .collect();

        let mut total_bytes = 0u64;
        for (tmp, _) in &staged {
            total_bytes += fs::metadata(tmp).map(|meta| meta.len()).unwrap_or(0);
        }

        replace_companion_set(&self.stem, &staged, &self.unique)?;
        self.guard.committed = true;

        Ok(TwoCbhWriteReport {
            games: self.games as usize,
            bytes: total_bytes,
        })
    }
}

fn cbh_record_zero(count: u32) -> [u8; RECORD_SIZE] {
    let mut header = [0u8; RECORD_SIZE];
    header[0x08] = RECORD_ZERO_GENERATION;
    header[0x0a..0x0c].copy_from_slice(&(RECORD_SIZE as u16).to_le_bytes());
    header[0x0c..0x0e].copy_from_slice(&CBG_VERSION.to_le_bytes());
    header[0x10..0x14].copy_from_slice(&count.to_le_bytes());
    header
}

fn companion_header(length: u64, version: u16) -> [u8; CBG_HEADER as usize] {
    let mut header = [0u8; CBG_HEADER as usize];
    header[0..8].copy_from_slice(&length.to_le_bytes());
    header[8..10].copy_from_slice(&(CBG_HEADER as u16).to_le_bytes());
    header[10..12].copy_from_slice(&version.to_le_bytes());
    header
}

fn lid_header(block_count: u32, event_count: u32) -> [u8; LID_FIRST] {
    let mut header = [0u8; LID_FIRST];
    put_be(&mut header, 0x00, LID_FIRST as u32);
    put_be(&mut header, 0x04, 6);
    put_be(&mut header, 0x08, 0x400);
    put_be(&mut header, 0x0c, 0);
    put_be(&mut header, 0x10, block_count.saturating_add(1));
    put_be(&mut header, 0x14, u32::MAX);
    put_be(&mut header, 0x18, u32::MAX);
    put_be(&mut header, 0x1c, 0x460);
    put_be(&mut header, 0x24, event_count);
    put_be(&mut header, 0x30, 0xdc);
    put_be(&mut header, 0x38, 2);
    put_be(&mut header, 0x44, 0x400);
    put_be(&mut header, 0x58, 0x13a);
    put_be(&mut header, 0x6c, 0x214);
    put_be(&mut header, 0x74, 1);
    put_be(&mut header, 0x80, u32::MAX);
    header
}

fn put_be(header: &mut [u8], at: usize, value: u32) {
    header[at..at + 4].copy_from_slice(&value.to_be_bytes());
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
                        "moving existing 2CBH file {} aside before replacement",
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
                    "installing new 2CBH file {} from {}",
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
        .unwrap_or("2cbh-export");
    target.with_file_name(format!(".{name}.{unique}.tmp"))
}

// ---------------------------------------------------------------------------
// Incremental merge into an existing 2CBH database
// ---------------------------------------------------------------------------

const LID_TEAM_SEC: usize = 4;
const DEFAULT_MAX_FIDE_PROBE: usize = 128;

/// Merge every `.2cbh` update directory/file and `.cbv` classic archive in
/// `update_sources` into `target_cbh`, deduplicating entities and games,
/// backfilling missing FIDE IDs, and rebuilding the `.2lgd` accelerator.
///
/// The returned `TwoCbhWriteReport::games` holds the count of newly appended
/// games. When 0 new games are appended, derived files are left untouched;
/// otherwise stale `.2lcd` is removed and `.2lgd` is rebuilt in place.
pub fn merge_updates_into_2cbh(
    target_cbh: &Path,
    update_sources: &[PathBuf],
) -> Result<TwoCbhWriteReport> {
    let stem = normalize_stem(target_cbh)?;
    let target_cbh_path = core_path(&stem, ".2cbh");
    let target_cbg_path = core_path(&stem, ".2cbg");
    let target_cba_path = core_path(&stem, ".2cba");
    let target_lid_path = core_path(&stem, ".2lid");
    for path in [
        &target_cbh_path,
        &target_cbg_path,
        &target_cba_path,
        &target_lid_path,
    ] {
        if !path.is_file() {
            bail!("missing target 2CBH companion file {}", path.display());
        }
    }

    // Scan existing target `.2lid` and `.2cbh`/`.2cbg` through read-only mmaps,
    // then drop the mmaps before opening the target files for read-write append.
    let (mut lid_state, mut seen_games, mut cbh_slots, mut cbg_end, mut cba_end) = {
        let lid_map = map_readonly(&target_lid_path, "target 2LID")?;
        let lid_state = TargetLidState::scan(&lid_map, &target_lid_path)?;

        let cbh_map = map_readonly(&target_cbh_path, "target 2CBH")?;
        let cbg_map = map_readonly(&target_cbg_path, "target 2CBG")?;
        let cba_map = map_readonly(&target_cba_path, "target 2CBA")?;
        if cbh_map.len() < RECORD_SIZE {
            bail!(
                "{} is shorter than one 2CBH header record",
                target_cbh_path.display()
            );
        }
        if cbg_map.len() < CBG_HEADER as usize {
            bail!(
                "{} is shorter than a 2CBG header",
                target_cbg_path.display()
            );
        }
        if cba_map.len() < CBG_HEADER as usize {
            bail!(
                "{} is shorter than a 2CBA header",
                target_cba_path.display()
            );
        }

        let file_slots = cbh_map.len() / RECORD_SIZE;
        let stored_slots = le_u32(&cbh_map[0x10..0x14]) as usize;
        let cbh_slots = if stored_slots > 0 && stored_slots <= file_slots {
            stored_slots
        } else {
            file_slots
        };
        let cbg_end = le_u64(&cbg_map[0..8])
            .max(CBG_HEADER)
            .min(cbg_map.len() as u64);
        let cba_end = le_u64(&cba_map[0..8])
            .max(CBG_HEADER)
            .min(cba_map.len() as u64);

        let mut seen = HashSet::with_capacity(cbh_slots);
        for slot in 1..cbh_slots {
            let rec = &cbh_map[slot * RECORD_SIZE..(slot + 1) * RECORD_SIZE];
            if rec[0] & 0x82 != 0 {
                continue;
            }
            let white = le_u32(&rec[0x18..0x1c]);
            let black = le_u32(&rec[0x20..0x24]);
            let cbg_off = le_u64(&rec[0x08..0x10]) as usize;
            let (slice, a_len) = cbg_content_prefix(&cbg_map, cbg_off);
            seen.insert(game_fingerprint(white, black, rec, slice, a_len));
        }

        (lid_state, seen, cbh_slots, cbg_end, cba_end)
    };

    let initial_cbg_end = cbg_end;
    let initial_cba_end = cba_end;
    let initial_slots = cbh_slots;

    let mut cbh_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&target_cbh_path)
        .with_context(|| format!("opening {} for merge", target_cbh_path.display()))?;
    cbh_file.seek(SeekFrom::Start(cbh_slots as u64 * RECORD_SIZE as u64))?;
    let mut cbh_writer = BufWriter::new(cbh_file);

    let mut cbg_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&target_cbg_path)
        .with_context(|| format!("opening {} for merge", target_cbg_path.display()))?;
    cbg_file.seek(SeekFrom::Start(cbg_end))?;
    let mut cbg_writer = BufWriter::new(cbg_file);

    let mut cba_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&target_cba_path)
        .with_context(|| format!("opening {} for merge", target_cba_path.display()))?;
    cba_file.seek(SeekFrom::Start(cba_end))?;
    let mut cba_writer = BufWriter::new(cba_file);

    let mut lid_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&target_lid_path)
        .with_context(|| format!("opening {} for merge", target_lid_path.display()))?;
    lid_state.pad_initial(&mut lid_file)?;

    let mut appended_games = 0usize;
    {
        let mut sink = MergeSink {
            lid_state: &mut lid_state,
            lid_file: &mut lid_file,
            cbh_writer: &mut cbh_writer,
            cbg_writer: &mut cbg_writer,
            cba_writer: &mut cba_writer,
            seen_games: &mut seen_games,
            cbg_offset: &mut cbg_end,
            cba_offset: &mut cba_end,
            cbh_slots: &mut cbh_slots,
            appended_games: &mut appended_games,
        };

        for source in update_sources {
            let ext = source
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase());
            if ext.as_deref() == Some("cbv") && source.is_file() {
                merge_cbv_source(source, &mut sink)
                    .with_context(|| format!("merging CBV archive {}", source.display()))?;
            } else if let Some(src_cbh) = resolve_2cbh_source(source)? {
                merge_2cbh_source(&src_cbh, &mut sink)
                    .with_context(|| format!("merging 2CBH update {}", src_cbh.display()))?;
            } else {
                bail!("unsupported update source {}", source.display());
            }
        }
    }

    let mut cbh_file = cbh_writer.into_inner().map_err(|e| e.into_error())?;
    if appended_games > 0 {
        let total_slots = u32::try_from(cbh_slots).context("merged 2CBH record count overflow")?;
        cbh_file.seek(SeekFrom::Start(0x10))?;
        cbh_file.write_all(&total_slots.to_le_bytes())?;
    }
    cbh_file.flush()?;
    drop(cbh_file);

    let mut cbg_file = cbg_writer.into_inner().map_err(|e| e.into_error())?;
    if cbg_end > initial_cbg_end {
        cbg_file.seek(SeekFrom::Start(0))?;
        cbg_file.write_all(&cbg_end.to_le_bytes())?;
    }
    cbg_file.flush()?;
    drop(cbg_file);

    let mut cba_file = cba_writer.into_inner().map_err(|e| e.into_error())?;
    if cba_end > initial_cba_end {
        cba_file.seek(SeekFrom::Start(0))?;
        cba_file.write_all(&cba_end.to_le_bytes())?;
    }
    cba_file.flush()?;
    drop(cba_file);

    lid_state.write_header_counts(&mut lid_file)?;
    lid_file.flush()?;
    drop(lid_file);

    if appended_games > 0 {
        let lcd = sibling(&stem, ".2lcd");
        if lcd.exists() {
            let _ = fs::remove_file(&lcd);
        }
        let lgd = sibling(&stem, ".2lgd");
        let reopened = super::TwoCbhFile::open(&target_cbh_path)
            .with_context(|| format!("reopening {} to build .2lgd", target_cbh_path.display()))?;
        super::accelerator::TwoCbhAccelerator::build_to(&reopened, &lgd)
            .with_context(|| format!("rebuilding .2lgd accelerator at {}", lgd.display()))?;
    }

    let bytes = (cbh_slots.saturating_sub(initial_slots) as u64) * RECORD_SIZE as u64
        + cbg_end.saturating_sub(initial_cbg_end)
        + cba_end.saturating_sub(initial_cba_end);

    Ok(TwoCbhWriteReport {
        games: appended_games,
        bytes,
    })
}

struct MergeSink<'a> {
    lid_state: &'a mut TargetLidState,
    lid_file: &'a mut File,
    cbh_writer: &'a mut BufWriter<File>,
    cbg_writer: &'a mut BufWriter<File>,
    cba_writer: &'a mut BufWriter<File>,
    seen_games: &'a mut HashSet<u64>,
    cbg_offset: &'a mut u64,
    cba_offset: &'a mut u64,
    cbh_slots: &'a mut usize,
    appended_games: &'a mut usize,
}

#[derive(Clone, Debug)]
struct LidLayout {
    first_offset: usize,
    stride: usize,
    num_sections: usize,
    offsets: [usize; 6],
    sizes: [usize; 6],
    counts: [u32; 6],
}

impl LidLayout {
    fn parse(bytes: &[u8], path: &Path) -> Result<Self> {
        if bytes.len() < 8 {
            bail!("{} is shorter than a 2LID header", path.display());
        }
        let first_offset = be_u32(&bytes[0..4]) as usize;
        let num_sections = be_u32(&bytes[4..8]) as usize;
        if !(1..=16).contains(&num_sections) || bytes.len() < 8 + num_sections * 20 {
            bail!("{} has an invalid 2LID section table", path.display());
        }
        let mut offsets = [0usize; 6];
        let mut sizes = [0usize; 6];
        let mut counts = [0u32; 6];
        let mut running = 0usize;
        for i in 0..num_sections {
            let desc = 8 + i * 20;
            let sz = be_u32(&bytes[desc..desc + 4]) as usize;
            let cnt = be_u32(&bytes[desc + 8..desc + 12]);
            if i < 6 {
                offsets[i] = running;
                sizes[i] = sz;
                counts[i] = cnt;
            }
            running = running.saturating_add(sz);
        }
        if first_offset > bytes.len() || running == 0 {
            bail!("{} has invalid 2LID geometry", path.display());
        }
        Ok(Self {
            first_offset,
            stride: running,
            num_sections,
            offsets,
            sizes,
            counts,
        })
    }

    fn allocated_blocks(&self, len: usize) -> usize {
        if self.stride == 0 || len <= self.first_offset {
            0
        } else {
            (len - self.first_offset).div_ceil(self.stride)
        }
    }

    fn raw_slice<'a>(&self, bytes: &'a [u8], id: u32, sec: usize) -> Option<&'a [u8]> {
        let size = *self.sizes.get(sec)?;
        let off = *self.offsets.get(sec)?;
        if size < 4 {
            return None;
        }
        let start = self
            .first_offset
            .checked_add(usize::try_from(id).ok()?.checked_mul(self.stride)?)?
            .checked_add(off)?;
        if start + 4 > bytes.len() {
            return None;
        }
        let avail = (bytes.len() - start).min(size);
        let total = le_u32(&bytes[start..start + 4]) as usize;
        if total < 4 || total > avail {
            return None;
        }
        Some(&bytes[start..start + total])
    }
}

struct TargetLidState {
    layout: LidLayout,
    allocated_blocks: usize,
    needs_initial_pad: bool,
    next_id: [u32; 6],
    player_by_name: HashMap<(String, String), (u32, Option<u32>, usize)>,
    player_by_fide: HashMap<u32, u32>,
    event_by_key: HashMap<(String, String), u32>,
    source_by_key: HashMap<(String, String), u32>,
    team_by_key: HashMap<String, u32>,
    empty_player_id: Option<u32>,
    backfilled_fide: bool,
}

impl TargetLidState {
    fn scan(bytes: &[u8], path: &Path) -> Result<Self> {
        let layout = LidLayout::parse(bytes, path)?;
        let allocated_blocks = layout.allocated_blocks(bytes.len());
        let expected_len = layout.first_offset + allocated_blocks * layout.stride;
        let needs_initial_pad = bytes.len() != expected_len;

        let mut next_id = [0u32; 6];
        let mut player_by_name = HashMap::with_capacity(allocated_blocks);
        let mut player_by_fide = HashMap::with_capacity(allocated_blocks / 2);
        let mut event_by_key = HashMap::with_capacity(allocated_blocks / 4);
        let mut source_by_key = HashMap::with_capacity(1024);
        let mut team_by_key = HashMap::with_capacity(allocated_blocks / 6);
        let mut empty_player_id = None;

        for id in 0..allocated_blocks {
            let id_u32 = id as u32;
            if let Some(block) = layout.raw_slice(bytes, id_u32, 0) {
                next_id[0] = id_u32 + 1;
                if let Some((last, first, fide, fide_off)) = parse_player_raw(block) {
                    if last.is_empty()
                        && first.is_empty()
                        && (id_u32 == 2 || empty_player_id.is_none())
                    {
                        empty_player_id = Some(id_u32);
                    }
                    if let Some(f) = fide {
                        player_by_fide.entry(f).or_insert(id_u32);
                    }
                    player_by_name
                        .entry((last, first))
                        .or_insert((id_u32, fide, fide_off));
                }
            }
            if (id < layout.counts[1] as usize || layout.counts[1] == 0)
                && let Some(block) = layout.raw_slice(bytes, id_u32, 1)
            {
                next_id[1] = id_u32 + 1;
                if let Some(key) = parse_two_string_key(block) {
                    event_by_key.entry(key).or_insert(id_u32);
                }
            }
            if let Some(block) = layout.raw_slice(bytes, id_u32, 2) {
                next_id[2] = id_u32 + 1;
                if let Some(key) = parse_two_string_key(block) {
                    source_by_key.entry(key).or_insert(id_u32);
                }
            }
            if layout.sizes[LID_TEAM_SEC] >= 8
                && let Some(block) = layout.raw_slice(bytes, id_u32, LID_TEAM_SEC)
            {
                next_id[LID_TEAM_SEC] = id_u32 + 1;
                if let Some(key) = parse_one_string_key(block) {
                    team_by_key.entry(key).or_insert(id_u32);
                }
            }
        }
        next_id[3] = layout.counts[3].min(allocated_blocks as u32);
        next_id[5] = layout.counts[5].min(allocated_blocks as u32);

        Ok(Self {
            layout,
            allocated_blocks,
            needs_initial_pad,
            next_id,
            player_by_name,
            player_by_fide,
            event_by_key,
            source_by_key,
            team_by_key,
            empty_player_id,
            backfilled_fide: false,
        })
    }

    fn pad_initial(&mut self, file: &mut File) -> Result<()> {
        if !self.needs_initial_pad {
            return Ok(());
        }
        let target_len =
            (self.layout.first_offset + self.allocated_blocks * self.layout.stride) as u64;
        file.set_len(target_len)?;
        self.needs_initial_pad = false;
        Ok(())
    }

    fn ensure_block(&mut self, file: &mut File, id: u32) -> Result<()> {
        let needed = id as usize + 1;
        if needed > self.allocated_blocks {
            let new_len = (self.layout.first_offset + needed * self.layout.stride) as u64;
            file.set_len(new_len)?;
            self.allocated_blocks = needed;
        }
        Ok(())
    }

    fn write_section_block(
        &mut self,
        file: &mut File,
        id: u32,
        sec: usize,
        raw: &[u8],
    ) -> Result<()> {
        let size = self.layout.sizes[sec];
        if size < 4 {
            return Ok(());
        }
        self.ensure_block(file, id)?;
        let start =
            self.layout.first_offset + id as usize * self.layout.stride + self.layout.offsets[sec];
        file.seek(SeekFrom::Start(start as u64))?;
        if raw.len() <= size {
            file.write_all(raw)?;
        } else {
            let mut clamped = raw[..size].to_vec();
            clamped[0..4].copy_from_slice(&(size as u32).to_le_bytes());
            file.write_all(&clamped)?;
        }
        Ok(())
    }

    fn write_header_counts(&self, file: &mut File) -> Result<()> {
        for sec in [0usize, 1, 2, LID_TEAM_SEC] {
            if sec < self.layout.num_sections && self.layout.sizes[sec] > 0 {
                let at = 8 + sec * 20 + 8;
                file.seek(SeekFrom::Start(at as u64))?;
                file.write_all(&self.next_id[sec].to_be_bytes())?;
            }
        }
        Ok(())
    }

    fn map_or_insert_player(
        &mut self,
        file: &mut File,
        last: String,
        first: String,
        fide: Option<u32>,
        raw_block: &[u8],
    ) -> Result<u32> {
        if last.is_empty()
            && first.is_empty()
            && fide.is_none()
            && let Some(empty_id) = self.empty_player_id
        {
            return Ok(empty_id);
        }
        let key = (last, first);
        if let Some(&(existing_id, existing_fide, existing_fide_off)) =
            self.player_by_name.get(&key)
        {
            if let Some(new_fide) = fide {
                if let Some(&fide_id) = self.player_by_fide.get(&new_fide) {
                    return Ok(fide_id);
                }
                if existing_fide.is_none() {
                    if existing_fide_off + 4 <= self.layout.sizes[0]
                        && (existing_id as usize) < self.allocated_blocks
                    {
                        let at = self.layout.first_offset
                            + existing_id as usize * self.layout.stride
                            + self.layout.offsets[0]
                            + existing_fide_off;
                        file.seek(SeekFrom::Start(at as u64))?;
                        file.write_all(&new_fide.to_le_bytes())?;
                        self.backfilled_fide = true;
                    }
                    self.player_by_name
                        .insert(key, (existing_id, Some(new_fide), existing_fide_off));
                    self.player_by_fide.entry(new_fide).or_insert(existing_id);
                    return Ok(existing_id);
                }
            } else {
                return Ok(existing_id);
            }
        } else if let Some(new_fide) = fide
            && let Some(&fide_id) = self.player_by_fide.get(&new_fide)
        {
            return Ok(fide_id);
        }

        let new_id = self.next_id[0];
        self.next_id[0] = new_id
            .checked_add(1)
            .context("2LID player table overflow")?;
        if key.0.is_empty() && key.1.is_empty() && self.empty_player_id.is_none() {
            self.empty_player_id = Some(new_id);
        }
        let new_fide_off = parse_player_raw(raw_block).map(|p| p.3).unwrap_or(28);
        self.write_section_block(file, new_id, 0, raw_block)?;
        self.player_by_name
            .entry(key)
            .or_insert((new_id, fide, new_fide_off));
        if let Some(new_fide) = fide {
            self.player_by_fide.entry(new_fide).or_insert(new_id);
        }
        Ok(new_id)
    }

    fn map_or_insert_event(
        &mut self,
        file: &mut File,
        place: String,
        title: String,
        raw_block: Option<&[u8]>,
    ) -> Result<u32> {
        let key = (place, title);
        if let Some(&id) = self.event_by_key.get(&key) {
            return Ok(id);
        }
        let new_id = self.next_id[1];
        self.next_id[1] = new_id.checked_add(1).context("2LID event table overflow")?;
        let owned;
        let bytes = if let Some(raw) = raw_block {
            raw
        } else {
            owned = event_record(&(key.0.clone(), key.1.clone()))?;
            &owned
        };
        self.write_section_block(file, new_id, 1, bytes)?;
        self.event_by_key.insert(key, new_id);
        Ok(new_id)
    }

    fn map_or_insert_source(
        &mut self,
        file: &mut File,
        title: String,
        publisher: String,
        raw_block: Option<&[u8]>,
    ) -> Result<u32> {
        let key = (title, publisher);
        if let Some(&id) = self.source_by_key.get(&key) {
            return Ok(id);
        }
        let new_id = self.next_id[2];
        self.next_id[2] = new_id
            .checked_add(1)
            .context("2LID source table overflow")?;
        let owned;
        let bytes = if let Some(raw) = raw_block {
            raw
        } else {
            owned = source_record(&(key.0.clone(), key.1.clone()))?;
            &owned
        };
        self.write_section_block(file, new_id, 2, bytes)?;
        self.source_by_key.insert(key, new_id);
        Ok(new_id)
    }

    fn map_or_insert_team(
        &mut self,
        file: &mut File,
        name_key: String,
        raw_block: &[u8],
    ) -> Result<u32> {
        if self.layout.sizes[LID_TEAM_SEC] < 8 {
            return Ok(u32::MAX);
        }
        if let Some(&id) = self.team_by_key.get(&name_key) {
            return Ok(id);
        }
        let new_id = self.next_id[LID_TEAM_SEC];
        self.next_id[LID_TEAM_SEC] = new_id.checked_add(1).context("2LID team table overflow")?;
        self.write_section_block(file, new_id, LID_TEAM_SEC, raw_block)?;
        self.team_by_key.insert(name_key, new_id);
        Ok(new_id)
    }
}

fn map_readonly(path: &Path, kind: &str) -> Result<Mmap> {
    let file = File::open(path).with_context(|| format!("opening {kind} {}", path.display()))?;
    unsafe { Mmap::map(&file) }.with_context(|| format!("mapping {kind} {}", path.display()))
}

fn resolve_2cbh_source(source: &Path) -> Result<Option<PathBuf>> {
    if source.is_file() {
        let is_2cbh = source
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("2cbh"));
        return Ok(is_2cbh.then(|| source.to_path_buf()));
    }
    if source.is_dir() {
        let mut found = None;
        for entry in fs::read_dir(source)
            .with_context(|| format!("reading update directory {}", source.display()))?
        {
            let path = entry?.path();
            if path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("2cbh"))
            {
                found = Some(path);
                break;
            }
        }
        return Ok(found);
    }
    Ok(None)
}

fn parse_two_strings_raw(block: &[u8]) -> Option<([&[u8]; 2], usize)> {
    let mut at = 4usize;
    let mut texts = [&[][..]; 2];
    for slot in &mut texts {
        let len = le_u32(block.get(at..at + 4)?) as usize;
        at += 4;
        *slot = block.get(at..at.checked_add(len)?)?;
        at += len;
    }
    Some((texts, at))
}

fn parse_player_raw(block: &[u8]) -> Option<(String, String, Option<u32>, usize)> {
    let (texts, tail) = parse_two_strings_raw(block)?;
    let last = decode_text(texts[0]).trim().to_lowercase();
    let first = decode_text(texts[1]).trim().to_lowercase();
    let fide_off = tail + 16;
    let fide = block
        .get(fide_off..fide_off + 4)
        .map(le_u32)
        .filter(|&v| v != 0 && v != u32::MAX);
    Some((last, first, fide, fide_off))
}

fn parse_two_string_key(block: &[u8]) -> Option<(String, String)> {
    let (texts, _) = parse_two_strings_raw(block)?;
    Some((
        decode_text(texts[0]).trim().to_lowercase(),
        decode_text(texts[1]).trim().to_lowercase(),
    ))
}

fn parse_one_string_key(block: &[u8]) -> Option<String> {
    let len = le_u32(block.get(4..8)?) as usize;
    let raw = block.get(8..8usize.checked_add(len)?)?;
    Some(decode_text(raw).trim().to_lowercase())
}

fn cbg_content_prefix(cbg: &[u8], off: usize) -> (&[u8], u32) {
    let Some(hdr) = cbg.get(off..off + 34) else {
        return (&[], 0);
    };
    if hdr[..8] != CBG_MAGIC {
        return (&[], 0);
    }
    let a = le_u32(&hdr[8..12]);
    let content_end = off + 24 + (a as usize);
    if content_end > cbg.len() {
        return (&[], 0);
    }
    let slice_len = (a as usize).min(DEFAULT_MAX_FIDE_PROBE);
    (&cbg[off + 24..off + 24 + slice_len], a)
}

fn game_fingerprint(white: u32, black: u32, rec: &[u8], content: &[u8], content_len: u32) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mix = |h: &mut u64, bytes: &[u8]| {
        for &b in bytes {
            *h ^= u64::from(b);
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    mix(&mut h, &white.to_le_bytes());
    mix(&mut h, &black.to_le_bytes());
    mix(&mut h, &rec[0x58..0x60]);
    mix(&mut h, &rec[0x80..0x84]);
    mix(&mut h, &rec[0xbc..0xc0]);
    mix(&mut h, &content_len.to_le_bytes());
    mix(&mut h, content);
    h
}

fn copy_reframed_record(
    src: &[u8],
    offset: usize,
    target_offset: u64,
    out: &mut BufWriter<File>,
) -> Result<u64> {
    let hdr = src
        .get(offset..offset + 24)
        .with_context(|| format!("companion frame truncated at offset {offset}"))?;
    if hdr[..8] != CBG_MAGIC {
        bail!("companion frame at offset {offset} has invalid magic");
    }
    let a = le_u32(&hdr[8..12]) as usize;
    let b = le_u32(&hdr[12..16]) as usize;
    let key = &hdr[16..24];
    let content_start = offset + 24;
    let term_start = content_start
        .checked_add(a)
        .context("companion content overflow")?;
    let body = src
        .get(content_start..term_start + 2)
        .with_context(|| format!("companion frame body truncated at offset {offset}"))?;

    let base_b = b & !7;
    let rem = ((target_offset + 34 + a as u64 + base_b as u64) % 8) as usize;
    let new_b = base_b + ((8 - rem) % 8);
    let new_total = 34u64 + a as u64 + new_b as u64;

    out.write_all(&CBG_MAGIC)?;
    out.write_all(&(a as u32).to_le_bytes())?;
    out.write_all(&(new_b as u32).to_le_bytes())?;
    out.write_all(key)?;
    out.write_all(body)?;
    if new_b > 0 {
        const ZEROES: [u8; 64] = [0u8; 64];
        let mut left = new_b;
        while left > 0 {
            let chunk = left.min(ZEROES.len());
            out.write_all(&ZEROES[..chunk])?;
            left -= chunk;
        }
    }
    out.write_all(&new_total.to_le_bytes())?;
    Ok(new_total)
}

fn merge_2cbh_source(src_cbh: &Path, sink: &mut MergeSink<'_>) -> Result<()> {
    let src_stem = normalize_stem(src_cbh)?;
    let src_cbh_path = core_path(&src_stem, ".2cbh");
    let src_cbg_path = core_path(&src_stem, ".2cbg");
    let src_cba_path = core_path(&src_stem, ".2cba");
    let src_lid_path = core_path(&src_stem, ".2lid");

    let cbh = map_readonly(&src_cbh_path, "source 2CBH")?;
    let cbg = map_readonly(&src_cbg_path, "source 2CBG")?;
    let cba = map_readonly(&src_cba_path, "source 2CBA")?;
    let lid = map_readonly(&src_lid_path, "source 2LID")?;

    let src_layout = LidLayout::parse(&lid, &src_lid_path)?;
    let src_blocks = src_layout.allocated_blocks(lid.len());

    let mut player_map: Vec<u32> = vec![u32::MAX; src_blocks];
    let mut event_map: Vec<u32> = vec![u32::MAX; src_blocks];
    let mut source_map: Vec<u32> = vec![u32::MAX; src_blocks];
    let mut team_map: Vec<u32> = vec![u32::MAX; src_blocks];

    for (id, slot) in player_map.iter_mut().enumerate() {
        let id_u32 = id as u32;
        if let Some(raw) = src_layout.raw_slice(&lid, id_u32, 0)
            && let Some((last, first, fide, _fide_off)) = parse_player_raw(raw)
        {
            let target_id =
                sink.lid_state
                    .map_or_insert_player(sink.lid_file, last, first, fide, raw)?;
            *slot = target_id;
        }
    }
    for (id, slot) in event_map.iter_mut().enumerate() {
        let id_u32 = id as u32;
        if let Some(raw) = src_layout.raw_slice(&lid, id_u32, 1)
            && let Some((place, title)) = parse_two_string_key(raw)
        {
            *slot = sink
                .lid_state
                .map_or_insert_event(sink.lid_file, place, title, Some(raw))?;
        }
    }
    for (id, slot) in source_map.iter_mut().enumerate() {
        let id_u32 = id as u32;
        if let Some(raw) = src_layout.raw_slice(&lid, id_u32, 2)
            && let Some((title, pub_name)) = parse_two_string_key(raw)
        {
            *slot =
                sink.lid_state
                    .map_or_insert_source(sink.lid_file, title, pub_name, Some(raw))?;
        }
    }
    if src_layout.sizes[LID_TEAM_SEC] >= 8 {
        for (id, slot) in team_map.iter_mut().enumerate() {
            let id_u32 = id as u32;
            if let Some(raw) = src_layout.raw_slice(&lid, id_u32, LID_TEAM_SEC)
                && let Some(key) = parse_one_string_key(raw)
            {
                *slot = sink.lid_state.map_or_insert_team(sink.lid_file, key, raw)?;
            }
        }
    }

    let file_slots = cbh.len() / RECORD_SIZE;
    let stored_slots = if cbh.len() >= RECORD_SIZE {
        le_u32(&cbh[0x10..0x14]) as usize
    } else {
        0
    };
    let src_slots = if stored_slots > 0 && stored_slots <= file_slots {
        stored_slots
    } else {
        file_slots
    };

    for slot in 1..src_slots {
        let src_rec = &cbh[slot * RECORD_SIZE..(slot + 1) * RECORD_SIZE];
        if src_rec[0] & 0x82 != 0 {
            continue;
        }
        let remap_entity = |raw_id: u32, map: &[u32], fallback: u32| -> u32 {
            map.get(raw_id as usize)
                .copied()
                .filter(|&v| v != u32::MAX)
                .unwrap_or(fallback)
        };
        let white_id = remap_entity(le_u32(&src_rec[0x18..0x1c]), &player_map, 0);
        let black_id = remap_entity(le_u32(&src_rec[0x20..0x24]), &player_map, 0);
        let event_id = remap_entity(le_u32(&src_rec[0x28..0x2c]), &event_map, 0);
        let raw_annot_lo = le_u32(&src_rec[0x30..0x34]);
        let raw_annot_hi = le_u32(&src_rec[0x34..0x38]);
        let annot_id = if raw_annot_hi != 0 {
            None
        } else if raw_annot_lo == 0 || raw_annot_lo == 2 {
            Some(sink.lid_state.empty_player_id.unwrap_or(raw_annot_lo))
        } else {
            Some(remap_entity(raw_annot_lo, &player_map, 2))
        };
        let source_id = remap_entity(le_u32(&src_rec[0x38..0x3c]), &source_map, 0);
        let remap_team = |off: usize| -> u64 {
            let lo = le_u32(&src_rec[off..off + 4]);
            let hi = le_u32(&src_rec[off + 4..off + 8]);
            if hi != 0 || lo == u32::MAX {
                u64::MAX
            } else {
                team_map
                    .get(lo as usize)
                    .copied()
                    .filter(|&v| v != u32::MAX)
                    .map(u64::from)
                    .unwrap_or(u64::MAX)
            }
        };
        let white_team = remap_team(0x40);
        let black_team = remap_team(0x48);

        let src_cbg_off = le_u64(&src_rec[0x08..0x10]) as usize;
        let src_cba_off = le_u64(&src_rec[0x10..0x18]) as usize;
        let (slice, a_len) = cbg_content_prefix(&cbg, src_cbg_off);
        if a_len == 0 {
            continue;
        }
        let fp = game_fingerprint(white_id, black_id, src_rec, slice, a_len);
        if !sink.seen_games.insert(fp) {
            continue;
        }

        let new_cbg_off = *sink.cbg_offset;
        let cbg_written = copy_reframed_record(&cbg, src_cbg_off, new_cbg_off, sink.cbg_writer)?;
        *sink.cbg_offset += cbg_written;

        let new_cba_off = *sink.cba_offset;
        let cba_written = if src_cba_off >= CBG_HEADER as usize
            && cba.get(src_cba_off..src_cba_off + 8) == Some(&CBG_MAGIC)
        {
            copy_reframed_record(&cba, src_cba_off, new_cba_off, sink.cba_writer)?
        } else {
            let empty_cba = frame_record(
                &[0x00, 0x20, 0xff, 0xff],
                [0xff, 0x7f],
                194,
                0x7fff_ffff,
                new_cba_off,
            );
            sink.cba_writer.write_all(&empty_cba)?;
            empty_cba.len() as u64
        };
        *sink.cba_offset += cba_written;

        let mut new_rec = [0u8; RECORD_SIZE];
        new_rec.copy_from_slice(src_rec);
        new_rec[0x08..0x10].copy_from_slice(&new_cbg_off.to_le_bytes());
        new_rec[0x10..0x18].copy_from_slice(&new_cba_off.to_le_bytes());
        new_rec[0x18..0x1c].copy_from_slice(&white_id.to_le_bytes());
        new_rec[0x20..0x24].copy_from_slice(&black_id.to_le_bytes());
        new_rec[0x28..0x2c].copy_from_slice(&event_id.to_le_bytes());
        if let Some(annot_id) = annot_id {
            new_rec[0x30..0x34].copy_from_slice(&annot_id.to_le_bytes());
        }
        new_rec[0x38..0x3c].copy_from_slice(&source_id.to_le_bytes());
        new_rec[0x40..0x48].copy_from_slice(&white_team.to_le_bytes());
        new_rec[0x48..0x50].copy_from_slice(&black_team.to_le_bytes());
        sink.cbh_writer.write_all(&new_rec)?;
        *sink.cbh_slots += 1;
        *sink.appended_games += 1;
    }

    Ok(())
}

fn merge_cbv_source(archive: &Path, sink: &mut MergeSink<'_>) -> Result<()> {
    let lib = crate::cbh::CbhLibrary::open_cbv(archive)?;
    for index in 0..lib.len() {
        let Some(indexed) = lib.get(index) else {
            continue;
        };
        if is_deleted_headers(indexed) {
            continue;
        }
        let Ok(game) = lib.load(index) else {
            continue;
        };
        if is_deleted(&game) {
            continue;
        }
        let Ok(EncodedMoves {
            start_id,
            custom_start,
            mut tokens,
        }) = encode_game_moves(&game)
        else {
            continue;
        };
        let base_padding = if tokens.is_empty() {
            194
        } else {
            let _ = tokens.pop();
            98
        };
        let content = TwoCbhWriter::build_cbg_content(start_id, custom_start.as_deref(), &tokens);

        let (w_last, w_first, w_fide) =
            player_key(game.header("White"), game.header("WhiteFideId"));
        let w_raw = player_record(&(w_last.clone(), w_first.clone(), w_fide))?;
        let white_id = sink.lid_state.map_or_insert_player(
            sink.lid_file,
            w_last.to_lowercase(),
            w_first.to_lowercase(),
            w_fide,
            &w_raw,
        )?;

        let (b_last, b_first, b_fide) =
            player_key(game.header("Black"), game.header("BlackFideId"));
        let b_raw = player_record(&(b_last.clone(), b_first.clone(), b_fide))?;
        let black_id = sink.lid_state.map_or_insert_player(
            sink.lid_file,
            b_last.to_lowercase(),
            b_first.to_lowercase(),
            b_fide,
            &b_raw,
        )?;

        let (e_place, e_title) = event_key(game.header("Site"), game.header("Event"));
        let e_raw = event_record(&(e_place.clone(), e_title.clone()))?;
        let event_id = sink.lid_state.map_or_insert_event(
            sink.lid_file,
            e_place.to_lowercase(),
            e_title.to_lowercase(),
            Some(&e_raw),
        )?;

        let (s_title, s_pub) = source_key(game.header("SourceTitle"), game.header("Source"));
        let s_raw = source_record(&(s_title.clone(), s_pub.clone()))?;
        let source_id = sink.lid_state.map_or_insert_source(
            sink.lid_file,
            s_title.to_lowercase(),
            s_pub.to_lowercase(),
            Some(&s_raw),
        )?;

        let (round, subround) = encode_round(game.header("Round"), game.header("Board"));
        let mut record = [0u8; RECORD_SIZE];
        record[0] = 0x01;
        record[1..4].copy_from_slice(&[0, 1, 1]);
        record[0x18..0x1c].copy_from_slice(&white_id.to_le_bytes());
        record[0x20..0x24].copy_from_slice(&black_id.to_le_bytes());
        record[0x28..0x2c].copy_from_slice(&event_id.to_le_bytes());
        record[0x30..0x34]
            .copy_from_slice(&sink.lid_state.empty_player_id.unwrap_or(2).to_le_bytes());
        record[0x38..0x3c].copy_from_slice(&source_id.to_le_bytes());
        record[0x40..0x50].fill(0xff);
        let packed = (u32::from(round) << 16) | u32::from(encode_result(game.result()));
        record[0x58..0x5c].copy_from_slice(&packed.to_le_bytes());
        record[0x5c..0x60].copy_from_slice(&subround.to_le_bytes());
        record[0x60..0x64].copy_from_slice(&parse_elo(game.header("WhiteElo")).to_le_bytes());
        record[0x70..0x74].copy_from_slice(&parse_elo(game.header("BlackElo")).to_le_bytes());
        if let Some(start_id) = start_id {
            let packed_start = START_ID_BASE as u16 + start_id;
            record[0x80..0x82].copy_from_slice(&packed_start.to_le_bytes());
        } else {
            let eco = crate::cbh::writer::encode_eco(game.header("ECO"));
            if eco > 0 {
                record[0x80..0x82].copy_from_slice(&eco.to_le_bytes());
            }
        }
        let medals = crate::library::medals::from_game(&game);
        if medals != 0 {
            record[0x82..0x84].copy_from_slice(&medals.to_le_bytes());
        }
        record[0x98..0xa0].copy_from_slice(&parse_u64_header(game.header("GameId")).to_le_bytes());
        record[0xb0..0xb8].copy_from_slice(&parse_u64_header(game.header("Beauty")).to_le_bytes());
        record[0xbc..0xc0].copy_from_slice(&encode_date(game.header("Date")).to_le_bytes());

        let prefix_len = content.len().min(DEFAULT_MAX_FIDE_PROBE);
        let fp = game_fingerprint(
            white_id,
            black_id,
            &record,
            &content[..prefix_len],
            content.len() as u32,
        );
        if !sink.seen_games.insert(fp) {
            continue;
        }

        let new_cbg_off = *sink.cbg_offset;
        let key = record_key(*sink.cbh_slots as u64, &content);
        let cbg_record = frame_record(&content, [0xff, 0xff], base_padding, key, new_cbg_off);
        sink.cbg_writer.write_all(&cbg_record)?;
        *sink.cbg_offset += cbg_record.len() as u64;

        let new_cba_off = *sink.cba_offset;
        let cba_record = frame_record(
            &[0x00, 0x20, 0xff, 0xff],
            [0xff, 0x7f],
            194,
            0x7fff_ffff,
            new_cba_off,
        );
        sink.cba_writer.write_all(&cba_record)?;
        *sink.cba_offset += cba_record.len() as u64;

        record[0x08..0x10].copy_from_slice(&new_cbg_off.to_le_bytes());
        record[0x10..0x18].copy_from_slice(&new_cba_off.to_le_bytes());
        sink.cbh_writer.write_all(&record)?;
        *sink.cbh_slots += 1;
        *sink.appended_games += 1;
    }
    Ok(())
}

fn is_deleted_headers(game: &crate::library::IndexedGame) -> bool {
    game.header("Deleted")
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

#[cfg(test)]
mod tests;
