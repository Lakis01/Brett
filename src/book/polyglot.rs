//! Standard Polyglot 64-bit Zobrist hash for `.bin` opening books.
//!
//! Polyglot keys differ from `cozy-chess` board hashes in both random constants
//! and en-passant rules: Polyglot hashes the en-passant file whenever the side
//! to move has an adjacent pawn on the fifth rank (fourth for Black), even if
//! capturing en passant would be illegal because of a pin.

use crate::chess::{CastlingSide, Chess, Color, File, Piece, Position, Rank, Role, Square};

use super::polyglot_keys::POLYGLOT_RANDOM_ARRAY;

/// Computes the 64-bit Polyglot Zobrist key of `pos`.
#[must_use]
pub fn position_key(pos: &Chess) -> u64 {
    let mut key = 0u64;
    let board = pos.board();

    for sq in Square::ALL {
        if let Some(piece) = board.piece_at(sq) {
            let index = 64 * piece_index(piece) + sq.to_u32() as usize;
            key ^= POLYGLOT_RANDOM_ARRAY[index];
        }
    }

    let castles = pos.castles();
    if castles.has(Color::White, CastlingSide::KingSide) {
        key ^= POLYGLOT_RANDOM_ARRAY[768];
    }
    if castles.has(Color::White, CastlingSide::QueenSide) {
        key ^= POLYGLOT_RANDOM_ARRAY[769];
    }
    if castles.has(Color::Black, CastlingSide::KingSide) {
        key ^= POLYGLOT_RANDOM_ARRAY[770];
    }
    if castles.has(Color::Black, CastlingSide::QueenSide) {
        key ^= POLYGLOT_RANDOM_ARRAY[771];
    }

    if let Some(ep_sq) = pos.maybe_ep_square() {
        let ep_file = ep_sq.file().to_u32();
        let turn = pos.turn();
        let pawn_rank = turn.fold_wb(Rank::Fifth, Rank::Fourth);
        let our_pawn = Piece::new(turn, Role::Pawn);
        let has_adjacent_pawn = [
            ep_file.checked_sub(1),
            (ep_file + 1 < 8).then_some(ep_file + 1),
        ]
        .into_iter()
        .flatten()
        .any(|file| {
            board.piece_at(Square::from_coords(File::new(file), pawn_rank)) == Some(our_pawn)
        });
        if has_adjacent_pawn {
            key ^= POLYGLOT_RANDOM_ARRAY[772 + ep_file as usize];
        }
    }

    if pos.turn() == Color::White {
        key ^= POLYGLOT_RANDOM_ARRAY[780];
    }

    key
}

const fn piece_index(piece: Piece) -> usize {
    let role_index = match piece.role {
        Role::Pawn => 0,
        Role::Knight => 1,
        Role::Bishop => 2,
        Role::Rook => 3,
        Role::Queen => 4,
        Role::King => 5,
    };
    let color_index = match piece.color {
        Color::Black => 0,
        Color::White => 1,
    };
    2 * role_index + color_index
}
