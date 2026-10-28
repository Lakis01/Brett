//! Standard Algebraic Notation: a resolved [`San`], and a [`SanPlus`] that
//! also carries the exact text it was read from.

use std::fmt;

use crate::chess::chess::{Chess, Position};
use crate::chess::r#move::Move;
use crate::chess::types::Role;

/// Error returned when SAN cannot be resolved to a legal move.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SanError {
    /// The SAN does not name a legal move in the given position.
    IllegalSan,
}

impl fmt::Display for SanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SanError::IllegalSan => "illegal san",
        })
    }
}

impl std::error::Error for SanError {}

/// A move in Standard Algebraic Notation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum San {
    /// A move that has already been resolved against a position.
    Move(Move),
    /// SAN text that has not been resolved against a position yet.
    Raw(String),
    /// A null move, written `--`.
    Null,
}

impl San {
    /// Resolves the SAN into a legal move in `pos`.
    ///
    /// # Errors
    ///
    /// Returns [`SanError::IllegalSan`] if the SAN does not name a legal move,
    /// and for the null move, which cannot be played.
    pub fn to_move(&self, pos: &Chess) -> Result<Move, SanError> {
        match self {
            San::Move(mv) => {
                if pos.legal_moves().contains(mv) {
                    Ok(*mv)
                } else {
                    Err(SanError::IllegalSan)
                }
            }
            San::Null => Err(SanError::IllegalSan),
            San::Raw(text) => {
                let text = normalize_san(text);
                let cozy = cozy_chess::util::parse_san_move(pos.cozy_board(), &text)
                    .map_err(|_| SanError::IllegalSan)?;
                let mv = pos.from_cozy(cozy);
                if pos.legal_moves().contains(&mv) {
                    Ok(mv)
                } else {
                    Err(SanError::IllegalSan)
                }
            }
        }
    }
}

/// A [`San`] together with the text it was read from.
///
/// Keeping the original text preserves the author's notation (for example a
/// `+` suffix that is not actually check) while [`San::to_move`] still resolves
/// the move against a position.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SanPlus {
    /// The resolved or unresolved move.
    pub san: San,
    /// The text as read, normalized only for null moves.
    pub text: String,
}

impl SanPlus {
    /// Parses ASCII bytes as SAN, keeping the trimmed text.
    ///
    /// # Errors
    ///
    /// Returns [`SanError::IllegalSan`] if the input is empty after trimming.
    pub fn from_ascii(ascii: &[u8]) -> Result<SanPlus, SanError> {
        let text = String::from_utf8_lossy(ascii);
        let text = text.trim_ascii();
        if text.is_empty() {
            return Err(SanError::IllegalSan);
        }
        if matches!(text, "--" | "Z0" | "z0") {
            return Ok(SanPlus {
                san: San::Null,
                text: "--".to_owned(),
            });
        }
        Ok(SanPlus {
            san: San::Raw(text.to_owned()),
            text: text.to_owned(),
        })
    }

    /// Writes `mv` in SAN, including a check or checkmate suffix.
    ///
    /// `pos` is the position before the move. An illegal move or a drop has no
    /// SAN in an orthodox position; such a move is rendered without a suffix
    /// instead of panicking.
    #[must_use]
    pub fn from_move(pos: Chess, mv: Move) -> SanPlus {
        let text = match mv.to_cozy() {
            Some(cozy) if pos.cozy_board().is_legal(cozy) => {
                cozy_chess::util::display_san_move(pos.cozy_board(), cozy).to_string()
            }
            _ => fallback_san(mv),
        };
        SanPlus {
            san: San::Move(mv),
            text,
        }
    }

    /// Resolves the move in `pos`.
    ///
    /// # Errors
    ///
    /// Returns [`SanError::IllegalSan`] if the move is not legal in `pos`.
    pub fn to_move(&self, pos: &Chess) -> Result<Move, SanError> {
        self.san.to_move(pos)
    }
}

impl fmt::Display for SanPlus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// Normalizes the notations that PGN files use but that SAN parsers do not
/// accept: castling written with zeroes, and a trailing en passant marker.
fn normalize_san(text: &str) -> String {
    let mut text = text.trim();
    if let Some(stripped) = text.strip_suffix("e.p.") {
        text = stripped.trim_end();
    } else if let Some(stripped) = text.strip_suffix("ep") {
        text = stripped.trim_end();
    }
    if let Some(rest) = text.strip_prefix("0-0-0") {
        format!("O-O-O{rest}")
    } else if let Some(rest) = text.strip_prefix("0-0") {
        format!("O-O{rest}")
    } else {
        text.to_owned()
    }
}

/// Renders a move without consulting a position, for moves that cannot be
/// displayed through `cozy_chess` (drops and illegal moves).
fn fallback_san(mv: Move) -> String {
    match mv {
        Move::Normal {
            role,
            from,
            capture,
            to,
            promotion,
        } => {
            let mut text = String::new();
            if role != Role::Pawn {
                text.push(role.char());
            }
            text.push_str(&from.to_string());
            if capture.is_some() {
                text.push('x');
            }
            text.push_str(&to.to_string());
            if let Some(promotion) = promotion {
                text.push('=');
                text.push(promotion.char());
            }
            text
        }
        Move::EnPassant { from, to } => format!("{}x{to}", from.file()),
        Move::Castle { king, rook } => {
            if rook.file() > king.file() {
                "O-O".to_owned()
            } else {
                "O-O-O".to_owned()
            }
        }
        Move::Put { role, to } => format!("{}@{to}", role.char()),
    }
}
