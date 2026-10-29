//! Decode a classic ChessBase `.cbg` move record into Brett's game tree.
//!
//! The record starts with a flags byte (low six bits are the encoding mode,
//! bit 6 an explicit start, bit 7 a guiding text) and a 24-bit size, followed
//! by an optional 28-byte start section and a mode-specific move stream.
//! This walker follows the stored depth-first order: every move is appended
//! under the current node, a variation marker saves the parent of the next
//! move, and an end marker restores it.  Keeping every node, without
//! de-duplicating equal moves, is important because `.cba` annotation
//! positions are indices into exactly that stored order.

use std::num::NonZeroU32;

use crate::chess::{
    Bitboard, Board, CastlingMode, CastlingSide, Chess, Color, EnPassantMode, Fen, File, Move,
    Piece, Position, Rank, Role, SanPlus, Setup, Square, UciMove,
};
use anyhow::{Context, Result, bail};

use super::tables;
use crate::domain::{Game, Node, Ply};

/// The decoded start of one game.
pub(super) struct Start {
    pub pos: Chess,
    pub chess960: bool,
    pub custom: bool,
    pub fen: String,
}

/// Decode the optional start section.  Returns the start and the number of
/// body bytes it consumed (the body starts after the four-byte record head).
pub(super) fn decode_start(flags: u8, body: &[u8]) -> Result<(Start, usize)> {
    let mode = flags & 0x3f;
    let explicit = flags & 0x40 != 0;
    let chess960 = mode == 10 || mode == 11;
    if chess960 && !explicit {
        bail!("Chess960 game has no explicit start position");
    }
    if !explicit {
        return Ok((
            Start {
                pos: Chess::default(),
                chess960: false,
                custom: false,
                fen: String::new(),
            },
            0,
        ));
    }
    let start = body
        .get(..28)
        .context("explicit start position is truncated")?;
    let extra = if chess960 {
        Some(
            body.get(28..36)
                .context("Chess960 start squares are truncated")?,
        )
    } else {
        None
    };
    let pos = start_position(start, extra, chess960)?;
    let fen = fen_tag(&pos, start[3]);
    Ok((
        Start {
            pos,
            chess960,
            custom: true,
            fen,
        },
        28 + extra.map_or(0, |_| 8),
    ))
}

fn fen_tag(pos: &Chess, stored_move_number: u8) -> String {
    let mut fen = Fen::from_position(pos, EnPassantMode::Legal).to_string();
    if stored_move_number == 0
        && let Some(stripped) = fen.strip_suffix(" 1")
    {
        fen = format!("{stripped} 0");
    }
    fen
}

fn start_position(start: &[u8], extra: Option<&[u8]>, chess960: bool) -> Result<Chess> {
    let pieces = decode_board(&start[4..28])?;
    let turn = if start[1] & 0x10 != 0 {
        Color::Black
    } else {
        Color::White
    };
    let castling = start[2] & 0x0f;
    let move_number = start[3];
    let ep = u16::from(start[1] & 0x0f);
    let (named_kings, named_rooks) = match extra {
        Some(bytes) => named_squares(bytes),
        None => ([None; 2], [None; 4]),
    };

    let mut board = Board::empty();
    for (square, piece) in pieces.iter().enumerate() {
        if let Some((color, role)) = piece {
            board.set_piece_at(
                Square::new(square as u32),
                Piece {
                    color: *color,
                    role: *role,
                },
            );
        }
    }

    let mut rights = Bitboard::EMPTY;
    for (color, kingside, bit, index) in [
        (Color::White, false, 1u8, 0usize),
        (Color::White, true, 2, 1),
        (Color::Black, false, 4, 2),
        (Color::Black, true, 8, 3),
    ] {
        if castling & bit != 0
            && let Some(file) = rook_file(
                &pieces,
                color,
                kingside,
                named_kings[color_index(color)],
                named_rooks[index],
                chess960,
            )
        {
            rights |=
                Bitboard::from_square(square(file, if color == Color::White { 0 } else { 7 }));
        }
    }

    let ep_square = usable_ep(&pieces, turn, (1..=8).contains(&ep).then(|| (ep - 1) as u8));
    let mode = if chess960 {
        CastlingMode::Chess960
    } else {
        CastlingMode::Standard
    };
    let setup = Setup {
        board,
        turn,
        castling_rights: rights,
        ep_square,
        halfmoves: 0,
        fullmoves: NonZeroU32::new(u32::from(move_number).max(1)).unwrap(),
    };
    Chess::from_setup(setup, mode)
        .map_err(|error| anyhow::anyhow!("illegal start position: {error}"))
}

fn color_index(color: Color) -> usize {
    match color {
        Color::White => 0,
        Color::Black => 1,
    }
}

fn decode_board(bits: &[u8]) -> Result<[Option<(Color, Role)>; 64]> {
    if bits.len() < 24 {
        bail!("start board is shorter than 24 bytes");
    }
    let mut board = [None; 64];
    let mut pos = 0usize;
    let mut bit = || -> Result<u8> {
        let byte = *bits
            .get(pos / 8)
            .context("start board runs past its 24 bytes")?;
        let value = byte >> (7 - pos % 8) & 1;
        pos += 1;
        Ok(value)
    };
    for cb in 0..64u8 {
        if bit()? == 0 {
            continue;
        }
        let color = if bit()? == 0 {
            Color::White
        } else {
            Color::Black
        };
        let code = (bit()? << 2) | (bit()? << 1) | bit()?;
        let role = match code {
            1 => Role::King,
            2 => Role::Queen,
            3 => Role::Knight,
            4 => Role::Bishop,
            5 => Role::Rook,
            6 => Role::Pawn,
            _ => bail!("start board has piece code {code}"),
        };
        // The stored square order is file-major (`a1`, `a2`, ...); Shakmaty
        // indices are rank-major (`a1`, `b1`, ...).
        let square = Square::new(u32::from((cb % 8) * 8 + cb / 8));
        board[usize::from(square)] = Some((color, role));
    }
    Ok(board)
}

fn named_squares(extra: &[u8]) -> ([Option<u8>; 2], [Option<u8>; 4]) {
    let file = |index: usize, rank: u8| {
        (extra[index] < 64 && extra[index] % 8 == rank).then_some(extra[index] / 8)
    };
    (
        [file(0, 0), file(1, 7)],
        [file(3, 0), file(2, 0), file(5, 7), file(4, 7)],
    )
}

fn rook_file(
    pieces: &[Option<(Color, Role)>; 64],
    color: Color,
    kingside: bool,
    named_king: Option<u8>,
    named_rook: Option<u8>,
    chess960: bool,
) -> Option<u8> {
    let rank = if color == Color::White { 0 } else { 7 };
    let at = |file: u8| pieces[usize::from(square(file, rank))];
    let king_file = (0..8u8).find(|&file| at(file) == Some((color, Role::King)))?;
    if named_king.is_some_and(|named| named != king_file) {
        return None;
    }
    let wing_rook =
        |file: u8| at(file) == Some((color, Role::Rook)) && (file > king_file) == kingside;
    if let Some(named) = named_rook
        && wing_rook(named)
    {
        return Some(named);
    }
    if chess960 {
        let files = (0..8u8).filter(|&file| wing_rook(file));
        return if kingside { files.max() } else { files.min() };
    }
    let home = if kingside { 7 } else { 0 };
    (king_file == 4 && wing_rook(home)).then_some(home)
}

fn usable_ep(
    pieces: &[Option<(Color, Role)>; 64],
    turn: Color,
    file: Option<u8>,
) -> Option<Square> {
    let file = file?;
    let (pawn_rank, our_rank, ep_rank, them) = match turn {
        Color::White => (4u8, 4u8, 5u8, Color::Black),
        Color::Black => (3, 3, 2, Color::White),
    };
    let at = |file: i8, rank: u8| {
        if !(0..8).contains(&file) {
            return None;
        }
        pieces[usize::from(square(file as u8, rank))]
    };
    if at(file as i8, pawn_rank) != Some((them, Role::Pawn)) {
        return None;
    }
    let neighbours = [at(file as i8 - 1, our_rank), at(file as i8 + 1, our_rank)];
    neighbours
        .contains(&Some((turn, Role::Pawn)))
        .then(|| square(file, ep_rank))
}

/// Decode the move stream of one game.  The caller has already installed the
/// start position in `game.nodes[0].pos`.
pub(super) fn decode_moves(game: &mut Game, mode: u8, stream: &[u8], chess960: bool) -> Result<()> {
    let (table, pre, simple) = match mode {
        0 => (&tables::MODE_0, true, false),
        4 => (&tables::MODE_4, false, false),
        5 => (&tables::MODE_5, false, true),
        10 => (&tables::MODE_10, false, false),
        other => bail!("move encoding mode {other} is not supported"),
    };
    let pieces = PieceLists::scan(&game.nodes[0].pos).context("scanning start position")?;
    let mut walker = Walker {
        game,
        table,
        pre,
        simple,
        chess960,
        pieces,
        current: 0,
        stack: Vec::new(),
        total: 0,
        branch_next: false,
        ended: false,
    };
    walker.walk(stream)
}

struct Walker<'a> {
    game: &'a mut Game,
    table: &'static [u8; 256],
    pre: bool,
    simple: bool,
    chess960: bool,
    pieces: PieceLists,
    current: usize,
    stack: Vec<(usize, PieceLists)>,
    total: u32,
    branch_next: bool,
    ended: bool,
}

impl Walker<'_> {
    fn walk(&mut self, stream: &[u8]) -> Result<()> {
        if self.simple {
            self.simple_stream(stream)
        } else {
            self.compact_stream(stream)
        }
        .and_then(|()| {
            if !self.ended && !stream.is_empty() {
                bail!("move stream: tree not terminated");
            }
            Ok(())
        })
    }

    fn translate(&self, byte: u8) -> u8 {
        let n = self.total as u8;
        if self.pre {
            self.table[byte.wrapping_sub(n) as usize]
        } else {
            self.table[byte as usize].wrapping_sub(n)
        }
    }

    fn start_variation(&mut self) -> Result<()> {
        if self.branch_next {
            bail!("two variation starts in a row");
        }
        if self.stack.len() >= 1024 {
            bail!("variations nested deeper than 1024");
        }
        self.branch_next = true;
        Ok(())
    }

    fn end_line(&mut self) -> Result<()> {
        if self.branch_next {
            bail!("a variation start before the end of a line");
        }
        match self.stack.pop() {
            Some((parent, pieces)) => {
                self.current = parent;
                self.pieces = pieces;
            }
            None => self.ended = true,
        }
        Ok(())
    }

    fn compact_stream(&mut self, stream: &[u8]) -> Result<()> {
        let mut i = 0usize;
        while i < stream.len() {
            let value = self.translate(stream[i]);
            i += 1;
            if value == 236 {
                continue;
            }
            if self.ended {
                break;
            }
            match value {
                254 => self.start_variation()?,
                255 => self.end_line()?,
                235 => {
                    let pair = stream
                        .get(i..i + 2)
                        .context("a two-byte move runs past the record")?;
                    let word = u16::from(self.translate(pair[0])) << 8
                        | u16::from(self.translate(pair[1]));
                    i += 2;
                    match self.by_squares(word, self.chess960)? {
                        Some(mv) => self.play(mv, word)?,
                        None => self.play_null()?,
                    }
                }
                237..=253 => bail!("code {value} is not used"),
                0 => self.play_null()?,
                code => {
                    let mv = self.by_code(code)?;
                    self.play(mv, u16::from(code))?;
                }
            }
        }
        Ok(())
    }

    fn simple_stream(&mut self, stream: &[u8]) -> Result<()> {
        let pairs = stream.len() / 2;
        for index in 0..pairs {
            if self.ended {
                break;
            }
            let pair = &stream[index * 2..index * 2 + 2];
            let word = u16::from(self.translate(pair[0])) << 8 | u16::from(self.translate(pair[1]));
            if word & 0x8000 != 0 {
                self.start_variation()?;
            }
            match self.by_squares(word & 0x3fff, false)? {
                Some(mv) => self.play(mv, word)?,
                None => self.play_null()?,
            }
            if word & 0x4000 != 0 {
                self.end_line()?;
            }
        }
        if !stream.len().is_multiple_of(2) && !self.ended {
            bail!("move stream: odd length for two-byte moves");
        }
        Ok(())
    }

    fn play(&mut self, mv: Move, code: u16) -> Result<()> {
        if self.total >= 100_000 {
            bail!("move stream is implausibly long");
        }
        let saved = if self.branch_next {
            Some((self.current, self.pieces))
        } else {
            None
        };
        self.branch_next = false;
        let before = self.game.nodes[self.current].pos.clone();
        let after = before.clone().play(mv).map_err(|_| {
            anyhow::anyhow!(
                "illegal move {} (word {code:#06x}) at ply {}",
                mv,
                self.total + 1
            )
        })?;
        let uci_mode = if self.chess960 {
            CastlingMode::Chess960
        } else {
            CastlingMode::Standard
        };
        let ply = Ply {
            san: SanPlus::from_move(before.clone(), mv).to_string(),
            uci: UciMove::from_move(mv, uci_mode).to_string(),
            number: before.fullmoves().get(),
            color: before.turn(),
        };
        self.pieces
            .update(&before, mv)
            .with_context(|| format!("piece list at ply {}", self.total + 1))?;
        let id = self.append(ply, after);
        self.current = id;
        self.total += 1;
        if let Some(saved) = saved {
            self.stack.push(saved);
        }
        Ok(())
    }

    fn play_null(&mut self) -> Result<()> {
        if self.total >= 100_000 {
            bail!("move stream is implausibly long");
        }
        let saved = if self.branch_next {
            Some((self.current, self.pieces))
        } else {
            None
        };
        self.branch_next = false;
        let before = self.game.nodes[self.current].pos.clone();
        let after = before
            .clone()
            .swap_turn()
            .context("null move leaves the side to move in check")?;
        let ply = Ply {
            san: "--".to_string(),
            uci: "0000".to_string(),
            number: before.fullmoves().get(),
            color: before.turn(),
        };
        let id = self.append(ply, after);
        self.current = id;
        self.total += 1;
        if let Some(saved) = saved {
            self.stack.push(saved);
        }
        Ok(())
    }

    fn append(&mut self, ply: Ply, after: Chess) -> usize {
        let id = self.game.nodes.len();
        self.game.nodes.push(Node {
            mv: Some(ply),
            comment: String::new(),
            clock: None,
            eval: None,
            nags: Vec::new(),
            parent: Some(self.current),
            children: Vec::new(),
            pos: after,
        });
        self.game.nodes[self.current].children.push(id);
        id
    }

    fn by_code(&mut self, code: u8) -> Result<Move> {
        const KING: [(i8, i8); 8] = [
            (0, 1),
            (1, 1),
            (1, 0),
            (1, -1),
            (0, -1),
            (-1, -1),
            (-1, 0),
            (-1, 1),
        ];
        const KNIGHT: [(i8, i8); 8] = [
            (2, 1),
            (1, 2),
            (-1, 2),
            (-2, 1),
            (-2, -1),
            (-1, -2),
            (1, -2),
            (2, -1),
        ];
        const Q: [(i8, i8); 4] = [(0, 1), (1, 0), (1, 1), (1, -1)];
        const R: [(i8, i8); 2] = [(0, 1), (1, 0)];
        const B: [(i8, i8); 2] = [(1, 1), (1, -1)];
        let us = self.game.nodes[self.current].pos.turn();
        let line = |k: u8, dirs: &[(i8, i8)]| {
            let (file_delta, rank_delta) = dirs[(k / 7) as usize];
            let steps = (k % 7 + 1) as i8;
            (file_delta * steps, rank_delta * steps)
        };
        let (kind, index, delta) = match code {
            1..=8 => {
                let from = self.game.nodes[self.current]
                    .pos
                    .board()
                    .king_of(us)
                    .context("one-byte king move without a king")?;
                let (df, dr) = KING[(code - 1) as usize];
                return self.ordinary(from, step(from, df, dr), None, u16::from(code));
            }
            9 => return self.castle(us, true, u16::from(code)),
            10 => return self.castle(us, false, u16::from(code)),
            11..=38 => (0, 0, line(code - 11, &Q)),
            39..=52 => (1, 0, line(code - 39, &R)),
            53..=66 => (1, 1, line(code - 53, &R)),
            67..=80 => (2, 0, line(code - 67, &B)),
            81..=94 => (2, 1, line(code - 81, &B)),
            95..=102 => (3, 0, KNIGHT[(code - 95) as usize]),
            103..=110 => (3, 1, KNIGHT[(code - 103) as usize]),
            111..=142 => {
                let (pawn, how) = ((code - 111) / 4, (code - 111) % 4);
                let from = self
                    .pieces
                    .pawn(us, pawn.into())
                    .with_context(|| format!("no pawn number {pawn} for {us:?}"))?;
                let forward: i8 = if us == Color::White { 1 } else { -1 };
                let (df, dr) = [
                    (0, forward),
                    (0, 2 * forward),
                    (forward, forward),
                    (-forward, forward),
                ][how as usize];
                let to = step(from, df, dr);
                if to.rank().to_u32() == 0 || to.rank().to_u32() == 7 {
                    bail!("a promotion in a one-byte move");
                }
                return self.ordinary(from, to, None, u16::from(code));
            }
            143..=170 => (0, 1, line(code - 143, &Q)),
            171..=198 => (0, 2, line(code - 171, &Q)),
            199..=212 => (1, 2, line(code - 199, &R)),
            213..=226 => (2, 2, line(code - 213, &B)),
            227..=234 => (3, 2, KNIGHT[(code - 227) as usize]),
            _ => bail!("code {code} is not a move"),
        };
        let from = self
            .pieces
            .kind(us, kind, index)
            .with_context(|| format!("no {kind} number {}", index + 1))?;
        let to = step(from, delta.0, delta.1);
        self.ordinary(from, to, None, u16::from(code))
    }

    fn by_squares(&mut self, word: u16, chess960: bool) -> Result<Option<Move>> {
        let from_cb = (word & 63) as u8;
        let to_cb = (word >> 6 & 63) as u8;
        let from = from_stored_square(from_cb);
        let to = from_stored_square(to_cb);
        let pos = &self.game.nodes[self.current].pos;
        let us = pos.turn();
        let back = if us == Color::White { 0 } else { 7 };
        if from_cb == to_cb {
            if word & 0x0fff == 0 {
                return Ok(None);
            }
            if chess960 && to.rank().to_u32() == u32::from(back) && to.file().to_u32() == 6 {
                return self.castle(us, true, word).map(Some);
            }
            if chess960 && to.rank().to_u32() == u32::from(back) && to.file().to_u32() == 2 {
                return self.castle(us, false, word).map(Some);
            }
            bail!("a move from {from} to itself");
        }
        let castling = !chess960
            && from == square(4, back)
            && to.rank().to_u32() == u32::from(back)
            && pos
                .board()
                .piece_at(from)
                .is_some_and(|piece| piece.color == us && piece.role == Role::King);
        if castling && to.file().to_u32() == 6 {
            return self.castle(us, true, word).map(Some);
        }
        if castling && to.file().to_u32() == 2 {
            return self.castle(us, false, word).map(Some);
        }
        let piece = pos
            .board()
            .piece_at(from)
            .with_context(|| format!("no piece on {from}"))?;
        if piece.color != us {
            bail!("move starts from an opponent piece on {from}");
        }
        let promotion = if piece.role == Role::Pawn
            && (to.rank().to_u32() == 0 || to.rank().to_u32() == 7)
        {
            Some([Role::Queen, Role::Rook, Role::Bishop, Role::Knight][(word >> 12 & 3) as usize])
        } else {
            None
        };
        self.ordinary(from, to, promotion, word).map(Some)
    }

    fn ordinary(
        &self,
        from: Square,
        to: Square,
        promotion: Option<Role>,
        word: u16,
    ) -> Result<Move> {
        let pos = &self.game.nodes[self.current].pos;
        let piece = pos
            .board()
            .piece_at(from)
            .with_context(|| format!("no piece on {from} (word {word:#06x})"))?;
        if piece.color != pos.turn() {
            bail!("move starts from an opponent piece on {from} (word {word:#06x})");
        }
        if pos.us().contains(to) {
            bail!("the move {from}-{to} lands on a piece of the side to move (word {word:#06x})");
        }
        let mv = if piece.role == Role::Pawn && Some(to) == pos.maybe_ep_square() {
            Move::EnPassant { from, to }
        } else {
            Move::Normal {
                role: piece.role,
                from,
                capture: pos.board().piece_at(to).map(|piece| piece.role),
                to,
                promotion,
            }
        };
        Ok(mv)
    }

    fn castle(&self, us: Color, kingside: bool, word: u16) -> Result<Move> {
        let pos = &self.game.nodes[self.current].pos;
        let side = if kingside {
            CastlingSide::KingSide
        } else {
            CastlingSide::QueenSide
        };
        let king = pos.board().king_of(us).context("castling without a king")?;
        let rook = pos
            .castles()
            .rook(us, side)
            .with_context(|| format!("castling ({word:#06x}) without the right"))?;
        Ok(Move::Castle { king, rook })
    }
}

pub(super) fn from_stored_square(cb: u8) -> Square {
    Square::new(u32::from((cb % 8) * 8 + cb / 8))
}

pub(super) fn to_stored_square(square: Square) -> u8 {
    (square.file().to_u32() as u8) * 8 + (square.rank().to_u32() as u8)
}

pub(super) fn square(file: u8, rank: u8) -> Square {
    Square::from_coords(File::new(u32::from(file)), Rank::new(u32::from(rank)))
}

fn step(from: Square, file_delta: i8, rank_delta: i8) -> Square {
    let file = ((from.file().to_u32() as i8 + file_delta) & 7) as u8;
    let rank = ((from.rank().to_u32() as i8 + rank_delta) & 7) as u8;
    square(file, rank)
}

/// Piece lists used by the compact encoding.  Queens, rooks, bishops and
/// knights are numbered in the order a file-major scan of the start position
/// finds them; pawns keep fixed numbers.  Captures shift the higher numbers
/// down and promotions append to the promoted kind.
#[derive(Clone, Copy)]
pub(super) struct PieceLists {
    kinds: [[KindList; 4]; 2],
    pawns: [[Option<Square>; 8]; 2],
}

#[derive(Clone, Copy, Default)]
struct KindList {
    squares: [Option<Square>; 10],
    len: usize,
}

impl KindList {
    fn push(&mut self, square: Square) -> bool {
        if self.len >= self.squares.len() {
            return false;
        }
        self.squares[self.len] = Some(square);
        self.len += 1;
        true
    }

    fn get(&self, index: usize) -> Option<Square> {
        (index < self.len).then(|| self.squares[index]).flatten()
    }

    fn position(&self, square: Square) -> Option<usize> {
        (0..self.len).find(|&index| self.squares[index] == Some(square))
    }

    fn remove(&mut self, square: Square) -> bool {
        let Some(index) = self.position(square) else {
            return false;
        };
        self.squares.copy_within(index + 1..self.len, index);
        self.len -= 1;
        self.squares[self.len] = None;
        true
    }

    fn relocate(&mut self, from: Square, to: Square) -> bool {
        let Some(index) = self.position(from) else {
            return false;
        };
        self.squares[index] = Some(to);
        true
    }
}

impl PieceLists {
    pub(super) fn scan(pos: &Chess) -> Result<Self> {
        let mut pieces = Self {
            kinds: [[KindList::default(); 4]; 2],
            pawns: [[None; 8]; 2],
        };
        let mut next_pawn = [0usize; 2];
        for cb in 0..64u8 {
            let square = from_stored_square(cb);
            let Some(piece) = pos.board().piece_at(square) else {
                continue;
            };
            let side = color_index(piece.color);
            if piece.role == Role::Pawn {
                let slot = pieces.pawns[side]
                    .get_mut(next_pawn[side])
                    .context("more than eight pawns in start position")?;
                *slot = Some(square);
                next_pawn[side] += 1;
            } else if let Some(kind) = kind_index(piece.role)
                && !pieces.kinds[side][kind].push(square)
            {
                bail!("more than ten pieces of one kind in start position");
            }
        }
        Ok(pieces)
    }

    pub(super) fn kind(&self, color: Color, kind: usize, index: usize) -> Option<Square> {
        self.kinds[color_index(color)][kind].get(index)
    }

    pub(super) fn pawn(&self, color: Color, index: usize) -> Option<Square> {
        self.pawns[color_index(color)].get(index).copied().flatten()
    }

    pub(super) fn kind_position(&self, color: Color, kind: usize, square: Square) -> Option<usize> {
        self.kinds[color_index(color)][kind].position(square)
    }

    pub(super) fn pawn_position(&self, color: Color, square: Square) -> Option<usize> {
        self.pawns[color_index(color)]
            .iter()
            .position(|&slot| slot == Some(square))
    }

    /// Encode `mv` as a one-byte compact code (`1..=234`), or `None` when the
    /// move must be written with the `235` two-byte escape (promotions and
    /// fourth-or-later pieces of one kind).
    pub(super) fn compact_code(&self, before: &Chess, mv: Move) -> Option<u8> {
        const KING: [(i8, i8); 8] = [
            (0, 1),
            (1, 1),
            (1, 0),
            (1, -1),
            (0, -1),
            (-1, -1),
            (-1, 0),
            (-1, 1),
        ];
        const KNIGHT: [(i8, i8); 8] = [
            (2, 1),
            (1, 2),
            (-1, 2),
            (-2, 1),
            (-2, -1),
            (-1, -2),
            (1, -2),
            (2, -1),
        ];
        const Q: [(i8, i8); 4] = [(0, 1), (1, 0), (1, 1), (1, -1)];
        const R: [(i8, i8); 2] = [(0, 1), (1, 0)];
        const B: [(i8, i8); 2] = [(1, 1), (1, -1)];

        if let Move::Castle { king, rook } = mv {
            return Some(if rook > king { 9 } else { 10 });
        }
        if mv.promotion().is_some() {
            return None;
        }
        let from = mv.from()?;
        let to = mv.to();
        let us = before.turn();
        let piece = before.board().piece_at(from)?;
        if piece.color != us {
            return None;
        }
        let line = |k: u8, dirs: &[(i8, i8)]| {
            let (file_delta, rank_delta) = dirs[(k / 7) as usize];
            let steps = (k % 7 + 1) as i8;
            (file_delta * steps, rank_delta * steps)
        };
        match piece.role {
            Role::King => (0..8u8)
                .find(|&k| step(from, KING[k as usize].0, KING[k as usize].1) == to)
                .map(|k| k + 1),
            Role::Pawn => {
                if to.rank().to_u32() == 0 || to.rank().to_u32() == 7 {
                    return None;
                }
                let pawn = self.pawn_position(us, from)? as u8;
                let forward: i8 = if us == Color::White { 1 } else { -1 };
                let deltas = [
                    (0, forward),
                    (0, 2 * forward),
                    (forward, forward),
                    (-forward, forward),
                ];
                let how = (0..4u8)
                    .find(|&h| step(from, deltas[h as usize].0, deltas[h as usize].1) == to)?;
                Some(111 + pawn * 4 + how)
            }
            role => {
                let kind = kind_index(role)?;
                let index = self.kind_position(us, kind, from)?;
                match (kind, index) {
                    (0, 0..=2) => {
                        let base = [11u8, 143, 171][index];
                        let k = (0..28u8).find(|&k| {
                            let (df, dr) = line(k, &Q);
                            step(from, df, dr) == to
                        })?;
                        Some(base + k)
                    }
                    (1, 0..=2) => {
                        let base = [39u8, 53, 199][index];
                        let k = (0..14u8).find(|&k| {
                            let (df, dr) = line(k, &R);
                            step(from, df, dr) == to
                        })?;
                        Some(base + k)
                    }
                    (2, 0..=2) => {
                        let base = [67u8, 81, 213][index];
                        let k = (0..14u8).find(|&k| {
                            let (df, dr) = line(k, &B);
                            step(from, df, dr) == to
                        })?;
                        Some(base + k)
                    }
                    (3, 0..=2) => {
                        let base = [95u8, 103, 227][index];
                        let k = (0..8u8).find(|&k| {
                            step(from, KNIGHT[k as usize].0, KNIGHT[k as usize].1) == to
                        })?;
                        Some(base + k)
                    }
                    _ => None,
                }
            }
        }
    }

    pub(super) fn update(&mut self, before: &Chess, mv: Move) -> Result<()> {
        let us = before.turn();
        let me = color_index(us);
        let them = color_index(us.other());
        if let Move::Castle { king, rook } = mv {
            let file = if rook > king { 5 } else { 3 };
            let rook_to = square(file, rook.rank().to_u32() as u8);
            if self.kinds[me][1].relocate(rook, rook_to) {
                return Ok(());
            }
            bail!("castling rook is missing from the piece list");
        }
        let from = mv.from().context("piece drop in a classic CBH game")?;
        let to = mv.to();
        let moving = before.board().piece_at(from).map(|piece| piece.role);
        let target = before.board().piece_at(to);
        let captured = match target {
            Some(piece) if piece.color != us => Some((piece.role, to)),
            _ if moving == Some(Role::Pawn) && from.file() != to.file() => {
                Some((Role::Pawn, Square::from_coords(to.file(), from.rank())))
            }
            _ => None,
        };
        if let Some((role, square)) = captured {
            let removed = match kind_index(role) {
                Some(kind) => self.kinds[them][kind].remove(square),
                None => self.pawns[them]
                    .iter_mut()
                    .find(|slot| **slot == Some(square))
                    .is_some_and(|slot| {
                        *slot = None;
                        true
                    }),
            };
            if !removed && role != Role::King {
                bail!("captured piece is missing from the piece list");
            }
        }
        match moving {
            Some(Role::Pawn) => {
                let slot = self.pawns[me]
                    .iter_mut()
                    .find(|slot| **slot == Some(from))
                    .context("moving pawn is missing from the piece list")?;
                match mv.promotion() {
                    Some(role) => {
                        *slot = None;
                        let kind = kind_index(role).context("invalid promotion role")?;
                        if !self.kinds[me][kind].push(to) {
                            bail!("too many promoted pieces");
                        }
                    }
                    None => *slot = Some(to),
                }
            }
            Some(Role::King) => {}
            Some(role) => {
                let kind =
                    kind_index(role).with_context(|| format!("unexpected moving role {role:?}"))?;
                if !self.kinds[me][kind].relocate(from, to) {
                    bail!("moving piece is missing from the piece list");
                }
            }
            None => bail!("move has no moving piece"),
        }
        Ok(())
    }
}

fn kind_index(role: Role) -> Option<usize> {
    match role {
        Role::Queen => Some(0),
        Role::Rook => Some(1),
        Role::Bishop => Some(2),
        Role::Knight => Some(3),
        _ => None,
    }
}
