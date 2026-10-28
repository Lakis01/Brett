//! Forsyth–Edwards Notation for facade positions, with X-FEN castling rights
//! so Chess960 positions survive a round trip.
//!
//! `cozy_chess` prints `KQkq` even when the castling rooks are not the
//! outermost rooks, so Brett reads and writes FEN itself.

use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;

use crate::chess::board::{Board, CastlingMode, Setup};
use crate::chess::chess::{Chess, EnPassantMode, PositionError};
use crate::chess::types::{Bitboard, Color, File, Piece, Rank, Role, Square};

/// Error returned when a FEN string cannot be parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FenError {
    /// The string has no fields, or more than six.
    InvalidFieldCount,
    /// The piece placement field is malformed.
    InvalidBoard,
    /// The side to move is neither `w` nor `b`.
    InvalidTurn,
    /// The castling field contains an unexpected character.
    InvalidCastling,
    /// The en passant field is neither `-` nor a square.
    InvalidEpSquare,
    /// The halfmove clock is not a non-negative integer.
    InvalidHalfmoves,
    /// The fullmove number is not a non-negative integer.
    InvalidFullmoves,
}

impl fmt::Display for FenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FenError::InvalidFieldCount => "invalid fen: wrong number of fields",
            FenError::InvalidBoard => "invalid fen: bad piece placement",
            FenError::InvalidTurn => "invalid fen: bad side to move",
            FenError::InvalidCastling => "invalid fen: bad castling rights",
            FenError::InvalidEpSquare => "invalid fen: bad en passant square",
            FenError::InvalidHalfmoves => "invalid fen: bad halfmove clock",
            FenError::InvalidFullmoves => "invalid fen: bad fullmove number",
        })
    }
}

impl std::error::Error for FenError {}

/// A parsed FEN: an unvalidated [`Setup`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fen {
    /// The parsed position, not yet checked for legality.
    pub setup: Setup,
}

impl Fen {
    /// Parses a FEN or X-FEN string.
    ///
    /// Fields are separated by ASCII whitespace. The piece placement is
    /// required; missing trailing fields default to `w - - 0 1`. Castling
    /// `K`/`Q` resolve to the outermost rook on the higher/lower file side of
    /// that color's king, for both colors; file letters `A`–`H`/`a`–`h` name
    /// the rook directly. Rights without a matching rook are dropped. A
    /// fullmove number of `0` is read as `1`.
    ///
    /// # Errors
    ///
    /// Returns a [`FenError`] describing the first malformed field.
    pub fn from_ascii(fen: &[u8]) -> Result<Fen, FenError> {
        let mut fields = fen
            .split(u8::is_ascii_whitespace)
            .filter(|field| !field.is_empty());
        let board = parse_board(fields.next().ok_or(FenError::InvalidFieldCount)?)?;
        let turn = match fields.next() {
            None | Some(b"w") => Color::White,
            Some(b"b") => Color::Black,
            Some(_) => return Err(FenError::InvalidTurn),
        };
        let castling_rights = match fields.next() {
            None => Bitboard::EMPTY,
            Some(field) => parse_castling(&board, field)?,
        };
        let ep_square = match fields.next() {
            None | Some(b"-") => None,
            Some(field) => Some(
                std::str::from_utf8(field)
                    .ok()
                    .and_then(|text| Square::from_str(text).ok())
                    .ok_or(FenError::InvalidEpSquare)?,
            ),
        };
        let halfmoves = match fields.next() {
            None => 0,
            Some(field) => parse_u32(field).ok_or(FenError::InvalidHalfmoves)?,
        };
        let fullmoves = match fields.next() {
            None => NonZeroU32::MIN,
            Some(field) => NonZeroU32::new(parse_u32(field).ok_or(FenError::InvalidFullmoves)?)
                .unwrap_or(NonZeroU32::MIN),
        };
        if fields.next().is_some() {
            return Err(FenError::InvalidFieldCount);
        }
        Ok(Fen {
            setup: Setup {
                board,
                turn,
                castling_rights,
                ep_square,
                halfmoves,
                fullmoves,
            },
        })
    }

    /// The parsed setup.
    #[must_use]
    pub fn into_setup(self) -> Setup {
        self.setup
    }

    /// Validates the setup into a playable position.
    ///
    /// # Errors
    ///
    /// Returns a [`PositionError`] if the setup is not a legal position.
    pub fn into_position(self, mode: CastlingMode) -> Result<Chess, PositionError> {
        Chess::from_setup(self.setup, mode)
    }

    /// The FEN of a position, emitting the en passant square per `ep_mode`.
    #[must_use]
    pub fn from_position(pos: &Chess, ep_mode: EnPassantMode) -> Fen {
        Fen {
            setup: pos.to_setup(ep_mode),
        }
    }
}

impl FromStr for Fen {
    type Err = FenError;

    fn from_str(fen: &str) -> Result<Fen, FenError> {
        Fen::from_ascii(fen.as_bytes())
    }
}

impl fmt::Display for Fen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let setup = &self.setup;
        for rank in Rank::ALL.into_iter().rev() {
            let mut empty = 0;
            for file in File::ALL {
                match setup.board.piece_at(Square::from_coords(file, rank)) {
                    Some(piece) => {
                        if empty > 0 {
                            write!(f, "{empty}")?;
                            empty = 0;
                        }
                        write!(f, "{}", piece.char())?;
                    }
                    None => empty += 1,
                }
            }
            if empty > 0 {
                write!(f, "{empty}")?;
            }
            if rank != Rank::First {
                f.write_str("/")?;
            }
        }
        f.write_str(setup.turn.fold_wb(" w ", " b "))?;
        write_castling(f, setup)?;
        match setup.ep_square {
            Some(square) => write!(f, " {square}")?,
            None => f.write_str(" -")?,
        }
        write!(f, " {} {}", setup.halfmoves, setup.fullmoves)
    }
}

fn parse_u32(field: &[u8]) -> Option<u32> {
    if field.is_empty() || !field.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(field).ok()?.parse().ok()
}

fn parse_board(field: &[u8]) -> Result<Board, FenError> {
    let mut board = Board::empty();
    let mut ranks = field.split(|&byte| byte == b'/');
    for rank in Rank::ALL.into_iter().rev() {
        let row = ranks.next().ok_or(FenError::InvalidBoard)?;
        let mut file = 0u32;
        for &byte in row {
            match byte {
                b'1'..=b'8' => file += u32::from(byte - b'0'),
                _ => {
                    let role = Role::from_char(char::from(byte)).ok_or(FenError::InvalidBoard)?;
                    if file >= 8 {
                        return Err(FenError::InvalidBoard);
                    }
                    let color = if byte.is_ascii_uppercase() {
                        Color::White
                    } else {
                        Color::Black
                    };
                    board.set_piece_at(
                        Square::from_coords(File::new(file), rank),
                        Piece::new(color, role),
                    );
                    file += 1;
                }
            }
            if file > 8 {
                return Err(FenError::InvalidBoard);
            }
        }
        if file != 8 {
            return Err(FenError::InvalidBoard);
        }
    }
    if ranks.next().is_some() {
        return Err(FenError::InvalidBoard);
    }
    Ok(board)
}

/// The back-rank squares holding `color`'s rooks.
fn back_rank_rooks(board: &Board, color: Color) -> impl Iterator<Item = Square> + '_ {
    let back = color.fold_wb(Rank::First, Rank::Eighth);
    File::ALL
        .into_iter()
        .map(move |file| Square::from_coords(file, back))
        .filter(move |&square| board.piece_at(square) == Some(Piece::new(color, Role::Rook)))
}

/// `color`'s king file, if the king stands on its back rank.
fn back_rank_king_file(board: &Board, color: Color) -> Option<u32> {
    let back = color.fold_wb(Rank::First, Rank::Eighth);
    board
        .king_of(color)
        .filter(|king| king.rank() == back)
        .map(|king| king.file().to_u32())
}

/// The outermost rook of `color` on the kingside (higher files) or
/// queenside (lower files) of its king.
fn outermost_rook(board: &Board, color: Color, kingside: bool) -> Option<Square> {
    let king_file = back_rank_king_file(board, color)?;
    let mut rooks = back_rank_rooks(board, color);
    if kingside {
        rooks
            .filter(|square| square.file().to_u32() > king_file)
            .last()
    } else {
        rooks.find(|square| square.file().to_u32() < king_file)
    }
}

fn parse_castling(board: &Board, field: &[u8]) -> Result<Bitboard, FenError> {
    let mut rights = Bitboard::EMPTY;
    if field == b"-" {
        return Ok(rights);
    }
    for &byte in field {
        let color = if byte.is_ascii_uppercase() {
            Color::White
        } else {
            Color::Black
        };
        let rook = match byte.to_ascii_lowercase() {
            b'k' => outermost_rook(board, color, true),
            b'q' => outermost_rook(board, color, false),
            letter @ b'a'..=b'h' => {
                let square = Square::from_coords(
                    File::new(u32::from(letter - b'a')),
                    color.fold_wb(Rank::First, Rank::Eighth),
                );
                (board.piece_at(square) == Some(Piece::new(color, Role::Rook))).then_some(square)
            }
            _ => return Err(FenError::InvalidCastling),
        };
        if let Some(square) = rook {
            rights |= Bitboard::from_square(square);
        }
    }
    Ok(rights)
}

/// Writes X-FEN castling rights: `K`/`Q` when the right belongs to the
/// outermost rook on that side of the king, otherwise the rook's file
/// letter. White first, kingside before queenside.
fn write_castling(f: &mut fmt::Formatter<'_>, setup: &Setup) -> fmt::Result {
    let mut any = false;
    for color in Color::ALL {
        let back = color.fold_wb(Rank::First, Rank::Eighth);
        let kingside = outermost_rook(&setup.board, color, true);
        let queenside = outermost_rook(&setup.board, color, false);
        for file in File::ALL.into_iter().rev() {
            let square = Square::from_coords(file, back);
            if !setup.castling_rights.contains(square) {
                continue;
            }
            let letter = if Some(square) == kingside {
                'K'
            } else if Some(square) == queenside {
                'Q'
            } else {
                char::from(b'A' + file.to_u32() as u8)
            };
            let letter = color.fold_wb(letter, letter.to_ascii_lowercase());
            write!(f, "{letter}")?;
            any = true;
        }
    }
    if !any {
        f.write_str("-")?;
    }
    Ok(())
}
