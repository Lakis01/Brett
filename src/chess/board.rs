//! Brett-owned board, setup, and castling types. These sit directly on top of
//! the [`cozy_chess`] engine types but expose a stable, option-friendly API.

use std::num::NonZeroU32;

use crate::chess::types::{Bitboard, Color, File, Piece, Rank, Role, Square};

/// A 64-square board, addressed by [`Square`]. Deliberately not a
/// `cozy_chess::Board`: an empty board must be representable so custom and
/// Chess960 starting positions can be assembled piece by piece.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Board {
    squares: [Option<Piece>; 64],
}

impl Board {
    /// A board with no pieces at all.
    #[must_use]
    pub fn empty() -> Board {
        Board {
            squares: [None; 64],
        }
    }

    /// The orthodox chess starting position.
    #[must_use]
    pub fn startpos() -> Board {
        let mut board = Board::empty();
        const BACK_RANK: [(File, Role); 8] = [
            (File::A, Role::Rook),
            (File::B, Role::Knight),
            (File::C, Role::Bishop),
            (File::D, Role::Queen),
            (File::E, Role::King),
            (File::F, Role::Bishop),
            (File::G, Role::Knight),
            (File::H, Role::Rook),
        ];
        for color in Color::ALL {
            let back = color.fold_wb(Rank::First, Rank::Eighth);
            let pawns = color.fold_wb(Rank::Second, Rank::Seventh);
            for (file, role) in BACK_RANK {
                board.set_piece_at(Square::from_coords(file, back), Piece::new(color, role));
                board.set_piece_at(
                    Square::from_coords(file, pawns),
                    Piece::new(color, Role::Pawn),
                );
            }
        }
        board
    }

    /// The piece on `square`, if any.
    #[must_use]
    pub fn piece_at(&self, square: Square) -> Option<Piece> {
        self.squares[square.to_u32() as usize]
    }

    /// Places a piece on `square`, replacing whatever was there.
    pub fn set_piece_at(&mut self, square: Square, piece: Piece) {
        self.squares[square.to_u32() as usize] = Some(piece);
    }

    /// Clears `square`, if it is not empty already.
    pub fn remove_piece_at(&mut self, square: Square) {
        self.squares[square.to_u32() as usize] = None;
    }

    /// The square of the given color's king, if it is on the board.
    #[must_use]
    pub fn king_of(&self, color: Color) -> Option<Square> {
        Square::ALL
            .into_iter()
            .find(|&square| self.piece_at(square) == Some(Piece::new(color, Role::King)))
    }

    /// The role of the piece on `square`, if any.
    #[must_use]
    pub fn role_at(&self, square: Square) -> Option<Role> {
        self.squares[square.to_u32() as usize].map(|piece| piece.role)
    }
}

/// A borrowed view of a `cozy_chess::Board` with facade accessors.
#[derive(Clone, Copy, Debug)]
pub struct BoardRef<'a>(&'a cozy_chess::Board);

impl<'a> BoardRef<'a> {
    /// Wraps a `cozy_chess::Board` for reading through facade types.
    #[must_use]
    pub fn new(board: &'a cozy_chess::Board) -> BoardRef<'a> {
        BoardRef(board)
    }

    /// The piece on `square`, if any.
    #[must_use]
    pub fn piece_at(&self, square: Square) -> Option<Piece> {
        if let (Some(role), Some(color)) = (
            self.0.piece_on(square.into()),
            self.0.color_on(square.into()),
        ) {
            Some(Piece::new(Color::from(color), Role::from(role)))
        } else {
            None
        }
    }

    /// The square of the given color's king, if it is on the board.
    ///
    /// Uses a bitboard lookup instead of `cozy_chess::Board::king()`, which
    /// panics when the board has no king.
    #[must_use]
    pub fn king_of(&self, color: Color) -> Option<Square> {
        let king = self
            .0
            .colored_pieces(color.into(), cozy_chess::Piece::King)
            .iter()
            .next();
        king.map(Square::from)
    }

    /// The role of the piece on `square`, if any.
    #[must_use]
    pub fn role_at(&self, square: Square) -> Option<Role> {
        self.0.piece_on(square.into()).map(Role::from)
    }
}

/// Everything needed to validate or play from a position: the board, the side
/// to move, castling rights as raw squares, the en-passant target square, and
/// the clocks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setup {
    /// The pieces on the board.
    pub board: Board,
    /// The side to move.
    pub turn: Color,
    /// Squares of rooks that may still castle, per their starting side.
    pub castling_rights: Bitboard,
    /// The en-passant target square, if a double-pawn-move capture is legal.
    pub ep_square: Option<Square>,
    /// The halfmove clock.
    pub halfmoves: u32,
    /// The fullmove number, starting from 1.
    pub fullmoves: NonZeroU32,
}

impl Setup {
    /// An empty board with White to move, no rights, no en passant, clocks at
    /// their initial values.
    #[must_use]
    pub fn empty() -> Setup {
        Setup {
            board: Board::empty(),
            turn: Color::White,
            castling_rights: Bitboard::EMPTY,
            ep_square: None,
            halfmoves: 0,
            fullmoves: NonZeroU32::MIN,
        }
    }

    /// Flips the side to move and clears the en-passant square. Clocks are
    /// untouched; the ordinary clock updates belong to the move logic.
    pub fn swap_turn(&mut self) {
        self.turn = self.turn.other();
        self.ep_square = None;
    }
}

impl Default for Setup {
    fn default() -> Setup {
        Setup {
            board: Board::startpos(),
            turn: Color::White,
            castling_rights: Bitboard::EMPTY
                | Bitboard::from_square(Square::A1)
                | Bitboard::from_square(Square::H1)
                | Bitboard::from_square(Square::A8)
                | Bitboard::from_square(Square::H8),
            ep_square: None,
            halfmoves: 0,
            fullmoves: NonZeroU32::MIN,
        }
    }
}

/// How castling rights in a [`Setup`] are interpreted: orthodox moves only, or
/// Chess960 rook-file moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CastlingMode {
    /// King moves two squares, rook ends beside the king. Rights are kept
    /// only for the orthodox king and rook files.
    Standard,
    /// King and rook finish on their Chess960 target squares. Any rook may
    /// own a right.
    Chess960,
}

impl CastlingMode {
    /// Returns whether this mode uses Chess960 semantics.
    #[must_use]
    pub const fn is_chess960(self) -> bool {
        matches!(self, CastlingMode::Chess960)
    }
}

/// Which way the castling king flees: the side with the `h`-rook or the side
/// with the `a`-rook.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CastlingSide {
    /// The kingside (short) rook.
    KingSide,
    /// The queenside (long) rook.
    QueenSide,
}

/// Castling rights resolved into concrete rook squares, one entry per color
/// and castle side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Castles {
    mode: CastlingMode,
    short: [Option<Square>; 2],
    long: [Option<Square>; 2],
}

impl Castles {
    /// Derives castling rights from a setup, interpreting its raw rook
    /// squares according to `mode`.
    ///
    /// A color keeps rights only when its king stands on its back rank and
    /// the rights claim a rook of the same color on that back rank. In
    /// [`CastlingMode::Standard`] the king must be on the E file and the rook
    /// on the H or A file; in [`CastlingMode::Chess960`] the outermost rook
    /// left of the king is the long rook and the outermost rook right of the
    /// king is the short rook.
    #[must_use]
    pub fn from_setup(setup: &Setup, mode: CastlingMode) -> Castles {
        derive_castles(
            mode,
            |color| (setup.board.king_of(color), setup.castling_rights),
            |square, color| setup.board.piece_at(square) == Some(Piece::new(color, Role::Rook)),
        )
    }

    /// Derives castling rights from a `cozy_chess` board according to
    /// `mode`, reading the rook files it has already validated.
    #[must_use]
    pub fn from_board(board: &cozy_chess::Board, mode: CastlingMode) -> Castles {
        derive_castles(
            mode,
            |color| {
                let king = board
                    .colored_pieces(color.into(), cozy_chess::Piece::King)
                    .iter()
                    .next()
                    .map(Square::from);
                let back = color.fold_wb(Rank::First, Rank::Eighth);
                let rights = board
                    .castle_rights(color.into())
                    .short
                    .into_iter()
                    .chain(board.castle_rights(color.into()).long)
                    .map(|file| {
                        Bitboard::from_square(Square::from_coords(File::new(file as u32), back))
                    })
                    .fold(Bitboard::EMPTY, |all, side| all | side);
                (king, rights)
            },
            |square, color| {
                board.color_on(square.into()) == Some(color.into())
                    && board.piece_on(square.into()) == Some(cozy_chess::Piece::Rook)
            },
        )
    }

    /// All castling rook squares for both colors.
    #[must_use]
    pub fn castling_rights(&self) -> Bitboard {
        let mut rights = Bitboard::EMPTY;
        for color in Color::ALL {
            for side in [CastlingSide::KingSide, CastlingSide::QueenSide] {
                if let Some(square) = self.rook(color, side) {
                    rights |= Bitboard::from_square(square);
                }
            }
        }
        rights
    }

    /// The rook square for a castling color and side, if that right exists.
    #[must_use]
    pub const fn rook(&self, color: Color, side: CastlingSide) -> Option<Square> {
        match side {
            CastlingSide::KingSide => self.short[color as usize],
            CastlingSide::QueenSide => self.long[color as usize],
        }
    }

    /// Returns whether a castling color and side is still allowed.
    #[must_use]
    pub const fn has(&self, color: Color, side: CastlingSide) -> bool {
        self.rook(color, side).is_some()
    }

    /// The mode these castling rights were derived with.
    #[must_use]
    pub const fn mode(&self) -> CastlingMode {
        self.mode
    }
}

/// Shared derivation for [`Castles::from_setup`] and
/// [`Castles::from_board`]: `king_and_rights` yields each color's king square
/// and the raw rights squares, `is_rook` says whether a back-rank rights
/// square really holds that color's rook.
fn derive_castles(
    mode: CastlingMode,
    king_and_rights: impl Fn(Color) -> (Option<Square>, Bitboard),
    is_rook: impl Fn(Square, Color) -> bool,
) -> Castles {
    let mut castles = Castles {
        mode,
        short: [None; 2],
        long: [None; 2],
    };
    for color in Color::ALL {
        let back = color.fold_wb(Rank::First, Rank::Eighth);
        let (Some(king), rights) = king_and_rights(color) else {
            continue;
        };
        if king.rank() != back {
            continue;
        }
        let king_file = king.file().to_u32();
        let mut short: Option<(Square, u32)> = None;
        let mut long: Option<(Square, u32)> = None;
        for square in Square::ALL {
            let file = square.file().to_u32();
            let candidate = file != king_file
                && square.rank() == back
                && rights.contains(square)
                && is_rook(square, color);
            if !candidate {
                continue;
            }
            if file > king_file {
                // Outermost rook on the kingside: largest file.
                if short.is_none_or(|(_, far)| file > far) {
                    short = Some((square, file));
                }
            } else {
                // Outermost rook on the queenside: smallest file.
                if long.is_none_or(|(_, far)| file < far) {
                    long = Some((square, file));
                }
            }
        }
        let mut short = short.map(|(square, _)| square);
        let mut long = long.map(|(square, _)| square);
        if !mode.is_chess960() {
            if king_file != File::E.to_u32() {
                short = None;
                long = None;
            } else {
                short = short.filter(|square| square.file() == File::H);
                long = long.filter(|square| square.file() == File::A);
            }
        }
        castles.short[color as usize] = short;
        castles.long[color as usize] = long;
    }
    castles
}
