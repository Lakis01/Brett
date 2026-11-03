//! Zero-allocation mainline move streamer for classic CBH (`.cbg` / `.cbj`).
//!
//! Unlike [`super::moves::decode_moves`], which constructs a full [`crate::domain::Game`]
//! AST with variations, comments, and annotations, this module decodes only the
//! mainline moves directly on a stack-allocated [`Chess`] position, emitting
//! `(position_key, move_code)` pairs to a caller-supplied closure.

use anyhow::{Context, Result, bail};

use super::moves::{PieceLists, from_stored_square, square};
use super::tables;
use crate::chess::{CastlingSide, Chess, Color, Move, Position, Role, Square};
use crate::position::encode_move;

/// Stream mainline moves from a raw CBH move stream without allocating game trees.
///
/// Calls `consumer(position_key, move_code)` for each mainline move up to `max_plies`.
pub fn stream_cbh_mainline<F>(
    start_pos: &Chess,
    mode: u8,
    stream: &[u8],
    chess960: bool,
    max_plies: u32,
    consumer: &mut F,
) -> Result<()>
where
    F: FnMut(u64, u32) + ?Sized,
{
    let (table, pre, simple) = match mode {
        0 => (&tables::MODE_0, true, false),
        4 => (&tables::MODE_4, false, false),
        5 => (&tables::MODE_5, false, true),
        10 => (&tables::MODE_10, false, false),
        other => bail!("move encoding mode {other} is not supported"),
    };
    let pieces = PieceLists::scan(start_pos).context("scanning start position")?;
    let mut walker = FastMainlineWalker {
        table,
        pre,
        simple,
        chess960,
        pieces,
        current_pos: start_pos.clone(),
        total: 0,
        plies: 0,
        max_plies,
        ended: false,
        consumer,
    };
    walker.walk(stream)
}

struct FastMainlineWalker<'a, F: ?Sized> {
    table: &'static [u8; 256],
    pre: bool,
    simple: bool,
    chess960: bool,
    pieces: PieceLists,
    current_pos: Chess,
    total: u32,
    plies: u32,
    max_plies: u32,
    ended: bool,
    consumer: &'a mut F,
}

impl<F: ?Sized + FnMut(u64, u32)> FastMainlineWalker<'_, F> {
    fn walk(&mut self, stream: &[u8]) -> Result<()> {
        if self.simple {
            self.simple_stream(stream)
        } else {
            self.compact_stream(stream)
        }
    }

    fn translate(&self, byte: u8) -> u8 {
        let n = self.total as u8;
        if self.pre {
            self.table[byte.wrapping_sub(n) as usize]
        } else {
            self.table[byte as usize].wrapping_sub(n)
        }
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
                // In CBH, 254 marks that the following move is a branch point with siblings
                // visited later. On the mainline, we simply continue forward.
                254 => {}
                // In CBH, 255 marks the leaf of the current line. The first line in the stream
                // is the mainline, so the first 255 terminates the mainline.
                255 => {
                    self.ended = true;
                    break;
                }
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
            match self.by_squares(word & 0x3fff, false)? {
                Some(mv) => self.play(mv, word)?,
                None => self.play_null()?,
            }
            if word & 0x4000 != 0 {
                // Leaf of the mainline line reached
                self.ended = true;
                break;
            }
        }
        Ok(())
    }

    fn play(&mut self, mv: Move, code: u16) -> Result<()> {
        if self.total >= 100_000 {
            bail!("move stream is implausibly long");
        }

        let key = self.current_pos.position_hash();
        if let Some(move_code) = encode_move(mv) {
            (self.consumer)(key, move_code);
        }
        self.plies += 1;

        let before = self.current_pos.clone();
        self.current_pos = before.clone().play(mv).map_err(|_| {
            anyhow::anyhow!(
                "illegal move {} (word {code:#06x}) at ply {}",
                mv,
                self.total + 1
            )
        })?;
        self.pieces
            .update(&before, mv)
            .with_context(|| format!("piece list at ply {}", self.total + 1))?;
        self.total += 1;
        if self.plies >= self.max_plies {
            self.ended = true;
        }
        Ok(())
    }

    fn play_null(&mut self) -> Result<()> {
        if self.total >= 100_000 {
            bail!("move stream is implausibly long");
        }
        self.current_pos = self
            .current_pos
            .clone()
            .swap_turn()
            .context("null move leaves the side to move in check")?;
        self.total += 1;
        if self.plies >= self.max_plies {
            self.ended = true;
        }
        Ok(())
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
        const Q: [(i8, i8); 8] = KING;
        const R: [(i8, i8); 4] = [(0, 1), (1, 0), (0, -1), (-1, 0)];
        const B: [(i8, i8); 4] = [(1, 1), (1, -1), (-1, -1), (-1, 1)];

        let step = |from: Square, dx: i8, dy: i8| -> Square {
            let f = from.file().to_u32() as i8 + dx;
            let r = from.rank().to_u32() as i8 + dy;
            if !(0..=7).contains(&f) || !(0..=7).contains(&r) {
                panic!("step runs off board: {from} + ({dx}, {dy})");
            }
            square(f as u8, r as u8)
        };
        let line = |offset: u8, dirs: &[(i8, i8)]| -> (i8, i8) {
            let n = dirs.len() as u8;
            let dir = dirs[(offset % n) as usize];
            let dist = (offset / n + 1) as i8;
            (dir.0 * dist, dir.1 * dist)
        };

        let us = self.current_pos.turn();
        let (kind, index, delta) = match code {
            1..=8 => (Role::King, 0, KING[(code - 1) as usize]),
            9..=16 => (Role::Knight, 0, KNIGHT[(code - 9) as usize]),
            17..=24 => (Role::Knight, 1, KNIGHT[(code - 17) as usize]),
            25..=52 => (Role::Queen, 0, line(code - 25, &Q)),
            53..=66 => (Role::Rook, 0, line(code - 53, &R)),
            67..=80 => (Role::Rook, 1, line(code - 67, &R)),
            81..=94 => (Role::Bishop, 0, line(code - 81, &B)),
            95..=108 => (Role::Bishop, 1, line(code - 95, &B)),
            109..=140 => {
                let p = (code - 109) / 4;
                let from = self
                    .pieces
                    .pawn(us, p as usize)
                    .with_context(|| format!("no pawn number {p}"))?;
                let dir: i8 = if us == Color::White { 1 } else { -1 };
                let (to, promo) = match (code - 109) % 4 {
                    0 => (step(from, 0, dir), None),
                    1 => (step(from, 0, 2 * dir), None),
                    2 => (step(from, -1, dir), None),
                    3 => (step(from, 1, dir), None),
                    _ => unreachable!(),
                };
                return self.ordinary(from, to, promo, u16::from(code));
            }
            141..=142 => {
                let to_cb = if code == 141 { 0x3e } else { 0x3a };
                let to = from_stored_square(to_cb);
                let back = if us == Color::White { 0 } else { 7 };
                let from = square(4, back);
                return self.ordinary(from, to, None, u16::from(code));
            }
            143..=170 => (Role::Queen, 1, line(code - 143, &Q)),
            171..=198 => (Role::Queen, 2, line(code - 171, &Q)),
            199..=212 => (Role::Rook, 2, line(code - 199, &R)),
            213..=226 => (Role::Bishop, 2, line(code - 213, &B)),
            227..=234 => (Role::Knight, 2, KNIGHT[(code - 227) as usize]),
            _ => bail!("code {code} is not a move"),
        };
        let kind_idx = match kind {
            Role::Queen => 0,
            Role::Rook => 1,
            Role::Bishop => 2,
            Role::Knight => 3,
            _ => 0,
        };
        let from = self
            .pieces
            .kind(us, kind_idx, index)
            .with_context(|| format!("no {kind:?} number {}", index + 1))?;
        let to = step(from, delta.0, delta.1);
        self.ordinary(from, to, None, u16::from(code))
    }

    fn by_squares(&mut self, word: u16, chess960: bool) -> Result<Option<Move>> {
        let from_cb = (word & 63) as u8;
        let to_cb = (word >> 6 & 63) as u8;
        let from = from_stored_square(from_cb);
        let to = from_stored_square(to_cb);
        let us = self.current_pos.turn();
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
            && self
                .current_pos
                .board()
                .piece_at(from)
                .is_some_and(|piece| piece.color == us && piece.role == Role::King);
        if castling && to.file().to_u32() == 6 {
            return self.castle(us, true, word).map(Some);
        }
        if castling && to.file().to_u32() == 2 {
            return self.castle(us, false, word).map(Some);
        }
        let piece = self
            .current_pos
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
        let piece = self
            .current_pos
            .board()
            .piece_at(from)
            .with_context(|| format!("no piece on {from} (word {word:#06x})"))?;
        if piece.color != self.current_pos.turn() {
            bail!("move starts from an opponent piece on {from} (word {word:#06x})");
        }
        if self.current_pos.us().contains(to) {
            bail!("the move {from}-{to} lands on a piece of the side to move (word {word:#06x})");
        }
        let mv = if piece.role == Role::Pawn && Some(to) == self.current_pos.maybe_ep_square() {
            Move::EnPassant { from, to }
        } else {
            Move::Normal {
                role: piece.role,
                from,
                capture: self
                    .current_pos
                    .board()
                    .piece_at(to)
                    .map(|piece| piece.role),
                to,
                promotion,
            }
        };
        Ok(mv)
    }

    fn castle(&self, us: Color, kingside: bool, word: u16) -> Result<Move> {
        let side = if kingside {
            CastlingSide::KingSide
        } else {
            CastlingSide::QueenSide
        };
        let king = self
            .current_pos
            .board()
            .king_of(us)
            .context("castling without a king")?;
        let rook = self
            .current_pos
            .castles()
            .rook(us, side)
            .with_context(|| format!("castling ({word:#06x}) without the right"))?;
        Ok(Move::Castle { king, rook })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Game;

    fn cb_square(square: &str) -> u8 {
        let bytes = square.as_bytes();
        (bytes[0] - b'a') * 8 + (bytes[1] - b'1')
    }

    fn move_word(from: &str, to: &str, promotion: u16) -> u16 {
        u16::from(cb_square(from)) | (u16::from(cb_square(to)) << 6) | (promotion << 12)
    }

    struct Compact {
        table: &'static [u8; 256],
        pre: bool,
        stream: Vec<u8>,
        ply: u32,
    }

    impl Compact {
        fn new(table: &'static [u8; 256], pre: bool) -> Self {
            Self {
                table,
                pre,
                stream: Vec::new(),
                ply: 0,
            }
        }

        fn encoded(&self, canonical: u8) -> u8 {
            let ply = self.ply as u8;
            for stored in 0u16..=255 {
                let index = stored as u8;
                let value = if self.pre {
                    self.table[index.wrapping_sub(ply) as usize]
                } else {
                    self.table[index as usize].wrapping_sub(ply)
                };
                if value == canonical {
                    return index;
                }
            }
            panic!("canonical byte {canonical} absent from table");
        }

        fn code(&mut self, canonical: u8) {
            let byte = self.encoded(canonical);
            self.stream.push(byte);
        }

        fn byte(&mut self, canonical: u8) {
            self.code(canonical);
        }

        fn mv(&mut self, from: &str, to: &str) {
            self.two_byte(move_word(from, to, 0));
        }

        fn two_byte(&mut self, word: u16) {
            self.code(235);
            self.byte((word >> 8) as u8);
            self.byte(word as u8);
            self.ply += 1;
        }

        fn start_variation(&mut self) {
            self.code(254);
        }

        fn end_line(&mut self) {
            self.code(255);
        }

        fn finish(mut self) -> Vec<u8> {
            self.end_line();
            self.stream
        }
    }

    #[test]
    fn stream_cbh_mainline_matches_decode_moves_on_compact_game() {
        let mut compact = Compact::new(&tables::MODE_0, true);
        compact.mv("e2", "e4");
        compact.mv("e7", "e5");
        compact.mv("g1", "f3");
        compact.start_variation();
        compact.mv("b8", "c6");
        compact.mv("f1", "b5");
        compact.end_line();
        compact.mv("d7", "d6");
        compact.mv("d2", "d4");
        let stream = compact.finish();

        let start_pos = Chess::default();
        let mut game = Game::default();
        crate::cbh::moves::decode_moves(&mut game, 0, &stream, false).unwrap();

        // Extract mainline from the decoded game AST
        let mut expected = Vec::new();
        let mut curr = 0;
        while let Some(&child) = game.nodes[curr].children.first() {
            let parent_pos = &game.nodes[curr].pos;
            let ply = game.nodes[child].mv.as_ref().unwrap();
            let ucimove = crate::chess::UciMove::from_ascii(ply.uci.as_bytes()).unwrap();
            let mv = ucimove.to_move(parent_pos).unwrap();
            expected.push((parent_pos.position_hash(), encode_move(mv).unwrap()));
            curr = child;
        }

        // Stream via zero-allocation fast streamer
        let mut streamed = Vec::new();
        stream_cbh_mainline(&start_pos, 0, &stream, false, u32::MAX, &mut |key, code| {
            streamed.push((key, code));
        })
        .unwrap();

        assert_eq!(streamed, expected);
        assert_eq!(streamed.len(), 5, "5 mainline moves (e4 e5 Nf3 Nc6 Bb5)");
    }

    #[test]
    fn stream_cbh_mainline_honours_max_plies() {
        let mut compact = Compact::new(&tables::MODE_0, true);
        compact.mv("e2", "e4");
        compact.mv("e7", "e5");
        compact.mv("g1", "f3");
        compact.mv("b8", "c6");
        let stream = compact.finish();

        let start_pos = Chess::default();
        let mut streamed = Vec::new();
        stream_cbh_mainline(&start_pos, 0, &stream, false, 2, &mut |key, code| {
            streamed.push((key, code));
        })
        .unwrap();

        assert_eq!(streamed.len(), 2, "must halt exactly at max_plies");
    }

    #[test]
    fn stream_cbh_mainline_handles_promotion() {
        use crate::chess::Fen;
        // Start position with pawn ready to promote on a7
        let fen: Fen = "8/P7/8/8/8/8/8/4K2k w - - 0 1".parse().unwrap();
        let start_pos: Chess = fen
            .into_position(crate::chess::CastlingMode::Standard)
            .unwrap();

        let mut compact = Compact::new(&tables::MODE_0, true);
        // a7-a8=Q
        compact.two_byte(move_word("a7", "a8", 1));
        let stream = compact.finish();

        let mut game = Game::default();
        game.nodes[0].pos = start_pos.clone();
        crate::cbh::moves::decode_moves(&mut game, 0, &stream, false).unwrap();

        let mut expected = Vec::new();
        let mut curr = 0;
        while let Some(&child) = game.nodes[curr].children.first() {
            let parent_pos = &game.nodes[curr].pos;
            let ply = game.nodes[child].mv.as_ref().unwrap();
            let ucimove = crate::chess::UciMove::from_ascii(ply.uci.as_bytes()).unwrap();
            let mv = ucimove.to_move(parent_pos).unwrap();
            expected.push((parent_pos.position_hash(), encode_move(mv).unwrap()));
            curr = child;
        }

        let mut streamed = Vec::new();
        stream_cbh_mainline(&start_pos, 0, &stream, false, u32::MAX, &mut |key, code| {
            streamed.push((key, code));
        })
        .unwrap();

        assert_eq!(streamed, expected);
        assert_eq!(streamed.len(), 1);
    }
}
