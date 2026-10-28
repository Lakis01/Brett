//! Facade moves: long algebraic [`Move`] and protocol-facing [`UciMove`].
//!
//! Castling is stored in the facade's own shape, with the king and rook
//! squares, while `cozy_chess` encodes it as a king move onto its own rook.
//! The conversions in this module hide that difference.

use std::fmt;
use std::str::FromStr;

use crate::chess::board::CastlingMode;
use crate::chess::chess::{Chess, Position};
use crate::chess::types::{File, Role, Square};

/// Error returned when a [`UciMove`] cannot be parsed or played.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UciMoveError {
    /// The text is not syntactically valid UCI.
    Invalid,
    /// The move is not legal in the given position.
    Illegal,
}

impl fmt::Display for UciMoveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            UciMoveError::Invalid => "invalid uci move",
            UciMoveError::Illegal => "illegal uci move",
        })
    }
}

impl std::error::Error for UciMoveError {}

/// A move in the facade's representation.
///
/// A [`Move::Put`] only exists so that games from variants with piece drops
/// can still be described; orthodox [`Chess`] positions never generate one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Move {
    /// A normal move, e.g. `e2e4` or `e7e8q`.
    Normal {
        /// The role of the moved piece.
        role: Role,
        /// The origin square.
        from: Square,
        /// The role of the captured piece, if any.
        capture: Option<Role>,
        /// The destination square.
        to: Square,
        /// The promotion role, if any.
        promotion: Option<Role>,
    },
    /// An en passant capture, e.g. `e5xd6`.
    EnPassant {
        /// The origin square.
        from: Square,
        /// The destination square, which is empty before the capture.
        to: Square,
    },
    /// Castling, e.g. `O-O` or `O-O-O`.
    Castle {
        /// The king's origin square.
        king: Square,
        /// The castling rook's origin square.
        rook: Square,
    },
    /// A piece drop, e.g. `Q@f7`.
    Put {
        /// The role of the dropped piece.
        role: Role,
        /// The destination square.
        to: Square,
    },
}

impl Move {
    /// The role of the moved piece.
    #[must_use]
    pub const fn role(self) -> Role {
        match self {
            Move::Normal { role, .. } | Move::Put { role, .. } => role,
            Move::EnPassant { .. } => Role::Pawn,
            Move::Castle { .. } => Role::King,
        }
    }

    /// The origin square, or `None` for drops.
    #[must_use]
    pub const fn from(self) -> Option<Square> {
        match self {
            Move::Normal { from, .. } | Move::EnPassant { from, .. } => Some(from),
            Move::Castle { king, .. } => Some(king),
            Move::Put { .. } => None,
        }
    }

    /// The destination square. For castling this is the rook's square.
    #[must_use]
    pub const fn to(self) -> Square {
        match self {
            Move::Normal { to, .. } | Move::EnPassant { to, .. } | Move::Put { to, .. } => to,
            Move::Castle { rook, .. } => rook,
        }
    }

    /// The promotion role, if any.
    #[must_use]
    pub const fn promotion(self) -> Option<Role> {
        match self {
            Move::Normal { promotion, .. } => promotion,
            _ => None,
        }
    }

    /// Converts the move to UCI notation under the given castling mode.
    #[must_use]
    pub fn to_uci(self, mode: CastlingMode) -> UciMove {
        UciMove::from_move(self, mode)
    }

    /// Converts the move to the engine's representation.
    ///
    /// Returns `None` for [`Move::Put`], which `cozy_chess` cannot express.
    pub(crate) fn to_cozy(self) -> Option<cozy_chess::Move> {
        Some(cozy_chess::Move {
            from: self.from()?.into(),
            to: self.to().into(),
            promotion: self.promotion().map(cozy_chess::Piece::from),
        })
    }
}

/// A move in the Universal Chess Interface representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UciMove {
    /// A normal move, e.g. `e2e4` or `e7e8q`.
    Normal {
        /// The origin square.
        from: Square,
        /// The destination square.
        to: Square,
        /// The promotion role, if any.
        promotion: Option<Role>,
    },
    /// A piece drop, e.g. `Q@f7`.
    Put {
        /// The role of the dropped piece.
        role: Role,
        /// The destination square.
        to: Square,
    },
    /// The null move, `0000`.
    Null,
}

impl UciMove {
    /// Parses a move in UCI notation.
    ///
    /// # Errors
    ///
    /// Returns [`UciMoveError::Invalid`] if the text is not syntactically
    /// valid UCI.
    pub fn from_ascii(ascii: &[u8]) -> Result<UciMove, UciMoveError> {
        match ascii.len() {
            4 if ascii == b"0000" => Ok(UciMove::Null),
            4 if ascii[1] == b'@' => {
                let role = Role::from_char(ascii[0] as char).ok_or(UciMoveError::Invalid)?;
                let to = square_from_ascii(&ascii[2..4]).ok_or(UciMoveError::Invalid)?;
                Ok(UciMove::Put { role, to })
            }
            4 => {
                let from = square_from_ascii(&ascii[0..2]).ok_or(UciMoveError::Invalid)?;
                let to = square_from_ascii(&ascii[2..4]).ok_or(UciMoveError::Invalid)?;
                Ok(UciMove::Normal {
                    from,
                    to,
                    promotion: None,
                })
            }
            5 => {
                let from = square_from_ascii(&ascii[0..2]).ok_or(UciMoveError::Invalid)?;
                let to = square_from_ascii(&ascii[2..4]).ok_or(UciMoveError::Invalid)?;
                let promotion = promotion_from_ascii(ascii[4]).ok_or(UciMoveError::Invalid)?;
                Ok(UciMove::Normal {
                    from,
                    to,
                    promotion: Some(promotion),
                })
            }
            _ => Err(UciMoveError::Invalid),
        }
    }

    /// Converts a facade move to UCI notation.
    ///
    /// Under [`CastlingMode::Standard`] a castle is written as the king's
    /// destination; under [`CastlingMode::Chess960`] it is written as the
    /// king moving onto the rook's square.
    #[must_use]
    pub fn from_move(mv: Move, mode: CastlingMode) -> UciMove {
        match mv {
            Move::Normal {
                from,
                to,
                promotion,
                ..
            } => UciMove::Normal {
                from,
                to,
                promotion,
            },
            Move::EnPassant { from, to } => UciMove::Normal {
                from,
                to,
                promotion: None,
            },
            Move::Castle { king, rook } => {
                let to = match mode {
                    CastlingMode::Standard => {
                        let file = if rook.file() > king.file() {
                            File::G
                        } else {
                            File::C
                        };
                        Square::from_coords(file, king.rank())
                    }
                    CastlingMode::Chess960 => rook,
                };
                UciMove::Normal {
                    from: king,
                    to,
                    promotion: None,
                }
            }
            Move::Put { role, to } => UciMove::Put { role, to },
        }
    }

    /// Resolves the UCI move into a legal facade move in `pos`.
    ///
    /// # Errors
    ///
    /// Returns [`UciMoveError::Illegal`] if the move is not legal in `pos`, or
    /// for the null move, which has no facade representation.
    pub fn to_move(self, pos: &Chess) -> Result<Move, UciMoveError> {
        match self {
            UciMove::Null => Err(UciMoveError::Illegal),
            UciMove::Put { role, to } => Ok(Move::Put { role, to }),
            UciMove::Normal { .. } => {
                let text = self.to_string();
                let cozy = cozy_chess::util::parse_uci_move(pos.cozy_board(), &text)
                    .map_err(|_| UciMoveError::Illegal)?;
                let mv = pos.from_cozy(cozy);
                if pos.legal_moves().contains(&mv) {
                    Ok(mv)
                } else {
                    Err(UciMoveError::Illegal)
                }
            }
        }
    }

    /// The origin square, or `None` for drops and the null move.
    #[must_use]
    pub const fn from(self) -> Option<Square> {
        match self {
            UciMove::Normal { from, .. } => Some(from),
            UciMove::Put { .. } | UciMove::Null => None,
        }
    }

    /// The destination square, or `None` for the null move.
    #[must_use]
    pub const fn to(self) -> Option<Square> {
        match self {
            UciMove::Normal { to, .. } | UciMove::Put { to, .. } => Some(to),
            UciMove::Null => None,
        }
    }

    /// The promotion role, if any.
    #[must_use]
    pub const fn promotion(self) -> Option<Role> {
        match self {
            UciMove::Normal { promotion, .. } => promotion,
            UciMove::Put { .. } | UciMove::Null => None,
        }
    }
}

impl FromStr for UciMove {
    type Err = UciMoveError;

    fn from_str(text: &str) -> Result<UciMove, UciMoveError> {
        UciMove::from_ascii(text.as_bytes())
    }
}

impl fmt::Display for UciMove {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            UciMove::Normal {
                from,
                to,
                promotion,
            } => {
                write!(f, "{from}{to}")?;
                if let Some(promotion) = promotion {
                    write!(f, "{}", promotion.char().to_ascii_lowercase())?;
                }
                Ok(())
            }
            UciMove::Put { role, to } => write!(f, "{}@{to}", role.char()),
            UciMove::Null => f.write_str("0000"),
        }
    }
}

/// Parses two ASCII bytes as a square.
fn square_from_ascii(ascii: &[u8]) -> Option<Square> {
    std::str::from_utf8(ascii).ok()?.parse().ok()
}

/// Parses a UCI promotion byte, which is limited to queen, rook, bishop, and
/// knight in either case.
fn promotion_from_ascii(ascii: u8) -> Option<Role> {
    match ascii.to_ascii_lowercase() {
        b'q' => Some(Role::Queen),
        b'r' => Some(Role::Rook),
        b'b' => Some(Role::Bishop),
        b'n' => Some(Role::Knight),
        _ => None,
    }
}
