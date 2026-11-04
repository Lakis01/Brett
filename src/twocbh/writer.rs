//! Native writer for ChessBase's current multi-file database format
//! (`.2cbh`, `.2cbg`, `.2cba`, and `.2lid`).
//!
//! Games are streamed to temporary files beside the target.  On completion the
//! existing companion files at the target stem are moved aside to backup paths
//! and replaced atomically, rolling back on error.  Derived `.2lcd`/`.2lgd`
//! indexes are removed with the backups so ChessBase rebuilds them.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::chess::{
    CastlingMode, Chess, Color, EnPassantMode, Fen, File as ChessFile, Move, Position, Rank, Role,
    SanPlus, Square, UciMove,
};
use crate::domain::{Game, Ply};

use super::moves;
use super::tokens::table;

use super::{
    CBG_MAGIC, LID_EVENT, LID_FIRST, LID_SOURCE, LID_STRIDE, RECORD_SIZE, START_ID_BASE, sibling,
};

const LID_PLAYER: usize = 0;

/// The four files that make up a 2CBH database.
pub const CORE_EXTENSIONS: [&str; 4] = ["2cbh", "2cbg", "2cba", "2lid"];

/// ChessBase index files derived from a 2CBH database.
pub const DERIVED_EXTENSIONS: [&str; 2] = ["2lcd", "2lgd"];

/// Every file that belongs to a 2CBH stem (core files plus derived indexes).
pub const ALL_EXTENSIONS: [&str; 6] = ["2cbh", "2cbg", "2cba", "2lid", "2lcd", "2lgd"];

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
}

#[derive(Debug)]
struct EncodedMoves {
    start_id: Option<u16>,
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
    let variant_standard = variant.is_empty() || variant == "standard" || variant == "chess";
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

    if root.castling_mode() != CastlingMode::Standard || fen != start_fen_prefix(&Chess::default())
    {
        bail!("2CBH export start position is not standard chess or Chess960: {fen}");
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
    let start_id = match start {
        Start::Standard => None,
        Start::Chess960(id) => Some(id),
    };
    let mut tokens = Vec::new();
    if let Some(&first) = game.nodes[0].children.first() {
        encode_line(game, first, false, start_id, &mut tokens)?;
    }
    Ok(EncodedMoves { start_id, tokens })
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
            guard,
        })
    }

    /// Encode, stream, and index one game.
    fn add_game(&mut self, game: &Game) -> Result<()> {
        let index = self.games;
        if !self.options.allow_annotation_loss
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

        let mut content = Vec::with_capacity(8 + tokens.len() * 2);
        match start_id {
            None => {
                content.extend_from_slice(&1u16.to_le_bytes());
                content.extend_from_slice(&0xfffcu16.to_le_bytes());
            }
            Some(id) => {
                content.extend_from_slice(&2u16.to_le_bytes());
                content.extend_from_slice(&0xfffbu16.to_le_bytes());
                content.extend_from_slice(&id.to_le_bytes());
                content.extend_from_slice(&0xfffcu16.to_le_bytes());
            }
        }
        for token in &tokens {
            content.extend_from_slice(&token.to_le_bytes());
        }

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
            record[0x80..0x84]
                .copy_from_slice(&(START_ID_BASE + u32::from(start_id)).to_le_bytes());
        } else {
            let eco = crate::cbh::writer::encode_eco(game.header("ECO"));
            if eco > 0 {
                record[0x80..0x82].copy_from_slice(&eco.to_le_bytes());
            }
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
        let staged = [
            (&self.cbg_tmp, core_path(&self.stem, ".2cbg")),
            (&self.cba_tmp, core_path(&self.stem, ".2cba")),
            (&self.lid_tmp, core_path(&self.stem, ".2lid")),
            (&self.cbh_tmp, core_path(&self.stem, ".2cbh")),
        ];

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

#[cfg(test)]
mod tests;
