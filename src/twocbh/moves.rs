//! Decode 2CBH move tokens into Brett's game tree.
//!
//! The token stream is a sequence of little-endian `u16` tokens.  `0xffff`
//! terminates a line, `0xfffd` marks the move that a following variation
//! replaces, and `0xfffc`/`0xfffb` are start markers.  Variations are stored
//! after the mainline terminator, deepest last, so they are replayed in reverse
//! marker order and inserted after the replaced move to preserve PGN order.
//!
//! This walker deliberately does not de-duplicate equal moves: the stored
//! order is part of the format, and annotation records index into it.

use std::num::NonZeroU32;

use anyhow::{Context, Result, bail};

use crate::chess::{
    Bitboard, Board, CastlingMode, CastlingSide, Chess, Color, EnPassantMode, Fen, File, Move,
    Piece, Position, Rank, Role, SanPlus, Setup, Square, UciMove,
};
use crate::domain::{Game, Node, Ply};

use super::tokens;

const END_LINE: u16 = 0xffff;
const VARIATION: u16 = 0xfffd;
const START: u16 = 0xfffc;
const START_960: u16 = 0xfffb;

/// Build a Chess960 starting position from its Scharnagl number.
pub(super) fn start_position_960(id: u16) -> Result<Chess> {
    if id >= 960 {
        bail!("Chess960 start id {id} is outside 0..=959");
    }
    let (king, rook_q, rook_k) = tokens::scharnagl(id);
    let mut back_rank = [None; 8];
    let light = [1usize, 3, 5, 7];
    let dark = [0usize, 2, 4, 6];
    let mut n = usize::from(id);
    back_rank[light[n % 4]] = Some(Role::Bishop);
    n /= 4;
    back_rank[dark[n % 4]] = Some(Role::Bishop);
    n /= 4;
    let mut remaining: Vec<usize> = (0..8).filter(|&file| back_rank[file].is_none()).collect();
    let queen = remaining.remove(n % 6);
    back_rank[queen] = Some(Role::Queen);
    n /= 6;
    let knights = combinations(&remaining, n);
    for file in knights {
        back_rank[file] = Some(Role::Knight);
    }
    back_rank[king as usize] = Some(Role::King);
    back_rank[rook_q as usize] = Some(Role::Rook);
    back_rank[rook_k as usize] = Some(Role::Rook);

    let mut board = Board::empty();
    let mut rights = Bitboard::EMPTY;
    for color in Color::ALL {
        let back = color.fold_wb(Rank::First, Rank::Eighth);
        let pawns = color.fold_wb(Rank::Second, Rank::Seventh);
        for (file, role) in back_rank.iter().enumerate() {
            let file = File::new(file as u32);
            if let Some(role) = role {
                board.set_piece_at(Square::from_coords(file, back), Piece::new(color, *role));
            }
            board.set_piece_at(
                Square::from_coords(file, pawns),
                Piece::new(color, Role::Pawn),
            );
        }
        rights |= Bitboard::from_square(Square::from_coords(File::new(rook_q as u32), back));
        rights |= Bitboard::from_square(Square::from_coords(File::new(rook_k as u32), back));
    }

    let setup = Setup {
        board,
        turn: Color::White,
        castling_rights: rights,
        ep_square: None,
        halfmoves: 0,
        fullmoves: NonZeroU32::MIN,
    };
    Chess::from_setup(setup, CastlingMode::Chess960).context("building Chess960 start position")
}

fn combinations(files: &[usize], index: usize) -> [usize; 2] {
    let mut seen = 0usize;
    for first in 0..files.len() {
        for second in first + 1..files.len() {
            if seen == index {
                return [files[first], files[second]];
            }
            seen += 1;
        }
    }
    [files[0], files[1]]
}

/// Decode a token stream into `game`.  The root position must already be set.
pub(super) fn decode_moves(game: &mut Game, tokens: &[u16], chess960: bool) -> Result<()> {
    let mode = game.nodes[0].pos.castling_mode();
    if chess960 != mode.is_chess960() {
        bail!(
            "move stream says Chess960={chess960} but the start position uses {:?} castling",
            mode
        );
    }
    let start = game.nodes[0].pos.clone();
    let mut parser = Parser {
        tokens,
        index: 0,
        mode,
    };
    parser.parse_line(game, 0, &start, false)?;
    if parser.index < tokens.len()
        && !tokens[parser.index..]
            .iter()
            .all(|&token| token == END_LINE)
    {
        bail!("move stream has trailing tokens after its final line");
    }
    Ok(())
}

struct Parser<'a> {
    tokens: &'a [u16],
    index: usize,
    mode: CastlingMode,
}

impl Parser<'_> {
    fn parse_line(
        &mut self,
        game: &mut Game,
        parent: usize,
        start: &Chess,
        variation: bool,
    ) -> Result<()> {
        let mut current = parent;
        let mut line = Vec::new();
        let mut branches = Vec::new();
        loop {
            let Some(&token) = self.tokens.get(self.index) else {
                bail!("move stream ends before its line terminator");
            };
            self.index += 1;
            match token {
                END_LINE => break,
                VARIATION => {
                    if line.is_empty() {
                        bail!("variation marker has no move to replace");
                    }
                    branches.push(line.len() - 1);
                }
                START | START_960 => {}
                0 => {
                    let before = game.nodes[current].pos.clone();
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
                    let id = append_node(game, current, ply, after, variation && line.is_empty());
                    line.push(id);
                    current = id;
                }
                code => {
                    let before = if line.is_empty() {
                        start.clone()
                    } else {
                        game.nodes[current].pos.clone()
                    };
                    let entry = tokens::table().entry(code);
                    let mv = move_from_token(&before, entry)
                        .with_context(|| format!("decoding 2CBH token {code}"))?;
                    let after = before
                        .clone()
                        .play(mv)
                        .with_context(|| format!("playing 2CBH token {code}"))?;
                    let ply = Ply {
                        san: SanPlus::from_move(before.clone(), mv).to_string(),
                        uci: UciMove::from_move(mv, self.mode).to_string(),
                        number: before.fullmoves().get(),
                        color: before.turn(),
                    };
                    let id = append_node(game, current, ply, after, variation && line.is_empty());
                    line.push(id);
                    current = id;
                }
            }
        }

        // Variations are stored deepest last.  Replay in reverse so the first
        // variation in the file ends up at child index 1.
        for &branch in branches.iter().rev() {
            let replaced = *line
                .get(branch)
                .context("variation marker points past the line")?;
            let attach = if branch == 0 {
                parent
            } else {
                line[branch - 1]
            };
            debug_assert_eq!(game.nodes[replaced].parent, Some(attach));
            let before = game.nodes[attach].pos.clone();
            self.parse_line(game, attach, &before, true)?;
        }
        Ok(())
    }
}

fn append_node(game: &mut Game, parent: usize, ply: Ply, after: Chess, variation: bool) -> usize {
    let id = game.nodes.len();
    game.nodes.push(Node {
        mv: Some(ply),
        comment: String::new(),
        clock: None,
        eval: None,
        nags: Vec::new(),
        parent: Some(parent),
        children: Vec::new(),
        pos: after,
    });
    let children = &mut game.nodes[parent].children;
    if variation && !children.is_empty() {
        children.insert(1, id);
    } else {
        children.push(id);
    }
    id
}

pub(crate) fn move_from_token(pos: &Chess, entry: [u16; 8]) -> Result<Move> {
    let [from, to_code, piece, aux, b3, b2, special6, special7] = entry;
    let from = from_stored_square(from as u8);
    let to = from_stored_square(to_code as u8);
    let board = pos.board();
    let moved = board
        .piece_at(from)
        .with_context(|| format!("token starts from empty square {from}"))?;
    let expected_role = role_from_code(piece & 7)
        .with_context(|| format!("token names unknown piece type {}", piece & 7))?;
    let expected_color = if piece & 8 != 0 {
        Color::Black
    } else {
        Color::White
    };
    if moved.role != expected_role || moved.color != expected_color {
        bail!(
            "token names {:?} {:?} on {from}, but the board has {:?} {:?}",
            expected_color,
            expected_role,
            moved.color,
            moved.role
        );
    }
    if moved.color != pos.turn() {
        bail!("token moves an opponent piece on {from}");
    }

    if moved.role == Role::King && (special6 & 7) == 5 {
        let side = match to.file() {
            File::G => CastlingSide::KingSide,
            File::C => CastlingSide::QueenSide,
            _ => bail!("castling token lands on {to} instead of the g/c file"),
        };
        let rook = pos
            .castles()
            .rook(pos.turn(), side)
            .with_context(|| format!("castling token on {from} has no {side:?} right"))?;
        return Ok(Move::Castle { king: from, rook });
    }

    if moved.role == Role::Pawn && special6 != 0 && b2 == b3 && b3 == to_code {
        let promotion = role_from_code(special6 & 7)
            .with_context(|| format!("promotion names unknown role {}", special6 & 7))?;
        return Ok(Move::Normal {
            role: Role::Pawn,
            from,
            capture: board.piece_at(to).map(|piece| piece.role),
            to,
            promotion: Some(promotion),
        });
    }

    if moved.role == Role::Pawn && aux == 0 && b2 == b3 && b3 != 0 && special7 != 0 {
        let captured = from_stored_square(b2 as u8);
        if board.piece_at(captured) != Some(Piece::new(pos.turn().other(), Role::Pawn)) {
            bail!("en-passant token names no opposing pawn on {captured}");
        }
        if pos.maybe_ep_square() != Some(to) {
            bail!("en-passant token lands on {to} without an en-passant target");
        }
        return Ok(Move::EnPassant { from, to });
    }

    Ok(Move::Normal {
        role: moved.role,
        from,
        capture: board.piece_at(to).map(|piece| piece.role),
        to,
        promotion: None,
    })
}

fn role_from_code(code: u16) -> Option<Role> {
    Some(match code {
        1 => Role::King,
        2 => Role::Queen,
        3 => Role::Knight,
        4 => Role::Bishop,
        5 => Role::Rook,
        6 => Role::Pawn,
        _ => return None,
    })
}

/// ChessBase stores squares as `file * 8 + rank`.
pub(super) fn from_stored_square(code: u8) -> Square {
    Square::from_coords(
        File::new(u32::from(code / 8)),
        Rank::new(u32::from(code % 8)),
    )
}

/// The inverse of [`from_stored_square`]: `file * 8 + rank`.
pub(super) fn to_stored_square(square: Square) -> u8 {
    (square.file().to_u32() as u8) * 8 + (square.rank().to_u32() as u8)
}

/// The FEN tag for a decoded start position.
pub(super) fn start_fen(pos: &Chess) -> String {
    Fen::from_position(pos, EnPassantMode::Legal).to_string()
}
