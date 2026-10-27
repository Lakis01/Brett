//! Native opening-book readers: Polyglot `.bin` and ChessBase `.ctg`.
//!
//! Polyglot books are a flat, sorted table of 16-byte entries keyed by the
//! standard Polyglot Zobrist hash.  shakmaty's [`Zobrist64`] uses the same
//! table, so lookup needs no separate hashing implementation.
//!
//! CTG books are the reverse-engineered ChessBase format: a 4096-byte page
//! file (`.ctg`), a page-offset table (`.cto`) and a small page-bound table
//! (`.ctb`).  The implementation below is an independent Rust reader written
//! from the published format description; the fixed hash constants and the
//! move-description table are format data, not code from a chess engine.
//! It reads the same information Brett needs for its opening tree: the legal
//! book moves, their win/loss/draw counts and the book's annotation byte.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use memmap2::{Mmap, MmapOptions};
use shakmaty::zobrist::Zobrist64;
use shakmaty::{Chess, Color, EnPassantMode, File as ChessFile, Move, Position, Role, Square};

use crate::position::{encode_move, position_key};

mod bkt;

const POLYGLOT_ENTRY: usize = 16;
const CTG_PAGE: usize = 4096;
const ABK_ENTRY: usize = 28;
const ABK_ROOT: usize = 900;

/// Which native opening-book format a [`Book`] uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookFormat {
    Polyglot,
    Ctg,
    Abk,
    Bkt,
}

impl BookFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Polyglot => "Polyglot .bin",
            Self::Ctg => "ChessBase .ctg",
            Self::Abk => "Arena .abk",
            Self::Bkt => "Shredder .bkt",
        }
    }

    fn from_extension(extension: &str) -> Option<Self> {
        match extension.to_ascii_lowercase().as_str() {
            "bin" => Some(Self::Polyglot),
            "ctg" => Some(Self::Ctg),
            "abk" => Some(Self::Abk),
            "bkt" => Some(Self::Bkt),
            _ => None,
        }
    }
}

/// One move from an opening book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BookMove {
    pub mv: Move,
    /// Polyglot weight, or CTG total games for the resulting position.
    pub weight: u64,
    pub games: u64,
    pub white: u64,
    pub draw: u64,
    pub black: u64,
    /// CTG's one-byte annotation code (`!`, `?`, `!!`, …), when present.
    pub annotation: Option<u8>,
    /// CTG's recommendation marker, when present.
    pub recommendation: Option<u8>,
    /// Shredder BKT move-quality letter (a–g), when present.
    pub quality: Option<char>,
}

impl BookMove {
    pub fn score_white(&self) -> Option<f32> {
        if self.games == 0 {
            return None;
        }
        Some((self.white as f32 + self.draw as f32 * 0.5) / self.games as f32)
    }
}

/// An opened opening book.  The backing files are memory-mapped and immutable.
pub struct Book {
    path: PathBuf,
    format: BookFormat,
    data: BookData,
    file_len: u64,
}

enum BookData {
    Polyglot(PolyglotBook),
    Ctg(CtgBook),
    Abk(AbkBook),
    Bkt(bkt::BktBook),
}

struct PolyglotBook {
    mmap: Mmap,
}

struct CtgBook {
    mmap: Mmap,
    offsets: Vec<u8>,
    lower: u32,
    upper: u32,
}

/// Arena `.abk` books are a tree of fixed 28-byte records.  The first 900
/// records are reserved for the header, so the root move chain starts at index
/// 900.  The tree is expanded once at open time into a position-keyed map; this
/// keeps probing independent of how many transpositions the book contains.
struct AbkBook {
    mmap: Mmap,
    positions: HashMap<u64, Vec<BookMove>>,
}

impl Book {
    /// Open a `.bin` or `.ctg` book.  For CTG the sibling `.cto` and `.ctb`
    /// files are opened automatically.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("");
        let format = BookFormat::from_extension(extension)
            .with_context(|| format!("{} is not a supported opening book", path.display()))?;
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let file_len = file.metadata()?.len();
        let mmap = unsafe { MmapOptions::new().map(&file) }
            .with_context(|| format!("mapping {}", path.display()))?;
        let data = match format {
            BookFormat::Polyglot => {
                if mmap.len() < POLYGLOT_ENTRY {
                    bail!("{} is too short to be a Polyglot book", path.display());
                }
                BookData::Polyglot(PolyglotBook { mmap })
            }
            BookFormat::Ctg => BookData::Ctg(CtgBook::open(path, mmap)?),
            BookFormat::Abk => BookData::Abk(AbkBook::open(mmap)?),
            BookFormat::Bkt => BookData::Bkt(bkt::BktBook::open(mmap)?),
        };
        Ok(Self {
            path: path.to_path_buf(),
            format,
            data,
            file_len,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn format(&self) -> BookFormat {
        self.format
    }

    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    pub fn summary(&self) -> String {
        let extra = match &self.data {
            BookData::Polyglot(book) => {
                format!("{} positions", book.mmap.len() / POLYGLOT_ENTRY)
            }
            BookData::Ctg(book) => format!(
                "{} pages · bounds {}–{}",
                book.mmap.len() / CTG_PAGE,
                book.lower,
                book.upper
            ),
            BookData::Abk(book) => format!(
                "{} entries · {} positions",
                book.mmap.len() / ABK_ENTRY,
                book.positions.len()
            ),
            BookData::Bkt(book) => book.summary(),
        };
        format!("{} · {}", self.format.label(), extra)
    }

    /// All book moves from `pos`, best first.  An empty vector means the book
    /// has no entry for this position.
    pub fn moves(&self, pos: &Chess) -> Result<Vec<BookMove>> {
        match &self.data {
            BookData::Polyglot(book) => book.moves(pos),
            BookData::Ctg(book) => book.moves(pos),
            BookData::Abk(book) => Ok(book.moves(pos)),
            BookData::Bkt(book) => book.moves(pos),
        }
    }
}

impl PolyglotBook {
    fn moves(&self, pos: &Chess) -> Result<Vec<BookMove>> {
        let key = pos.zobrist_hash::<Zobrist64>(EnPassantMode::Legal).0;
        let count = self.mmap.len() / POLYGLOT_ENTRY;
        let mut low = 0usize;
        let mut high = count;
        while low < high {
            let mid = low + (high - low) / 2;
            let entry_key = self.key_at(mid);
            if entry_key < key {
                low = mid + 1;
            } else {
                high = mid;
            }
        }

        let mut moves: BTreeMap<u32, BookMove> = BTreeMap::new();
        let mut index = low;
        while index < count && self.key_at(index) == key {
            let at = index * POLYGLOT_ENTRY;
            let raw = u16::from_be_bytes([self.mmap[at + 8], self.mmap[at + 9]]);
            let weight = u16::from_be_bytes([self.mmap[at + 10], self.mmap[at + 11]]);
            if let Some(mv) = decode_polyglot_move(pos, raw) {
                let code = encode_move(mv).unwrap_or(u32::from(raw));
                let entry = moves.entry(code).or_insert(BookMove {
                    mv,
                    weight: 0,
                    games: 0,
                    white: 0,
                    draw: 0,
                    black: 0,
                    annotation: None,
                    recommendation: None,
                    quality: None,
                });
                entry.weight += u64::from(weight);
            }
            index += 1;
        }
        let mut moves: Vec<BookMove> = moves.into_values().collect();
        moves.sort_unstable_by(|a, b| {
            b.weight
                .cmp(&a.weight)
                .then(b.games.cmp(&a.games))
                .then_with(|| move_code(a.mv).cmp(&move_code(b.mv)))
        });
        Ok(moves)
    }

    fn key_at(&self, index: usize) -> u64 {
        let at = index * POLYGLOT_ENTRY;
        u64::from_be_bytes(self.mmap[at..at + 8].try_into().unwrap())
    }
}

impl CtgBook {
    fn open(path: &Path, mmap: Mmap) -> Result<Self> {
        let stem = path.with_extension("");
        let cto_path = sibling(&stem, ".cto");
        let ctb_path = sibling(&stem, ".ctb");
        if !cto_path.is_file() {
            bail!(
                "CTG book {} is missing its page-offset file {}",
                path.display(),
                cto_path.display()
            );
        }
        let offsets = fs::read(&cto_path)
            .with_context(|| format!("reading CTG offsets {}", cto_path.display()))?;
        let (lower, upper) = if ctb_path.is_file() {
            let bounds = fs::read(&ctb_path)
                .with_context(|| format!("reading CTG bounds {}", ctb_path.display()))?;
            if bounds.len() < 12 {
                bail!(
                    "{} is too short to be a CTG bounds file",
                    ctb_path.display()
                );
            }
            (
                u32::from_be_bytes(bounds[4..8].try_into().unwrap()),
                u32::from_be_bytes(bounds[8..12].try_into().unwrap()),
            )
        } else {
            (0, (mmap.len() / CTG_PAGE).saturating_sub(1) as u32)
        };
        Ok(Self {
            mmap,
            offsets,
            lower,
            upper,
        })
    }

    fn moves(&self, pos: &Chess) -> Result<Vec<BookMove>> {
        let Some(data) = self.position_data(pos) else {
            return Ok(Vec::new());
        };
        let page = data.page;
        if page.is_empty() {
            return Ok(Vec::new());
        }
        let count = usize::from(page[0] >> 1);
        let legal: Vec<Move> = pos.legal_moves().into_iter().collect();
        let mut moves = Vec::new();
        for index in 0..count {
            let at = 1 + index * 2;
            if at >= page.len() {
                break;
            }
            let Some((from, to, promotion)) = pseudo_move(&data.board, page[at]) else {
                continue;
            };
            let from = if data.flip { flip_file(from) } else { from };
            let to = if data.flip { flip_file(to) } else { to };
            let from = if data.invert { flip_rank(from) } else { from };
            let to = if data.invert { flip_rank(to) } else { to };
            let Some(mv) = legal
                .iter()
                .copied()
                .find(|mv| move_matches(*mv, from, to, promotion))
            else {
                continue;
            };
            let stats = pos
                .clone()
                .play(mv)
                .ok()
                .and_then(|child| self.position_data(&child))
                .map(|child| self.stats(&child))
                .unwrap_or_default();
            let annotation = page.get(2 + index * 2).copied();
            let recommendation = page.get(usize::from(page[0]) + 30).copied();
            moves.push(BookMove {
                mv,
                weight: stats.total(),
                games: stats.total(),
                white: stats.white,
                draw: stats.draw,
                black: stats.black,
                annotation,
                recommendation,
                quality: None,
            });
        }
        moves.sort_unstable_by(|a, b| {
            b.games
                .cmp(&a.games)
                .then(b.weight.cmp(&a.weight))
                .then_with(|| move_code(a.mv).cmp(&move_code(b.mv)))
        });
        Ok(moves)
    }

    fn position_data(&self, pos: &Chess) -> Option<CtgPositionData> {
        let mut board = [' '; 64];
        for index in 0..64u32 {
            let square = Square::new(index);
            if let Some(piece) = pos.board().piece_at(square) {
                let rank = u32::from(square.rank());
                let file = u32::from(square.file());
                board[((7 - rank) * 8 + file) as usize] = piece.char();
            }
        }
        let ep = pos.ep_square(EnPassantMode::Legal);
        let invert = pos.turn() == Color::Black;
        let flip = needs_flipping(pos);
        if invert {
            invert_board(&mut board);
        }
        if flip {
            flip_board(&mut board);
        }
        let encoded = encode_position(&board, ep, pos)?;
        let hash = gen_position_hash(&encoded)?;
        let page = self.lookup(hash, &encoded)?;
        Some(CtgPositionData {
            board,
            page,
            invert,
            flip,
        })
    }

    fn lookup(&self, hash: u32, encoded: &[u8; 32]) -> Option<Vec<u8>> {
        let mut mask = 0u32;
        while mask < 0x7fff_ffff {
            let page_num = (hash & mask).wrapping_add(mask);
            if page_num >= self.lower {
                if let Some(page) = self.read_page(page_num, encoded) {
                    return Some(page);
                }
                if page_num >= self.upper {
                    break;
                }
            }
            mask = mask.wrapping_mul(2).wrapping_add(1);
        }
        None
    }

    fn read_page(&self, page_num: u32, encoded: &[u8; 32]) -> Option<Vec<u8>> {
        let index = page_num as usize * 4 + 16;
        let offset = self.offsets.get(index..index + 4)?;
        let page_pos = u32::from_be_bytes(offset.try_into().ok()?) as usize;
        if page_pos == 0xffff_ffff {
            return None;
        }
        if (page_pos + 2).checked_mul(CTG_PAGE)? > self.mmap.len() {
            return None;
        }
        let start = (page_pos + 1) * CTG_PAGE;
        let page = self.mmap.get(start..start + CTG_PAGE)?;
        let positions = usize::from(u16::from_be_bytes([page[0], page[1]]));
        let encoded_len = usize::from(encoded.first()? & 0x1f);
        if encoded_len == 0 {
            return None;
        }
        let mut at = 4usize;
        for _ in 0..positions {
            let entry_size = usize::from(*page.get(at)?) % 32;
            if entry_size == 0 {
                break;
            }
            if entry_size == encoded_len
                && at + entry_size <= CTG_PAGE
                && page[at..at + entry_size] == encoded[..encoded_len]
            {
                let position_len = usize::from(*page.get(at + entry_size)?);
                let end = at + entry_size + position_len + 33;
                if end > CTG_PAGE {
                    return None;
                }
                return Some(page[at + entry_size..end].to_vec());
            }
            at += entry_size + usize::from(*page.get(at + entry_size)?) + 33;
            if at >= CTG_PAGE {
                break;
            }
        }
        None
    }

    fn stats(&self, data: &CtgPositionData) -> CtgStats {
        let page = &data.page;
        if page.is_empty() {
            return CtgStats::default();
        }
        let length = usize::from(page[0]);
        let offset = length;
        let Some(bytes) = page.get(offset..offset + 12) else {
            return CtgStats::default();
        };
        // Layout: total, losses, wins, draws (all 24-bit big-endian).
        let mut white = be_u24(&bytes[6..9]);
        let mut black = be_u24(&bytes[3..6]);
        let draw = be_u24(&bytes[9..12]);
        if data.invert {
            std::mem::swap(&mut white, &mut black);
        }
        CtgStats {
            white: u64::from(white),
            draw: u64::from(draw),
            black: u64::from(black),
        }
    }
}

impl AbkBook {
    fn open(mmap: Mmap) -> Result<Self> {
        if mmap.len() < ABK_ROOT * ABK_ENTRY + ABK_ENTRY {
            bail!("Arena book is too short to contain a root entry");
        }
        let signature = mmap.get(1..4).unwrap_or_default();
        if signature != b"AB2" {
            bail!("Arena book is missing the .AB2 signature");
        }
        let mut positions: HashMap<u64, BTreeMap<u32, BookMove>> = HashMap::new();
        let mut visited = HashSet::new();
        let mut stack = vec![(ABK_ROOT, Chess::default())];
        let entry_count = mmap.len() / ABK_ENTRY;

        while let Some((first, pos)) = stack.pop() {
            let mut current = first;
            let mut chain = HashSet::new();
            while current != 0 && current < entry_count && chain.insert(current) {
                let entry = abk_entry(&mmap, current)?;
                let next_sibling = entry.next_sibling;
                let next_move = entry.next_move;
                if !visited.insert(current) {
                    current = next_sibling;
                    continue;
                }
                let Some(mv) = abk_move(&pos, &entry) else {
                    current = next_sibling;
                    continue;
                };
                let key = position_key(&pos);
                let code = encode_move(mv).unwrap_or(u32::MAX);
                let games = entry.games.max(0) as u64;
                let won = entry.won.max(0) as u64;
                let lost = entry.lost.max(0) as u64;
                let draw = games.saturating_sub(won.saturating_add(lost));
                let (white, black) = if pos.turn() == Color::White {
                    (won, lost)
                } else {
                    (lost, won)
                };
                let book_move = positions
                    .entry(key)
                    .or_default()
                    .entry(code)
                    .or_insert(BookMove {
                        mv,
                        weight: 0,
                        games: 0,
                        white: 0,
                        draw: 0,
                        black: 0,
                        annotation: None,
                        recommendation: None,
                        quality: None,
                    });
                book_move.weight = book_move
                    .weight
                    .saturating_add(games.max(u64::from(entry.priority)));
                book_move.games = book_move.games.saturating_add(games);
                book_move.white = book_move.white.saturating_add(white);
                book_move.draw = book_move.draw.saturating_add(draw);
                book_move.black = book_move.black.saturating_add(black);

                if next_move != 0
                    && next_move < entry_count
                    && let Ok(child) = pos.clone().play(mv)
                {
                    stack.push((next_move, child));
                }
                current = next_sibling;
            }
        }

        let positions = positions
            .into_iter()
            .map(|(key, moves)| {
                let mut moves: Vec<_> = moves.into_values().collect();
                moves.sort_unstable_by(|a, b| {
                    b.games
                        .cmp(&a.games)
                        .then(b.weight.cmp(&a.weight))
                        .then_with(|| move_code(a.mv).cmp(&move_code(b.mv)))
                });
                (key, moves)
            })
            .collect();
        Ok(Self { mmap, positions })
    }

    fn moves(&self, pos: &Chess) -> Vec<BookMove> {
        self.positions
            .get(&position_key(pos))
            .cloned()
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug)]
struct AbkEntry {
    from: u8,
    to: u8,
    promotion: i8,
    priority: u8,
    games: i32,
    won: i32,
    lost: i32,
    next_move: usize,
    next_sibling: usize,
}

fn abk_entry(mmap: &Mmap, index: usize) -> Result<AbkEntry> {
    let at = index
        .checked_mul(ABK_ENTRY)
        .context("Arena book entry index overflow")?;
    let bytes = mmap
        .get(at..at + ABK_ENTRY)
        .context("Arena book entry is outside the file")?;
    let i32_at = |offset: usize| i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    Ok(AbkEntry {
        from: bytes[0],
        to: bytes[1],
        promotion: bytes[2] as i8,
        priority: bytes[3],
        games: i32_at(4),
        won: i32_at(8),
        lost: i32_at(12),
        next_move: i32_at(20).max(0) as usize,
        next_sibling: i32_at(24).max(0) as usize,
    })
}

fn abk_move(pos: &Chess, entry: &AbkEntry) -> Option<Move> {
    let from = Square::new(u32::from(entry.from));
    let to = Square::new(u32::from(entry.to));
    let promotion = match entry.promotion.unsigned_abs() {
        0 => None,
        1 => Some(Role::Rook),
        2 => Some(Role::Knight),
        3 => Some(Role::Bishop),
        4 => Some(Role::Queen),
        _ => return None,
    };
    pos.legal_moves()
        .into_iter()
        .find(|mv| move_matches(*mv, from, to, promotion))
}

#[derive(Clone, Copy, Debug, Default)]
struct CtgStats {
    white: u64,
    draw: u64,
    black: u64,
}

impl CtgStats {
    fn total(self) -> u64 {
        self.white + self.draw + self.black
    }
}

struct CtgPositionData {
    board: [char; 64],
    page: Vec<u8>,
    invert: bool,
    flip: bool,
}

fn decode_polyglot_move(pos: &Chess, raw: u16) -> Option<Move> {
    let to = Square::new(u32::from(raw & 0x3f));
    let from = Square::new(u32::from((raw >> 6) & 0x3f));
    let promotion = match (raw >> 12) & 0x7 {
        0 => None,
        1 => Some(Role::Knight),
        2 => Some(Role::Bishop),
        3 => Some(Role::Rook),
        4 => Some(Role::Queen),
        _ => return None,
    };
    pos.legal_moves()
        .into_iter()
        .find(|mv| move_matches(*mv, from, to, promotion))
}

fn move_matches(mv: Move, from: Square, to: Square, promotion: Option<Role>) -> bool {
    match mv {
        Move::Normal {
            from: actual_from,
            to: actual_to,
            promotion: actual_promotion,
            ..
        } => actual_from == from && actual_to == to && actual_promotion == promotion,
        Move::EnPassant {
            from: actual_from,
            to: actual_to,
        } => actual_from == from && actual_to == to && promotion.is_none(),
        Move::Castle { king, rook } => {
            king == from && castle_destination(king, rook) == to && promotion.is_none()
        }
        Move::Put { .. } => false,
    }
}

fn castle_destination(king: Square, rook: Square) -> Square {
    let file = if rook.file() > king.file() {
        ChessFile::G
    } else {
        ChessFile::C
    };
    Square::from_coords(file, king.rank())
}

fn move_code(mv: Move) -> u32 {
    encode_move(mv).unwrap_or(u32::MAX)
}

fn sibling(stem: &Path, extension: &str) -> PathBuf {
    let mut name = stem.as_os_str().to_owned();
    name.push(extension);
    PathBuf::from(name)
}

fn needs_flipping(pos: &Chess) -> bool {
    if !pos.castles().castling_rights().is_empty() {
        return false;
    }
    pos.board()
        .king_of(pos.turn())
        .is_some_and(|king| king.file().to_u32() <= 3)
}

fn invert_board(board: &mut [char; 64]) {
    for y in 0..8 {
        for x in 0..8 {
            let index = y * 8 + x;
            board[index] = invert_piece(board[index]);
        }
    }
    for y in 0..4 {
        for x in 0..8 {
            board.swap(y * 8 + x, (7 - y) * 8 + x);
        }
    }
}

fn invert_piece(piece: char) -> char {
    match piece {
        'P' => 'p',
        'p' => 'P',
        'N' => 'n',
        'n' => 'N',
        'B' => 'b',
        'b' => 'B',
        'R' => 'r',
        'r' => 'R',
        'Q' => 'q',
        'q' => 'Q',
        'K' => 'k',
        'k' => 'K',
        other => other,
    }
}

fn flip_board(board: &mut [char; 64]) {
    for y in 0..8 {
        for x in 0..4 {
            board.swap(y * 8 + x, y * 8 + 7 - x);
        }
    }
}

fn flip_rank(square: Square) -> Square {
    Square::from_coords(square.file(), square.rank().flip_vertical())
}

fn flip_file(square: Square) -> Square {
    Square::from_coords(square.file().flip_horizontal(), square.rank())
}

fn pseudo_move(board: &[char; 64], encoding: u8) -> Option<(Square, Square, Option<Role>)> {
    let (_, piece, occurrence, forward, right) = MOVE_TABLE
        .iter()
        .find(|entry| entry.0 == encoding)
        .copied()?;
    let mut seen = 0u32;
    for y in 0..8 {
        for x in 0..8 {
            if board[(7 - y) * 8 + x] != piece {
                continue;
            }
            seen += 1;
            if seen != occurrence as u32 {
                continue;
            }
            let from = Square::from_coords(ChessFile::new(x as u32), shakmaty::Rank::new(y as u32));
            let to_x = (x as i32 + i32::from(right)).rem_euclid(8);
            let to_y = (y as i32 + i32::from(forward)).rem_euclid(8);
            let to = Square::from_coords(
                ChessFile::new(to_x as u32),
                shakmaty::Rank::new(to_y as u32),
            );
            return Some((from, to, None));
        }
    }
    None
}

fn be_u24(bytes: &[u8]) -> u32 {
    (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2])
}

fn encode_position(board: &[char; 64], ep: Option<Square>, pos: &Chess) -> Option<[u8; 32]> {
    let mut encoded = [0u8; 32];
    let mut bit_position = 8usize;

    // The board is stored file by file, rank by rank.  The caller has already
    // applied the black-to-move inversion and the horizontal mirror, so the
    // bytes here are in the book's canonical orientation.
    for file in 0..8 {
        for rank in 0..8 {
            let piece = board[(7 - rank) * 8 + file];
            let (bits, num_bits) = match piece {
                ' ' => (0x0, 1),
                'P' => (0x3, 3),
                'p' => (0x7, 3),
                'N' => (0x9, 5),
                'n' => (0x19, 5),
                'B' => (0x5, 5),
                'b' => (0x15, 5),
                'R' => (0xd, 5),
                'r' => (0x1d, 5),
                'Q' => (0x11, 6),
                'q' => (0x31, 6),
                'K' => (0x1, 6),
                'k' => (0x21, 6),
                _ => return None,
            };
            append_bits(&mut encoded, &mut bit_position, bits, num_bits);
        }
    }

    let rights = pos.castles().castling_rights();
    let invert = pos.turn() == Color::Black;
    let white = if invert { Color::Black } else { Color::White };
    let black = white.other();
    let mut castle = 0u8;
    if has_kingside(rights, white) {
        castle |= 4;
    }
    if has_queenside(rights, white) {
        castle |= 8;
    }
    if has_kingside(rights, black) {
        castle |= 1;
    }
    if has_queenside(rights, black) {
        castle |= 2;
    }

    let ep_file = ep.map(|square| square.file().to_u32());
    let mirrored_ep = ep_file.map(|file| if needs_flipping(pos) { 7 - file } else { file });
    let mut flag_bits = castle;
    let mut flag_len = if castle != 0 { 4 } else { 0 };
    if let Some(file) = mirrored_ep {
        flag_bits <<= 3;
        let mut file = file;
        for index in 0..3 {
            if file & 1 != 0 {
                flag_bits |= 1 << (2 - index);
            }
            file >>= 1;
        }
        flag_len += 3;
    }

    let remainder = bit_position % 8;
    if 8 - remainder < flag_len {
        let pad = 8 - remainder;
        append_bits(&mut encoded, &mut bit_position, 0, pad);
    }
    let mut pad = 8i32 - (bit_position % 8) as i32 - flag_len as i32;
    if pad < 0 {
        pad += 8;
    }
    append_bits(&mut encoded, &mut bit_position, 0, pad as usize);
    append_bits(&mut encoded, &mut bit_position, flag_bits, flag_len);

    let length = bit_position.div_ceil(8);
    if length > 32 {
        return None;
    }
    encoded[0] = length as u8;
    if ep_file.is_some() {
        encoded[0] |= 0x20;
    }
    if castle != 0 {
        encoded[0] |= 0x40;
    }
    Some(encoded)
}

fn append_bits(encoded: &mut [u8; 32], bit_position: &mut usize, bits: u8, num_bits: usize) {
    for index in 0..num_bits {
        if (bits >> index) & 1 != 0 {
            let byte = *bit_position / 8;
            let offset = *bit_position % 8;
            encoded[byte] |= 1 << (7 - offset);
        }
        *bit_position += 1;
    }
}

fn has_kingside(rights: shakmaty::Bitboard, color: Color) -> bool {
    rights.contains(match color {
        Color::White => Square::H1,
        Color::Black => Square::H8,
    })
}

fn has_queenside(rights: shakmaty::Bitboard, color: Color) -> bool {
    rights.contains(match color {
        Color::White => Square::A1,
        Color::Black => Square::A8,
    })
}

fn gen_position_hash(encoded: &[u8; 32]) -> Option<u32> {
    let mut hash: i32 = 0;
    let mut tmp: i16 = 0;
    let length = usize::from(encoded[0] & 0x1f);
    for &byte in encoded.iter().take(length) {
        let ch = i32::from(byte);
        tmp = tmp.wrapping_add((((0x0f - (ch & 0x0f)) << 2) + 1) as i16);
        hash = hash.wrapping_add(HASH_CODES[(tmp & 0x3f) as usize] as i32);
        tmp = tmp.wrapping_add((((0xf0 - (ch & 0xf0)) >> 2) + 1) as i16);
        hash = hash.wrapping_add(HASH_CODES[(tmp & 0x3f) as usize] as i32);
    }
    Some(hash as u32)
}

/// The 64 fixed position-hash constants used by the CTG format.
const HASH_CODES: [u32; 64] = [
    0x3100d2bf, 0x3118e3de, 0x34ab1372, 0x2807a847, 0x1633f566, 0x2143b359, 0x26d56488, 0x3b9e6f59,
    0x37755656, 0x3089ca7b, 0x18e92d85, 0x0cd0e9d8, 0x1a9e3b54, 0x3eaa902f, 0x0d9bfaae, 0x2f32b45b,
    0x31ed6102, 0x3d3c8398, 0x146660e3, 0x0f8d4b76, 0x02c77a5f, 0x146c8799, 0x1c47f51f, 0x249f8f36,
    0x24772043, 0x1fbc1e4d, 0x1e86b3fa, 0x37df36a6, 0x16ed30e4, 0x02c3148e, 0x216e5929, 0x0636b34e,
    0x317f9f56, 0x15f09d70, 0x131026fb, 0x38c784b1, 0x29ac3305, 0x2b485dc5, 0x3c049ddc, 0x35a9fbcd,
    0x31d5373b, 0x2b246799, 0x0a2923d3, 0x08a96e9d, 0x30031a9f, 0x08f525b5, 0x33611c06, 0x2409db98,
    0x0ca4feb2, 0x1000b71e, 0x30566e32, 0x39447d31, 0x194e3752, 0x08233a95, 0x0f38fe36, 0x29c7cd57,
    0x0f7b3a39, 0x328e8a16, 0x1e7d1388, 0x0fba78f5, 0x274c7e7c, 0x1e8be65c, 0x2fa0b0bb, 0x1eb6c371,
];

/// CTG move descriptors: encoding, piece, occurrence, forward, right.
///
/// These are format constants from the published CTG layout.  They describe
/// how a one-byte move code names a piece and a board displacement; the lookup
/// below then resolves the result against the legal moves of the position.
const MOVE_TABLE: &[(u8, char, i8, i8, i8)] = &[
    (0x00, 'P', 5, 1, 1),
    (0x01, 'N', 2, -1, -2),
    (0x03, 'Q', 2, 0, 2),
    (0x04, 'P', 2, 1, 0),
    (0x05, 'Q', 1, 1, 0),
    (0x06, 'P', 4, 1, -1),
    (0x08, 'Q', 2, 0, 4),
    (0x09, 'B', 2, 6, 6),
    (0x0a, 'K', 1, -1, 0),
    (0x0c, 'P', 1, 1, -1),
    (0x0d, 'B', 1, 3, 3),
    (0x0e, 'R', 2, 0, 3),
    (0x0f, 'N', 1, -1, -2),
    (0x12, 'B', 1, 7, 7),
    (0x13, 'K', 1, 1, 0),
    (0x14, 'P', 8, 1, 1),
    (0x15, 'B', 1, 5, 5),
    (0x18, 'P', 7, 1, 0),
    (0x1a, 'Q', 2, 6, 0),
    (0x1b, 'B', 1, 1, -1),
    (0x1d, 'B', 2, 7, 7),
    (0x21, 'R', 2, 0, 7),
    (0x22, 'B', 2, 2, -2),
    (0x23, 'Q', 2, 6, 6),
    (0x24, 'P', 8, 1, -1),
    (0x26, 'B', 1, 7, -7),
    (0x27, 'P', 3, 1, -1),
    (0x28, 'Q', 1, 5, 5),
    (0x29, 'Q', 1, 0, 6),
    (0x2a, 'N', 2, -2, 1),
    (0x2d, 'P', 6, 1, 1),
    (0x2e, 'B', 1, 1, 1),
    (0x2f, 'Q', 1, 0, 1),
    (0x30, 'N', 2, -2, -1),
    (0x31, 'Q', 1, 0, 3),
    (0x32, 'B', 2, 5, 5),
    (0x34, 'N', 1, 2, 1),
    (0x36, 'N', 1, 1, 2),
    (0x37, 'Q', 1, 4, 0),
    (0x38, 'Q', 2, 4, -4),
    (0x39, 'Q', 1, 0, 5),
    (0x3a, 'B', 1, 6, 6),
    (0x3b, 'Q', 2, 5, -5),
    (0x3c, 'B', 1, 5, -5),
    (0x41, 'Q', 2, 5, 5),
    (0x42, 'Q', 1, 7, -7),
    (0x44, 'K', 1, -1, 1),
    (0x45, 'Q', 1, 3, 3),
    (0x4a, 'P', 8, 2, 0),
    (0x4b, 'Q', 1, 5, -5),
    (0x4c, 'N', 2, 2, 1),
    (0x4d, 'Q', 2, 1, 0),
    (0x50, 'R', 1, 6, 0),
    (0x52, 'R', 1, 0, 6),
    (0x54, 'B', 2, 1, -1),
    (0x55, 'P', 3, 1, 0),
    (0x5c, 'P', 7, 1, 1),
    (0x5f, 'P', 5, 2, 0),
    (0x61, 'Q', 1, 6, 6),
    (0x62, 'P', 2, 2, 0),
    (0x63, 'Q', 2, 7, -7),
    (0x66, 'B', 1, 3, -3),
    (0x67, 'K', 1, 1, 1),
    (0x69, 'R', 2, 7, 0),
    (0x6a, 'B', 1, 4, 4),
    (0x6b, 'K', 1, 0, 2),
    (0x6e, 'R', 1, 0, 5),
    (0x6f, 'Q', 2, 7, 7),
    (0x72, 'B', 2, 7, -7),
    (0x74, 'Q', 1, 0, 2),
    (0x79, 'B', 2, 6, -6),
    (0x7a, 'R', 1, 3, 0),
    (0x7b, 'R', 2, 6, 0),
    (0x7c, 'P', 3, 1, 1),
    (0x7d, 'R', 2, 1, 0),
    (0x7e, 'Q', 1, 3, -3),
    (0x7f, 'R', 1, 0, 1),
    (0x80, 'Q', 1, 6, -6),
    (0x81, 'R', 1, 1, 0),
    (0x82, 'P', 6, 1, -1),
    (0x85, 'N', 1, 2, -1),
    (0x86, 'R', 1, 0, 7),
    (0x87, 'R', 1, 5, 0),
    (0x8a, 'N', 1, -2, 1),
    (0x8b, 'P', 1, 1, 1),
    (0x8c, 'K', 1, -1, -1),
    (0x8e, 'Q', 2, 2, -2),
    (0x8f, 'Q', 1, 0, 7),
    (0x92, 'Q', 2, 1, 1),
    (0x94, 'Q', 1, 3, 0),
    (0x96, 'P', 2, 1, 1),
    (0x97, 'K', 1, 0, -1),
    (0x98, 'R', 1, 0, 3),
    (0x99, 'R', 1, 4, 0),
    (0x9a, 'Q', 1, 6, 0),
    (0x9b, 'P', 3, 2, 0),
    (0x9d, 'Q', 1, 2, 0),
    (0x9f, 'B', 2, 4, -4),
    (0xa0, 'Q', 2, 3, 0),
    (0xa2, 'Q', 1, 2, 2),
    (0xa3, 'P', 8, 1, 0),
    (0xa5, 'R', 2, 5, 0),
    (0xa9, 'R', 2, 0, 2),
    (0xab, 'Q', 2, 6, -6),
    (0xad, 'R', 2, 0, 4),
    (0xae, 'Q', 2, 3, 3),
    (0xb0, 'Q', 2, 4, 0),
    (0xb1, 'P', 6, 2, 0),
    (0xb2, 'B', 1, 6, -6),
    (0xb5, 'R', 2, 0, 5),
    (0xb7, 'Q', 1, 5, 0),
    (0xb9, 'B', 2, 3, 3),
    (0xbb, 'P', 5, 1, 0),
    (0xbc, 'Q', 2, 0, 5),
    (0xbd, 'Q', 2, 2, 0),
    (0xbe, 'K', 1, 0, 1),
    (0xc1, 'B', 1, 2, 2),
    (0xc2, 'B', 2, 2, 2),
    (0xc3, 'B', 1, 2, -2),
    (0xc4, 'R', 2, 0, 1),
    (0xc5, 'R', 2, 4, 0),
    (0xc6, 'Q', 2, 5, 0),
    (0xc7, 'P', 7, 1, -1),
    (0xc8, 'P', 7, 2, 0),
    (0xc9, 'Q', 2, 7, 0),
    (0xca, 'B', 2, 3, -3),
    (0xcb, 'P', 6, 1, 0),
    (0xcc, 'B', 2, 5, -5),
    (0xcd, 'R', 1, 0, 2),
    (0xcf, 'P', 4, 1, 0),
    (0xd1, 'P', 2, 1, -1),
    (0xd2, 'N', 2, 1, 2),
    (0xd3, 'N', 2, 1, -2),
    (0xd7, 'Q', 1, 1, -1),
    (0xd8, 'R', 2, 0, 6),
    (0xd9, 'Q', 1, 2, -2),
    (0xda, 'N', 1, -2, -1),
    (0xdb, 'P', 1, 2, 0),
    (0xde, 'P', 5, 1, -1),
    (0xdf, 'K', 1, 1, -1),
    (0xe0, 'N', 2, -1, 2),
    (0xe1, 'R', 1, 7, 0),
    (0xe3, 'R', 2, 3, 0),
    (0xe5, 'Q', 1, 0, 4),
    (0xe6, 'P', 4, 2, 0),
    (0xe7, 'Q', 1, 4, 4),
    (0xe8, 'R', 1, 2, 0),
    (0xe9, 'N', 1, -1, 2),
    (0xeb, 'P', 4, 1, 1),
    (0xec, 'P', 1, 1, 0),
    (0xed, 'Q', 1, 7, 7),
    (0xee, 'Q', 2, 1, -1),
    (0xef, 'R', 1, 0, 4),
    (0xf0, 'Q', 2, 0, 7),
    (0xf1, 'Q', 1, 1, 1),
    (0xf3, 'N', 2, 2, -1),
    (0xf4, 'R', 2, 2, 0),
    (0xf5, 'B', 2, 1, 1),
    (0xf6, 'K', 1, 0, -2),
    (0xf7, 'N', 1, 1, -2),
    (0xf8, 'Q', 2, 0, 1),
    (0xf9, 'Q', 2, 6, 0),
    (0xfa, 'Q', 2, 0, 3),
    (0xfb, 'Q', 2, 2, 2),
    (0xfd, 'Q', 1, 7, 0),
    (0xfe, 'Q', 2, 3, -3),
];

#[cfg(test)]
mod tests {
    use super::*;
    use shakmaty::CastlingMode;

    #[test]
    fn polyglot_start_position_has_moves() {
        let path = std::env::var_os("BRETT_BOOK_SAMPLE");
        let Some(path) = path else {
            return;
        };
        let book = Book::open(path).unwrap();
        let moves = book.moves(&Chess::default()).unwrap();
        assert!(!moves.is_empty());
    }

    #[test]
    fn ctg_move_table_covers_known_encodings() {
        assert!(MOVE_TABLE.iter().any(|entry| entry.0 == 0x00));
        assert!(MOVE_TABLE.iter().any(|entry| entry.0 == 0xfe));
    }

    #[test]
    fn ctg_start_position_has_moves() {
        let path = std::env::var_os("BRETT_CTG_SAMPLE");
        let Some(path) = path else {
            return;
        };
        let book = Book::open(path).unwrap();
        let moves = book.moves(&Chess::default()).unwrap();
        assert!(
            !moves.is_empty(),
            "the sample CTG should cover the start position"
        );
        assert!(moves.iter().any(|mv| mv.games > 0));
        assert!(
            moves
                .iter()
                .any(|mv| { mv.mv.to_uci(shakmaty::CastlingMode::Standard).to_string() == "e2e4" })
        );
        assert!(
            moves
                .iter()
                .any(|mv| { mv.mv.to_uci(shakmaty::CastlingMode::Standard).to_string() == "d2d4" })
        );
    }

    #[test]
    fn abk_start_position_has_moves() {
        let path = std::env::var_os("BRETT_ABK_SAMPLE");
        let Some(path) = path else {
            return;
        };
        let book = Book::open(path).unwrap();
        let moves = book.moves(&Chess::default()).unwrap();
        assert!(
            !moves.is_empty(),
            "the sample ABK should cover the start position"
        );
        assert!(
            moves
                .iter()
                .any(|mv| { mv.mv.to_uci(shakmaty::CastlingMode::Standard).to_string() == "e2e4" })
        );
        assert!(
            moves
                .iter()
                .any(|mv| { mv.mv.to_uci(shakmaty::CastlingMode::Standard).to_string() == "d2d4" })
        );
        assert!(moves.iter().any(|mv| mv.games > 0));
    }

    #[test]
    fn bkt_start_position_has_moves() {
        let path = std::env::var_os("BRETT_BKT_SAMPLE");
        let Some(path) = path else {
            return;
        };
        let book = Book::open(path).unwrap();
        assert_eq!(book.format(), BookFormat::Bkt);
        let moves = book.moves(&Chess::default()).unwrap();
        assert!(
            !moves.is_empty(),
            "the sample BKT should cover the start position"
        );
        for uci in ["e2e4", "d2d4", "c2c4", "g1f3"] {
            assert!(
                moves
                    .iter()
                    .any(|mv| mv.mv.to_uci(CastlingMode::Standard).to_string() == uci),
                "missing {uci}"
            );
        }
        if let Some(e4) = moves
            .iter()
            .find(|mv| mv.mv.to_uci(CastlingMode::Standard).to_string() == "e2e4")
            && e4.games > 0
        {
            assert_eq!(e4.games, e4.white + e4.draw + e4.black);
            assert!(e4.white > 0 && e4.black > 0);
        }

        let mut pos = Chess::default();
        let e4 = shakmaty::uci::UciMove::from_ascii(b"e2e4")
            .unwrap()
            .to_move(&pos)
            .unwrap();
        pos.play_unchecked(e4);
        let replies = book.moves(&pos).unwrap();
        assert!(
            !replies.is_empty(),
            "the sample BKT should cover 1.e4 for Black"
        );
    }

    #[test]
    fn polyglot_decodes_common_opening_move() {
        let pos = Chess::default();
        // e2e4 in Polyglot encoding: to e4 (28) in the low bits, from e2 (12)
        // in the next six bits, no promotion.
        let raw = 28 | (12 << 6);
        let mv = decode_polyglot_move(&pos, raw).unwrap();
        assert_eq!(mv.to_uci(CastlingMode::Standard).to_string(), "e2e4");
    }
}
