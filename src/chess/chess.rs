//! The `Chess` position type: a `cozy_chess` board paired with a castling
//! mode, exposing facade accessors.

use std::fmt;
use std::num::NonZeroU32;

use cozy_chess::{BoardBuilder, GameStatus};

use crate::chess::board::{Board, BoardRef, Castles, CastlingMode, CastlingSide, Setup};
use crate::chess::r#move::Move;
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

/// Error returned when a move cannot be played on a [`Chess`] position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlayError {
    /// The move is not legal in this position.
    IllegalMove,
    /// Piece drops have no representation in orthodox chess.
    UnsupportedDrop,
}

impl fmt::Display for PlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PlayError::IllegalMove => "illegal move",
            PlayError::UnsupportedDrop => "piece drops are not supported",
        })
    }
}

impl std::error::Error for PlayError {}

/// When a position reports its en passant square.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EnPassantMode {
    /// Only when an en passant capture is legal for the side to move.
    Legal,
    /// Whenever the last move was a double pawn push.
    Always,
}

/// Read and play access to a chess position.
pub trait Position {
    /// A read view of the pieces on the board.
    #[must_use]
    fn board(&self) -> BoardRef<'_>;

    /// The side to move.
    #[must_use]
    fn turn(&self) -> Color;

    /// The castling rights.
    #[must_use]
    fn castles(&self) -> Castles;

    /// The en-passant target square after a double pawn push, whether or not
    /// a capture there is legal.
    #[must_use]
    fn maybe_ep_square(&self) -> Option<Square>;

    /// The en-passant target square according to `mode`.
    #[must_use]
    fn ep_square(&self, mode: EnPassantMode) -> Option<Square>;

    /// The halfmove clock.
    #[must_use]
    fn halfmoves(&self) -> u32;

    /// The fullmove number, starting from 1.
    #[must_use]
    fn fullmoves(&self) -> NonZeroU32;

    /// All legal moves in the current position.
    #[must_use]
    fn legal_moves(&self) -> Vec<Move>;

    /// All pieces of the side to move.
    #[must_use]
    fn us(&self) -> Bitboard;

    /// Plays a legal move, returning the resulting position.
    ///
    /// # Errors
    ///
    /// Returns [`PlayError::IllegalMove`] if the move is not legal, and
    /// [`PlayError::UnsupportedDrop`] for [`Move::Put`], which orthodox chess
    /// cannot express.
    fn play(self, mv: Move) -> Result<Self, PlayError>
    where
        Self: Sized;

    /// Plays a move without checking its legality.
    ///
    /// # Panics
    ///
    /// Panics for [`Move::Put`], which orthodox chess cannot express, and for
    /// moves that corrupt the engine's board state. Only pass moves returned by
    /// [`Position::legal_moves`].
    #[must_use]
    fn play_unchecked(self, mv: Move) -> Self
    where
        Self: Sized;

    /// Whether the side to move is checkmated.
    #[must_use]
    fn is_checkmate(&self) -> bool;
}

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

    /// The underlying castling mode.
    #[must_use]
    pub const fn castling_mode(&self) -> CastlingMode {
        self.mode
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

    /// The position as a setup. The en-passant square is kept according to
    /// `ep_mode`: always when one exists, or only when an en passant capture
    /// is legal.
    #[must_use]
    pub fn to_setup(&self, ep_mode: EnPassantMode) -> Setup {
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
            ep_square: self.ep_square(ep_mode),
            halfmoves: self.halfmoves(),
            fullmoves: self.fullmoves(),
        }
    }

    /// The position as a setup, keeping the en-passant square whenever one
    /// exists. Equivalent to `to_setup(EnPassantMode::Always)`.
    #[must_use]
    pub fn to_setup_always(&self) -> Setup {
        self.to_setup(EnPassantMode::Always)
    }

    /// The same position with the opposite side to move. The en-passant
    /// square is dropped, and the clocks keep their connection to the moves
    /// that were played.
    pub fn swap_turn(&self) -> Result<Chess, PositionError> {
        let mut setup = self.to_setup(EnPassantMode::Always);
        setup.swap_turn();
        Chess::from_setup(setup, self.mode)
    }

    /// A 64-bit hash of the piece placement, side to move, castling rights,
    /// and legal en passant square. Clocks are not included.
    ///
    /// An en passant file only contributes when the capture is legal, so two
    /// positions that differ only by an unusable en passant square hash
    /// alike. Not Polyglot-compatible.
    #[must_use]
    pub fn position_hash(&self) -> u64 {
        if self.ep_square(EnPassantMode::Legal).is_some() {
            self.board.hash()
        } else {
            self.board.hash_without_ep()
        }
    }

    /// Converts a `cozy_chess` move into a facade [`Move`].
    ///
    /// The engine encodes castling as a king move onto its own rook, so a king
    /// move onto a castling rook square becomes [`Move::Castle`]. A pawn
    /// moving diagonally onto an empty en passant square becomes
    /// [`Move::EnPassant`]. Everything else is a [`Move::Normal`].
    ///
    /// Moves that are not legal in this position are still converted, using a
    /// pawn role when the origin square is empty; callers that accept
    /// untrusted moves must check the result against [`Position::legal_moves`].
    // The name comes from the migration brief and reads well at call sites.
    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn from_cozy(&self, mv: cozy_chess::Move) -> Move {
        let from = Square::from(mv.from);
        let to = Square::from(mv.to);
        let role = self.board.piece_on(mv.from).map_or(Role::Pawn, Role::from);
        if role == Role::King {
            let castles = self.castles();
            let side = self.turn();
            let is_castle = castles.rook(side, CastlingSide::KingSide) == Some(to)
                || castles.rook(side, CastlingSide::QueenSide) == Some(to);
            if is_castle {
                return Move::Castle {
                    king: from,
                    rook: to,
                };
            }
        }
        if role == Role::Pawn
            && from.file() != to.file()
            && self.board.piece_on(mv.to).is_none()
            && self.maybe_ep_square() == Some(to)
        {
            return Move::EnPassant { from, to };
        }
        Move::Normal {
            role,
            from,
            capture: self.board.piece_on(mv.to).map(Role::from),
            to,
            promotion: mv.promotion.map(Role::from),
        }
    }

    /// The underlying engine board, for the facade's own conversions.
    pub(crate) fn cozy_board(&self) -> &cozy_chess::Board {
        &self.board
    }
}

impl Position for Chess {
    fn board(&self) -> BoardRef<'_> {
        BoardRef::new(&self.board)
    }

    fn turn(&self) -> Color {
        Color::from(self.board.side_to_move())
    }

    /// The castling rights, re-derived from the validated board.
    fn castles(&self) -> Castles {
        Castles::from_board(&self.board, self.mode)
    }

    /// The engine stores only a file; the rank follows from the side to move.
    fn maybe_ep_square(&self) -> Option<Square> {
        self.board.en_passant().map(|file| {
            Square::from_coords(
                File::new(file as u32),
                self.turn().fold_wb(Rank::Sixth, Rank::Third),
            )
        })
    }

    fn ep_square(&self, mode: EnPassantMode) -> Option<Square> {
        let square = self.maybe_ep_square()?;
        match mode {
            EnPassantMode::Always => Some(square),
            EnPassantMode::Legal => self
                .legal_moves()
                .iter()
                .any(|mv| matches!(mv, Move::EnPassant { .. }))
                .then_some(square),
        }
    }

    fn halfmoves(&self) -> u32 {
        u32::from(self.board.halfmove_clock())
    }

    fn fullmoves(&self) -> NonZeroU32 {
        NonZeroU32::new(u32::from(self.board.fullmove_number())).unwrap_or(NonZeroU32::MIN)
    }

    fn legal_moves(&self) -> Vec<Move> {
        let mut moves = Vec::new();
        self.board.generate_moves(|piece_moves| {
            for mv in piece_moves {
                moves.push(self.from_cozy(mv));
            }
            false
        });
        moves
    }

    fn us(&self) -> Bitboard {
        self.us_of(self.turn())
    }

    fn play(self, mv: Move) -> Result<Chess, PlayError> {
        let cozy = mv.to_cozy().ok_or(PlayError::UnsupportedDrop)?;
        let mut board = self.board;
        board.try_play(cozy).map_err(|_| PlayError::IllegalMove)?;
        Ok(Chess {
            board,
            mode: self.mode,
        })
    }

    fn play_unchecked(self, mv: Move) -> Chess {
        let cozy = mv
            .to_cozy()
            .expect("piece drops are not supported in orthodox chess");
        let mut board = self.board;
        board.play_unchecked(cozy);
        Chess {
            board,
            mode: self.mode,
        }
    }

    fn is_checkmate(&self) -> bool {
        self.board.status() == GameStatus::Won
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
