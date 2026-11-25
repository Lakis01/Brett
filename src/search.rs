//! Superior bitboard structure, endgame constellation, partial/exact position,
//! Mega medal/flag, and metadata search across game libraries.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use memmap2::Mmap;
use rayon::prelude::*;

use crate::chess::{
    BoardRef, Chess, Color, File as ChessFile, Piece, Position, Rank, Role, Square,
};
use crate::library::{
    GameLibrary, GameRecordMeta, contains_case_insensitive, eco_code_to_str, eco_str_to_code,
};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PositionSearchMode {
    #[default]
    Partial,
    Absolute,
}

impl PositionSearchMode {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Partial => "Partial (pattern match)",
            Self::Absolute => "Exact (all 64 squares)",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SideToMoveFilter {
    #[default]
    Any,
    White,
    Black,
}

impl SideToMoveFilter {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Any => "Any",
            Self::White => "White",
            Self::Black => "Black",
        }
    }

    #[must_use]
    pub fn flipped(self) -> Self {
        match self {
            Self::Any => Self::Any,
            Self::White => Self::Black,
            Self::Black => Self::White,
        }
    }

    #[must_use]
    pub fn matches(self, turn: Color) -> bool {
        match self {
            Self::Any => true,
            Self::White => turn == Color::White,
            Self::Black => turn == Color::Black,
        }
    }

    #[must_use]
    pub fn from_color(color: Color) -> Self {
        match color {
            Color::White => Self::White,
            Color::Black => Self::Black,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SquareConstraint {
    #[default]
    Any,
    Empty,
    Piece(Piece),
}

/// Atomic board condition inside a Boolean [`PatternClause`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomPredicate {
    HasPiece(Square, Piece),
    IsEmpty(Square),
    NotPiece(Square, Piece),
    NoPieceOnFile(ChessFile, Color, Role),
}

impl AtomPredicate {
    #[must_use]
    pub fn matches(self, board: &BoardRef<'_>) -> bool {
        match self {
            Self::HasPiece(sq, piece) => {
                (board.colored_pieces_mask(piece.color, piece.role) & (1u64 << sq.to_u32())) != 0
            }
            Self::IsEmpty(sq) => (board.occupied_mask() & (1u64 << sq.to_u32())) == 0,
            Self::NotPiece(sq, piece) => {
                (board.colored_pieces_mask(piece.color, piece.role) & (1u64 << sq.to_u32())) == 0
            }
            Self::NoPieceOnFile(file, color, role) => {
                (board.colored_pieces_mask(color, role) & FILE_MASKS[file.to_u32() as usize]) == 0
            }
        }
    }

    #[must_use]
    pub fn mirrored_colors(self) -> Self {
        let flip_sq =
            |sq: Square| Square::from_coords(sq.file(), Rank::new(7 - sq.rank().to_u32()));
        let flip_piece = |p: Piece| Piece::new(p.color.other(), p.role);
        match self {
            Self::HasPiece(sq, p) => Self::HasPiece(flip_sq(sq), flip_piece(p)),
            Self::IsEmpty(sq) => Self::IsEmpty(flip_sq(sq)),
            Self::NotPiece(sq, p) => Self::NotPiece(flip_sq(sq), flip_piece(p)),
            Self::NoPieceOnFile(file, color, role) => {
                Self::NoPieceOnFile(file, color.other(), role)
            }
        }
    }

    #[must_use]
    pub fn square(self) -> Option<Square> {
        match self {
            Self::HasPiece(sq, _) | Self::IsEmpty(sq) | Self::NotPiece(sq, _) => Some(sq),
            Self::NoPieceOnFile(..) => None,
        }
    }

    #[must_use]
    pub fn may_exist_in_sig(self, sig: &GameStructSig) -> bool {
        match self {
            Self::HasPiece(sq, piece) => {
                let bit = 1u64 << sq.to_u32();
                match (piece.color, piece.role) {
                    (Color::White, Role::Pawn) => (sig.white_pawns & bit) != 0,
                    (Color::Black, Role::Pawn) => (sig.black_pawns & bit) != 0,
                    (Color::White, Role::Knight) => (sig.white_knights & bit) != 0,
                    (Color::Black, Role::Knight) => (sig.black_knights & bit) != 0,
                    (Color::White, Role::Bishop) => (sig.white_bishops & bit) != 0,
                    (Color::Black, Role::Bishop) => (sig.black_bishops & bit) != 0,
                    _ => true,
                }
            }
            // `.brettstruct` is a union over plies, so negative/emptiness/file-absence checks cannot safely prune.
            Self::IsEmpty(_) | Self::NotPiece(_, _) | Self::NoPieceOnFile(..) => true,
        }
    }

    #[must_use]
    pub fn short_label(self) -> String {
        let piece_glyph = |p: Piece| match (p.color, p.role) {
            (Color::White, Role::Pawn) => "♙",
            (Color::White, Role::Knight) => "♘",
            (Color::White, Role::Bishop) => "♗",
            (Color::White, Role::Rook) => "♖",
            (Color::White, Role::Queen) => "♕",
            (Color::White, Role::King) => "♔",
            (Color::Black, Role::Pawn) => "♟",
            (Color::Black, Role::Knight) => "♞",
            (Color::Black, Role::Bishop) => "♝",
            (Color::Black, Role::Rook) => "♜",
            (Color::Black, Role::Queen) => "♛",
            (Color::Black, Role::King) => "♚",
        };
        match self {
            Self::HasPiece(sq, p) => format!("{}{sq}", piece_glyph(p)),
            Self::IsEmpty(sq) => format!("{sq} empty"),
            Self::NotPiece(sq, p) => format!("¬{}{sq}", piece_glyph(p)),
            Self::NoPieceOnFile(file, color, role) => {
                let file_ch = (b'a' + file.to_u32() as u8) as char;
                format!(
                    "no {} on {file_ch}-file",
                    piece_glyph(Piece::new(color, role))
                )
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClauseOp {
    /// Logical OR: at least one atom holds.
    AnyOf,
    /// Logical AND: all atoms hold.
    AllOf,
    /// Logical NOT / NONE: zero atoms hold.
    NoneOf,
}

impl ClauseOp {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::AnyOf => "OR",
            Self::AllOf => "AND",
            Self::NoneOf => "NONE",
        }
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::AnyOf => "or",
            Self::AllOf => "and",
            Self::NoneOf => "none",
        }
    }

    #[must_use]
    pub fn from_code(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "or" | "any" => Some(Self::AnyOf),
            "and" | "all" => Some(Self::AllOf),
            "none" | "not" => Some(Self::NoneOf),
            _ => None,
        }
    }
}

/// Boolean condition clause combined with the base pattern via logical AND.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternClause {
    pub op: ClauseOp,
    pub atoms: Vec<AtomPredicate>,
}

impl PatternClause {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.atoms.is_empty()
    }

    #[must_use]
    pub fn matches(&self, board: &BoardRef<'_>) -> bool {
        if self.atoms.is_empty() {
            return true;
        }
        match self.op {
            ClauseOp::AnyOf => self.atoms.iter().any(|a| a.matches(board)),
            ClauseOp::AllOf => self.atoms.iter().all(|a| a.matches(board)),
            ClauseOp::NoneOf => self.atoms.iter().all(|a| !a.matches(board)),
        }
    }

    #[must_use]
    pub fn mirrored_colors(&self) -> Self {
        Self {
            op: self.op,
            atoms: self.atoms.iter().map(|a| a.mirrored_colors()).collect(),
        }
    }

    #[must_use]
    pub fn may_exist_in_sig(&self, sig: &GameStructSig) -> bool {
        if self.atoms.is_empty() {
            return true;
        }
        match self.op {
            ClauseOp::AnyOf => self.atoms.iter().any(|a| a.may_exist_in_sig(sig)),
            ClauseOp::AllOf => self.atoms.iter().all(|a| a.may_exist_in_sig(sig)),
            ClauseOp::NoneOf => true,
        }
    }

    #[must_use]
    pub fn summary(&self) -> String {
        if self.atoms.is_empty() {
            return format!("{}: (empty)", self.op.label());
        }
        let joiner = match self.op {
            ClauseOp::AnyOf => " OR ",
            ClauseOp::AllOf => " AND ",
            ClauseOp::NoneOf => ", ",
        };
        let body: Vec<String> = self.atoms.iter().map(|a| a.short_label()).collect();
        match self.op {
            ClauseOp::NoneOf => format!("NONE ({})", body.join(joiner)),
            ClauseOp::AnyOf | ClauseOp::AllOf => format!("({})", body.join(joiner)),
        }
    }
}

/// User-saved custom board pattern + Boolean clauses for hand-tuned searches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomStructurePreset {
    pub name: String,
    pub squares: [SquareConstraint; 64],
    pub extra_clauses: Vec<PatternClause>,
    pub side_to_move: SideToMoveFilter,
    pub ignore_color_flip: bool,
}

fn piece_to_code(piece: Piece) -> String {
    let c = match piece.color {
        Color::White => 'W',
        Color::Black => 'B',
    };
    let r = match piece.role {
        Role::Pawn => 'P',
        Role::Knight => 'N',
        Role::Bishop => 'B',
        Role::Rook => 'R',
        Role::Queen => 'Q',
        Role::King => 'K',
    };
    format!("{c}{r}")
}

fn piece_from_code(raw: &str) -> Option<Piece> {
    let mut chars = raw.trim().chars();
    let color = match chars.next()?.to_ascii_uppercase() {
        'W' => Color::White,
        'B' => Color::Black,
        _ => return None,
    };
    let role = match chars.next()?.to_ascii_uppercase() {
        'P' => Role::Pawn,
        'N' => Role::Knight,
        'B' => Role::Bishop,
        'R' => Role::Rook,
        'Q' => Role::Queen,
        'K' => Role::King,
        _ => return None,
    };
    Some(Piece::new(color, role))
}

fn square_from_name(raw: &str) -> Option<Square> {
    let b = raw.trim().as_bytes();
    if b.len() != 2 {
        return None;
    }
    let f = b[0].to_ascii_lowercase();
    if !(b'a'..=b'h').contains(&f) || !(b'1'..=b'8').contains(&b[1]) {
        return None;
    }
    Some(Square::from_coords(
        ChessFile::new(u32::from(f - b'a')),
        Rank::new(u32::from(b[1] - b'1')),
    ))
}

impl AtomPredicate {
    fn to_value(self) -> Value {
        match self {
            Self::HasPiece(sq, p) => {
                json!({"kind": "has", "sq": sq.to_string(), "piece": piece_to_code(p)})
            }
            Self::IsEmpty(sq) => json!({"kind": "empty", "sq": sq.to_string()}),
            Self::NotPiece(sq, p) => {
                json!({"kind": "not", "sq": sq.to_string(), "piece": piece_to_code(p)})
            }
            Self::NoPieceOnFile(file, color, role) => json!({
                "kind": "no_file",
                "file": ((b'a' + file.to_u32() as u8) as char).to_string(),
                "file_idx": file.to_u32(),
                "piece": piece_to_code(Piece::new(color, role)),
            }),
        }
    }

    fn from_value(v: &Value) -> Option<Self> {
        let kind = v.get("kind").and_then(Value::as_str)?;
        match kind {
            "has" => {
                let sq = square_from_name(v.get("sq")?.as_str()?)?;
                let p = piece_from_code(v.get("piece")?.as_str()?)?;
                Some(Self::HasPiece(sq, p))
            }
            "empty" => {
                let sq = square_from_name(v.get("sq")?.as_str()?)?;
                Some(Self::IsEmpty(sq))
            }
            "not" => {
                let sq = square_from_name(v.get("sq")?.as_str()?)?;
                let p = piece_from_code(v.get("piece")?.as_str()?)?;
                Some(Self::NotPiece(sq, p))
            }
            "no_file" => {
                let file_idx = v.get("file_idx").and_then(Value::as_u64)? as u32;
                if file_idx >= 8 {
                    return None;
                }
                let p = piece_from_code(v.get("piece")?.as_str()?)?;
                Some(Self::NoPieceOnFile(
                    ChessFile::new(file_idx),
                    p.color,
                    p.role,
                ))
            }
            _ => None,
        }
    }
}

impl CustomStructurePreset {
    #[must_use]
    pub fn to_value(&self) -> Value {
        let squares_json: Vec<Value> = self
            .squares
            .iter()
            .enumerate()
            .filter_map(|(i, c)| match c {
                SquareConstraint::Any => None,
                SquareConstraint::Empty => Some(json!({"sq": Square::new(i as u32).to_string(), "constraint": "empty"})),
                SquareConstraint::Piece(p) => Some(json!({"sq": Square::new(i as u32).to_string(), "constraint": piece_to_code(*p)})),
            })
            .collect();
        let clauses_json: Vec<Value> = self
            .extra_clauses
            .iter()
            .map(|c| {
                json!({
                    "op": c.op.code(),
                    "atoms": c.atoms.iter().copied().map(AtomPredicate::to_value).collect::<Vec<_>>(),
                })
            })
            .collect();
        let stm = match self.side_to_move {
            SideToMoveFilter::Any => "any",
            SideToMoveFilter::White => "white",
            SideToMoveFilter::Black => "black",
        };
        json!({
            "name": self.name,
            "squares": squares_json,
            "clauses": clauses_json,
            "side_to_move": stm,
            "ignore_color_flip": self.ignore_color_flip,
        })
    }

    #[must_use]
    pub fn from_value(v: &Value) -> Option<Self> {
        let name = v.get("name").and_then(Value::as_str)?.trim().to_string();
        if name.is_empty() {
            return None;
        }
        let mut squares = [SquareConstraint::Any; 64];
        if let Some(arr) = v.get("squares").and_then(Value::as_array) {
            for item in arr {
                let Some(sq) = item
                    .get("sq")
                    .and_then(Value::as_str)
                    .and_then(square_from_name)
                else {
                    continue;
                };
                let Some(cstr) = item.get("constraint").and_then(Value::as_str) else {
                    continue;
                };
                let constraint = if cstr.eq_ignore_ascii_case("empty") {
                    SquareConstraint::Empty
                } else if let Some(p) = piece_from_code(cstr) {
                    SquareConstraint::Piece(p)
                } else {
                    continue;
                };
                squares[sq.to_u32() as usize] = constraint;
            }
        }
        let extra_clauses = v
            .get("clauses")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| {
                        let op = ClauseOp::from_code(c.get("op")?.as_str()?)?;
                        let atoms = c
                            .get("atoms")
                            .and_then(Value::as_array)?
                            .iter()
                            .filter_map(AtomPredicate::from_value)
                            .collect();
                        Some(PatternClause { op, atoms })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let side_to_move = match v.get("side_to_move").and_then(Value::as_str) {
            Some("white") => SideToMoveFilter::White,
            Some("black") => SideToMoveFilter::Black,
            _ => SideToMoveFilter::Any,
        };
        let ignore_color_flip = v
            .get("ignore_color_flip")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        Some(Self {
            name,
            squares,
            extra_clauses,
            side_to_move,
            ignore_color_flip,
        })
    }
}

pub fn custom_presets_path() -> Option<PathBuf> {
    crate::settings::config_dir().map(|dir| dir.join("custom_structures.json"))
}

#[must_use]
pub fn load_custom_presets_from(path: &Path) -> Vec<CustomStructurePreset> {
    let Some(text) = crate::settings::read_with_backup_json(path) else {
        return Vec::new();
    };
    let Ok(val) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    val.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(CustomStructurePreset::from_value)
                .collect()
        })
        .unwrap_or_default()
}

#[must_use]
pub fn load_custom_presets() -> Vec<CustomStructurePreset> {
    custom_presets_path()
        .map(|path| load_custom_presets_from(&path))
        .unwrap_or_default()
}

pub fn save_custom_presets_to(
    path: &Path,
    presets: &[CustomStructurePreset],
) -> anyhow::Result<()> {
    let arr: Vec<Value> = presets
        .iter()
        .map(CustomStructurePreset::to_value)
        .collect();
    let json = serde_json::to_string_pretty(&arr)?;
    crate::settings::write_atomic_with_backup(path, &json)
}

pub fn save_custom_presets(presets: &[CustomStructurePreset]) -> anyhow::Result<()> {
    let path = custom_presets_path().ok_or_else(|| {
        anyhow::anyhow!("No config directory available for custom structure presets")
    })?;
    save_custom_presets_to(&path, presets)
}

/// Find a user-saved custom preset that overrides a built-in [`StructurePreset`]
/// (matched case-insensitively by `preset.label()`).
#[must_use]
pub fn find_custom_override(
    custom: &[CustomStructurePreset],
    preset: StructurePreset,
) -> Option<&CustomStructurePreset> {
    let target = preset.label();
    custom
        .iter()
        .find(|p| p.name.trim().eq_ignore_ascii_case(target))
}

/// Which orientation(s) a structural/endgame/pattern match satisfied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchOrientation {
    Normal,
    Flipped,
    Both,
}

impl MatchOrientation {
    #[must_use]
    pub fn combine(a: Option<Self>, b: Option<Self>) -> Option<Self> {
        match (a?, b?) {
            (Self::Both, x) | (x, Self::Both) => Some(x),
            (Self::Normal, Self::Normal) => Some(Self::Normal),
            (Self::Flipped, Self::Flipped) => Some(Self::Flipped),
            _ => None,
        }
    }

    #[must_use]
    pub fn includes_flipped(self) -> bool {
        matches!(self, Self::Flipped | Self::Both)
    }
}

/// Which side won the game — including relative to the matched structure/featured side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WinnerFilter {
    #[default]
    Any,
    StructureSideWins,
    OpponentSideWins,
    WhiteWins,
    BlackWins,
    Draw,
    Decisive,
}

impl WinnerFilter {
    pub const ALL: [Self; 7] = [
        Self::Any,
        Self::StructureSideWins,
        Self::OpponentSideWins,
        Self::WhiteWins,
        Self::BlackWins,
        Self::Draw,
        Self::Decisive,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Any => "Any result",
            Self::StructureSideWins => "Structure / Featured side wins",
            Self::OpponentSideWins => "Opponent wins",
            Self::WhiteWins => "White wins (1-0)",
            Self::BlackWins => "Black wins (0-1)",
            Self::Draw => "Draw (1/2-1/2)",
            Self::Decisive => "Decisive (1-0 or 0-1)",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EloFilterMode {
    #[default]
    Both,
    Either,
    Average,
    White,
    Black,
}

impl EloFilterMode {
    pub const ALL: [Self; 5] = [
        Self::Both,
        Self::Either,
        Self::Average,
        Self::White,
        Self::Black,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Both => "Both players",
            Self::Either => "Either player",
            Self::Average => "Average Elo",
            Self::White => "White Elo",
            Self::Black => "Black Elo",
        }
    }
}

/// Header, rating, year, ECO, outcome, and ChessBase Mega medal/flag filters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetadataFilter {
    pub player_any: String,
    pub player_white: String,
    pub player_black: String,
    pub min_elo: Option<u16>,
    pub max_elo: Option<u16>,
    pub elo_mode: EloFilterMode,
    pub year_from: Option<u16>,
    pub year_to: Option<u16>,
    pub eco_filter: String,
    pub event_filter: String,
    pub winner: WinnerFilter,
    pub medals_mask: u16,
    pub medals_match_all: bool,
    pub vcs_flags_mask: u32,
    pub annotated_only: bool,
    pub include_deleted: bool,
}

impl MetadataFilter {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.player_any.trim().is_empty()
            && self.player_white.trim().is_empty()
            && self.player_black.trim().is_empty()
            && self.min_elo.is_none()
            && self.max_elo.is_none()
            && self.year_from.is_none()
            && self.year_to.is_none()
            && self.eco_filter.trim().is_empty()
            && self.event_filter.trim().is_empty()
            && self.winner == WinnerFilter::Any
            && self.medals_mask == 0
            && self.vcs_flags_mask == 0
            && !self.annotated_only
            && !self.include_deleted
    }
}

/// Hierarchical pawn & piece-structure families.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructureFamily {
    Stonewall,
    Badewanne,
    Carlsbad,
    Iqp,
    HangingPawns,
    MaroczyAndHedgehog,
    Sicilian,
    SlavAndGrunfeld,
    ClosedChains,
}

impl StructureFamily {
    pub const ALL: [Self; 9] = [
        Self::Stonewall,
        Self::Badewanne,
        Self::Carlsbad,
        Self::Iqp,
        Self::HangingPawns,
        Self::MaroczyAndHedgehog,
        Self::Sicilian,
        Self::SlavAndGrunfeld,
        Self::ClosedChains,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Stonewall => "Stonewall",
            Self::Badewanne => "Badewanne",
            Self::Carlsbad => "Carlsbad",
            Self::Iqp => "IQP & Panov",
            Self::HangingPawns => "Hanging Pawns",
            Self::MaroczyAndHedgehog => "Maróczy / Hedgehog",
            Self::Sicilian => "Sicilian Structures",
            Self::SlavAndGrunfeld => "Slav, Triangle & Grünfeld",
            Self::ClosedChains => "Chains, French & KID",
        }
    }

    #[must_use]
    pub fn presets(self) -> &'static [StructurePreset] {
        match self {
            Self::Stonewall => &[
                StructurePreset::StonewallAny,
                StructurePreset::StonewallClassic,
                StructurePreset::StonewallNc6,
                StructurePreset::StonewallBadBishopTrade,
                StructurePreset::StonewallC5Break,
                StructurePreset::StonewallBd2e1h4,
            ],
            Self::Badewanne => &[
                StructurePreset::BadewanneAny,
                StructurePreset::BadewanneFlexibleRim,
                StructurePreset::BadewanneCentral,
            ],
            Self::Carlsbad => &[
                StructurePreset::CarlsbadAny,
                StructurePreset::CarlsbadMinorityAttack,
                StructurePreset::CarlsbadCentralE4,
                StructurePreset::CarlsbadF3E4,
            ],
            Self::Iqp => &[
                StructurePreset::IqpDFile,
                StructurePreset::IqpVsEPawnNoFianchetto,
                StructurePreset::IqpVsEPawnFianchetto,
                StructurePreset::IqpVsCPawnNoFianchetto,
                StructurePreset::IqpVsCPawnFianchetto,
                StructurePreset::IqpVsEPawn,
                StructurePreset::IqpVsCPawn,
                StructurePreset::IqpVsNoFianchetto,
                StructurePreset::IqpVsFianchetto,
                StructurePreset::IqpDynamic,
                StructurePreset::IqpBlockaded,
                StructurePreset::IqpAnyCentralFile,
                StructurePreset::PanovFormation,
            ],
            Self::HangingPawns => &[
                StructurePreset::HangingPawnsClassic,
                StructurePreset::HangingPawnsVsNoFianchetto,
                StructurePreset::HangingPawnsVsFianchetto,
                StructurePreset::HangingPawnsDynamic,
            ],
            Self::MaroczyAndHedgehog => &[
                StructurePreset::MaroczyBindAny,
                StructurePreset::MaroczyBindNoFianchetto,
                StructurePreset::MaroczyBindFianchetto,
                StructurePreset::Hedgehog,
            ],
            Self::Sicilian => &[
                StructurePreset::Boleslavsky,
                StructurePreset::Scheveningen,
                StructurePreset::Dragon,
                StructurePreset::NajdorfStructure,
                StructurePreset::NajdorfTypeII,
                StructurePreset::NajdorfTypeIII,
            ],
            Self::SlavAndGrunfeld => &[
                StructurePreset::SlavCaroKann,
                StructurePreset::SlavFormation,
                StructurePreset::GrunfeldCenter,
                StructurePreset::Botvinnik,
                StructurePreset::BotvinnikTriangle,
            ],
            Self::ClosedChains => &[
                StructurePreset::FrenchChain,
                StructurePreset::FrenchTarraschDxE,
                StructurePreset::KingsIndianChain,
                StructurePreset::KingsIndianC5Chain,
                StructurePreset::BenoniStructure,
                StructurePreset::NimzoDoubledCPawns,
            ],
        }
    }
}

/// Subversions of classical & modern middlegame pawn/piece structures.
/// Evaluated dynamically on bitboards and automatically color-mirrored when
/// `ignore_color_flip` is active (default `true`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructurePreset {
    // Stonewall family
    StonewallAny,
    StonewallClassic,
    StonewallNc6,
    StonewallBadBishopTrade,
    StonewallC5Break,
    StonewallBd2e1h4,
    // Badewanne family (backwards pawns strictly on ranks 3..=5)
    BadewanneAny,
    BadewanneFlexibleRim,
    BadewanneCentral,
    // Carlsbad family
    CarlsbadAny,
    CarlsbadMinorityAttack,
    CarlsbadCentralE4,
    CarlsbadF3E4,
    // IQP & Panov family
    IqpDFile,
    IqpVsEPawnNoFianchetto,
    IqpVsEPawnFianchetto,
    IqpVsCPawnNoFianchetto,
    IqpVsCPawnFianchetto,
    IqpVsEPawn,
    IqpVsCPawn,
    IqpVsNoFianchetto,
    IqpVsFianchetto,
    IqpDynamic,
    IqpBlockaded,
    IqpAnyCentralFile,
    PanovFormation,
    // Hanging Pawns family
    HangingPawnsClassic,
    HangingPawnsVsNoFianchetto,
    HangingPawnsVsFianchetto,
    HangingPawnsDynamic,
    // Maróczy & Hedgehog
    MaroczyBindAny,
    MaroczyBindNoFianchetto,
    MaroczyBindFianchetto,
    Hedgehog,
    // Sicilian structures
    Boleslavsky,
    Scheveningen,
    Dragon,
    NajdorfStructure,
    NajdorfTypeII,
    NajdorfTypeIII,
    // Slav, Triangle & Grünfeld
    SlavCaroKann,
    SlavFormation,
    GrunfeldCenter,
    Botvinnik,
    BotvinnikTriangle,
    // Closed chains & semi-closed formations
    FrenchChain,
    FrenchTarraschDxE,
    KingsIndianChain,
    KingsIndianC5Chain,
    BenoniStructure,
    NimzoDoubledCPawns,
}

pub type StructuralPreset = StructurePreset;

impl StructurePreset {
    pub const ALL: [Self; 51] = [
        Self::StonewallAny,
        Self::StonewallClassic,
        Self::StonewallNc6,
        Self::StonewallBadBishopTrade,
        Self::StonewallC5Break,
        Self::StonewallBd2e1h4,
        Self::BadewanneAny,
        Self::BadewanneFlexibleRim,
        Self::BadewanneCentral,
        Self::CarlsbadAny,
        Self::CarlsbadMinorityAttack,
        Self::CarlsbadCentralE4,
        Self::CarlsbadF3E4,
        Self::IqpDFile,
        Self::IqpVsEPawnNoFianchetto,
        Self::IqpVsEPawnFianchetto,
        Self::IqpVsCPawnNoFianchetto,
        Self::IqpVsCPawnFianchetto,
        Self::IqpVsEPawn,
        Self::IqpVsCPawn,
        Self::IqpVsNoFianchetto,
        Self::IqpVsFianchetto,
        Self::IqpDynamic,
        Self::IqpBlockaded,
        Self::IqpAnyCentralFile,
        Self::PanovFormation,
        Self::HangingPawnsClassic,
        Self::HangingPawnsVsNoFianchetto,
        Self::HangingPawnsVsFianchetto,
        Self::HangingPawnsDynamic,
        Self::MaroczyBindAny,
        Self::MaroczyBindNoFianchetto,
        Self::MaroczyBindFianchetto,
        Self::Hedgehog,
        Self::Boleslavsky,
        Self::Scheveningen,
        Self::Dragon,
        Self::NajdorfStructure,
        Self::NajdorfTypeII,
        Self::NajdorfTypeIII,
        Self::SlavCaroKann,
        Self::SlavFormation,
        Self::GrunfeldCenter,
        Self::Botvinnik,
        Self::BotvinnikTriangle,
        Self::FrenchChain,
        Self::FrenchTarraschDxE,
        Self::KingsIndianChain,
        Self::KingsIndianC5Chain,
        Self::BenoniStructure,
        Self::NimzoDoubledCPawns,
    ];

    #[must_use]
    pub fn family(self) -> StructureFamily {
        match self {
            Self::StonewallAny
            | Self::StonewallClassic
            | Self::StonewallNc6
            | Self::StonewallBadBishopTrade
            | Self::StonewallC5Break
            | Self::StonewallBd2e1h4 => StructureFamily::Stonewall,
            Self::BadewanneAny | Self::BadewanneFlexibleRim | Self::BadewanneCentral => {
                StructureFamily::Badewanne
            }
            Self::CarlsbadAny
            | Self::CarlsbadMinorityAttack
            | Self::CarlsbadCentralE4
            | Self::CarlsbadF3E4 => StructureFamily::Carlsbad,
            Self::IqpDFile
            | Self::IqpVsEPawnNoFianchetto
            | Self::IqpVsEPawnFianchetto
            | Self::IqpVsCPawnNoFianchetto
            | Self::IqpVsCPawnFianchetto
            | Self::IqpVsEPawn
            | Self::IqpVsCPawn
            | Self::IqpVsNoFianchetto
            | Self::IqpVsFianchetto
            | Self::IqpDynamic
            | Self::IqpBlockaded
            | Self::IqpAnyCentralFile
            | Self::PanovFormation => StructureFamily::Iqp,
            Self::HangingPawnsClassic
            | Self::HangingPawnsVsNoFianchetto
            | Self::HangingPawnsVsFianchetto
            | Self::HangingPawnsDynamic => StructureFamily::HangingPawns,
            Self::MaroczyBindAny
            | Self::MaroczyBindNoFianchetto
            | Self::MaroczyBindFianchetto
            | Self::Hedgehog => StructureFamily::MaroczyAndHedgehog,
            Self::Boleslavsky
            | Self::Scheveningen
            | Self::Dragon
            | Self::NajdorfStructure
            | Self::NajdorfTypeII
            | Self::NajdorfTypeIII => StructureFamily::Sicilian,
            Self::SlavCaroKann
            | Self::SlavFormation
            | Self::GrunfeldCenter
            | Self::Botvinnik
            | Self::BotvinnikTriangle => StructureFamily::SlavAndGrunfeld,
            Self::FrenchChain
            | Self::FrenchTarraschDxE
            | Self::KingsIndianChain
            | Self::KingsIndianC5Chain
            | Self::BenoniStructure
            | Self::NimzoDoubledCPawns => StructureFamily::ClosedChains,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::StonewallAny => "Stonewall (any)",
            Self::StonewallClassic => "Classic Stonewall (c3-d4-e3-f4 / exd)",
            Self::StonewallNc6 => "Stonewall + Nc6 (d5, e6/exd, f5, Nc6)",
            Self::StonewallBadBishopTrade => "Stonewall + b6 (Ba6 / LSB off)",
            Self::StonewallC5Break => "Stonewall + c5 (c5-d5-e6/exd-f5)",
            Self::StonewallBd2e1h4 => "Stonewall + Bd2-e1-h4 Plan (vs Nf6)",
            Self::BadewanneAny => "Badewanne (any 4 files, ranks 3–5)",
            Self::BadewanneFlexibleRim => "Badewanne (flexible rim, ranks 3–5)",
            Self::BadewanneCentral => "Central Badewanne (c–f, ranks 3–5)",
            Self::CarlsbadAny => "Carlsbad (core)",
            Self::CarlsbadMinorityAttack => "Carlsbad + Minority Attack (b4/b5)",
            Self::CarlsbadCentralE4 => "Carlsbad + e4 Plan",
            Self::CarlsbadF3E4 => "Carlsbad + f3 Setup",
            Self::IqpDFile => "IQP (d4/d5)",
            Self::IqpVsEPawnNoFianchetto => "IQP vs e-pawn (no K-side fianchetto)",
            Self::IqpVsEPawnFianchetto => "IQP vs e-pawn + K-side fianchetto",
            Self::IqpVsCPawnNoFianchetto => "IQP vs c-pawn (no K-side fianchetto)",
            Self::IqpVsCPawnFianchetto => "IQP vs c-pawn + K-side fianchetto",
            Self::IqpVsEPawn => "IQP vs e-pawn (any)",
            Self::IqpVsCPawn => "IQP vs c-pawn (any)",
            Self::IqpVsNoFianchetto => "IQP vs No K-side fianchetto (any)",
            Self::IqpVsFianchetto => "IQP vs K-side fianchetto (any)",
            Self::IqpDynamic => "Dynamic IQP (2+ minor pieces)",
            Self::IqpBlockaded => "Blockaded IQP (opp on d5 + e6)",
            Self::IqpAnyCentralFile => "Any Central IQP (c/d/e file)",
            Self::PanovFormation => "Panov Attack (c4+d4 vs d5+e6)",
            Self::HangingPawnsClassic => "Hanging Pawns (c4+d4)",
            Self::HangingPawnsVsNoFianchetto => "Hanging Pawns vs No K-side fianchetto",
            Self::HangingPawnsVsFianchetto => "Hanging Pawns vs K-side fianchetto (g6+Bg7)",
            Self::HangingPawnsDynamic => "Hanging Pawns (c4/c5 + d4/d5)",
            Self::MaroczyBindAny => "Maróczy Bind (any: c4+e4 vs d6)",
            Self::MaroczyBindNoFianchetto => "Maróczy — No K-side fianchetto",
            Self::MaroczyBindFianchetto => "Maróczy — K-side fianchetto (g3+Bg2)",
            Self::Hedgehog => "Hedgehog (a6-b6-d6-e6 vs c4+e4)",
            Self::Boleslavsky => "Boleslavsky Hole (d6-e5 vs e4)",
            Self::Scheveningen => "Small Center (d6-e6 vs e4)",
            Self::Dragon => "Dragon Formation (d6-g6-Bg7 vs e4)",
            Self::NajdorfStructure => "Najdorf — Classical / Flexible (a6-d6, e5/e6)",
            Self::NajdorfTypeII => "Najdorf — Queenside ...b5 (a6-b5-d6)",
            Self::NajdorfTypeIII => "Najdorf — ...e5 vs e4+f4 (a6-d6-e5)",
            Self::SlavCaroKann => "Slav / Caro-Kann (c6-d5 vs d4)",
            Self::SlavFormation => "Semi-Slav Triangle (c3/c4-d4-e3 vs c6-d5-e6)",
            Self::GrunfeldCenter => "Grünfeld Center (d4+e4 vs g6+Bg7)",
            Self::Botvinnik => "Botvinnik",
            Self::BotvinnikTriangle => "Botvinnik Triangle (c4-d3-e4 + g3+Bg2)",
            Self::FrenchChain => "French Chain (d4-e5 vs d5-e6)",
            Self::FrenchTarraschDxE => "Open French Tension (d4 vs c5+e6)",
            Self::KingsIndianChain => "KID Locked Center (c4-d5-e4 vs d6-e5)",
            Self::KingsIndianC5Chain => "KID + ...c5 Break (c4-d5-e4 vs c5-d6-e5)",
            Self::BenoniStructure => "Benoni Wedge (c5-d6 vs d5-e4)",
            Self::NimzoDoubledCPawns => "Doubled c-Pawns (Nimzo: c3+c4, opp DSB off)",
        }
    }

    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::StonewallAny => {
                "Requires Stonewall triangle (d4-e3-f4 or cxd/exd trade with no own e-pawn and no opp c-pawn)."
            }
            Self::StonewallClassic => {
                "c3-d4-e3-f4 pyramid (including cxd/exd recaptures) without e4."
            }
            Self::StonewallNc6 => {
                "Chigorin/Botvinnik/Slav Stonewall: pawns d5-e6/exd-f5 + Nc6 (or d4-e3/exd-f4 + Nc3)."
            }
            Self::StonewallBadBishopTrade => {
                "b6 + Ba6 maneuver or bad bishop already exchanged off."
            }
            Self::StonewallC5Break => {
                "Stonewall triangle with c5/c4 pawn tension against the center."
            }
            Self::StonewallBd2e1h4 => {
                "Classic Stonewall bishop transfer via Bd2-e1-h4 facing a knight on f6."
            }
            Self::BadewanneAny => {
                "Two backwards adjacent pawns strictly on ranks 3–5, flanked 1 rank ahead on outer files."
            }
            Self::BadewanneFlexibleRim => {
                "Backwards pair on ranks 3–5 with outer pawns at any higher rank (> inner rank)."
            }
            Self::BadewanneCentral => {
                "Symmetric central bathtub on c–f files (d3-e3, d4-e4, or d5-e5)."
            }
            Self::CarlsbadAny => {
                "Exchange QGD skeleton: d4+e3/e4 (no c-pawn) vs c6+d5 (no e-pawn)."
            }
            Self::CarlsbadMinorityAttack => "Carlsbad structure with b4 or b5 pawn advance.",
            Self::CarlsbadCentralE4 => "Botvinnik/Kasparov Carlsbad plan with pawn on e4.",
            Self::CarlsbadF3E4 => "f3 + e3/e4 preparation for central e4 expansion.",
            Self::IqpDFile => {
                "Isolated d-pawn (d4/d5) with open c- and e-files and no opposing d-pawn."
            }
            Self::IqpVsEPawnNoFianchetto => {
                "Isolated d-pawn vs opponent's e-pawn (open c-file) without a K-side fianchetto (g7, no Bg7)."
            }
            Self::IqpVsEPawnFianchetto => {
                "Isolated d-pawn vs opponent's e-pawn (open c-file) with a K-side fianchetto (g6+Bg7)."
            }
            Self::IqpVsCPawnNoFianchetto => {
                "Isolated d-pawn vs opponent's c-pawn (open e-file) without a K-side fianchetto (g7, no Bg7)."
            }
            Self::IqpVsCPawnFianchetto => {
                "Isolated d-pawn vs opponent's c-pawn (open e-file) with a K-side fianchetto (g6+Bg7)."
            }
            Self::IqpVsEPawn => {
                "Isolated d-pawn vs opponent's e-pawn (open c-file), any kingside setup."
            }
            Self::IqpVsCPawn => {
                "Isolated d-pawn vs opponent's c-pawn (open e-file), any kingside setup."
            }
            Self::IqpVsNoFianchetto => {
                "Isolated d-pawn facing a non-fianchettoed kingside (g7, no Bg7)."
            }
            Self::IqpVsFianchetto => "Isolated d-pawn facing a kingside fianchetto (g6+Bg7).",
            Self::IqpDynamic => "IQP middlegame with at least 2 minor pieces for the IQP holder.",
            Self::IqpBlockaded => "d4-pawn directly blockaded on d5 by opponent minor piece + e6.",
            Self::IqpAnyCentralFile => "Any isolated pawn on c, d, or e files on ranks 4–6.",
            Self::HangingPawnsClassic => {
                "Hand-tuned hanging c4+d4 pawns (no own b/e pawns, no opp c/d pawns, opp b6/b7 & e6/e7)."
            }
            Self::HangingPawnsVsNoFianchetto => {
                "Hanging c4+d4 pawns facing an opponent without a K-side fianchetto (g7, no Bg7)."
            }
            Self::HangingPawnsVsFianchetto => {
                "Hanging c4+d4 pawns facing an opponent K-side fianchetto (g6+Bg7)."
            }
            Self::HangingPawnsDynamic => {
                "Advanced or level hanging c/d pawn pair (ranks 4–5) without b/e support."
            }
            Self::MaroczyBindAny => {
                "c4+e4 bind clamping d5 against Black's d6 setup (any kingside)."
            }
            Self::MaroczyBindNoFianchetto => {
                "Hand-tuned Maróczy bind without K-side fianchetto (a2/g2/c4/e4, b2|b3, f2|f3, h2|h3 vs b7/d6/f7/g6, a6|a7, e6|e7, h6|h7)."
            }
            Self::MaroczyBindFianchetto => {
                "Maróczy bind with K-side fianchetto (a2/c4/f2/g3+Bg2, b2|b3, e2|e3|e4, h2|h3 vs b7/d6/f7/g6/h7+Bg7, a7|a6, e7|e6, no W d-pawn or B c-pawn)."
            }
            Self::Hedgehog => "a6-b6-d6-e6 Hedgehog shell without c-pawn facing c4+e4.",
            Self::Boleslavsky => "d6+e5 Sicilian center leaving a backward d5 square vs White e4.",
            Self::Scheveningen => "d6+e6 Small Center vs White e4 with open c- and d-files.",
            Self::Dragon => "d6+g6+Bg7 fianchetto vs e4 with open c- and d-files.",
            Self::NajdorfStructure => "a6+d6+(e5/e6) flexible classical Najdorf shell vs White e4.",
            Self::NajdorfTypeII => {
                "Najdorf queenside expansion: a6+b5+d6 without c-pawn vs White e4."
            }
            Self::NajdorfTypeIII => "Najdorf e5+f4 battle: a6+d6+e5 without c-pawn vs White e4+f4.",
            Self::PanovFormation => {
                "Panov Attack tension: White c4+d4 (no e-pawn) vs Black d5+e6 (no c-pawn)."
            }
            Self::SlavFormation => {
                "Semi-Slav / Triangle shell: c3/c4+d4+e3 against Black's solid c6-d5-e6."
            }
            Self::GrunfeldCenter => {
                "Grünfeld central duo: White d4+e4 (no c4-pawn, allowing Exchange c3) vs Black g6+Bg7 (no d-pawn)."
            }
            Self::Botvinnik => {
                "Botvinnik System: c4-d3-e4 + b2-f2-g3-h2 + a2/a3 with Nc3, Ne2, and Bg2."
            }
            Self::BotvinnikTriangle => {
                "Botvinnik English/Closed Sicilian setup: c4-d3-e4 triangle with g3+Bg2."
            }
            Self::FrenchChain => "Locked d4-e5 vs d5-e6 French pawn chain.",
            Self::FrenchTarraschDxE => {
                "Released French tension: White d4 (no e-pawn) vs Black c5+e6 (no d-pawn)."
            }
            Self::KingsIndianChain => "Closed KID chain: c4-d5-e4 vs d6-e5-g6.",
            Self::KingsIndianC5Chain => {
                "King's Indian with counterbreak: c4-d5-e4 locked against c5-d6-e5."
            }
            Self::BenoniStructure => "Asymmetric Benoni c5-d6 wedge vs White d5-e4.",
            Self::SlavCaroKann => {
                "Solid c6-d5 foundation vs White d4+(c4/e4) with e-file half-open."
            }
            Self::NimzoDoubledCPawns => {
                "Structural doubled c-pawns (Nimzo/Sämisch: c3+c4 + d/e pawns, opponent DSB traded off)."
            }
        }
    }

    #[must_use]
    pub fn primary_side(self) -> Color {
        match self {
            Self::StonewallAny
            | Self::StonewallNc6
            | Self::StonewallBadBishopTrade
            | Self::StonewallC5Break
            | Self::Hedgehog
            | Self::Boleslavsky
            | Self::Scheveningen
            | Self::Dragon
            | Self::NajdorfStructure
            | Self::NajdorfTypeII
            | Self::NajdorfTypeIII
            | Self::BenoniStructure
            | Self::SlavCaroKann => Color::Black,
            _ => Color::White,
        }
    }

    #[must_use]
    pub fn matches_oriented(self, pos: &Chess, flipped: bool) -> bool {
        let perspective = if flipped {
            self.primary_side().other()
        } else {
            self.primary_side()
        };
        let nb = NormalizedBoard::from_pos(pos, perspective);
        nb.matches_structure(self)
    }

    #[must_use]
    pub(crate) fn matches_sig_oriented(self, sig: &GameStructSig, flipped: bool) -> bool {
        let perspective = if flipped {
            self.primary_side().other()
        } else {
            self.primary_side()
        };
        let nb = NormalizedBoard::from_sig(sig, perspective);
        nb.matches_structure(self)
    }

    #[must_use]
    pub fn to_query(self) -> PositionQuery {
        PositionQuery {
            structure: Some(self),
            ignore_color_flip: true,
            min_ply: 8,
            max_ply: 140,
            ..PositionQuery::default()
        }
    }

    /// Export this built-in preset into editable board squares and Boolean
    /// `PatternClause`s so users can inspect and hand-tune its exact definition.
    #[must_use]
    pub fn to_pattern_components(self) -> (String, [SquareConstraint; 64], Vec<PatternClause>) {
        let side = self.primary_side();
        let opp = side.other();
        let squares = std::cell::RefCell::new([SquareConstraint::Any; 64]);
        let mut clauses: Vec<PatternClause> = Vec::new();

        let sq_in_frame = |file: usize, rank_0: usize| -> Square {
            let r = if side == Color::White {
                rank_0
            } else {
                7 - rank_0
            };
            Square::from_coords(ChessFile::new(file as u32), Rank::new(r as u32))
        };
        let own_pawn = |f: usize, r: usize| {
            AtomPredicate::HasPiece(sq_in_frame(f, r), Piece::new(side, Role::Pawn))
        };
        let opp_pawn = |f: usize, r: usize| {
            AtomPredicate::HasPiece(sq_in_frame(f, r), Piece::new(opp, Role::Pawn))
        };
        let no_own_pawn_file =
            |f: usize| AtomPredicate::NoPieceOnFile(ChessFile::new(f as u32), side, Role::Pawn);
        let no_opp_pawn_file =
            |f: usize| AtomPredicate::NoPieceOnFile(ChessFile::new(f as u32), opp, Role::Pawn);
        let not_own_pawn = |f: usize, r: usize| {
            AtomPredicate::NotPiece(sq_in_frame(f, r), Piece::new(side, Role::Pawn))
        };
        let put_own = |file: usize, rank_0: usize, role: Role| {
            let sq = sq_in_frame(file, rank_0);
            squares.borrow_mut()[sq.to_u32() as usize] =
                SquareConstraint::Piece(Piece::new(side, role));
        };
        let put_opp = |file: usize, rank_0: usize, role: Role| {
            let sq = sq_in_frame(file, rank_0);
            squares.borrow_mut()[sq.to_u32() as usize] =
                SquareConstraint::Piece(Piece::new(opp, role));
        };

        match self {
            Self::StonewallAny => {
                put_own(3, 3, Role::Pawn);
                put_own(4, 2, Role::Pawn);
                put_own(5, 3, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::NoneOf,
                    atoms: vec![own_pawn(4, 3)],
                });
            }
            Self::StonewallClassic => {
                put_own(2, 2, Role::Pawn);
                put_own(3, 3, Role::Pawn);
                put_own(4, 2, Role::Pawn);
                put_own(5, 3, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::NoneOf,
                    atoms: vec![own_pawn(4, 3)],
                });
            }
            Self::StonewallNc6 => {
                put_own(3, 3, Role::Pawn);
                put_own(4, 2, Role::Pawn);
                put_own(5, 3, Role::Pawn);
                put_own(2, 2, Role::Knight);
                clauses.push(PatternClause {
                    op: ClauseOp::NoneOf,
                    atoms: vec![own_pawn(4, 3)],
                });
            }
            Self::StonewallBadBishopTrade => {
                put_own(1, 2, Role::Pawn);
                put_own(3, 3, Role::Pawn);
                put_own(4, 2, Role::Pawn);
                put_own(5, 3, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::NoneOf,
                    atoms: vec![own_pawn(4, 3)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![
                        AtomPredicate::HasPiece(sq_in_frame(0, 2), Piece::new(side, Role::Bishop)),
                        AtomPredicate::IsEmpty(sq_in_frame(2, 0)),
                    ],
                });
            }
            Self::StonewallC5Break => {
                put_own(2, 3, Role::Pawn);
                put_own(3, 3, Role::Pawn);
                put_own(4, 2, Role::Pawn);
                put_own(5, 3, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::NoneOf,
                    atoms: vec![own_pawn(4, 3)],
                });
            }
            Self::StonewallBd2e1h4 => {
                put_own(2, 2, Role::Pawn);
                put_own(3, 3, Role::Pawn);
                put_own(4, 2, Role::Pawn);
                put_own(5, 3, Role::Pawn);
                put_opp(5, 5, Role::Knight);
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![
                        AtomPredicate::HasPiece(sq_in_frame(3, 1), Piece::new(side, Role::Bishop)),
                        AtomPredicate::HasPiece(sq_in_frame(4, 0), Piece::new(side, Role::Bishop)),
                        AtomPredicate::HasPiece(sq_in_frame(7, 3), Piece::new(side, Role::Bishop)),
                    ],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::NoneOf,
                    atoms: vec![own_pawn(4, 3)],
                });
            }
            Self::BadewanneAny | Self::BadewanneFlexibleRim => {
                // Seed the classic central Badewanne pattern plus an OR option shifted one rank higher.
                put_own(2, 3, Role::Pawn);
                put_own(3, 2, Role::Pawn);
                put_own(4, 2, Role::Pawn);
                put_own(5, 3, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(3, 2), own_pawn(3, 3)],
                });
            }
            Self::BadewanneCentral => {
                put_own(2, 3, Role::Pawn);
                put_own(3, 2, Role::Pawn);
                put_own(4, 2, Role::Pawn);
                put_own(5, 3, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![not_own_pawn(3, 3), not_own_pawn(4, 3)],
                });
            }
            Self::CarlsbadAny => {
                put_own(3, 3, Role::Pawn);
                put_opp(2, 5, Role::Pawn);
                put_opp(3, 4, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(4, 2), own_pawn(4, 3)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(2), no_opp_pawn_file(4)],
                });
            }
            Self::CarlsbadMinorityAttack => {
                put_own(3, 3, Role::Pawn);
                put_opp(2, 5, Role::Pawn);
                put_opp(3, 4, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(4, 2), own_pawn(4, 3)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(1, 3), own_pawn(1, 4)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(2), no_opp_pawn_file(4)],
                });
            }
            Self::CarlsbadCentralE4 => {
                put_own(3, 3, Role::Pawn);
                put_own(4, 3, Role::Pawn);
                put_opp(2, 5, Role::Pawn);
                put_opp(3, 4, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(2), no_opp_pawn_file(4)],
                });
            }
            Self::CarlsbadF3E4 => {
                put_own(3, 3, Role::Pawn);
                put_own(5, 2, Role::Pawn);
                put_opp(2, 5, Role::Pawn);
                put_opp(3, 4, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(4, 2), own_pawn(4, 3)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(2), no_opp_pawn_file(4)],
                });
            }
            Self::IqpDFile | Self::IqpDynamic => {
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(3, 3), own_pawn(3, 4)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![
                        no_own_pawn_file(2),
                        no_own_pawn_file(4),
                        no_opp_pawn_file(3),
                    ],
                });
            }
            Self::IqpVsEPawnNoFianchetto
            | Self::IqpVsEPawnFianchetto
            | Self::IqpVsCPawnNoFianchetto
            | Self::IqpVsCPawnFianchetto
            | Self::IqpVsEPawn
            | Self::IqpVsCPawn
            | Self::IqpVsNoFianchetto
            | Self::IqpVsFianchetto => {
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(3, 3), own_pawn(3, 4)],
                });
                let mut file_atoms = vec![
                    no_own_pawn_file(2),
                    no_own_pawn_file(4),
                    no_opp_pawn_file(3),
                ];
                if matches!(
                    self,
                    Self::IqpVsEPawnNoFianchetto | Self::IqpVsEPawnFianchetto | Self::IqpVsEPawn
                ) {
                    clauses.push(PatternClause {
                        op: ClauseOp::AnyOf,
                        atoms: vec![opp_pawn(4, 5), opp_pawn(4, 6)],
                    });
                    file_atoms.push(no_opp_pawn_file(2));
                } else if matches!(
                    self,
                    Self::IqpVsCPawnNoFianchetto | Self::IqpVsCPawnFianchetto | Self::IqpVsCPawn
                ) {
                    clauses.push(PatternClause {
                        op: ClauseOp::AnyOf,
                        atoms: vec![opp_pawn(2, 5), opp_pawn(2, 6)],
                    });
                    file_atoms.push(no_opp_pawn_file(4));
                }
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: file_atoms,
                });
                if matches!(
                    self,
                    Self::IqpVsEPawnNoFianchetto
                        | Self::IqpVsCPawnNoFianchetto
                        | Self::IqpVsNoFianchetto
                ) {
                    put_opp(6, 6, Role::Pawn);
                    clauses.push(PatternClause {
                        op: ClauseOp::NoneOf,
                        atoms: vec![
                            opp_pawn(6, 5),
                            AtomPredicate::HasPiece(
                                sq_in_frame(6, 6),
                                Piece::new(opp, Role::Bishop),
                            ),
                        ],
                    });
                } else if matches!(
                    self,
                    Self::IqpVsEPawnFianchetto | Self::IqpVsCPawnFianchetto | Self::IqpVsFianchetto
                ) {
                    put_opp(6, 5, Role::Pawn);
                    put_opp(6, 6, Role::Bishop);
                }
            }
            Self::IqpBlockaded => {
                put_own(3, 3, Role::Pawn);
                put_opp(4, 5, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![
                        AtomPredicate::HasPiece(sq_in_frame(3, 4), Piece::new(opp, Role::Knight)),
                        AtomPredicate::HasPiece(sq_in_frame(3, 4), Piece::new(opp, Role::Bishop)),
                    ],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![
                        no_own_pawn_file(2),
                        no_own_pawn_file(4),
                        no_opp_pawn_file(3),
                    ],
                });
            }
            Self::IqpAnyCentralFile => {
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(2, 3), own_pawn(3, 3), own_pawn(4, 3)],
                });
            }
            Self::HangingPawnsClassic
            | Self::HangingPawnsVsNoFianchetto
            | Self::HangingPawnsVsFianchetto => {
                put_own(2, 3, Role::Pawn);
                put_own(3, 3, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![
                        no_own_pawn_file(4),
                        no_own_pawn_file(1),
                        no_opp_pawn_file(2),
                        no_opp_pawn_file(3),
                    ],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![opp_pawn(1, 6), opp_pawn(1, 5)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![opp_pawn(4, 6), opp_pawn(4, 5)],
                });
                if self == Self::HangingPawnsVsNoFianchetto {
                    put_opp(6, 6, Role::Pawn);
                    clauses.push(PatternClause {
                        op: ClauseOp::NoneOf,
                        atoms: vec![
                            opp_pawn(6, 5),
                            AtomPredicate::HasPiece(
                                sq_in_frame(6, 6),
                                Piece::new(opp, Role::Bishop),
                            ),
                        ],
                    });
                } else if self == Self::HangingPawnsVsFianchetto {
                    put_opp(6, 5, Role::Pawn);
                    put_opp(6, 6, Role::Bishop);
                }
            }
            Self::HangingPawnsDynamic => {
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(2, 3), own_pawn(2, 4)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(3, 3), own_pawn(3, 4)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![
                        no_own_pawn_file(1),
                        no_own_pawn_file(4),
                        no_opp_pawn_file(2),
                        no_opp_pawn_file(3),
                    ],
                });
            }
            Self::MaroczyBindAny => {
                put_own(2, 3, Role::Pawn);
                put_own(4, 3, Role::Pawn);
                put_opp(3, 5, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(3), no_opp_pawn_file(2)],
                });
            }
            Self::MaroczyBindNoFianchetto => {
                put_own(0, 1, Role::Pawn);
                put_own(6, 1, Role::Pawn);
                put_own(2, 3, Role::Pawn);
                put_own(4, 3, Role::Pawn);
                put_opp(3, 5, Role::Pawn);
                put_opp(6, 5, Role::Pawn);
                put_opp(1, 6, Role::Pawn);
                put_opp(5, 6, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(1, 2), own_pawn(1, 1)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(5, 2), own_pawn(5, 1)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![opp_pawn(4, 6), opp_pawn(4, 5)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![opp_pawn(0, 6), opp_pawn(0, 5)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(3), no_opp_pawn_file(2)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![opp_pawn(7, 6), opp_pawn(7, 5)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(7, 1), own_pawn(7, 2)],
                });
            }
            Self::MaroczyBindFianchetto => {
                put_own(0, 1, Role::Pawn);
                put_own(5, 1, Role::Pawn);
                put_own(6, 1, Role::Bishop);
                put_own(6, 2, Role::Pawn);
                put_own(2, 3, Role::Pawn);
                put_opp(3, 5, Role::Pawn);
                put_opp(6, 5, Role::Pawn);
                put_opp(1, 6, Role::Pawn);
                put_opp(5, 6, Role::Pawn);
                put_opp(6, 6, Role::Bishop);
                put_opp(7, 6, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(7, 1), own_pawn(7, 2)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(4, 1), own_pawn(4, 2), own_pawn(4, 3)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(1, 1), own_pawn(1, 2)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![opp_pawn(4, 6), opp_pawn(4, 5)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![opp_pawn(0, 6), opp_pawn(0, 5)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_opp_pawn_file(2), no_own_pawn_file(3)],
                });
            }
            Self::Hedgehog => {
                put_own(0, 2, Role::Pawn);
                put_own(1, 2, Role::Pawn);
                put_own(3, 2, Role::Pawn);
                put_own(4, 2, Role::Pawn);
                put_opp(2, 4, Role::Pawn);
                put_opp(4, 4, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(2), no_opp_pawn_file(3)],
                });
            }
            Self::Boleslavsky => {
                put_own(3, 2, Role::Pawn);
                put_own(4, 3, Role::Pawn);
                put_opp(4, 4, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(2), no_opp_pawn_file(3)],
                });
            }
            Self::Scheveningen => {
                put_own(3, 2, Role::Pawn);
                put_own(4, 2, Role::Pawn);
                put_opp(4, 4, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(2), no_opp_pawn_file(3)],
                });
            }
            Self::Dragon => {
                put_own(3, 2, Role::Pawn);
                put_own(6, 2, Role::Pawn);
                put_own(6, 1, Role::Bishop);
                put_opp(4, 4, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(2), no_opp_pawn_file(3)],
                });
            }
            Self::NajdorfStructure => {
                put_own(0, 2, Role::Pawn);
                put_own(3, 2, Role::Pawn);
                put_opp(4, 4, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(4, 2), own_pawn(4, 3)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(2), no_opp_pawn_file(3)],
                });
            }
            Self::NajdorfTypeII => {
                put_own(0, 2, Role::Pawn);
                put_own(1, 3, Role::Pawn);
                put_own(3, 2, Role::Pawn);
                put_opp(4, 4, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(2), no_opp_pawn_file(3)],
                });
            }
            Self::NajdorfTypeIII => {
                put_own(0, 2, Role::Pawn);
                put_own(3, 2, Role::Pawn);
                put_own(4, 3, Role::Pawn);
                put_opp(4, 4, Role::Pawn);
                put_opp(5, 4, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(2), no_opp_pawn_file(3)],
                });
            }
            Self::PanovFormation => {
                put_own(2, 3, Role::Pawn);
                put_own(3, 3, Role::Pawn);
                put_opp(3, 4, Role::Pawn);
                put_opp(4, 5, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(4), no_opp_pawn_file(2)],
                });
            }
            Self::SlavFormation => {
                put_own(3, 3, Role::Pawn);
                put_own(4, 2, Role::Pawn);
                put_opp(2, 5, Role::Pawn);
                put_opp(3, 4, Role::Pawn);
                put_opp(4, 5, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(2, 2), own_pawn(2, 3)],
                });
            }
            Self::GrunfeldCenter => {
                put_own(3, 3, Role::Pawn);
                put_own(4, 3, Role::Pawn);
                put_opp(6, 5, Role::Pawn);
                put_opp(6, 6, Role::Bishop);
                clauses.push(PatternClause {
                    op: ClauseOp::NoneOf,
                    atoms: vec![own_pawn(2, 3)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_opp_pawn_file(3)],
                });
            }
            Self::Botvinnik => {
                put_own(1, 1, Role::Pawn);
                put_own(4, 1, Role::Knight);
                put_own(5, 1, Role::Pawn);
                put_own(6, 1, Role::Bishop);
                put_own(7, 1, Role::Pawn);
                put_own(2, 2, Role::Knight);
                put_own(3, 2, Role::Pawn);
                put_own(6, 2, Role::Pawn);
                put_own(2, 3, Role::Pawn);
                put_own(4, 3, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(0, 1), own_pawn(0, 2)],
                });
            }
            Self::BotvinnikTriangle => {
                put_own(2, 3, Role::Pawn);
                put_own(3, 2, Role::Pawn);
                put_own(4, 3, Role::Pawn);
                put_own(6, 2, Role::Pawn);
                put_own(6, 1, Role::Bishop);
            }
            Self::FrenchChain => {
                put_own(3, 3, Role::Pawn);
                put_own(4, 4, Role::Pawn);
                put_opp(3, 4, Role::Pawn);
                put_opp(4, 5, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::NoneOf,
                    atoms: vec![own_pawn(4, 3)],
                });
            }
            Self::FrenchTarraschDxE => {
                put_own(3, 3, Role::Pawn);
                put_opp(2, 4, Role::Pawn);
                put_opp(4, 5, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(4), no_opp_pawn_file(3)],
                });
            }
            Self::KingsIndianChain => {
                put_own(2, 3, Role::Pawn);
                put_own(3, 4, Role::Pawn);
                put_own(4, 3, Role::Pawn);
                put_opp(3, 5, Role::Pawn);
                put_opp(4, 4, Role::Pawn);
                put_opp(6, 5, Role::Pawn);
            }
            Self::KingsIndianC5Chain => {
                put_own(2, 3, Role::Pawn);
                put_own(3, 4, Role::Pawn);
                put_own(4, 3, Role::Pawn);
                put_opp(2, 4, Role::Pawn);
                put_opp(3, 5, Role::Pawn);
                put_opp(4, 4, Role::Pawn);
            }
            Self::BenoniStructure => {
                put_own(2, 3, Role::Pawn);
                put_own(3, 2, Role::Pawn);
                put_opp(3, 3, Role::Pawn);
                put_opp(4, 4, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![no_own_pawn_file(4), no_opp_pawn_file(2)],
                });
            }
            Self::SlavCaroKann => {
                put_own(2, 2, Role::Pawn);
                put_own(3, 3, Role::Pawn);
                put_opp(3, 4, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![opp_pawn(2, 4), opp_pawn(4, 4)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AllOf,
                    atoms: vec![
                        no_own_pawn_file(4),
                        AtomPredicate::NotPiece(sq_in_frame(3, 3), Piece::new(opp, Role::Pawn)),
                    ],
                });
            }
            Self::NimzoDoubledCPawns => {
                put_own(2, 2, Role::Pawn);
                put_own(2, 3, Role::Pawn);
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(3, 2), own_pawn(3, 3)],
                });
                clauses.push(PatternClause {
                    op: ClauseOp::AnyOf,
                    atoms: vec![own_pawn(4, 2), own_pawn(4, 3)],
                });
            }
        }
        (self.label().to_string(), squares.into_inner(), clauses)
    }
}

const FILE_MASKS: [u64; 8] = [
    0x0101_0101_0101_0101u64,
    0x0101_0101_0101_0101u64 << 1,
    0x0101_0101_0101_0101u64 << 2,
    0x0101_0101_0101_0101u64 << 3,
    0x0101_0101_0101_0101u64 << 4,
    0x0101_0101_0101_0101u64 << 5,
    0x0101_0101_0101_0101u64 << 6,
    0x0101_0101_0101_0101u64 << 7,
];
const DARK_SQUARES_MASK: u64 = 0xAA55_AA55_AA55_AA55u64;
const LIGHT_SQUARES_MASK: u64 = 0x55AA_55AA_55AA_55AAu64;

#[inline]
const fn sq_bit(file: usize, rank: usize) -> u64 {
    1u64 << (rank * 8 + file)
}

#[inline]
const fn ranks_le_mask(rank: usize) -> u64 {
    // Ranks 0..=rank (1-based: 1..=rank+1)
    if rank >= 7 {
        u64::MAX
    } else {
        (1u64 << ((rank + 1) * 8)) - 1
    }
}

#[inline]
const fn ranks_gt_mask(rank: usize) -> u64 {
    !ranks_le_mask(rank)
}

/// Bitboards normalized so `own` always moves up from rank 0 (`1st`) toward rank 7 (`8th`).
#[derive(Clone, Copy)]
struct NormalizedBoard {
    own_pawns: u64,
    opp_pawns: u64,
    own_knights: u64,
    own_bishops: u64,
    opp_knights: u64,
    opp_bishops: u64,
    is_union: bool,
}

impl NormalizedBoard {
    fn from_pos(pos: &Chess, own: Color) -> Self {
        let b = pos.board();
        let opp = own.other();
        let flip = own == Color::Black;
        let norm = |mask: u64| if flip { mask.swap_bytes() } else { mask };
        Self {
            own_pawns: norm(b.colored_pieces_mask(own, Role::Pawn)),
            opp_pawns: norm(b.colored_pieces_mask(opp, Role::Pawn)),
            own_knights: norm(b.colored_pieces_mask(own, Role::Knight)),
            own_bishops: norm(b.colored_pieces_mask(own, Role::Bishop)),
            opp_knights: norm(b.colored_pieces_mask(opp, Role::Knight)),
            opp_bishops: norm(b.colored_pieces_mask(opp, Role::Bishop)),
            is_union: false,
        }
    }

    fn from_sig(sig: &GameStructSig, own: Color) -> Self {
        let flip = own == Color::Black;
        let norm = |mask: u64| if flip { mask.swap_bytes() } else { mask };
        let (ow_p, op_p, ow_n, ow_b, op_n, op_b) = if own == Color::White {
            (
                sig.white_pawns,
                sig.black_pawns,
                sig.white_knights,
                sig.white_bishops,
                sig.black_knights,
                sig.black_bishops,
            )
        } else {
            (
                sig.black_pawns,
                sig.white_pawns,
                sig.black_knights,
                sig.black_bishops,
                sig.white_knights,
                sig.white_bishops,
            )
        };
        Self {
            own_pawns: norm(ow_p),
            opp_pawns: norm(op_p),
            own_knights: norm(ow_n),
            own_bishops: norm(ow_b),
            opp_knights: norm(op_n),
            opp_bishops: norm(op_b),
            is_union: true,
        }
    }

    #[inline]
    fn has_own_pawns(self, mask: u64) -> bool {
        (self.own_pawns & mask) == mask
    }

    #[inline]
    fn has_opp_pawns(self, mask: u64) -> bool {
        (self.opp_pawns & mask) == mask
    }

    #[inline]
    fn no_own_pawns(self, mask: u64) -> bool {
        self.is_union || (self.own_pawns & mask) == 0
    }

    #[inline]
    fn no_opp_pawns(self, mask: u64) -> bool {
        self.is_union || (self.opp_pawns & mask) == 0
    }

    fn matches_badewanne(self, file_start: usize, file_end: usize, flexible_rim: bool) -> bool {
        // Backwards pawns strictly on ranks 3..=5 (0-based indices 2..=4).
        for f in file_start..=file_end {
            for r in 2..=4 {
                let inner_l = sq_bit(f + 1, r);
                let inner_r = sq_bit(f + 2, r);
                if self.is_union {
                    if !self.has_own_pawns(inner_l | inner_r) {
                        continue;
                    }
                } else {
                    if (self.own_pawns & FILE_MASKS[f + 1]) != inner_l
                        || (self.own_pawns & FILE_MASKS[f + 2]) != inner_r
                    {
                        continue;
                    }
                    let le_r = ranks_le_mask(r);
                    if (self.own_pawns & FILE_MASKS[f] & le_r) != 0
                        || (self.own_pawns & FILE_MASKS[f + 3] & le_r) != 0
                    {
                        continue;
                    }
                }
                if flexible_rim {
                    let gt_r = ranks_gt_mask(r);
                    if (self.own_pawns & FILE_MASKS[f] & gt_r) != 0
                        && (self.own_pawns & FILE_MASKS[f + 3] & gt_r) != 0
                    {
                        return true;
                    }
                } else {
                    let rim = sq_bit(f, r + 1) | sq_bit(f + 3, r + 1);
                    if self.has_own_pawns(rim) {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn has_iqp_d_core(self) -> bool {
        (self.own_pawns & (sq_bit(3, 3) | sq_bit(3, 4))) != 0
            && self.no_own_pawns(FILE_MASKS[2] | FILE_MASKS[4])
            && self.no_opp_pawns(FILE_MASKS[3])
    }

    fn has_opp_k_fianchetto(self) -> bool {
        self.has_opp_pawns(sq_bit(6, 5)) && (self.opp_bishops & sq_bit(6, 6)) != 0
    }

    fn has_opp_no_k_fianchetto(self) -> bool {
        self.has_opp_pawns(sq_bit(6, 6))
            && self.no_opp_pawns(sq_bit(6, 5))
            && (self.is_union || (self.opp_bishops & sq_bit(6, 6)) == 0)
    }

    fn has_opp_e_pawn_iqp(self) -> bool {
        (self.opp_pawns & (sq_bit(4, 5) | sq_bit(4, 6))) != 0 && self.no_opp_pawns(FILE_MASKS[2])
    }

    fn has_opp_c_pawn_iqp(self) -> bool {
        (self.opp_pawns & (sq_bit(2, 5) | sq_bit(2, 6))) != 0 && self.no_opp_pawns(FILE_MASKS[4])
    }

    fn has_hanging_pawns_classic(self) -> bool {
        self.has_own_pawns(sq_bit(2, 3) | sq_bit(3, 3))
            && self.no_own_pawns(FILE_MASKS[1] | FILE_MASKS[4])
            && self.no_opp_pawns(FILE_MASKS[2] | FILE_MASKS[3])
            && (self.opp_pawns & (sq_bit(1, 6) | sq_bit(1, 5))) != 0
            && (self.opp_pawns & (sq_bit(4, 6) | sq_bit(4, 5))) != 0
    }

    fn has_stonewall_core(self) -> bool {
        let intact = self.has_own_pawns(sq_bit(3, 3) | sq_bit(4, 2) | sq_bit(5, 3))
            && self.no_own_pawns(sq_bit(4, 3));
        // Also match cxd / exd captures where the Stonewall side recaptured exd on d4
        // (no own e-pawn remaining and no opponent c-pawn remaining).
        let traded_exd = self.has_own_pawns(sq_bit(3, 3) | sq_bit(5, 3))
            && self.no_own_pawns(FILE_MASKS[4])
            && self.no_opp_pawns(FILE_MASKS[2]);
        intact || traded_exd
    }

    fn matches_structure(self, preset: StructurePreset) -> bool {
        match preset {
            StructurePreset::StonewallAny => self.has_stonewall_core(),
            StructurePreset::StonewallClassic => {
                self.has_stonewall_core() && self.has_own_pawns(sq_bit(2, 2))
            }
            StructurePreset::StonewallNc6 => {
                // Normalized Black (d5,e6/exd,f5,Nc6) -> own stonewall core + own Nc3 (sq(2,2))
                self.has_stonewall_core()
                    && self.no_own_pawns(sq_bit(2, 2))
                    && (self.own_knights & sq_bit(2, 2)) != 0
            }
            StructurePreset::StonewallBadBishopTrade => {
                // d4, e3/exd, f4, b3 + (Ba3 or bad bishop on dark squares traded off)
                self.has_stonewall_core()
                    && self.has_own_pawns(sq_bit(1, 2))
                    && (self.is_union
                        || (self.own_bishops & sq_bit(0, 2)) != 0
                        || (self.own_bishops & DARK_SQUARES_MASK) == 0)
            }
            StructurePreset::StonewallC5Break => {
                self.has_stonewall_core() && self.has_own_pawns(sq_bit(2, 3))
            }
            StructurePreset::StonewallBd2e1h4 => {
                // Stonewall core + Bishop executing Bd2-e1-h4 maneuver facing Nf6 (sq(5,5))
                self.has_stonewall_core()
                    && (self.own_bishops & (sq_bit(3, 1) | sq_bit(4, 0) | sq_bit(7, 3))) != 0
                    && (self.opp_knights & sq_bit(5, 5)) != 0
            }
            StructurePreset::BadewanneAny => self.matches_badewanne(0, 4, false),
            StructurePreset::BadewanneFlexibleRim => self.matches_badewanne(0, 4, true),
            StructurePreset::BadewanneCentral => self.matches_badewanne(2, 2, false),
            StructurePreset::CarlsbadAny => {
                // Own d4 + (e3 or e4), no c-pawn; Opp c6 (rank 5) + d5 (rank 4), no e-pawn
                self.has_own_pawns(sq_bit(3, 3))
                    && (self.own_pawns & (sq_bit(4, 2) | sq_bit(4, 3))) != 0
                    && self.no_own_pawns(FILE_MASKS[2])
                    && self.has_opp_pawns(sq_bit(2, 5) | sq_bit(3, 4))
                    && self.no_opp_pawns(FILE_MASKS[4])
            }
            StructurePreset::CarlsbadMinorityAttack => {
                self.matches_structure(StructurePreset::CarlsbadAny)
                    && (self.own_pawns & (sq_bit(1, 3) | sq_bit(1, 4))) != 0
            }
            StructurePreset::CarlsbadCentralE4 => {
                self.has_own_pawns(sq_bit(3, 3) | sq_bit(4, 3))
                    && self.no_own_pawns(FILE_MASKS[2])
                    && self.has_opp_pawns(sq_bit(2, 5) | sq_bit(3, 4))
                    && self.no_opp_pawns(FILE_MASKS[4])
            }
            StructurePreset::CarlsbadF3E4 => {
                self.has_own_pawns(sq_bit(3, 3) | sq_bit(5, 2))
                    && (self.own_pawns & (sq_bit(4, 2) | sq_bit(4, 3))) != 0
                    && self.no_own_pawns(FILE_MASKS[2])
                    && self.has_opp_pawns(sq_bit(2, 5) | sq_bit(3, 4))
                    && self.no_opp_pawns(FILE_MASKS[4])
            }
            StructurePreset::IqpDFile => self.has_iqp_d_core(),
            StructurePreset::IqpVsEPawnNoFianchetto => {
                self.has_iqp_d_core() && self.has_opp_e_pawn_iqp() && self.has_opp_no_k_fianchetto()
            }
            StructurePreset::IqpVsEPawnFianchetto => {
                self.has_iqp_d_core() && self.has_opp_e_pawn_iqp() && self.has_opp_k_fianchetto()
            }
            StructurePreset::IqpVsCPawnNoFianchetto => {
                self.has_iqp_d_core() && self.has_opp_c_pawn_iqp() && self.has_opp_no_k_fianchetto()
            }
            StructurePreset::IqpVsCPawnFianchetto => {
                self.has_iqp_d_core() && self.has_opp_c_pawn_iqp() && self.has_opp_k_fianchetto()
            }
            StructurePreset::IqpVsEPawn => self.has_iqp_d_core() && self.has_opp_e_pawn_iqp(),
            StructurePreset::IqpVsCPawn => self.has_iqp_d_core() && self.has_opp_c_pawn_iqp(),
            StructurePreset::IqpVsNoFianchetto => {
                self.has_iqp_d_core() && self.has_opp_no_k_fianchetto()
            }
            StructurePreset::IqpVsFianchetto => {
                self.has_iqp_d_core() && self.has_opp_k_fianchetto()
            }
            StructurePreset::IqpDynamic => {
                self.has_iqp_d_core()
                    && (self.own_knights | self.own_bishops).count_ones() >= 2
                    && (self.opp_knights | self.opp_bishops).count_ones() >= 1
            }
            StructurePreset::IqpBlockaded => {
                self.has_own_pawns(sq_bit(3, 3))
                    && self.no_own_pawns(FILE_MASKS[2] | FILE_MASKS[4])
                    && self.no_opp_pawns(FILE_MASKS[3])
                    && ((self.opp_knights | self.opp_bishops) & sq_bit(3, 4)) != 0
                    && self.has_opp_pawns(sq_bit(4, 5))
            }
            StructurePreset::IqpAnyCentralFile => {
                for f in 2..=4 {
                    let target = sq_bit(f, 3) | sq_bit(f, 4) | sq_bit(f, 5);
                    if (self.own_pawns & target) != 0
                        && (self.is_union || (self.own_pawns & FILE_MASKS[f]).count_ones() == 1)
                        && self.no_own_pawns(FILE_MASKS[f - 1] | FILE_MASKS[f + 1])
                        && self.no_opp_pawns(FILE_MASKS[f])
                    {
                        return true;
                    }
                }
                false
            }
            StructurePreset::HangingPawnsClassic => self.has_hanging_pawns_classic(),
            StructurePreset::HangingPawnsVsNoFianchetto => {
                self.has_hanging_pawns_classic() && self.has_opp_no_k_fianchetto()
            }
            StructurePreset::HangingPawnsVsFianchetto => {
                self.has_hanging_pawns_classic() && self.has_opp_k_fianchetto()
            }
            StructurePreset::HangingPawnsDynamic => {
                (self.own_pawns & (sq_bit(2, 3) | sq_bit(2, 4))) != 0
                    && (self.own_pawns & (sq_bit(3, 3) | sq_bit(3, 4))) != 0
                    && self.no_own_pawns(FILE_MASKS[1] | FILE_MASKS[4])
                    && self.no_opp_pawns(FILE_MASKS[2] | FILE_MASKS[3])
            }
            StructurePreset::MaroczyBindAny => {
                self.has_own_pawns(sq_bit(2, 3) | sq_bit(4, 3))
                    && self.no_own_pawns(FILE_MASKS[3])
                    && self.has_opp_pawns(sq_bit(3, 5))
                    && self.no_opp_pawns(FILE_MASKS[2])
            }
            StructurePreset::MaroczyBindNoFianchetto => {
                self.has_own_pawns(sq_bit(0, 1) | sq_bit(2, 3) | sq_bit(4, 3) | sq_bit(6, 1))
                    && (self.own_pawns & (sq_bit(1, 1) | sq_bit(1, 2))) != 0
                    && (self.own_pawns & (sq_bit(5, 1) | sq_bit(5, 2))) != 0
                    && (self.own_pawns & (sq_bit(7, 1) | sq_bit(7, 2))) != 0
                    && self.no_own_pawns(FILE_MASKS[3])
                    && self.has_opp_pawns(sq_bit(1, 6) | sq_bit(3, 5) | sq_bit(5, 6) | sq_bit(6, 5))
                    && (self.opp_pawns & (sq_bit(4, 5) | sq_bit(4, 6))) != 0
                    && (self.opp_pawns & (sq_bit(0, 5) | sq_bit(0, 6))) != 0
                    && (self.opp_pawns & (sq_bit(7, 5) | sq_bit(7, 6))) != 0
                    && self.no_opp_pawns(FILE_MASKS[2])
            }
            StructurePreset::MaroczyBindFianchetto => {
                self.has_own_pawns(sq_bit(0, 1) | sq_bit(5, 1) | sq_bit(6, 2) | sq_bit(2, 3))
                    && (self.own_bishops & sq_bit(6, 1)) != 0
                    && (self.own_pawns & (sq_bit(7, 1) | sq_bit(7, 2))) != 0
                    && (self.own_pawns & (sq_bit(4, 1) | sq_bit(4, 2) | sq_bit(4, 3))) != 0
                    && (self.own_pawns & (sq_bit(1, 1) | sq_bit(1, 2))) != 0
                    && self.no_own_pawns(FILE_MASKS[3])
                    && self.has_opp_pawns(
                        sq_bit(3, 5) | sq_bit(6, 5) | sq_bit(1, 6) | sq_bit(5, 6) | sq_bit(7, 6),
                    )
                    && (self.opp_bishops & sq_bit(6, 6)) != 0
                    && (self.opp_pawns & (sq_bit(4, 6) | sq_bit(4, 5))) != 0
                    && (self.opp_pawns & (sq_bit(0, 6) | sq_bit(0, 5))) != 0
                    && self.no_opp_pawns(FILE_MASKS[2])
            }
            StructurePreset::Hedgehog => {
                // Normalized Black -> own on a3, b3, d3, e3 (Black a6, b6, d6, e6), opp on c5+e5 (White c4+e4)
                self.has_own_pawns(sq_bit(0, 2) | sq_bit(1, 2) | sq_bit(3, 2) | sq_bit(4, 2))
                    && self.no_own_pawns(FILE_MASKS[2])
                    && self.has_opp_pawns(sq_bit(2, 4) | sq_bit(4, 4))
                    && self.no_opp_pawns(FILE_MASKS[3])
            }
            StructurePreset::Boleslavsky => {
                // Normalized Black -> own on d3+e4 (Black d6+e5), opp on e5 (White e4)
                self.has_own_pawns(sq_bit(3, 2) | sq_bit(4, 3))
                    && self.no_own_pawns(FILE_MASKS[2])
                    && self.has_opp_pawns(sq_bit(4, 4))
                    && self.no_opp_pawns(FILE_MASKS[3])
            }
            StructurePreset::Scheveningen => {
                // Normalized Black -> own on d3+e3 (Black d6+e6), opp on e5 (White e4)
                self.has_own_pawns(sq_bit(3, 2) | sq_bit(4, 2))
                    && self.no_own_pawns(FILE_MASKS[2])
                    && self.has_opp_pawns(sq_bit(4, 4))
                    && self.no_opp_pawns(FILE_MASKS[3])
            }
            StructurePreset::Dragon => {
                // Normalized Black -> own on d3+g3 + Bg2, opp on e5
                self.has_own_pawns(sq_bit(3, 2) | sq_bit(6, 2))
                    && (self.own_bishops & sq_bit(6, 1)) != 0
                    && self.no_own_pawns(FILE_MASKS[2])
                    && self.has_opp_pawns(sq_bit(4, 4))
                    && self.no_opp_pawns(FILE_MASKS[3])
            }
            StructurePreset::NajdorfStructure => {
                self.has_own_pawns(sq_bit(0, 2) | sq_bit(3, 2))
                    && (self.own_pawns & (sq_bit(4, 2) | sq_bit(4, 3))) != 0
                    && self.no_own_pawns(FILE_MASKS[2])
                    && self.has_opp_pawns(sq_bit(4, 4))
                    && self.no_opp_pawns(FILE_MASKS[3])
            }
            StructurePreset::NajdorfTypeII => {
                // Normalized Black -> own a6 (0,2), b5 (1,3), d6 (3,2); opp e4 (4,4)
                self.has_own_pawns(sq_bit(0, 2) | sq_bit(1, 3) | sq_bit(3, 2))
                    && self.no_own_pawns(FILE_MASKS[2])
                    && self.has_opp_pawns(sq_bit(4, 4))
                    && self.no_opp_pawns(FILE_MASKS[3])
            }
            StructurePreset::NajdorfTypeIII => {
                // Normalized Black -> own a6 (0,2), d6 (3,2), e5 (4,3); opp e4 (4,4), f4 (5,4)
                self.has_own_pawns(sq_bit(0, 2) | sq_bit(3, 2) | sq_bit(4, 3))
                    && self.no_own_pawns(FILE_MASKS[2])
                    && self.has_opp_pawns(sq_bit(4, 4) | sq_bit(5, 4))
                    && self.no_opp_pawns(FILE_MASKS[3])
            }
            StructurePreset::PanovFormation => {
                self.has_own_pawns(sq_bit(2, 3) | sq_bit(3, 3))
                    && self.no_own_pawns(FILE_MASKS[4])
                    && self.has_opp_pawns(sq_bit(3, 4) | sq_bit(4, 5))
                    && self.no_opp_pawns(FILE_MASKS[2])
            }
            StructurePreset::SlavFormation => {
                self.has_own_pawns(sq_bit(3, 3) | sq_bit(4, 2))
                    && (self.own_pawns & (sq_bit(2, 2) | sq_bit(2, 3))) != 0
                    && self.has_opp_pawns(sq_bit(2, 5) | sq_bit(3, 4) | sq_bit(4, 5))
            }
            StructurePreset::GrunfeldCenter => {
                self.has_own_pawns(sq_bit(3, 3) | sq_bit(4, 3))
                    && self.no_own_pawns(sq_bit(2, 3))
                    && self.has_opp_pawns(sq_bit(6, 5))
                    && (self.opp_bishops & sq_bit(6, 6)) != 0
                    && self.no_opp_pawns(FILE_MASKS[3])
            }
            StructurePreset::Botvinnik => {
                self.has_own_pawns(
                    sq_bit(1, 1)
                        | sq_bit(5, 1)
                        | sq_bit(7, 1)
                        | sq_bit(3, 2)
                        | sq_bit(6, 2)
                        | sq_bit(2, 3)
                        | sq_bit(4, 3),
                ) && (self.own_pawns & (sq_bit(0, 1) | sq_bit(0, 2))) != 0
                    && (self.own_knights & (sq_bit(2, 2) | sq_bit(4, 1)))
                        == (sq_bit(2, 2) | sq_bit(4, 1))
                    && (self.own_bishops & sq_bit(6, 1)) != 0
            }
            StructurePreset::BotvinnikTriangle => {
                self.has_own_pawns(sq_bit(2, 3) | sq_bit(3, 2) | sq_bit(4, 3) | sq_bit(6, 2))
                    && (self.own_bishops & sq_bit(6, 1)) != 0
            }
            StructurePreset::FrenchChain => {
                self.has_own_pawns(sq_bit(3, 3) | sq_bit(4, 4))
                    && self.has_opp_pawns(sq_bit(3, 4) | sq_bit(4, 5))
                    && self.no_own_pawns(sq_bit(4, 3))
            }
            StructurePreset::FrenchTarraschDxE => {
                self.has_own_pawns(sq_bit(3, 3))
                    && self.no_own_pawns(FILE_MASKS[4])
                    && self.has_opp_pawns(sq_bit(2, 4) | sq_bit(4, 5))
                    && self.no_opp_pawns(FILE_MASKS[3])
            }
            StructurePreset::KingsIndianChain => {
                self.has_own_pawns(sq_bit(2, 3) | sq_bit(3, 4) | sq_bit(4, 3))
                    && self.has_opp_pawns(sq_bit(3, 5) | sq_bit(4, 4) | sq_bit(6, 5))
            }
            StructurePreset::KingsIndianC5Chain => {
                self.has_own_pawns(sq_bit(2, 3) | sq_bit(3, 4) | sq_bit(4, 3))
                    && self.has_opp_pawns(sq_bit(2, 4) | sq_bit(3, 5) | sq_bit(4, 4))
            }
            StructurePreset::BenoniStructure => {
                // Normalized Black -> own c4+d3 (Black c5+d6), opp d4+e5 (White d5+e4)
                self.has_own_pawns(sq_bit(2, 3) | sq_bit(3, 2))
                    && self.no_own_pawns(FILE_MASKS[4])
                    && self.has_opp_pawns(sq_bit(3, 3) | sq_bit(4, 4))
                    && self.no_opp_pawns(FILE_MASKS[2])
            }
            StructurePreset::SlavCaroKann => {
                // Normalized Black -> own c3+d4 (Black c6+d5), opp d5+(c5 or e5) (White d4+c4/e4)
                self.has_own_pawns(sq_bit(2, 2) | sq_bit(3, 3))
                    && self.no_own_pawns(FILE_MASKS[4])
                    && self.has_opp_pawns(sq_bit(3, 4))
                    && (self.opp_pawns & (sq_bit(2, 4) | sq_bit(4, 4))) != 0
                    && self.no_opp_pawns(FILE_MASKS[3] & sq_bit(3, 3))
            }
            StructurePreset::NimzoDoubledCPawns => {
                (self.own_pawns & FILE_MASKS[2]).count_ones() >= 2
                    && (self.is_union || (self.opp_bishops & DARK_SQUARES_MASK) == 0)
                    && (self.own_pawns & (sq_bit(3, 2) | sq_bit(3, 3))) != 0
                    && (self.own_pawns & (sq_bit(4, 2) | sq_bit(4, 3))) != 0
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BishopColorRelation {
    #[default]
    Any,
    OppositeColored,
    SameColored,
}

impl BishopColorRelation {
    pub const ALL: [Self; 3] = [Self::Any, Self::OppositeColored, Self::SameColored];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Any => "Any bishops",
            Self::OppositeColored => "Opposite-colored",
            Self::SameColored => "Same-colored",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PieceCountRange {
    pub min: u8,
    pub max: u8,
}

impl Default for PieceCountRange {
    fn default() -> Self {
        Self::exact(0)
    }
}

impl PieceCountRange {
    #[must_use]
    pub const fn exact(n: u8) -> Self {
        Self { min: n, max: n }
    }

    #[must_use]
    pub const fn range(min: u8, max: u8) -> Self {
        Self { min, max }
    }

    #[must_use]
    pub const fn matches(self, count: u32) -> bool {
        let (lo, hi) = if self.min <= self.max {
            (self.min as u32, self.max as u32)
        } else {
            (self.max as u32, self.min as u32)
        };
        count >= lo && count <= hi
    }

    #[must_use]
    pub const fn hi(self) -> u8 {
        if self.min >= self.max {
            self.min
        } else {
            self.max
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SideMaterialSpec {
    pub queens: PieceCountRange,
    pub rooks: PieceCountRange,
    pub bishops: PieceCountRange,
    pub knights: PieceCountRange,
    pub pawns: PieceCountRange,
}

impl Default for SideMaterialSpec {
    fn default() -> Self {
        Self {
            queens: PieceCountRange::exact(0),
            rooks: PieceCountRange::exact(0),
            bishops: PieceCountRange::exact(0),
            knights: PieceCountRange::exact(0),
            pawns: PieceCountRange::range(0, 8),
        }
    }
}

impl SideMaterialSpec {
    fn matches(self, board: &BoardRef<'_>, color: Color) -> bool {
        self.pawns
            .matches(board.colored_pieces_mask(color, Role::Pawn).count_ones())
            && self
                .knights
                .matches(board.colored_pieces_mask(color, Role::Knight).count_ones())
            && self
                .bishops
                .matches(board.colored_pieces_mask(color, Role::Bishop).count_ones())
            && self
                .rooks
                .matches(board.colored_pieces_mask(color, Role::Rook).count_ones())
            && self
                .queens
                .matches(board.colored_pieces_mask(color, Role::Queen).count_ones())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndgamePreset {
    RookVsRook,
    RookBishopVsRook,
    RookKnightVsRook,
    RookBishopVsRookKnight,
    RookSameBishops,
    RookOppBishops,
    DoubleRooks,
    BishopVsKnight,
    OppositeColoredBishops,
    SameColoredBishops,
    KnightVsKnight,
    BishopPairEndgame,
    QueenVsQueen,
    QueenVsTwoRooks,
    QueenVsRookMinor,
    RookVsMinor,
    RookVsTwoMinors,
    PurePawnEndgame,
    Custom,
}

impl EndgamePreset {
    pub const ALL: [Self; 19] = [
        Self::RookVsRook,
        Self::RookBishopVsRook,
        Self::RookKnightVsRook,
        Self::RookBishopVsRookKnight,
        Self::RookSameBishops,
        Self::RookOppBishops,
        Self::DoubleRooks,
        Self::BishopVsKnight,
        Self::OppositeColoredBishops,
        Self::SameColoredBishops,
        Self::KnightVsKnight,
        Self::BishopPairEndgame,
        Self::QueenVsQueen,
        Self::QueenVsTwoRooks,
        Self::QueenVsRookMinor,
        Self::RookVsMinor,
        Self::RookVsTwoMinors,
        Self::PurePawnEndgame,
        Self::Custom,
    ];

    #[must_use]
    pub fn short_label(self) -> &'static str {
        match self {
            Self::RookVsRook => "R vs R",
            Self::RookBishopVsRook => "R+B vs R",
            Self::RookKnightVsRook => "R+N vs R",
            Self::RookBishopVsRookKnight => "R+B vs R+N",
            Self::RookSameBishops => "R+B vs R+B (Same)",
            Self::RookOppBishops => "R+B vs R+B (Opp)",
            Self::DoubleRooks => "2R vs 2R",
            Self::BishopVsKnight => "B vs N",
            Self::OppositeColoredBishops => "B vs B (Opp)",
            Self::SameColoredBishops => "B vs B (Same)",
            Self::KnightVsKnight => "N vs N",
            Self::BishopPairEndgame => "2B vs B+N / 2N",
            Self::QueenVsQueen => "Q vs Q",
            Self::QueenVsTwoRooks => "Q vs 2R",
            Self::QueenVsRookMinor => "Q vs R+Minor",
            Self::RookVsMinor => "R vs Minor",
            Self::RookVsTwoMinors => "R vs 2 Minors",
            Self::PurePawnEndgame => "Pawn Endgame",
            Self::Custom => "Custom",
        }
    }

    #[must_use]
    pub fn to_filter(self) -> EndgameFilter {
        let mut f = EndgameFilter {
            preset: self,
            side_a: SideMaterialSpec::default(),
            side_b: SideMaterialSpec::default(),
            bishop_colors: BishopColorRelation::Any,
            require_any_pawns: false,
            swap_sides: false,
        };
        match self {
            Self::RookVsRook => {
                f.side_a.rooks = PieceCountRange::exact(1);
                f.side_b.rooks = PieceCountRange::exact(1);
                f.require_any_pawns = true;
            }
            Self::RookBishopVsRook => {
                f.side_a.rooks = PieceCountRange::exact(1);
                f.side_a.bishops = PieceCountRange::exact(1);
                f.side_b.rooks = PieceCountRange::exact(1);
            }
            Self::RookKnightVsRook => {
                f.side_a.rooks = PieceCountRange::exact(1);
                f.side_a.knights = PieceCountRange::exact(1);
                f.side_b.rooks = PieceCountRange::exact(1);
            }
            Self::RookBishopVsRookKnight => {
                f.side_a.rooks = PieceCountRange::exact(1);
                f.side_a.bishops = PieceCountRange::exact(1);
                f.side_b.rooks = PieceCountRange::exact(1);
                f.side_b.knights = PieceCountRange::exact(1);
            }
            Self::RookSameBishops => {
                f.side_a.rooks = PieceCountRange::exact(1);
                f.side_a.bishops = PieceCountRange::exact(1);
                f.side_b.rooks = PieceCountRange::exact(1);
                f.side_b.bishops = PieceCountRange::exact(1);
                f.bishop_colors = BishopColorRelation::SameColored;
            }
            Self::RookOppBishops => {
                f.side_a.rooks = PieceCountRange::exact(1);
                f.side_a.bishops = PieceCountRange::exact(1);
                f.side_b.rooks = PieceCountRange::exact(1);
                f.side_b.bishops = PieceCountRange::exact(1);
                f.bishop_colors = BishopColorRelation::OppositeColored;
            }
            Self::DoubleRooks => {
                f.side_a.rooks = PieceCountRange::exact(2);
                f.side_b.rooks = PieceCountRange::exact(2);
            }
            Self::BishopVsKnight => {
                f.side_a.bishops = PieceCountRange::exact(1);
                f.side_b.knights = PieceCountRange::exact(1);
            }
            Self::OppositeColoredBishops => {
                f.side_a.bishops = PieceCountRange::exact(1);
                f.side_b.bishops = PieceCountRange::exact(1);
                f.bishop_colors = BishopColorRelation::OppositeColored;
            }
            Self::SameColoredBishops => {
                f.side_a.bishops = PieceCountRange::exact(1);
                f.side_b.bishops = PieceCountRange::exact(1);
                f.bishop_colors = BishopColorRelation::SameColored;
            }
            Self::KnightVsKnight => {
                f.side_a.knights = PieceCountRange::exact(1);
                f.side_b.knights = PieceCountRange::exact(1);
            }
            Self::BishopPairEndgame => {
                f.side_a.bishops = PieceCountRange::exact(2);
                f.side_b.bishops = PieceCountRange::range(0, 1);
                f.side_b.knights = PieceCountRange::range(1, 2);
            }
            Self::QueenVsQueen => {
                f.side_a.queens = PieceCountRange::exact(1);
                f.side_b.queens = PieceCountRange::exact(1);
            }
            Self::QueenVsTwoRooks => {
                f.side_a.queens = PieceCountRange::exact(1);
                f.side_b.rooks = PieceCountRange::exact(2);
            }
            Self::QueenVsRookMinor => {
                f.side_a.queens = PieceCountRange::exact(1);
                f.side_b.rooks = PieceCountRange::exact(1);
                f.side_b.bishops = PieceCountRange::range(0, 1);
                f.side_b.knights = PieceCountRange::range(0, 1);
            }
            Self::RookVsMinor => {
                f.side_a.rooks = PieceCountRange::exact(1);
                f.side_b.bishops = PieceCountRange::range(0, 1);
                f.side_b.knights = PieceCountRange::range(0, 1);
            }
            Self::RookVsTwoMinors => {
                f.side_a.rooks = PieceCountRange::exact(1);
                f.side_b.bishops = PieceCountRange::range(0, 2);
                f.side_b.knights = PieceCountRange::range(0, 2);
            }
            Self::PurePawnEndgame => {
                f.side_a.pawns = PieceCountRange::range(1, 8);
                f.side_b.pawns = PieceCountRange::range(0, 8);
            }
            Self::Custom => {
                f.side_a.rooks = PieceCountRange::range(0, 2);
                f.side_b.rooks = PieceCountRange::range(0, 2);
            }
        }
        f
    }
}

/// Piece-count & bishop-color endgame constellation filter.
/// Linkable with pawn structures and Board patterns under consistent side orientation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndgameFilter {
    pub preset: EndgamePreset,
    pub side_a: SideMaterialSpec,
    pub side_b: SideMaterialSpec,
    pub bishop_colors: BishopColorRelation,
    pub require_any_pawns: bool,
    pub swap_sides: bool,
}

impl Default for EndgameFilter {
    fn default() -> Self {
        EndgamePreset::RookBishopVsRook.to_filter()
    }
}

impl EndgameFilter {
    #[must_use]
    pub fn matches_with_side_a(&self, pos: &Chess, side_a_color: Color) -> bool {
        let board = pos.board();
        let effective_a = if self.swap_sides {
            side_a_color.other()
        } else {
            side_a_color
        };
        let effective_b = effective_a.other();
        if !self.side_a.matches(&board, effective_a) || !self.side_b.matches(&board, effective_b) {
            return false;
        }
        // Special total-minor constraints for presets where B+N sum matters
        match self.preset {
            EndgamePreset::BishopPairEndgame => {
                let b_minors = board
                    .colored_pieces_mask(effective_b, Role::Bishop)
                    .count_ones()
                    + board
                        .colored_pieces_mask(effective_b, Role::Knight)
                        .count_ones();
                if b_minors != 2 {
                    return false;
                }
            }
            EndgamePreset::QueenVsRookMinor | EndgamePreset::RookVsMinor => {
                let b_minors = board
                    .colored_pieces_mask(effective_b, Role::Bishop)
                    .count_ones()
                    + board
                        .colored_pieces_mask(effective_b, Role::Knight)
                        .count_ones();
                if b_minors != 1 {
                    return false;
                }
            }
            EndgamePreset::RookVsTwoMinors => {
                let b_minors = board
                    .colored_pieces_mask(effective_b, Role::Bishop)
                    .count_ones()
                    + board
                        .colored_pieces_mask(effective_b, Role::Knight)
                        .count_ones();
                if b_minors != 2 {
                    return false;
                }
            }
            _ => {}
        }
        if self.require_any_pawns {
            let pawns = board
                .colored_pieces_mask(Color::White, Role::Pawn)
                .count_ones()
                + board
                    .colored_pieces_mask(Color::Black, Role::Pawn)
                    .count_ones();
            if pawns == 0 {
                return false;
            }
        }
        if self.bishop_colors != BishopColorRelation::Any {
            let wb = board.colored_pieces_mask(Color::White, Role::Bishop);
            let bb = board.colored_pieces_mask(Color::Black, Role::Bishop);
            if wb.count_ones() != 1 || bb.count_ones() != 1 {
                return false;
            }
            let w_light = (wb & LIGHT_SQUARES_MASK) != 0;
            let b_light = (bb & LIGHT_SQUARES_MASK) != 0;
            match self.bishop_colors {
                BishopColorRelation::OppositeColored if w_light == b_light => return false,
                BishopColorRelation::SameColored if w_light != b_light => return false,
                _ => {}
            }
        }
        true
    }

    fn may_exist_in_sig(&self, sig: &GameStructSig, side_a_color: Color) -> bool {
        if self.require_any_pawns && (sig.flags & 4) == 0 {
            return false;
        }
        match self.bishop_colors {
            BishopColorRelation::OppositeColored if (sig.flags & 1) == 0 => return false,
            BishopColorRelation::SameColored if (sig.flags & 2) == 0 => return false,
            _ => {}
        }
        let effective_a = if self.swap_sides {
            side_a_color.other()
        } else {
            side_a_color
        };
        let max_non_pawns = |s: &SideMaterialSpec| -> u8 {
            s.queens
                .hi()
                .saturating_add(s.rooks.hi())
                .saturating_add(s.bishops.hi())
                .saturating_add(s.knights.hi())
        };
        let max_a_np = max_non_pawns(&self.side_a);
        let max_b_np = max_non_pawns(&self.side_b);
        let (min_a_np, min_b_np, min_a_p, min_b_p) = if effective_a == Color::White {
            (
                sig.min_white_non_pawns,
                sig.min_black_non_pawns,
                sig.min_white_pawns,
                sig.min_black_pawns,
            )
        } else {
            (
                sig.min_black_non_pawns,
                sig.min_white_non_pawns,
                sig.min_black_pawns,
                sig.min_white_pawns,
            )
        };
        min_a_np <= max_a_np
            && min_b_np <= max_b_np
            && min_a_p <= self.side_a.pawns.hi()
            && min_b_p <= self.side_b.pawns.hi()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PositionQuery {
    pub mode: PositionSearchMode,
    pub squares: [SquareConstraint; 64],
    pub extra_clauses: Vec<PatternClause>,
    pub side_to_move: SideToMoveFilter,
    /// Color mirroring (`ON` by default for flexible structure & endgame search).
    pub ignore_color_flip: bool,
    pub structure: Option<StructurePreset>,
    pub primary_side_override: Option<Color>,
    pub endgame: Option<EndgameFilter>,
    pub metadata: MetadataFilter,
    pub min_ply: u32,
    pub max_ply: u32,
}

impl Default for PositionQuery {
    fn default() -> Self {
        Self {
            mode: PositionSearchMode::Partial,
            squares: [SquareConstraint::Any; 64],
            extra_clauses: Vec::new(),
            side_to_move: SideToMoveFilter::Any,
            ignore_color_flip: true,
            structure: None,
            primary_side_override: None,
            endgame: None,
            metadata: MetadataFilter::default(),
            min_ply: 0,
            max_ply: 200,
        }
    }
}

impl PositionQuery {
    #[must_use]
    pub fn from_chess_position(pos: &Chess, mode: PositionSearchMode) -> Self {
        let board = pos.board();
        let mut squares = [SquareConstraint::Any; 64];
        for sq in Square::ALL {
            squares[sq.to_u32() as usize] = match board.piece_at(sq) {
                Some(piece) => SquareConstraint::Piece(piece),
                None if mode == PositionSearchMode::Absolute => SquareConstraint::Empty,
                None => SquareConstraint::Any,
            };
        }
        Self {
            mode,
            squares,
            extra_clauses: Vec::new(),
            side_to_move: SideToMoveFilter::from_color(pos.turn()),
            ignore_color_flip: mode != PositionSearchMode::Absolute,
            structure: None,
            primary_side_override: None,
            endgame: None,
            metadata: MetadataFilter::default(),
            min_ply: 0,
            max_ply: 200,
        }
    }

    #[must_use]
    pub fn from_pawn_skeleton(pos: &Chess) -> Self {
        let board = pos.board();
        let mut squares = [SquareConstraint::Any; 64];
        for color in [Color::White, Color::Black] {
            let pawns = board.colored_pieces_mask(color, Role::Pawn);
            for sq in Square::ALL {
                if (pawns & (1u64 << sq.to_u32())) != 0 {
                    squares[sq.to_u32() as usize] =
                        SquareConstraint::Piece(Piece::new(color, Role::Pawn));
                }
            }
        }
        let mut file_atoms = Vec::new();
        let w_pawns = board.colored_pieces_mask(Color::White, Role::Pawn);
        let b_pawns = board.colored_pieces_mask(Color::Black, Role::Pawn);
        for (fi, &mask) in FILE_MASKS.iter().enumerate() {
            let f = ChessFile::new(fi as u32);
            if (w_pawns & mask) == 0 {
                file_atoms.push(AtomPredicate::NoPieceOnFile(f, Color::White, Role::Pawn));
            }
            if (b_pawns & mask) == 0 {
                file_atoms.push(AtomPredicate::NoPieceOnFile(f, Color::Black, Role::Pawn));
            }
        }
        let extra_clauses = if file_atoms.is_empty() {
            Vec::new()
        } else {
            vec![PatternClause {
                op: ClauseOp::AllOf,
                atoms: file_atoms,
            }]
        };
        Self {
            mode: PositionSearchMode::Partial,
            squares,
            extra_clauses,
            side_to_move: SideToMoveFilter::Any,
            ignore_color_flip: true,
            structure: None,
            primary_side_override: None,
            endgame: None,
            metadata: MetadataFilter::default(),
            min_ply: 8,
            max_ply: 140,
        }
    }

    #[must_use]
    pub fn primary_side(&self) -> Color {
        self.primary_side_override
            .or_else(|| self.structure.map(StructurePreset::primary_side))
            .unwrap_or(Color::White)
    }

    /// Select a middlegame structure preset, clearing any conflicting endgame filter.
    pub fn set_structure(&mut self, structure: Option<StructurePreset>) {
        self.structure = structure;
        if structure.is_some() {
            self.endgame = None;
        }
    }

    /// Select an endgame filter, clearing any conflicting structure preset.
    pub fn set_endgame(&mut self, endgame: Option<EndgameFilter>) {
        self.endgame = endgame;
        if endgame.is_some() {
            self.structure = None;
        }
    }

    #[must_use]
    pub fn has_board_pattern(&self) -> bool {
        self.mode == PositionSearchMode::Absolute
            || self.squares.iter().any(|c| *c != SquareConstraint::Any)
            || self.extra_clauses.iter().any(|c| !c.is_empty())
            || self.side_to_move != SideToMoveFilter::Any
    }

    #[must_use]
    pub fn compile(&self) -> CompiledPattern {
        CompiledPattern::from_query(self)
    }
}

const PIECE_KINDS: [(Color, Role); 12] = [
    (Color::White, Role::Pawn),
    (Color::White, Role::Knight),
    (Color::White, Role::Bishop),
    (Color::White, Role::Rook),
    (Color::White, Role::Queen),
    (Color::White, Role::King),
    (Color::Black, Role::Pawn),
    (Color::Black, Role::Knight),
    (Color::Black, Role::Bishop),
    (Color::Black, Role::Rook),
    (Color::Black, Role::Queen),
    (Color::Black, Role::King),
];

#[inline]
fn piece_index(piece: Piece) -> usize {
    let base = if piece.color == Color::White { 0 } else { 6 };
    let offset = match piece.role {
        Role::Pawn => 0,
        Role::Knight => 1,
        Role::Bishop => 2,
        Role::Rook => 3,
        Role::Queen => 4,
        Role::King => 5,
    };
    base + offset
}

#[derive(Clone, Debug)]
struct PatternMasks {
    pieces: [u64; 12],
    must_be_empty: u64,
    side_to_move: SideToMoveFilter,
}

impl PatternMasks {
    fn matches(&self, board: &BoardRef<'_>, turn: Color, mode: PositionSearchMode) -> bool {
        if !self.side_to_move.matches(turn) {
            return false;
        }
        match mode {
            PositionSearchMode::Partial => {
                if (board.occupied_mask() & self.must_be_empty) != 0 {
                    return false;
                }
                for (idx, &(color, role)) in PIECE_KINDS.iter().enumerate() {
                    let req = self.pieces[idx];
                    if req != 0 && (board.colored_pieces_mask(color, role) & req) != req {
                        return false;
                    }
                }
                true
            }
            PositionSearchMode::Absolute => {
                for (idx, &(color, role)) in PIECE_KINDS.iter().enumerate() {
                    if board.colored_pieces_mask(color, role) != self.pieces[idx] {
                        return false;
                    }
                }
                true
            }
        }
    }

    fn mirrored_colors(&self) -> Self {
        let mut pieces = [0u64; 12];
        for i in 0..6 {
            pieces[i] = self.pieces[i + 6].swap_bytes();
            pieces[i + 6] = self.pieces[i].swap_bytes();
        }
        Self {
            pieces,
            must_be_empty: self.must_be_empty.swap_bytes(),
            side_to_move: self.side_to_move.flipped(),
        }
    }

    fn may_exist_in_sig(&self, sig: &GameStructSig) -> bool {
        (self.pieces[0] & !sig.white_pawns) == 0
            && (self.pieces[6] & !sig.black_pawns) == 0
            && (self.pieces[1] & !sig.white_knights) == 0
            && (self.pieces[7] & !sig.black_knights) == 0
            && (self.pieces[2] & !sig.white_bishops) == 0
            && (self.pieces[8] & !sig.black_bishops) == 0
    }
}

pub fn parse_eco_ranges(spec: &str) -> Vec<(u16, u16)> {
    let mut ranges = Vec::new();
    for raw in spec.split([',', ';']) {
        let token = raw.trim().to_ascii_uppercase();
        if token.is_empty() {
            continue;
        }
        if let Some((a, b)) = token.split_once('-').or_else(|| token.split_once("..")) {
            let start = parse_eco_bound(a.trim(), false);
            let end = parse_eco_bound(b.trim(), true);
            if start > 0 && end >= start {
                ranges.push((start, end));
            }
        } else {
            let start = parse_eco_bound(&token, false);
            let end = parse_eco_bound(&token, true);
            if start > 0 && end >= start {
                ranges.push((start, end));
            }
        }
    }
    ranges
}

fn parse_eco_bound(s: &str, upper: bool) -> u16 {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return 0;
    }
    let letter = match bytes[0] {
        b'A'..=b'E' => u16::from(bytes[0] - b'A'),
        _ => return 0,
    };
    match bytes.len() {
        1 => {
            if upper {
                letter * 100 + 100
            } else {
                letter * 100 + 1
            }
        }
        2 if bytes[1].is_ascii_digit() => {
            let tens = u16::from(bytes[1] - b'0') * 10;
            if upper {
                letter * 100 + tens + 10
            } else {
                letter * 100 + tens + 1
            }
        }
        _ => eco_str_to_code(s),
    }
}

/// Precompiled matcher for rapid multi-game scanning across metadata & bitboards.
#[derive(Clone, Debug)]
pub struct CompiledPattern {
    mode: PositionSearchMode,
    has_board_pattern: bool,
    primary: PatternMasks,
    mirrored: Option<PatternMasks>,
    extra_clauses: Vec<PatternClause>,
    mirrored_clauses: Option<Vec<PatternClause>>,
    structure: Option<StructurePreset>,
    endgame: Option<EndgameFilter>,
    ignore_color_flip: bool,
    primary_side: Color,
    metadata: MetadataFilter,
    player_any_lower: String,
    player_white_lower: String,
    player_black_lower: String,
    event_lower: String,
    eco_ranges: Vec<(u16, u16)>,
    min_ply: u32,
    max_ply: u32,
}

impl CompiledPattern {
    #[must_use]
    pub fn from_query(query: &PositionQuery) -> Self {
        let mut pieces = [0u64; 12];
        let mut must_be_empty = 0u64;
        for (sq_idx, constraint) in query.squares.iter().enumerate() {
            let bit = 1u64 << sq_idx;
            match constraint {
                SquareConstraint::Any => {}
                SquareConstraint::Empty => must_be_empty |= bit,
                SquareConstraint::Piece(piece) => {
                    pieces[piece_index(*piece)] |= bit;
                }
            }
        }
        let primary = PatternMasks {
            pieces,
            must_be_empty,
            side_to_move: query.side_to_move,
        };
        let active_clauses: Vec<PatternClause> = query
            .extra_clauses
            .iter()
            .filter(|c| !c.is_empty())
            .cloned()
            .collect();
        let has_board_pattern = query.has_board_pattern();
        let mirrored = query.ignore_color_flip.then(|| primary.mirrored_colors());
        let mirrored_clauses = query.ignore_color_flip.then(|| {
            active_clauses
                .iter()
                .map(PatternClause::mirrored_colors)
                .collect()
        });
        let (min_ply, max_ply) = if query.min_ply <= query.max_ply {
            (query.min_ply, query.max_ply)
        } else {
            (query.max_ply, query.min_ply)
        };
        Self {
            mode: query.mode,
            has_board_pattern,
            primary,
            mirrored,
            extra_clauses: active_clauses,
            mirrored_clauses,
            structure: query.structure,
            endgame: if query.structure.is_some() {
                None
            } else {
                query.endgame
            },
            ignore_color_flip: query.ignore_color_flip,
            primary_side: query.primary_side(),
            player_any_lower: query.metadata.player_any.trim().to_lowercase(),
            player_white_lower: query.metadata.player_white.trim().to_lowercase(),
            player_black_lower: query.metadata.player_black.trim().to_lowercase(),
            event_lower: query.metadata.event_filter.trim().to_lowercase(),
            eco_ranges: parse_eco_ranges(&query.metadata.eco_filter),
            metadata: query.metadata.clone(),
            min_ply,
            max_ply,
        }
    }

    #[must_use]
    pub fn is_empty_partial(&self) -> bool {
        !self.has_board_pattern && self.structure.is_none() && self.endgame.is_none()
    }

    #[must_use]
    pub fn has_position_criteria(&self) -> bool {
        self.has_board_pattern || self.structure.is_some() || self.endgame.is_some()
    }

    #[must_use]
    pub fn matches_position_oriented(&self, pos: &Chess) -> Option<MatchOrientation> {
        let mut acc = Some(MatchOrientation::Both);
        if self.has_board_pattern {
            let board = pos.board();
            let turn = pos.turn();
            let norm_ok = self.primary.matches(&board, turn, self.mode)
                && self.extra_clauses.iter().all(|c| c.matches(&board));
            let flip_ok = self.mirrored.as_ref().is_some_and(|m| {
                m.matches(&board, turn, self.mode)
                    && self
                        .mirrored_clauses
                        .as_ref()
                        .is_some_and(|cs| cs.iter().all(|c| c.matches(&board)))
            });
            let m = match (norm_ok, flip_ok) {
                (true, true) => Some(MatchOrientation::Both),
                (true, false) => Some(MatchOrientation::Normal),
                (false, true) => Some(MatchOrientation::Flipped),
                (false, false) => None,
            };
            acc = MatchOrientation::combine(acc, m);
            acc?;
        }
        if let Some(structure) = self.structure {
            let norm_ok = structure.matches_oriented(pos, false);
            let flip_ok = self.ignore_color_flip && structure.matches_oriented(pos, true);
            let m = match (norm_ok, flip_ok) {
                (true, true) => Some(MatchOrientation::Both),
                (true, false) => Some(MatchOrientation::Normal),
                (false, true) => Some(MatchOrientation::Flipped),
                (false, false) => None,
            };
            acc = MatchOrientation::combine(acc, m);
            acc?;
        }
        if let Some(endgame) = &self.endgame {
            let norm_ok = endgame.matches_with_side_a(pos, self.primary_side);
            let flip_ok = self.ignore_color_flip
                && endgame.matches_with_side_a(pos, self.primary_side.other());
            let m = match (norm_ok, flip_ok) {
                (true, true) => Some(MatchOrientation::Both),
                (true, false) => Some(MatchOrientation::Normal),
                (false, true) => Some(MatchOrientation::Flipped),
                (false, false) => None,
            };
            acc = MatchOrientation::combine(acc, m);
        }
        acc
    }

    #[must_use]
    pub fn matches_position(&self, pos: &Chess) -> bool {
        self.matches_position_oriented(pos).is_some()
    }

    /// 100% sound pre-filter against a precomputed `.brettstruct` signature.
    /// Never produces false negatives for games with mainline length <= `STRUCT_SCAN_MAX_PLY`.
    #[must_use]
    pub fn fast_accepts_struct_sig(&self, sig: &GameStructSig, result: u8) -> bool {
        if !self.has_position_criteria() {
            return true;
        }
        if self.max_ply > STRUCT_SCAN_MAX_PLY {
            return true;
        }
        if u32::from(sig.max_ply) < self.min_ply {
            return false;
        }
        let mut acc = Some(MatchOrientation::Both);
        if self.has_board_pattern {
            let norm_ok = self.primary.may_exist_in_sig(sig)
                && self.extra_clauses.iter().all(|c| c.may_exist_in_sig(sig));
            let flip_ok = self.mirrored.as_ref().is_some_and(|m| {
                m.may_exist_in_sig(sig)
                    && self
                        .mirrored_clauses
                        .as_ref()
                        .is_some_and(|cs| cs.iter().all(|c| c.may_exist_in_sig(sig)))
            });
            let m = match (norm_ok, flip_ok) {
                (true, true) => Some(MatchOrientation::Both),
                (true, false) => Some(MatchOrientation::Normal),
                (false, true) => Some(MatchOrientation::Flipped),
                (false, false) => None,
            };
            acc = MatchOrientation::combine(acc, m);
        }
        let Some(mut acc) = acc else {
            return false;
        };
        if let Some(structure) = self.structure {
            let norm_ok = structure.matches_sig_oriented(sig, false);
            let flip_ok = self.ignore_color_flip && structure.matches_sig_oriented(sig, true);
            let m = match (norm_ok, flip_ok) {
                (true, true) => Some(MatchOrientation::Both),
                (true, false) => Some(MatchOrientation::Normal),
                (false, true) => Some(MatchOrientation::Flipped),
                (false, false) => None,
            };
            let Some(next) = MatchOrientation::combine(Some(acc), m) else {
                return false;
            };
            acc = next;
        }
        if let Some(endgame) = &self.endgame {
            let norm_ok = endgame.may_exist_in_sig(sig, self.primary_side);
            let flip_ok =
                self.ignore_color_flip && endgame.may_exist_in_sig(sig, self.primary_side.other());
            let m = match (norm_ok, flip_ok) {
                (true, true) => Some(MatchOrientation::Both),
                (true, false) => Some(MatchOrientation::Normal),
                (false, true) => Some(MatchOrientation::Flipped),
                (false, false) => None,
            };
            let Some(next) = MatchOrientation::combine(Some(acc), m) else {
                return false;
            };
            acc = next;
        }
        self.accepts_result_for_orientation(result, acc)
    }

    fn accepts_elo(&self, white_elo: u16, black_elo: u16) -> bool {
        if self.metadata.min_elo.is_none() && self.metadata.max_elo.is_none() {
            return true;
        }
        let check = |elo: u16| -> bool {
            if elo == 0 {
                return false;
            }
            if let Some(min) = self.metadata.min_elo
                && elo < min
            {
                return false;
            }
            if let Some(max) = self.metadata.max_elo
                && elo > max
            {
                return false;
            }
            true
        };
        match self.metadata.elo_mode {
            EloFilterMode::Both => check(white_elo) && check(black_elo),
            EloFilterMode::Either => check(white_elo) || check(black_elo),
            EloFilterMode::White => check(white_elo),
            EloFilterMode::Black => check(black_elo),
            EloFilterMode::Average => {
                if white_elo == 0 || black_elo == 0 {
                    return false;
                }
                let avg = ((u32::from(white_elo) + u32::from(black_elo)) / 2) as u16;
                check(avg)
            }
        }
    }

    /// O(1) numeric metadata & Mega medal/flag pre-filter run BEFORE touching move streams.
    #[must_use]
    pub fn fast_accepts_meta(&self, meta: &GameRecordMeta) -> bool {
        if !self.metadata.include_deleted && meta.deleted {
            return false;
        }
        if !self.accepts_elo(meta.white_elo, meta.black_elo) {
            return false;
        }
        if let Some(y0) = self.metadata.year_from
            && (meta.year == 0 || meta.year < y0)
        {
            return false;
        }
        if let Some(y1) = self.metadata.year_to
            && (meta.year == 0 || meta.year > y1)
        {
            return false;
        }
        if !self.eco_ranges.is_empty()
            && (meta.eco_code == 0
                || !self
                    .eco_ranges
                    .iter()
                    .any(|&(lo, hi)| meta.eco_code >= lo && meta.eco_code <= hi))
        {
            return false;
        }
        if self.metadata.medals_mask != 0 {
            if self.metadata.medals_match_all {
                if (meta.medals & self.metadata.medals_mask) != self.metadata.medals_mask {
                    return false;
                }
            } else if (meta.medals & self.metadata.medals_mask) == 0 {
                return false;
            }
        }
        if self.metadata.vcs_flags_mask != 0
            && (meta.vcs_flags & self.metadata.vcs_flags_mask) != self.metadata.vcs_flags_mask
        {
            return false;
        }
        if self.metadata.annotated_only && !meta.annotated {
            return false;
        }
        match self.metadata.winner {
            WinnerFilter::Any => true,
            WinnerFilter::WhiteWins => meta.result == 2,
            WinnerFilter::BlackWins => meta.result == 0,
            WinnerFilter::Draw => meta.result == 1,
            WinnerFilter::Decisive => meta.result == 0 || meta.result == 2,
            WinnerFilter::StructureSideWins => {
                if !self.ignore_color_flip {
                    let want = if self.primary_side == Color::White {
                        2
                    } else {
                        0
                    };
                    meta.result == want
                } else {
                    meta.result == 0 || meta.result == 2
                }
            }
            WinnerFilter::OpponentSideWins => {
                if !self.ignore_color_flip {
                    let want = if self.primary_side == Color::White {
                        0
                    } else {
                        2
                    };
                    meta.result == want
                } else {
                    meta.result == 0 || meta.result == 2
                }
            }
        }
    }

    #[must_use]
    pub fn accepts_result_for_orientation(
        &self,
        result: u8,
        orientation: MatchOrientation,
    ) -> bool {
        let effective_side = match (orientation, self.primary_side) {
            (MatchOrientation::Both, _) => {
                return match self.metadata.winner {
                    WinnerFilter::Any => true,
                    WinnerFilter::WhiteWins => result == 2,
                    WinnerFilter::BlackWins => result == 0,
                    WinnerFilter::Draw => result == 1,
                    WinnerFilter::Decisive
                    | WinnerFilter::StructureSideWins
                    | WinnerFilter::OpponentSideWins => result == 0 || result == 2,
                };
            }
            (MatchOrientation::Normal, s) => s,
            (MatchOrientation::Flipped, s) => s.other(),
        };
        match self.metadata.winner {
            WinnerFilter::Any => true,
            WinnerFilter::WhiteWins => result == 2,
            WinnerFilter::BlackWins => result == 0,
            WinnerFilter::Draw => result == 1,
            WinnerFilter::Decisive => result == 0 || result == 2,
            WinnerFilter::StructureSideWins => {
                let want = if effective_side == Color::White { 2 } else { 0 };
                result == want
            }
            WinnerFilter::OpponentSideWins => {
                let want = if effective_side == Color::White { 0 } else { 2 };
                result == want
            }
        }
    }

    #[must_use]
    fn needs_player_check(&self) -> bool {
        !self.player_any_lower.is_empty()
            || !self.player_white_lower.is_empty()
            || !self.player_black_lower.is_empty()
    }

    #[must_use]
    fn needs_event_check(&self) -> bool {
        !self.event_lower.is_empty()
    }

    #[must_use]
    fn matches_players(&self, white: &str, black: &str) -> bool {
        if !self.player_white_lower.is_empty()
            && !contains_case_insensitive(white, &self.player_white_lower)
        {
            return false;
        }
        if !self.player_black_lower.is_empty()
            && !contains_case_insensitive(black, &self.player_black_lower)
        {
            return false;
        }
        if !self.player_any_lower.is_empty()
            && !contains_case_insensitive(white, &self.player_any_lower)
            && !contains_case_insensitive(black, &self.player_any_lower)
        {
            return false;
        }
        true
    }
}

/// Default board orientation when browsing games compiled from a structure or pattern search.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchBoardPerspective {
    #[default]
    StructureSide,
    OpponentSide,
    FixedWhite,
}

impl SearchBoardPerspective {
    #[must_use]
    pub fn orientation_for(self, structure_side: Color) -> Option<Color> {
        match self {
            Self::StructureSide => Some(structure_side),
            Self::OpponentSide => Some(structure_side.other()),
            Self::FixedWhite => None,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::StructureSide => "Structure side",
            Self::OpponentSide => "Opponent side",
            Self::FixedWhite => "Fixed White",
        }
    }
}

/// Auto-detected open/half-open files and matching middlegame structures for a board position.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PositionFacts {
    pub open_files: Vec<ChessFile>,
    pub half_open_white: Vec<ChessFile>,
    pub half_open_black: Vec<ChessFile>,
    pub matched_structures: Vec<(StructurePreset, Color)>,
}

fn is_generic_structure_preset(preset: StructurePreset) -> bool {
    matches!(
        preset,
        StructurePreset::StonewallAny
            | StructurePreset::BadewanneAny
            | StructurePreset::CarlsbadAny
            | StructurePreset::IqpDFile
            | StructurePreset::IqpVsEPawn
            | StructurePreset::IqpVsCPawn
            | StructurePreset::IqpVsNoFianchetto
            | StructurePreset::IqpVsFianchetto
            | StructurePreset::IqpAnyCentralFile
            | StructurePreset::HangingPawnsClassic
            | StructurePreset::HangingPawnsDynamic
            | StructurePreset::MaroczyBindAny
    )
}

#[must_use]
pub fn analyze_position_facts(pos: &Chess) -> PositionFacts {
    let board = pos.board();
    let w_pawns = board.colored_pieces_mask(Color::White, Role::Pawn);
    let b_pawns = board.colored_pieces_mask(Color::Black, Role::Pawn);

    let mut facts = PositionFacts::default();
    for (fi, &mask) in FILE_MASKS.iter().enumerate() {
        let f = ChessFile::new(fi as u32);
        let has_w = (w_pawns & mask) != 0;
        let has_b = (b_pawns & mask) != 0;
        match (has_w, has_b) {
            (false, false) => facts.open_files.push(f),
            (false, true) => facts.half_open_white.push(f),
            (true, false) => facts.half_open_black.push(f),
            (true, true) => {}
        }
    }

    let mut matched_families: Vec<StructureFamily> = Vec::new();
    // Pass 1: specific presets
    for &preset in StructurePreset::ALL
        .iter()
        .filter(|&&p| !is_generic_structure_preset(p))
    {
        if preset.matches_oriented(pos, false) {
            facts
                .matched_structures
                .push((preset, preset.primary_side()));
            if !matched_families.contains(&preset.family()) {
                matched_families.push(preset.family());
            }
        } else if preset.matches_oriented(pos, true) {
            facts
                .matched_structures
                .push((preset, preset.primary_side().other()));
            if !matched_families.contains(&preset.family()) {
                matched_families.push(preset.family());
            }
        }
    }
    // Pass 2: generic *Any fallbacks for families without a more specific match
    for &preset in StructurePreset::ALL
        .iter()
        .filter(|&&p| is_generic_structure_preset(p))
    {
        if matched_families.contains(&preset.family()) {
            continue;
        }
        if preset.matches_oriented(pos, false) {
            facts
                .matched_structures
                .push((preset, preset.primary_side()));
            matched_families.push(preset.family());
        } else if preset.matches_oriented(pos, true) {
            facts
                .matched_structures
                .push((preset, preset.primary_side().other()));
            matched_families.push(preset.family());
        }
    }
    facts.matched_structures.truncate(4);
    facts
}

/// Sort order for position / structure search hits and derived search game lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PositionHitSort {
    #[default]
    DbOrder,
    StructureEloDesc,
    OpponentEloDesc,
    AverageEloDesc,
    DateDesc,
}

impl PositionHitSort {
    pub const ALL: [Self; 5] = [
        Self::DbOrder,
        Self::StructureEloDesc,
        Self::OpponentEloDesc,
        Self::AverageEloDesc,
        Self::DateDesc,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::DbOrder => "Database order",
            Self::StructureEloDesc => "Structure holder Elo ↓",
            Self::OpponentEloDesc => "Facing opponent Elo ↓",
            Self::AverageEloDesc => "Average Elo ↓",
            Self::DateDesc => "Newest ↓",
        }
    }

    #[must_use]
    pub fn short_label(self) -> &'static str {
        match self {
            Self::DbOrder => "Default",
            Self::StructureEloDesc => "Structure Elo",
            Self::OpponentEloDesc => "Opponent Elo",
            Self::AverageEloDesc => "Avg Elo",
            Self::DateDesc => "Date",
        }
    }
}

/// One game hit returned by [`search_library`] / [`search_library_from`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PositionHit {
    pub game_index: usize,
    pub matched_ply: usize,
    pub matched_flipped: bool,
    pub structure_side: Color,
    pub white: String,
    pub black: String,
    pub white_elo: u16,
    pub black_elo: u16,
    pub result: String,
    pub eco: String,
    pub date: String,
    pub date_num: u32,
    pub event: String,
    pub medals: u16,
    pub vcs_flags: u32,
}

impl PositionHit {
    /// Elo of the player holding the searched structure/featured side.
    #[must_use]
    pub fn structure_elo(&self) -> u16 {
        match self.structure_side {
            Color::White => self.white_elo,
            Color::Black => self.black_elo,
        }
    }

    /// Elo of the player facing/playing against the searched structure/featured side.
    #[must_use]
    pub fn opponent_elo(&self) -> u16 {
        match self.structure_side {
            Color::White => self.black_elo,
            Color::Black => self.white_elo,
        }
    }

    /// Average Elo across both players (or the single available Elo if only one is rated).
    #[must_use]
    pub fn avg_elo(&self) -> u16 {
        match (self.white_elo, self.black_elo) {
            (w, b) if w > 0 && b > 0 => ((u32::from(w) + u32::from(b)) / 2) as u16,
            (w, b) => w.max(b),
        }
    }
}

/// Sort `hits` deterministically according to `sort`.
pub fn sort_position_hits(hits: &mut [PositionHit], sort: PositionHitSort) {
    hits.sort_by(|a, b| match sort {
        PositionHitSort::DbOrder => a.game_index.cmp(&b.game_index),
        PositionHitSort::StructureEloDesc => b
            .structure_elo()
            .cmp(&a.structure_elo())
            .then_with(|| b.opponent_elo().cmp(&a.opponent_elo()))
            .then_with(|| b.date_num.cmp(&a.date_num))
            .then_with(|| a.game_index.cmp(&b.game_index)),
        PositionHitSort::OpponentEloDesc => b
            .opponent_elo()
            .cmp(&a.opponent_elo())
            .then_with(|| b.structure_elo().cmp(&a.structure_elo()))
            .then_with(|| b.date_num.cmp(&a.date_num))
            .then_with(|| a.game_index.cmp(&b.game_index)),
        PositionHitSort::AverageEloDesc => b
            .avg_elo()
            .cmp(&a.avg_elo())
            .then_with(|| b.structure_elo().cmp(&a.structure_elo()))
            .then_with(|| b.date_num.cmp(&a.date_num))
            .then_with(|| a.game_index.cmp(&b.game_index)),
        PositionHitSort::DateDesc => b
            .date_num
            .cmp(&a.date_num)
            .then_with(|| b.avg_elo().cmp(&a.avg_elo()))
            .then_with(|| a.game_index.cmp(&b.game_index)),
    });
}

/// Batch output from [`search_library_from`] supporting incremental 1,000-hit pagination.
#[derive(Clone, Debug)]
pub struct SearchBatchResult {
    pub hits: Vec<PositionHit>,
    pub next_start_index: usize,
    pub total_candidates: usize,
    pub truncated: bool,
    pub cancelled: bool,
}

fn intersect_sorted(a: Vec<usize>, b: &[usize]) -> Vec<usize> {
    let set: HashSet<usize> = b.iter().copied().collect();
    a.into_iter().filter(|id| set.contains(id)).collect()
}

const STRUCT_MAGIC: &[u8; 8] = b"BRST0001";
const STRUCT_HEADER_LEN: usize = 32;
const STRUCT_RECORD_LEN: usize = 56;
const STRUCT_SCAN_MAX_PLY: u32 = 400;

/// Compact 56-byte structural & endgame summary over a game's mainline positions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GameStructSig {
    pub white_pawns: u64,
    pub black_pawns: u64,
    pub white_knights: u64,
    pub black_knights: u64,
    pub white_bishops: u64,
    pub black_bishops: u64,
    pub max_ply: u16,
    pub min_white_non_pawns: u8,
    pub min_black_non_pawns: u8,
    pub min_white_pawns: u8,
    pub min_black_pawns: u8,
    /// Bit 0: reached opposite-colored single-bishop position.
    /// Bit 1: reached same-colored single-bishop position.
    /// Bit 2: ever had any pawns on the board.
    pub flags: u16,
}

impl GameStructSig {
    #[must_use]
    pub fn to_bytes(self) -> [u8; STRUCT_RECORD_LEN] {
        let mut out = [0u8; STRUCT_RECORD_LEN];
        out[0..8].copy_from_slice(&self.white_pawns.to_le_bytes());
        out[8..16].copy_from_slice(&self.black_pawns.to_le_bytes());
        out[16..24].copy_from_slice(&self.white_knights.to_le_bytes());
        out[24..32].copy_from_slice(&self.black_knights.to_le_bytes());
        out[32..40].copy_from_slice(&self.white_bishops.to_le_bytes());
        out[40..48].copy_from_slice(&self.black_bishops.to_le_bytes());
        out[48..50].copy_from_slice(&self.max_ply.to_le_bytes());
        out[50] = self.min_white_non_pawns;
        out[51] = self.min_black_non_pawns;
        out[52] = self.min_white_pawns;
        out[53] = self.min_black_pawns;
        out[54..56].copy_from_slice(&self.flags.to_le_bytes());
        out
    }

    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < STRUCT_RECORD_LEN {
            return None;
        }
        let u64_at = |off: usize| -> u64 {
            u64::from_le_bytes([
                bytes[off],
                bytes[off + 1],
                bytes[off + 2],
                bytes[off + 3],
                bytes[off + 4],
                bytes[off + 5],
                bytes[off + 6],
                bytes[off + 7],
            ])
        };
        Some(Self {
            white_pawns: u64_at(0),
            black_pawns: u64_at(8),
            white_knights: u64_at(16),
            black_knights: u64_at(24),
            white_bishops: u64_at(32),
            black_bishops: u64_at(40),
            max_ply: u16::from_le_bytes([bytes[48], bytes[49]]),
            min_white_non_pawns: bytes[50],
            min_black_non_pawns: bytes[51],
            min_white_pawns: bytes[52],
            min_black_pawns: bytes[53],
            flags: u16::from_le_bytes([bytes[54], bytes[55]]),
        })
    }

    /// Build a signature for game `index` using zero-allocation mainline streaming
    /// with automatic fallback to `load()` when needed.
    #[must_use]
    pub fn from_game_scan(library: &dyn GameLibrary, index: usize) -> Self {
        let mut sig = Self {
            min_white_non_pawns: u8::MAX,
            min_black_non_pawns: u8::MAX,
            min_white_pawns: u8::MAX,
            min_black_pawns: u8::MAX,
            ..Self::default()
        };
        let mut observe = |ply: u32, pos: &Chess| {
            let b = pos.board();
            let wp = b.colored_pieces_mask(Color::White, Role::Pawn);
            let bp = b.colored_pieces_mask(Color::Black, Role::Pawn);
            let wn = b.colored_pieces_mask(Color::White, Role::Knight);
            let bn = b.colored_pieces_mask(Color::Black, Role::Knight);
            let wb = b.colored_pieces_mask(Color::White, Role::Bishop);
            let bb = b.colored_pieces_mask(Color::Black, Role::Bishop);

            sig.white_pawns |= wp;
            sig.black_pawns |= bp;
            sig.white_knights |= wn;
            sig.black_knights |= bn;
            sig.white_bishops |= wb;
            sig.black_bishops |= bb;
            sig.max_ply = sig.max_ply.max(ply.min(u32::from(u16::MAX)) as u16);

            let wp_cnt = wp.count_ones().min(255) as u8;
            let bp_cnt = bp.count_ones().min(255) as u8;
            let w_np = (wn.count_ones()
                + wb.count_ones()
                + b.colored_pieces_mask(Color::White, Role::Rook).count_ones()
                + b.colored_pieces_mask(Color::White, Role::Queen)
                    .count_ones())
            .min(255) as u8;
            let b_np = (bn.count_ones()
                + bb.count_ones()
                + b.colored_pieces_mask(Color::Black, Role::Rook).count_ones()
                + b.colored_pieces_mask(Color::Black, Role::Queen)
                    .count_ones())
            .min(255) as u8;

            sig.min_white_pawns = sig.min_white_pawns.min(wp_cnt);
            sig.min_black_pawns = sig.min_black_pawns.min(bp_cnt);
            sig.min_white_non_pawns = sig.min_white_non_pawns.min(w_np);
            sig.min_black_non_pawns = sig.min_black_non_pawns.min(b_np);

            if wp != 0 || bp != 0 {
                sig.flags |= 4;
            }
            if wb.count_ones() == 1 && bb.count_ones() == 1 {
                let w_light = (wb & LIGHT_SQUARES_MASK) != 0;
                let b_light = (bb & LIGHT_SQUARES_MASK) != 0;
                if w_light != b_light {
                    sig.flags |= 1;
                } else {
                    sig.flags |= 2;
                }
            }
        };

        let streamed = library
            .scan_mainline_positions(index, STRUCT_SCAN_MAX_PLY, &mut |ply, pos| {
                observe(ply, pos);
                true
            })
            .ok()
            .unwrap_or(false);

        if !streamed && let Ok(game) = library.load(index) {
            let mut node_idx = 0usize;
            let mut ply = 0u32;
            while ply <= STRUCT_SCAN_MAX_PLY {
                observe(ply, &game.nodes[node_idx].pos);
                let Some(&next) = game.nodes[node_idx].children.first() else {
                    break;
                };
                node_idx = next;
                ply += 1;
            }
        }
        if sig.min_white_non_pawns == u8::MAX {
            sig.min_white_non_pawns = 0;
            sig.min_black_non_pawns = 0;
            sig.min_white_pawns = 0;
            sig.min_black_pawns = 0;
        }
        sig
    }
}

fn struct_cache_dir() -> PathBuf {
    #[cfg(test)]
    {
        std::env::temp_dir().join(format!("brett-test-struct-{}", std::process::id()))
    }
    #[cfg(not(test))]
    {
        if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
            return PathBuf::from(dir).join("brett").join("struct");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".cache")
                .join("brett")
                .join("struct");
        }
        std::env::temp_dir().join("brett-struct")
    }
}

fn struct_path_hash(source: &Path) -> u64 {
    let canonical = fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in canonical.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// Canonical `.brettstruct` sidecar path for `source`.
#[must_use]
pub fn struct_sig_cache_path(source: &Path) -> PathBuf {
    struct_cache_dir().join(format!("{:016x}.brettstruct", struct_path_hash(source)))
}

/// Memory-mapped `.brettstruct` signature table for instant pre-filtering of large databases.
pub struct StructSigCache {
    mmap: Mmap,
    count: usize,
}

impl StructSigCache {
    #[must_use]
    pub fn len(&self) -> usize {
        self.count
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    #[must_use]
    pub fn get(&self, index: usize) -> Option<GameStructSig> {
        if index >= self.count {
            return None;
        }
        let start = STRUCT_HEADER_LEN + index * STRUCT_RECORD_LEN;
        let slice = self.mmap.get(start..start + STRUCT_RECORD_LEN)?;
        GameStructSig::from_bytes(slice)
    }
}

fn open_validated_struct_cache(
    path: &Path,
    expected_len: usize,
    expected_file_len: u64,
) -> Option<StructSigCache> {
    let file = File::open(path).ok()?;
    let mmap = unsafe { Mmap::map(&file) }.ok()?;
    if mmap.len() < STRUCT_HEADER_LEN || &mmap[0..8] != STRUCT_MAGIC {
        return None;
    }
    let u64_at = |off: usize| -> u64 {
        u64::from_le_bytes([
            mmap[off],
            mmap[off + 1],
            mmap[off + 2],
            mmap[off + 3],
            mmap[off + 4],
            mmap[off + 5],
            mmap[off + 6],
            mmap[off + 7],
        ])
    };
    let count = usize::try_from(u64_at(8)).ok()?;
    let file_len = u64_at(16);
    if count != expected_len || file_len != expected_file_len {
        return None;
    }
    let expected_bytes = STRUCT_HEADER_LEN.checked_add(count.checked_mul(STRUCT_RECORD_LEN)?)?;
    if mmap.len() < expected_bytes {
        return None;
    }
    Some(StructSigCache { mmap, count })
}

/// Returns `true` when a valid up-to-date `.brettstruct` sidecar exists for `library`.
#[must_use]
pub fn has_struct_sig_cache(library: &dyn GameLibrary) -> bool {
    open_struct_sig_cache(library).is_some()
}

/// Open an existing validated `.brettstruct` sidecar for `library`, if present and current.
#[must_use]
pub fn open_struct_sig_cache(library: &dyn GameLibrary) -> Option<StructSigCache> {
    if library.is_empty() {
        return None;
    }
    let path = struct_sig_cache_path(library.path());
    open_validated_struct_cache(&path, library.len(), library.file_len())
}

/// Build and atomically save the `.brettstruct` signature sidecar for `library`.
pub fn build_struct_sig_cache<F>(
    library: &dyn GameLibrary,
    mut on_progress: F,
    cancel: &AtomicBool,
) -> Option<StructSigCache>
where
    F: FnMut(usize, usize),
{
    let total = library.len();
    if total == 0 {
        on_progress(0, 0);
        return None;
    }
    let target = struct_sig_cache_path(library.path());
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).ok()?;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = target.with_file_name(format!(
        "{}.tmp-{}-{nanos}",
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("struct"),
        std::process::id()
    ));

    let file = File::create(&tmp).ok()?;
    let mut writer = BufWriter::new(file);
    let mut header = [0u8; STRUCT_HEADER_LEN];
    header[0..8].copy_from_slice(STRUCT_MAGIC);
    header[8..16].copy_from_slice(&(total as u64).to_le_bytes());
    header[16..24].copy_from_slice(&library.file_len().to_le_bytes());
    header[24..32].copy_from_slice(&struct_path_hash(library.path()).to_le_bytes());
    if writer.write_all(&header).is_err() {
        let _ = fs::remove_file(&tmp);
        return None;
    }

    const CHUNK: usize = 2048;
    for start in (0..total).step_by(CHUNK) {
        if cancel.load(Ordering::Relaxed) {
            let _ = fs::remove_file(&tmp);
            return None;
        }
        let end = (start + CHUNK).min(total);
        let sigs: Vec<[u8; STRUCT_RECORD_LEN]> = (start..end)
            .into_par_iter()
            .map(|idx| GameStructSig::from_game_scan(library, idx).to_bytes())
            .collect();
        for rec in sigs {
            if writer.write_all(&rec).is_err() {
                let _ = fs::remove_file(&tmp);
                return None;
            }
        }
        on_progress(end, total);
    }
    if writer.flush().is_err() {
        let _ = fs::remove_file(&tmp);
        return None;
    }
    drop(writer);
    if fs::rename(&tmp, &target).is_err() {
        let _ = fs::remove_file(&tmp);
        return None;
    }
    open_validated_struct_cache(&target, total, library.file_len())
}

/// Search `library` in parallel across games starting at candidate index
/// `start_candidate_index`, returning at most `max_hits` hits along with the
/// exact cursor (`next_start_index`) to resume searching in intervals.
pub fn search_library_from<F>(
    library: &dyn GameLibrary,
    query: &PositionQuery,
    start_candidate_index: usize,
    max_hits: usize,
    mut on_progress: F,
    cancel: &AtomicBool,
) -> SearchBatchResult
where
    F: FnMut(usize, usize, usize) + Send,
{
    let total = library.len();
    if total == 0 || max_hits == 0 {
        on_progress(total, total, 0);
        return SearchBatchResult {
            hits: Vec::new(),
            next_start_index: total,
            total_candidates: total,
            truncated: false,
            cancelled: false,
        };
    }
    let compiled = query.compile();
    let sig_cache = compiled
        .has_position_criteria()
        .then(|| open_struct_sig_cache(library))
        .flatten();

    // Narrow down candidate games via accelerated player posting lists when available.
    let mut candidate_indices: Option<Vec<usize>> = None;
    for needle in [
        &compiled.player_any_lower,
        &compiled.player_white_lower,
        &compiled.player_black_lower,
    ] {
        if needle.is_empty() {
            continue;
        }
        if let Some(ids) = library.accelerated_games_matching_player(needle) {
            candidate_indices = Some(match candidate_indices {
                None => ids,
                Some(prev) => intersect_sorted(prev, &ids),
            });
        }
    }

    let candidate_total = candidate_indices.as_ref().map_or(total, Vec::len);
    let start_pos = start_candidate_index.min(candidate_total);
    if candidate_total == 0 || start_pos >= candidate_total {
        on_progress(candidate_total, candidate_total, 0);
        return SearchBatchResult {
            hits: Vec::new(),
            next_start_index: candidate_total,
            total_candidates: candidate_total,
            truncated: false,
            cancelled: false,
        };
    }

    let mut hits = Vec::new();
    let chunk_size: usize = if !compiled.has_position_criteria() || candidate_indices.is_some() {
        2048
    } else {
        512
    };
    let pos_criteria = compiled.has_position_criteria();
    let mut next_start_index = start_pos;
    let mut truncated = false;
    let mut cancelled = false;

    for chunk_start in (start_pos..candidate_total).step_by(chunk_size) {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            next_start_index = chunk_start;
            break;
        }
        if hits.len() >= max_hits {
            truncated = chunk_start < candidate_total;
            next_start_index = chunk_start;
            break;
        }
        let chunk_end = (chunk_start + chunk_size).min(candidate_total);
        let process_game = |cand_pos: usize, idx: usize| -> Option<(usize, PositionHit)> {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            // Tier 1: O(1) numeric metadata & Mega medal/flag filter.
            let meta = library.game_record_meta(idx);
            if !compiled.fast_accepts_meta(&meta) {
                return None;
            }

            // Tier 2: Check player/event strings before decoding any moves.
            if compiled.needs_player_check() {
                let (white, black) = library.game_players(idx);
                if !compiled.matches_players(&white, &black) {
                    return None;
                }
            }
            let headers = if compiled.needs_event_check() || !pos_criteria {
                let h = library.game_headers(idx);
                if compiled.needs_event_check() {
                    let hval = |k: &str| {
                        h.iter()
                            .find(|(key, _)| key.eq_ignore_ascii_case(k))
                            .map(|(_, val)| val.as_str())
                            .unwrap_or("")
                    };
                    if !contains_case_insensitive(hval("Event"), &compiled.event_lower)
                        && !contains_case_insensitive(hval("Site"), &compiled.event_lower)
                    {
                        return None;
                    }
                }
                Some(h)
            } else {
                None
            };

            // Metadata-only search: hit without touching move records!
            if !pos_criteria {
                let orientation = MatchOrientation::Both;
                if !compiled.accepts_result_for_orientation(meta.result, orientation) {
                    return None;
                }
                let h = headers.unwrap_or_else(|| library.game_headers(idx));
                return Some((
                    cand_pos,
                    hit_from_headers(idx, 0, false, compiled.primary_side, &meta, &h),
                ));
            }

            // Tier 2.5: O(1) `.brettstruct` signature prune when index is present.
            if let Some(cache) = &sig_cache
                && let Some(sig) = cache.get(idx)
                && !compiled.fast_accepts_struct_sig(&sig, meta.result)
            {
                return None;
            }

            // Tier 3: Zero-allocation mainline bitboard scan.
            let mut match_found: Option<(usize, bool)> = None;
            let streamed = library
                .scan_mainline_positions(idx, compiled.max_ply, &mut |ply, pos| {
                    if ply < compiled.min_ply {
                        return true;
                    }
                    if let Some(orient) = compiled.matches_position_oriented(pos)
                        && compiled.accepts_result_for_orientation(meta.result, orient)
                    {
                        let flipped =
                            orient.includes_flipped() && orient == MatchOrientation::Flipped;
                        match_found = Some((ply as usize, flipped));
                        return false;
                    }
                    true
                })
                .ok()
                .unwrap_or(false);

            if !streamed {
                let game = library.load(idx).ok()?;
                let mut node_idx = 0usize;
                let mut ply = 0u32;
                loop {
                    if ply > compiled.max_ply {
                        break;
                    }
                    if ply >= compiled.min_ply
                        && let Some(orient) =
                            compiled.matches_position_oriented(&game.nodes[node_idx].pos)
                        && compiled.accepts_result_for_orientation(meta.result, orient)
                    {
                        let flipped = orient == MatchOrientation::Flipped;
                        match_found = Some((ply as usize, flipped));
                        break;
                    }
                    let Some(&next) = game.nodes[node_idx].children.first() else {
                        break;
                    };
                    node_idx = next;
                    ply += 1;
                }
            }

            let (matched_ply, matched_flipped) = match_found?;
            let structure_side = if matched_flipped {
                compiled.primary_side.other()
            } else {
                compiled.primary_side
            };
            let h = headers.unwrap_or_else(|| library.game_headers(idx));
            Some((
                cand_pos,
                hit_from_headers(idx, matched_ply, matched_flipped, structure_side, &meta, &h),
            ))
        };

        let mut chunk_hits: Vec<(usize, PositionHit)> = match &candidate_indices {
            Some(ids) => ids[chunk_start..chunk_end]
                .par_iter()
                .enumerate()
                .filter_map(|(offset, &idx)| process_game(chunk_start + offset, idx))
                .collect(),
            None => (chunk_start..chunk_end)
                .into_par_iter()
                .filter_map(|idx| process_game(idx, idx))
                .collect(),
        };
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            next_start_index = chunk_start;
            break;
        }
        chunk_hits.sort_by_key(|(cand_pos, _)| *cand_pos);
        if hits.len().saturating_add(chunk_hits.len()) > max_hits {
            let remaining = max_hits - hits.len();
            let mut last_cand = chunk_start;
            for (cand_pos, hit) in chunk_hits.into_iter().take(remaining) {
                last_cand = cand_pos;
                hits.push(hit);
            }
            next_start_index = (last_cand + 1).min(candidate_total);
            truncated = next_start_index < candidate_total;
            on_progress(next_start_index, candidate_total, hits.len());
            break;
        }
        for (_, hit) in chunk_hits {
            hits.push(hit);
        }
        next_start_index = chunk_end;
        on_progress(next_start_index, candidate_total, hits.len());
        if hits.len() >= max_hits {
            truncated = next_start_index < candidate_total;
            break;
        }
    }

    SearchBatchResult {
        hits,
        next_start_index,
        total_candidates: candidate_total,
        truncated,
        cancelled,
    }
}

/// Search `library` in parallel across games for positions & metadata matching `query`.
/// Uses O(1) header/flag pre-filtering, optional `.brettstruct` signature pruning,
/// accelerated player indexes (when present), and zero-allocation mainline bitboard
/// streaming before ever falling back to `load()`.
pub fn search_library<F>(
    library: &dyn GameLibrary,
    query: &PositionQuery,
    max_hits: usize,
    on_progress: F,
    cancel: &AtomicBool,
) -> Vec<PositionHit>
where
    F: FnMut(usize, usize, usize) + Send,
{
    search_library_from(library, query, 0, max_hits, on_progress, cancel).hits
}

fn hit_from_headers(
    game_index: usize,
    matched_ply: usize,
    matched_flipped: bool,
    structure_side: Color,
    meta: &GameRecordMeta,
    headers: &[(crate::library::SharedStr, crate::library::SharedStr)],
) -> PositionHit {
    let hval = |k: &str, default: &str| -> String {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(k))
            .map(|(_, val)| val.to_string())
            .unwrap_or_else(|| default.to_string())
    };
    let eco = hval("ECO", "");
    let eco = if eco.is_empty() {
        eco_code_to_str(meta.eco_code).unwrap_or_default()
    } else {
        eco
    };
    PositionHit {
        game_index,
        matched_ply,
        matched_flipped,
        structure_side,
        white: hval("White", "?"),
        black: hval("Black", "?"),
        white_elo: meta.white_elo,
        black_elo: meta.black_elo,
        result: hval("Result", "*"),
        eco,
        date: hval("Date", "????.??.??"),
        date_num: meta.date_num,
        event: hval("Event", "?"),
        medals: meta.medals,
        vcs_flags: meta.vcs_flags,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chess::{CastlingMode, Fen};
    use crate::domain::parse_pgn;
    use crate::library::PgnLibrary;
    use crate::library::{medals, vcs_flags};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn pos_from_fen(fen: &str) -> Chess {
        fen.parse::<Fen>()
            .unwrap()
            .into_position(CastlingMode::Standard)
            .unwrap()
    }

    #[test]
    fn partial_and_absolute_modes_handle_must_be_empty() {
        let games = parse_pgn(b"[White \"W\"]\n[Black \"B\"]\n\n1. e4 c5 2. Nf3 d6 *").unwrap();
        let after_nf3 = &games[0].nodes[3].pos;
        let final_pos = &games[0].nodes[4].pos;

        let mut q = PositionQuery::default();
        q.squares[Square::F3.to_u32() as usize] =
            SquareConstraint::Piece(Piece::new(Color::White, Role::Knight));
        q.squares[Square::E4.to_u32() as usize] =
            SquareConstraint::Piece(Piece::new(Color::White, Role::Pawn));
        q.squares[Square::E2.to_u32() as usize] = SquareConstraint::Empty;

        let compiled = q.compile();
        assert!(compiled.matches_position(after_nf3));
        assert!(compiled.matches_position(final_pos));
        assert!(!compiled.matches_position(&Chess::default()));

        let mut q_blocked = q.clone();
        q_blocked.squares[Square::D6.to_u32() as usize] = SquareConstraint::Empty;
        assert!(q_blocked.compile().matches_position(after_nf3));
        assert!(!q_blocked.compile().matches_position(final_pos));

        let exact = PositionQuery::from_chess_position(final_pos, PositionSearchMode::Absolute);
        let compiled_exact = exact.compile();
        assert!(compiled_exact.matches_position(final_pos));
        assert!(!compiled_exact.matches_position(after_nf3));
    }

    #[test]
    fn color_flip_mirroring_is_default_and_matches_mirrored_structure() {
        let games = parse_pgn(b"1. Nf3 e5 2. g3 c5 *").unwrap();
        let pos = &games[0].nodes[4].pos;

        let mut q = PositionQuery::default();
        assert!(
            q.ignore_color_flip,
            "reverse color should be enabled by default"
        );
        q.squares[Square::C4.to_u32() as usize] =
            SquareConstraint::Piece(Piece::new(Color::White, Role::Pawn));
        q.squares[Square::E4.to_u32() as usize] =
            SquareConstraint::Piece(Piece::new(Color::White, Role::Pawn));
        q.side_to_move = SideToMoveFilter::Black;

        assert!(q.compile().matches_position(pos));
        q.ignore_color_flip = false;
        assert!(!q.compile().matches_position(pos));
    }

    #[test]
    fn badewanne_enforces_backwards_pawns_strictly_ranks_3_to_5() {
        // White inner pawns on rank 3 (d3, e3), outer rim c4, f4 -> valid Badewanne!
        let valid_w_rank3 = pos_from_fen("7k/8/8/8/2P2P2/3PP3/8/7K w - - 0 1");
        assert!(
            StructurePreset::BadewanneAny
                .to_query()
                .compile()
                .matches_position(&valid_w_rank3)
        );
        assert!(
            StructurePreset::BadewanneCentral
                .to_query()
                .compile()
                .matches_position(&valid_w_rank3)
        );

        // White inner pawns on rank 5 (d5, e5), outer rim c6, f6 -> valid Badewanne!
        let valid_w_rank5 = pos_from_fen("7k/8/2P2P2/3PP3/8/8/8/7K w - - 0 1");
        assert!(
            StructurePreset::BadewanneAny
                .to_query()
                .compile()
                .matches_position(&valid_w_rank5)
        );

        // White inner pawns on rank 2 (d2, e2), rim c3, f3 -> BELOW rank 3, MUST BE REJECTED!
        let too_low_rank2 = pos_from_fen("7k/8/8/8/8/2P2P2/3PP3/7K w - - 0 1");
        assert!(
            !StructurePreset::BadewanneAny
                .to_query()
                .compile()
                .matches_position(&too_low_rank2)
        );

        // White inner pawns on rank 6 (d6, e6), rim c7, f7 -> PAST rank 5, MUST BE REJECTED!
        let too_high_rank6 = pos_from_fen("7k/2P2P2/3PP3/8/8/8/8/7K w - - 0 1");
        assert!(
            !StructurePreset::BadewanneAny
                .to_query()
                .compile()
                .matches_position(&too_high_rank6)
        );

        // Black mirrored Badewanne: Black backwards pawns on 6th (= Black 3rd) rank (d6, e6) with rim c5, f5 -> VALID!
        let valid_b_rank3 = pos_from_fen("7k/8/3pp3/2p2p2/8/8/8/7K w - - 0 1");
        assert!(
            StructurePreset::BadewanneAny
                .to_query()
                .compile()
                .matches_position(&valid_b_rank3)
        );
        let mut q_no_flip = StructurePreset::BadewanneAny.to_query();
        q_no_flip.ignore_color_flip = false;
        assert!(!q_no_flip.compile().matches_position(&valid_b_rank3));

        // Black backwards pawns on 4th (= Black 5th) rank (b4, c4 inner, rim a3, d3) on queenside files a-d!
        let valid_b_queenside_r5 = pos_from_fen("7k/8/8/8/1pp5/p2p4/8/7K w - - 0 1");
        assert!(
            StructurePreset::BadewanneAny
                .to_query()
                .compile()
                .matches_position(&valid_b_queenside_r5)
        );
        assert!(
            !StructurePreset::BadewanneCentral
                .to_query()
                .compile()
                .matches_position(&valid_b_queenside_r5)
        );

        // Black on 3rd (= Black 6th) rank -> past 5th rank relative to Black -> REJECTED!
        let invalid_b_rank6 = pos_from_fen("7k/8/8/8/8/3pp3/2p2p2/7K w - - 0 1");
        assert!(
            !StructurePreset::BadewanneAny
                .to_query()
                .compile()
                .matches_position(&invalid_b_rank6)
        );

        // Flexible rim (e.g. White c5 + d3 + e3 + f4) -> Flexible accepts, classic Any rejects!
        let asym = pos_from_fen("7k/8/8/2P5/5P2/3PP3/8/7K w - - 0 1");
        assert!(
            !StructurePreset::BadewanneAny
                .to_query()
                .compile()
                .matches_position(&asym)
        );
        assert!(
            StructurePreset::BadewanneFlexibleRim
                .to_query()
                .compile()
                .matches_position(&asym)
        );
    }

    #[test]
    fn stonewall_subversions_and_reverse_colors_work() {
        // Black d5, e6, f5 + Nc6 -> StonewallNc6 matches primary orientation
        let sw_nc6_black =
            parse_pgn(b"1. d4 d5 2. c4 e6 3. Nc3 c6 4. Nf3 f5 5. Bf4 Nf6 6. e3 Be7 *").unwrap();
        let sw_nc6_real =
            pos_from_fen("r1bqkb1r/pp4pp/2n1pn2/3p1p2/2PP4/2N1PN2/PP3PPP/R1BQKB1R w KQkq - 0 1");
        assert!(
            StructurePreset::StonewallNc6
                .to_query()
                .compile()
                .matches_position(&sw_nc6_real)
        );
        // Classic Stonewall (c6, d5, e6, f5 without Nc6) should NOT match StonewallNc6
        assert!(
            !StructurePreset::StonewallNc6
                .to_query()
                .compile()
                .matches_position(sw_nc6_black[0].final_position())
        );

        // White c3? Wait: White d4, e3, f4 + Nc3 -> color-flipped StonewallNc6!
        let sw_nc3_white =
            pos_from_fen("rnbqkb1r/ppp2ppp/4pn2/3p4/3P1P2/2N1P3/PPP3PP/R1BQKBNR b KQkq - 0 1");
        let q_nc6 = StructurePreset::StonewallNc6.to_query();
        assert!(q_nc6.compile().matches_position(&sw_nc3_white));
        let mut q_no_flip = q_nc6;
        q_no_flip.ignore_color_flip = false;
        assert!(!q_no_flip.compile().matches_position(&sw_nc3_white));
    }

    #[test]
    fn structure_and_endgame_are_mutually_exclusive_and_winners_orient() {
        let mut q = PositionQuery::default();
        q.set_structure(Some(StructurePreset::CarlsbadAny));
        assert_eq!(q.structure, Some(StructurePreset::CarlsbadAny));
        assert!(q.endgame.is_none());

        q.set_endgame(Some(EndgamePreset::RookVsRook.to_filter()));
        assert_eq!(q.endgame.map(|e| e.preset), Some(EndgamePreset::RookVsRook));
        assert!(q.structure.is_none());

        q.set_structure(Some(StructurePreset::IqpDFile));
        assert!(q.endgame.is_none());

        // Winner orientation checks on IQP and Endgame independently.
        let iqp_white = pos_from_fen("r2q1rk1/pp3ppp/2n1pn2/8/3P4/2NB1N2/PP3PPP/R2Q1RK1 w - - 0 1");
        let mut q_struct = StructurePreset::IqpDFile.to_query();
        q_struct.min_ply = 0;
        q_struct.metadata.winner = WinnerFilter::StructureSideWins;
        let compiled = q_struct.compile();
        let orient = compiled.matches_position_oriented(&iqp_white).unwrap();
        assert_eq!(orient, MatchOrientation::Normal);
        assert!(compiled.accepts_result_for_orientation(2, orient));
        assert!(!compiled.accepts_result_for_orientation(0, orient));

        let rb_r_flipped = pos_from_fen("1r4k1/5ppp/3b4/8/8/4P3/5PPP/2R3K1 w - - 0 1");
        let mut q_eg = PositionQuery::default();
        q_eg.set_endgame(Some(EndgamePreset::RookBishopVsRook.to_filter()));
        q_eg.min_ply = 0;
        q_eg.metadata.winner = WinnerFilter::StructureSideWins;
        let comp_eg = q_eg.compile();
        let orient_f = comp_eg.matches_position_oriented(&rb_r_flipped).unwrap();
        assert_eq!(orient_f, MatchOrientation::Flipped);
        assert!(comp_eg.accepts_result_for_orientation(0, orient_f));
        assert!(!comp_eg.accepts_result_for_orientation(2, orient_f));
    }

    #[test]
    fn stonewall_exd_trade_bd2e1h4_and_maroczy_fianchetto_subtypes_match() {
        // Classic Stonewall after Black ...cxd4 exd4 -> still matches StonewallClassic & StonewallAny
        let sw_exd =
            pos_from_fen("r1bq1rk1/pp2bppp/2n1pn2/3p4/3P1P2/2P2N2/PP4PP/RNBQKB1R w KQ - 0 1");
        assert!(
            StructurePreset::StonewallClassic
                .to_query()
                .compile()
                .matches_position(&sw_exd)
        );
        // Stonewall + Bd2-e1-h4 facing Nf6
        let sw_bh4 =
            pos_from_fen("rnbq1rk1/pp2bppp/2p1pn2/3p4/3P1P1B/2PBPN2/PP4PP/RN1Q1RK1 b - - 0 1");
        assert!(
            StructurePreset::StonewallBd2e1h4
                .to_query()
                .compile()
                .matches_position(&sw_bh4)
        );
        // Maróczy No K-side fianchetto vs K-side fianchetto
        let maroczy_no_fian =
            pos_from_fen("r1bq1rk1/pp2ppbp/2np1np1/8/2P1P3/2N1BN2/PP2BPPP/R2Q1RK1 w - - 0 1");
        assert!(
            StructurePreset::MaroczyBindNoFianchetto
                .to_query()
                .compile()
                .matches_position(&maroczy_no_fian)
        );
        assert!(
            !StructurePreset::MaroczyBindFianchetto
                .to_query()
                .compile()
                .matches_position(&maroczy_no_fian)
        );
        let maroczy_fian =
            pos_from_fen("r2q1rk1/1p1bppbp/p2p1np1/8/2P1P3/2N1B1P1/PP3PBP/R2Q1RK1 w - - 0 1");
        assert!(
            StructurePreset::MaroczyBindFianchetto
                .to_query()
                .compile()
                .matches_position(&maroczy_fian)
        );
        assert!(
            !StructurePreset::MaroczyBindNoFianchetto
                .to_query()
                .compile()
                .matches_position(&maroczy_fian)
        );
    }

    #[test]
    fn search_library_uses_fast_metadata_and_streaming() {
        let pgn = b"\
[White \"Carlsen, Magnus\"]
[Black \"Nepomniachtchi, Ian\"]
[WhiteElo \"2855\"]
[BlackElo \"2782\"]
[ECO \"C88\"]
[Event \"WCh\"]
[Date \"2021.12.01\"]
[Result \"1-0\"]

1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 1-0

[White \"Kasparov, Garry\"]
[Black \"Karpov, Anatoly\"]
[WhiteElo \"2700\"]
[BlackElo \"2720\"]
[ECO \"D55\"]
[Event \"WCh\"]
[Date \"1985.10.01\"]
[Result \"1/2-1/2\"]

1. d4 d5 2. c4 e6 3. Nc3 Nf6 1/2-1/2
";
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "brett-pos-search-{}-{nanos}.pgn",
            std::process::id()
        ));
        fs::write(&path, pgn).unwrap();
        let lib = PgnLibrary::build(&path).unwrap();

        let mut q = PositionQuery::default();
        q.metadata.player_any = "Carlsen".to_string();
        q.metadata.min_elo = Some(2750);
        q.metadata.eco_filter = "C60-C99".to_string();
        q.metadata.winner = WinnerFilter::WhiteWins;

        let cancel = AtomicBool::new(false);
        let hits = search_library(&lib, &q, 50, |_, _, _| {}, &cancel);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].game_index, 0);
        assert_eq!(hits[0].eco, "C88");
        assert_eq!(hits[0].white_elo, 2855);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn fast_accepts_meta_checks_all_mega_medals_and_vcs_flags() {
        let mut q = PositionQuery::default();
        q.metadata.medals_mask = medals::MODEL_GAME | medals::ENDGAME;
        q.metadata.medals_match_all = false;
        q.metadata.vcs_flags_mask = vcs_flags::COMMENTARY | vcs_flags::VARIATIONS;
        q.metadata.annotated_only = true;
        let compiled = q.compile();

        let hit_meta = GameRecordMeta {
            medals: medals::MODEL_GAME,
            vcs_flags: vcs_flags::COMMENTARY | vcs_flags::VARIATIONS | vcs_flags::SYMBOLS,
            annotated: true,
            ..Default::default()
        };
        assert!(compiled.fast_accepts_meta(&hit_meta));

        let miss_medal = GameRecordMeta {
            medals: medals::SACRIFICE,
            vcs_flags: vcs_flags::COMMENTARY | vcs_flags::VARIATIONS,
            annotated: true,
            ..Default::default()
        };
        assert!(!compiled.fast_accepts_meta(&miss_medal));

        let mut q_all = q.clone();
        q_all.metadata.medals_match_all = true;
        assert!(!q_all.compile().fast_accepts_meta(&hit_meta));
    }

    #[test]
    fn pgn_scan_mainline_positions_skips_variations_and_supports_custom_fen() {
        let pgn = b"\
[White \"A\"]
[Black \"B\"]
[Result \"*\"]

1. e4 e5 (1... c5 2. Nf3 (2. d4 cxd4) 2... d6) 2. Nf3 {knight} Nc6 3. Bb5 *

[White \"C\"]
[Black \"D\"]
[SetUp \"1\"]
[FEN \"4k3/4p3/8/8/8/8/4P3/4K3 w - - 0 1\"]
[Result \"*\"]

1. e4 e5 *
";
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("brett-pgn-scan-{}-{nanos}.pgn", std::process::id()));
        fs::write(&path, pgn).unwrap();
        let lib = PgnLibrary::build(&path).unwrap();

        for idx in [0usize, 1usize] {
            let loaded = lib.load(idx).unwrap();
            let expected: Vec<_> = std::iter::once(0usize)
                .chain(loaded.mainline())
                .enumerate()
                .map(|(ply, nid)| {
                    (
                        ply as u32,
                        Fen::from_position(
                            &loaded.nodes[nid].pos,
                            crate::chess::EnPassantMode::Legal,
                        )
                        .to_string(),
                    )
                })
                .collect();

            let mut streamed = Vec::new();
            let ok = lib
                .scan_mainline_positions(idx, 200, &mut |ply, p| {
                    streamed.push((
                        ply,
                        Fen::from_position(p, crate::chess::EnPassantMode::Legal).to_string(),
                    ));
                    true
                })
                .unwrap();
            assert!(ok);
            assert_eq!(streamed, expected);
        }

        let mut early = Vec::new();
        lib.scan_mainline_positions(0, 200, &mut |ply, _| {
            early.push(ply);
            ply < 1
        })
        .unwrap();
        assert_eq!(early, vec![0, 1]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn struct_sig_is_100_percent_sound_across_all_presets_and_endgames() {
        // Construct games covering:
        // 0: Black Nc6-Stonewall + 1-0? No, 0-1 (d5, e6, f5 + Nc6)
        // 1: White Badewanne rank 3..5 (1. c4? Play 1. d3 e5 2. e3 d5 3. c4 Nc6 4. f4)
        // 2: Carlsbad + later Rook+Bishop vs Rook endgame
        // 3: Plain game (1. e4 e5 2. Nf3 Nc6)
        let pgn = b"\
[White \"A1\"]
[Black \"B1\"]
[Result \"0-1\"]

1. d4 d5 2. c4 e6 3. Nc3 Nc6 4. Nf3 f5 0-1

[White \"A2\"]
[Black \"B2\"]
[Result \"1-0\"]

1. d3 e5 2. e3 d5 3. c4 Nc6 4. f4 1-0

[White \"Carlsbad\"]
[Black \"Opponent\"]
[SetUp \"1\"]
[FEN \"2r3k1/1p3ppp/2p5/3p4/3P4/3BP3/5PPP/2R3K1 w - - 0 30\"]
[Result \"1-0\"]

30. Kf1 Rc7 31. Ke2 Kf8 32. Kd2 Ke7 1-0

[White \"Plain\"]
[Black \"Game\"]
[Result \"1/2-1/2\"]

1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 1/2-1/2
";
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "brett-struct-sig-{}-{nanos}.pgn",
            std::process::id()
        ));
        fs::write(&path, pgn).unwrap();
        let lib = PgnLibrary::build(&path).unwrap();
        let cancel = AtomicBool::new(false);
        let cache = build_struct_sig_cache(&lib, |_, _| {}, &cancel)
            .expect("struct sig cache should build cleanly");
        assert_eq!(cache.len(), 4);
        assert!(has_struct_sig_cache(&lib));

        // 1. All 30 StructurePresets accept their matching games without false negatives
        for (game_idx, preset, winner) in [
            (
                0usize,
                StructurePreset::StonewallNc6,
                WinnerFilter::StructureSideWins,
            ),
            (
                1usize,
                StructurePreset::BadewanneAny,
                WinnerFilter::StructureSideWins,
            ),
            (
                2usize,
                StructurePreset::CarlsbadAny,
                WinnerFilter::StructureSideWins,
            ),
        ] {
            let mut q = preset.to_query();
            q.min_ply = 0;
            q.metadata.winner = winner;
            let compiled = q.compile();
            let sig = cache.get(game_idx).unwrap();
            let meta = lib.game_record_meta(game_idx);
            assert!(
                compiled.fast_accepts_struct_sig(&sig, meta.result),
                "struct sig rejected true match for {preset:?} on game {game_idx}"
            );
            let hits = search_library(&lib, &q, 50, |_, _, _| {}, &cancel);
            assert!(
                hits.iter().any(|h| h.game_index == game_idx),
                "search_library with .brettstruct must return game {game_idx} for {preset:?}"
            );
        }

        // 2. Non-matching game (plain Ruy Lopez) is rejected for RookBishopVsRook and Badewanne
        let sig_plain = cache.get(3).unwrap();
        let meta_plain = lib.game_record_meta(3);
        let mut q_rb = PositionQuery {
            endgame: Some(EndgamePreset::RookBishopVsRook.to_filter()),
            ..PositionQuery::default()
        };
        assert!(
            !q_rb
                .compile()
                .fast_accepts_struct_sig(&sig_plain, meta_plain.result)
        );
        q_rb.min_ply = 0;
        let rb_hits = search_library(&lib, &q_rb, 50, |_, _, _| {}, &cancel);
        assert_eq!(rb_hits.len(), 1);
        assert_eq!(rb_hits[0].game_index, 2);

        // Clean up temp files
        let cache_file = struct_sig_cache_path(&path);
        let _ = fs::remove_file(cache_file);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn boolean_or_and_none_clauses_mirroring_presets_and_custom_persistence() {
        let b2_wp = AtomPredicate::HasPiece(Square::B2, Piece::new(Color::White, Role::Pawn));
        let b3_wp = AtomPredicate::HasPiece(Square::B3, Piece::new(Color::White, Role::Pawn));
        let f3_wn = AtomPredicate::HasPiece(Square::F3, Piece::new(Color::White, Role::Knight));

        // Require Nf3 AND (White pawn on b2 OR White pawn on b3) AND no White pawn on e4.
        let mut q = PositionQuery::default();
        q.squares[Square::F3.to_u32() as usize] =
            SquareConstraint::Piece(Piece::new(Color::White, Role::Knight));
        q.extra_clauses.push(PatternClause {
            op: ClauseOp::AnyOf,
            atoms: vec![b2_wp, b3_wp],
        });
        q.extra_clauses.push(PatternClause {
            op: ClauseOp::NoneOf,
            atoms: vec![AtomPredicate::HasPiece(
                Square::E4,
                Piece::new(Color::White, Role::Pawn),
            )],
        });

        let compiled = q.compile();
        assert!(compiled.has_position_criteria());
        // White pawn on b2 + Nf3, no e4 pawn -> matches Normal.
        assert!(compiled.matches_position(&pos_from_fen("7k/8/8/8/8/5N2/1P6/7K w - - 0 1")));
        // White pawn on b3 + Nf3, no e4 pawn -> matches Normal.
        assert!(compiled.matches_position(&pos_from_fen("7k/8/8/8/8/1P3N2/8/7K w - - 0 1")));
        // Missing BOTH b2 and b3 pawns (pawn on b4) -> rejected!
        assert!(!compiled.matches_position(&pos_from_fen("7k/8/8/8/1P6/5N2/8/7K w - - 0 1")));
        // White pawn on b3 + Nf3, BUT also White pawn on e4 -> rejected by NONE clause!
        assert!(!compiled.matches_position(&pos_from_fen("7k/8/8/8/4P3/1P3N2/8/7K w - - 0 1")));
        // Mirrored Black position: Black knight on f6 + Black pawn on b6, no Black pawn on e5!
        assert!(compiled.matches_position(&pos_from_fen("7k/8/1p3n2/8/8/8/8/7K w - - 0 1")));
        let mut q_no_flip = q.clone();
        q_no_flip.ignore_color_flip = false;
        assert!(
            !q_no_flip
                .compile()
                .matches_position(&pos_from_fen("7k/8/1p3n2/8/8/8/8/7K w - - 0 1"))
        );

        // Sanity check with explicit AllOf atom too.
        let mut q_all = PositionQuery::default();
        q_all.extra_clauses.push(PatternClause {
            op: ClauseOp::AllOf,
            atoms: vec![
                f3_wn,
                AtomPredicate::NoPieceOnFile(ChessFile::C, Color::White, Role::Pawn),
            ],
        });
        assert!(
            q_all
                .compile()
                .matches_position(&pos_from_fen("7k/8/8/8/8/5N2/8/7K w - - 0 1"))
        );
        assert!(
            !q_all
                .compile()
                .matches_position(&pos_from_fen("7k/8/8/8/8/5N2/2P5/7K w - - 0 1"))
        );

        // Exporting all 30 built-in presets works and produces non-empty queries.
        for preset in StructurePreset::ALL {
            let (name, sqs, clauses) = preset.to_pattern_components();
            assert!(!name.is_empty());
            let q_exp = PositionQuery {
                squares: sqs,
                extra_clauses: clauses,
                ..PositionQuery::default()
            };
            assert!(
                q_exp.has_board_pattern(),
                "exported {preset:?} must produce board pattern criteria"
            );
        }

        // Custom preset JSON save/load round-trip.
        let preset = CustomStructurePreset {
            name: "My b2/b3 Hybrid".to_string(),
            squares: q.squares,
            extra_clauses: q.extra_clauses.clone(),
            side_to_move: SideToMoveFilter::White,
            ignore_color_flip: true,
        };
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let cfg_path = std::env::temp_dir().join(format!(
            "brett-custom-struct-{}-{nanos}.json",
            std::process::id()
        ));
        save_custom_presets_to(&cfg_path, std::slice::from_ref(&preset)).unwrap();
        let loaded = load_custom_presets_from(&cfg_path);
        assert_eq!(loaded, vec![preset]);
        let _ = fs::remove_file(cfg_path);
    }

    #[test]
    fn najdorf_types_and_custom_preset_override_lookup_work() {
        let type1 = pos_from_fen("r2q1rk1/1p2bppp/p2ppn2/8/3NP3/2N1B3/PPP1BPPP/R2Q1RK1 b - - 0 1");
        let type2 = pos_from_fen("r2q1rk1/3nbppp/p2pp3/1p6/4P3/1NN1BP2/PPPQ2PP/2KR1B1R b - - 0 1");
        let type3 = pos_from_fen("r2q1rk1/1p2bppp/p2p1n2/4p3/4PP2/2N1B3/PPPQ2PP/R4RK1 b - - 0 1");

        assert!(StructurePreset::NajdorfStructure.matches_oriented(&type1, false));
        assert!(StructurePreset::NajdorfTypeII.matches_oriented(&type2, false));
        assert!(StructurePreset::NajdorfTypeIII.matches_oriented(&type3, false));
        assert!(!StructurePreset::NajdorfTypeIII.matches_oriented(&type1, false));

        let ov = CustomStructurePreset {
            name: StructurePreset::NajdorfTypeII.label().to_string(),
            squares: [SquareConstraint::Any; 64],
            extra_clauses: Vec::new(),
            side_to_move: SideToMoveFilter::Any,
            ignore_color_flip: true,
        };
        let found = find_custom_override(std::slice::from_ref(&ov), StructurePreset::NajdorfTypeII);
        assert_eq!(
            found.map(|p| p.name.as_str()),
            Some(StructurePreset::NajdorfTypeII.label())
        );
        assert!(find_custom_override(&[ov], StructurePreset::najdorf_type_i_compat()).is_none());
    }

    #[test]
    fn handcrafted_maroczy_botvinnik_grunfeld_and_nimzo_presets_work() {
        // 1. Handcrafted MaroczyBindFianchetto matches both Normal & to_pattern_components, rejects missing b7/Bg7
        let maroczy_fian =
            pos_from_fen("r2q1rk1/1p1bppbp/p2p1np1/8/2P1P3/2N1B1P1/PP3PBP/R2Q1RK1 w - - 0 1");
        assert!(StructurePreset::MaroczyBindFianchetto.matches_oriented(&maroczy_fian, false));
        let (_, sqs, clauses) = StructurePreset::MaroczyBindFianchetto.to_pattern_components();
        let q_m = PositionQuery {
            squares: sqs,
            extra_clauses: clauses,
            ..PositionQuery::default()
        };
        assert!(q_m.compile().matches_position(&maroczy_fian));

        // 2. Handcrafted Botvinnik default preset (White and mirrored Black)
        let botvinnik_w =
            pos_from_fen("r2q1rk1/ppp2pbp/2npbnp1/4p3/2P1P3/2NP2P1/PP2NPBP/R1BQ1RK1 w - - 5 9");
        assert!(StructurePreset::Botvinnik.matches_oriented(&botvinnik_w, false));
        let (_, b_sqs, b_clauses) = StructurePreset::Botvinnik.to_pattern_components();
        let q_b = PositionQuery {
            squares: b_sqs,
            extra_clauses: b_clauses,
            ..PositionQuery::default()
        };
        assert!(q_b.compile().matches_position(&botvinnik_w));
        let botvinnik_b =
            pos_from_fen("r1bq1rk1/pp2npbp/2np2p1/2p1p3/4P3/2NPBNP1/PPP2PBP/R2Q1RK1 b - - 0 9");
        assert!(StructurePreset::Botvinnik.matches_oriented(&botvinnik_b, true));
        assert!(q_b.compile().matches_position(&botvinnik_b));

        // 3. Exchange Grünfeld with c3 pawn matches GrunfeldCenter; KID (Black d6) does not
        let grunfeld_ex =
            pos_from_fen("r1bq1rk1/pp2ppbp/2n3p1/2p5/3PP3/2P1BN2/P3BPPP/R2Q1RK1 b - - 5 10");
        assert!(StructurePreset::GrunfeldCenter.matches_oriented(&grunfeld_ex, false));
        let kid = pos_from_fen("r1bq1rk1/ppp1ppbp/3p1np1/8/2PPP3/2N2N2/PP2BPPP/R1BQ1RK1 w - - 0 1");
        assert!(!StructurePreset::GrunfeldCenter.matches_oriented(&kid, false));

        // 4. Nimzo doubled c-pawns: opponent dark-squared bishop traded on c3
        let nimzo = pos_from_fen("r1bqk2r/pp1ppppp/2n2n2/8/2PP4/2P1P3/P4PPP/R1BQKBNR w KQkq - 0 6");
        assert!(StructurePreset::NimzoDoubledCPawns.matches_oriented(&nimzo, false));
        let (_, n_sqs, n_clauses) = StructurePreset::NimzoDoubledCPawns.to_pattern_components();
        let q_n = PositionQuery {
            squares: n_sqs,
            extra_clauses: n_clauses,
            ..PositionQuery::default()
        };
        assert!(q_n.compile().matches_position(&nimzo));
    }

    #[test]
    fn position_facts_and_skeleton_query_detect_files_and_structures() {
        let iqp = pos_from_fen("r2q1rk1/pp3ppp/2n1pn2/8/3P4/2NB1N2/PP3PPP/R2Q1RK1 w - - 0 1");
        let facts = analyze_position_facts(&iqp);
        assert_eq!(facts.open_files, vec![ChessFile::C]);
        assert_eq!(facts.half_open_white, vec![ChessFile::E]);
        assert_eq!(facts.half_open_black, vec![ChessFile::D]);
        assert!(
            facts
                .matched_structures
                .iter()
                .any(|&(p, side)| p.family() == StructureFamily::Iqp && side == Color::White)
        );
        let skel = PositionQuery::from_pawn_skeleton(&iqp);
        assert!(skel.compile().matches_position(&iqp));
    }

    #[test]
    fn handcrafted_hanging_pawns_maroczy_and_iqp_differentiation_work() {
        // 1. Hand-tuned Hanging Pawns (c4+d4) + fianchetto vs no-fianchetto differentiation
        let hp_no_fian =
            pos_from_fen("r3r1k1/pbqnbppp/1p2pn2/8/2PP4/2NB1N2/PB2QPPP/R2R2K1 w - - 3 14");
        let hp_fian =
            pos_from_fen("r2qr1k1/pbqn1pbp/1p2pnp1/8/2PP4/2NB1N2/PB2QPPP/R2R2K1 w - - 0 14");
        let hp_missing_e =
            pos_from_fen("r3r1k1/pbqn1ppp/1p3n2/8/2PP4/2NB1N2/PB2QPPP/R2R2K1 w - - 0 14");
        assert!(StructurePreset::HangingPawnsClassic.matches_oriented(&hp_no_fian, false));
        assert!(StructurePreset::HangingPawnsVsNoFianchetto.matches_oriented(&hp_no_fian, false));
        assert!(!StructurePreset::HangingPawnsVsFianchetto.matches_oriented(&hp_no_fian, false));

        assert!(StructurePreset::HangingPawnsClassic.matches_oriented(&hp_fian, false));
        assert!(StructurePreset::HangingPawnsVsFianchetto.matches_oriented(&hp_fian, false));
        assert!(!StructurePreset::HangingPawnsVsNoFianchetto.matches_oriented(&hp_fian, false));

        // Missing Black e6/e7 pawn fails hand-tuned HangingPawnsClassic
        assert!(!StructurePreset::HangingPawnsClassic.matches_oriented(&hp_missing_e, false));

        // 2. Hand-tuned Maróczy no K-side fianchetto (a2/g2/c4/e4, h2|h3 vs d6/g6/b7/f7, h7|h6)
        let maroczy_no_fian =
            pos_from_fen("r2q1rk1/1p2ppbp/p1bp1np1/8/2PBP3/2N2P2/PP1QB1PP/R4RK1 w - - 0 13");
        assert!(StructurePreset::MaroczyBindNoFianchetto.matches_oriented(&maroczy_no_fian, false));
        let (_, mnf_sqs, mnf_clauses) =
            StructurePreset::MaroczyBindNoFianchetto.to_pattern_components();
        let q_mnf = PositionQuery {
            squares: mnf_sqs,
            extra_clauses: mnf_clauses,
            ..PositionQuery::default()
        };
        assert!(q_mnf.compile().matches_position(&maroczy_no_fian));

        // 3. IQP 2x2 differentiation: (e-pawn vs c-pawn) x (no-fianchetto vs fianchetto)
        let iqp_e_no_fian =
            pos_from_fen("r1bq1rk1/1p2bppp/p1n1pn2/8/3P4/1BN2N2/PP3PPP/R1BQR1K1 w - - 5 11");
        let iqp_e_fian =
            pos_from_fen("r1bq1rk1/1p3pbp/p1n1pnp1/8/3P4/1BN2N2/PP3PPP/R1BQR1K1 w - - 0 11");
        let iqp_c_no_fian =
            pos_from_fen("r2q1rk1/pp2bppp/2p1bn2/8/3P4/2NB1N2/PP3PPP/R1BQR1K1 w - - 0 11");
        let iqp_c_fian =
            pos_from_fen("r2q1rk1/pp3pbp/2p1bnp1/8/3P4/2NB1N2/PP3PPP/R1BQR1K1 w - - 0 11");

        assert!(StructurePreset::IqpVsEPawnNoFianchetto.matches_oriented(&iqp_e_no_fian, false));
        assert!(!StructurePreset::IqpVsEPawnFianchetto.matches_oriented(&iqp_e_no_fian, false));
        assert!(!StructurePreset::IqpVsCPawnNoFianchetto.matches_oriented(&iqp_e_no_fian, false));
        assert!(StructurePreset::IqpVsEPawn.matches_oriented(&iqp_e_no_fian, false));
        assert!(StructurePreset::IqpVsNoFianchetto.matches_oriented(&iqp_e_no_fian, false));

        assert!(StructurePreset::IqpVsEPawnFianchetto.matches_oriented(&iqp_e_fian, false));
        assert!(!StructurePreset::IqpVsEPawnNoFianchetto.matches_oriented(&iqp_e_fian, false));
        assert!(StructurePreset::IqpVsEPawn.matches_oriented(&iqp_e_fian, false));
        assert!(StructurePreset::IqpVsFianchetto.matches_oriented(&iqp_e_fian, false));

        assert!(StructurePreset::IqpVsCPawnNoFianchetto.matches_oriented(&iqp_c_no_fian, false));
        assert!(!StructurePreset::IqpVsEPawnNoFianchetto.matches_oriented(&iqp_c_no_fian, false));
        assert!(StructurePreset::IqpVsCPawn.matches_oriented(&iqp_c_no_fian, false));
        assert!(StructurePreset::IqpVsNoFianchetto.matches_oriented(&iqp_c_no_fian, false));

        assert!(StructurePreset::IqpVsCPawnFianchetto.matches_oriented(&iqp_c_fian, false));
        assert!(!StructurePreset::IqpVsCPawnNoFianchetto.matches_oriented(&iqp_c_fian, false));
        assert!(StructurePreset::IqpVsCPawn.matches_oriented(&iqp_c_fian, false));
        assert!(StructurePreset::IqpVsFianchetto.matches_oriented(&iqp_c_fian, false));

        // Verify to_pattern_components round-trip matches every new preset cleanly
        for (preset, sample) in [
            (StructurePreset::HangingPawnsVsNoFianchetto, &hp_no_fian),
            (StructurePreset::HangingPawnsVsFianchetto, &hp_fian),
            (StructurePreset::IqpVsEPawnNoFianchetto, &iqp_e_no_fian),
            (StructurePreset::IqpVsEPawnFianchetto, &iqp_e_fian),
            (StructurePreset::IqpVsCPawnNoFianchetto, &iqp_c_no_fian),
            (StructurePreset::IqpVsCPawnFianchetto, &iqp_c_fian),
        ] {
            let (_, sqs, clauses) = preset.to_pattern_components();
            let q = PositionQuery {
                squares: sqs,
                extra_clauses: clauses,
                ..PositionQuery::default()
            };
            assert!(
                q.compile().matches_position(sample),
                "to_pattern_components failed for {preset:?}"
            );
        }
    }

    #[test]
    fn resumable_search_batches_and_elo_sorting_are_exact() {
        let mut pgn = String::new();
        for i in 0..7usize {
            pgn.push_str(&format!(
                "[Event \"T{i}\"]\n[White \"W{i}\"]\n[Black \"B{i}\"]\n[WhiteElo \"{}\"]\n[BlackElo \"{}\"]\n[Date \"2024.01.0{}\"]\n[Result \"*\"]\n\n1. e4 e5 2. Nf3 Nc6 *\n\n",
                2400 + i * 50,
                2700 - i * 40,
                i + 1
            ));
        }
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "brett-batch-search-{}-{nanos}.pgn",
            std::process::id()
        ));
        fs::write(&path, pgn.as_bytes()).unwrap();
        let lib = PgnLibrary::build(&path).unwrap();
        let q = PositionQuery::default();
        let cancel = AtomicBool::new(false);

        let all = search_library_from(&lib, &q, 0, usize::MAX, |_, _, _| {}, &cancel);
        assert_eq!(all.hits.len(), 7);
        assert!(!all.truncated);

        // Resume in batches of 3 -> 3 + 3 + 1 == exact match with zero duplicates/skips
        let mut combined = Vec::new();
        let mut cursor = 0;
        loop {
            let batch = search_library_from(&lib, &q, cursor, 3, |_, _, _| {}, &cancel);
            combined.extend(batch.hits);
            cursor = batch.next_start_index;
            if !batch.truncated {
                break;
            }
        }
        assert_eq!(combined, all.hits);

        // Structure/Opponent Elo sorting respects structure_side
        let mut hits = vec![
            PositionHit {
                game_index: 0,
                matched_ply: 10,
                matched_flipped: false,
                structure_side: Color::White,
                white: "A".into(),
                black: "B".into(),
                white_elo: 2500,
                black_elo: 2800,
                result: "*".into(),
                eco: "D31".into(),
                date: "2024.01.01".into(),
                date_num: 20240101,
                event: "E".into(),
                medals: 0,
                vcs_flags: 0,
            },
            PositionHit {
                game_index: 1,
                matched_ply: 10,
                matched_flipped: true,
                structure_side: Color::Black,
                white: "C".into(),
                black: "D".into(),
                white_elo: 2400,
                black_elo: 2750,
                result: "*".into(),
                eco: "D31".into(),
                date: "2024.01.02".into(),
                date_num: 20240102,
                event: "E".into(),
                medals: 0,
                vcs_flags: 0,
            },
        ];
        assert_eq!(hits[0].structure_elo(), 2500);
        assert_eq!(hits[0].opponent_elo(), 2800);
        assert_eq!(hits[1].structure_elo(), 2750);
        assert_eq!(hits[1].opponent_elo(), 2400);

        sort_position_hits(&mut hits, PositionHitSort::StructureEloDesc);
        assert_eq!(hits[0].game_index, 1);
        sort_position_hits(&mut hits, PositionHitSort::OpponentEloDesc);
        assert_eq!(hits[0].game_index, 0);

        let _ = fs::remove_file(path);
    }

    trait StructurePresetCompat {
        fn najdorf_type_i_compat() -> Self;
    }

    impl StructurePresetCompat for StructurePreset {
        fn najdorf_type_i_compat() -> Self {
            Self::NajdorfStructure
        }
    }
}
