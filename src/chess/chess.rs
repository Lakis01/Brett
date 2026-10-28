//! The `Chess` position type: a `cozy_chess` board paired with a castling
//! mode, exposing facade accessors.

use std::fmt;
use std::num::NonZeroU32;

use cozy_chess::BoardBuilder;

use crate::chess::board::{Board, BoardRef, Castles, CastlingMode, CastlingSide, Setup};
use crate::chess::types::{Bitboard, Color, File, Rank, Role, Square};

/// Error returned when a [`Setup`] cannot be turned into a valid
/// [`Chess`] position.
#[derive(Clone, Debug)]
pub struct PositionError(String);

impl fmt::Display for PositionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid position: {}", self.0)
    }
}

impl std::error::Error for PositionError {}

/// A playable position: a validated `cozy_chess` board plus the
/// [`CastlingMode`] it was built with.
#[derive(Clone, Debug)]
pub struct Chess {
    board: cozy_chess::Board,
    mode: CastlingMode,
}

impl Chess {
    /// Validates a setup into a playable position.
    ///
    /// Clocks are clamped for validation: halfmoves to at most 100, fullmoves
    /// to at most `u16::MAX` (and always at least 1).
    pub fn from_setup(setup: Setup, mode: CastlingMode) -> Result<Chess, PositionError> {
        let castles = Castles::from_setup(&setup, mode);
        let mut builder = BoardBuilder::empty();
        for square in Square::ALL {
            if let Some(piece) = setup.board.piece_at(square) {
                builder.board[square.to_u32() as usize] = Some((
                    cozy_chess::Piece::from(piece.role),
                    cozy_chess::Color::from(piece.color),
                ));
            }
        }
        builder.side_to_move = cozy_chess::Color::from(setup.turn);
        for color in Color::ALL {
            builder.castle_rights[color as usize] = cozy_chess::CastleRights {
                short: castles
                    .rook(color, CastlingSide::KingSide)
                    .map(|square| cozy_chess::Square::from(square).file()),
                long: castles
                    .rook(color, CastlingSide::QueenSide)
                    .map(|square| cozy_chess::Square::from(square).file()),
            };
        }
        builder.en_passant = setup.ep_square.map(cozy_chess::Square::from);
        builder.halfmove_clock = setup.halfmoves.min(100) as u8;
        builder.fullmove_number = setup.fullmoves.get().min(u16::MAX as u32) as u16;
        let board = builder
            .build()
            .map_err(|error| PositionError(error.to_string()))?;
        Ok(Chess { board, mode })
    }

    /// A read view of the pieces on the board.
    #[must_use]
    pub fn board(&self) -> BoardRef<'_> {
        BoardRef::new(&self.board)
    }

    /// The side to move.
    #[must_use]
    pub fn turn(&self) -> Color {
        Color::from(self.board.side_to_move())
    }

    /// The castling rights, re-derived from the validated board.
    #[must_use]
    pub fn castles(&self) -> Castles {
        Castles::from_board(&self.board, self.mode)
    }

    /// The underlying castling mode.
    #[must_use]
    pub const fn castling_mode(&self) -> CastlingMode {
        self.mode
    }

    /// The en-passant target square, if a double-pawn-move capture is legal
    /// right now.
    #[must_use]
    pub fn maybe_ep_square(&self) -> Option<Square> {
        self.board.en_passant().map(|file| {
            Square::from_coords(
                File::new(file as u32),
                self.turn().fold_wb(Rank::Sixth, Rank::Third),
            )
        })
    }

    /// The halfmove clock.
    #[must_use]
    pub fn halfmoves(&self) -> u32 {
        u32::from(self.board.halfmove_clock())
    }

    /// The fullmove number, starting from 1.
    #[must_use]
    pub fn fullmoves(&self) -> NonZeroU32 {
        NonZeroU32::new(u32::from(self.board.fullmove_number())).unwrap_or(NonZeroU32::MIN)
    }

    /// All pieces of the side to move.
    #[must_use]
    pub fn us(&self) -> Bitboard {
        self.us_of(self.turn())
    }

    fn us_of(&self, color: Color) -> Bitboard {
        let mut pieces = Bitboard::EMPTY;
        for role in Role::ALL {
            pieces |= Bitboard::from(
                self.board
                    .colored_pieces(color.into(), cozy_chess::Piece::from(role)),
            );
        }
        pieces
    }

    /// The position as a setup, keeping the en-passant square whenever one
    /// exists.
    ///
    /// The en-passant-aware [`Setup`] variant arrives in Task 4 with
    /// `EnPassantMode`; until then this is the always-emit form.
    #[must_use]
    pub fn to_setup_always(&self) -> Setup {
        let mut board = Board::empty();
        for square in Square::ALL {
            if let Some(piece) = self.board().piece_at(square) {
                board.set_piece_at(square, piece);
            }
        }
        Setup {
            board,
            turn: self.turn(),
            castling_rights: self.castles().castling_rights(),
            ep_square: self.maybe_ep_square(),
            halfmoves: self.halfmoves(),
            fullmoves: self.fullmoves(),
        }
    }

    /// The same position with the opposite side to move. The en-passant
    /// square is dropped, and the clocks keep their connection to the moves
    /// that were played.
    pub fn swap_turn(&self) -> Result<Chess, PositionError> {
        let mut setup = self.to_setup_always();
        setup.swap_turn();
        Chess::from_setup(setup, self.mode)
    }
}

impl Default for Chess {
    /// The orthodox chess starting position.
    ///
    /// # Panics
    ///
    /// Panics only if the start position stops validating, which would be a
    /// bug in the facade itself.
    fn default() -> Chess {
        Chess::from_setup(Setup::default(), CastlingMode::Standard)
            .expect("the orthodox start position is valid")
    }
}
