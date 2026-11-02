//! Scalar chess types: colors, roles, pieces, files, ranks, squares, and
//! bitboards. All types are thin value wrappers with option-free accessors.

use std::fmt;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};
use std::str::FromStr;

/// Error returned when a string cannot be parsed as a [`Square`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ParseSquareError;

impl fmt::Display for ParseSquareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid square, expected two characters like \"e4\"")
    }
}

impl std::error::Error for ParseSquareError {}

/// The side to move: [`Color::White`] or [`Color::Black`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Color {
    White,
    Black,
}

impl Color {
    /// All colors: white first, then black.
    pub const ALL: [Color; 2] = [Color::White, Color::Black];

    /// Returns the opposite color.
    #[must_use]
    pub const fn other(self) -> Color {
        match self {
            Color::White => Color::Black,
            Color::Black => Color::White,
        }
    }

    /// Selects a value according to the color: `white` for
    /// [`Color::White`], `black` for [`Color::Black`].
    #[must_use]
    pub fn fold_wb<T>(self, white: T, black: T) -> T {
        match self {
            Color::White => white,
            Color::Black => black,
        }
    }
}

impl From<Color> for cozy_chess::Color {
    fn from(color: Color) -> Self {
        color.fold_wb(cozy_chess::Color::White, cozy_chess::Color::Black)
    }
}

impl From<cozy_chess::Color> for Color {
    fn from(color: cozy_chess::Color) -> Self {
        match color {
            cozy_chess::Color::White => Color::White,
            cozy_chess::Color::Black => Color::Black,
        }
    }
}

/// The piece type of a piece, without its color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Pawn,
    Knight,
    Bishop,
    Rook,
    Queen,
    King,
}

impl Role {
    /// All roles in ascending value order.
    pub const ALL: [Role; 6] = [
        Role::Pawn,
        Role::Knight,
        Role::Bishop,
        Role::Rook,
        Role::Queen,
        Role::King,
    ];

    /// Returns the uppercase SAN/PGN character for the role.
    #[must_use]
    pub const fn char(self) -> char {
        match self {
            Role::Pawn => 'P',
            Role::Knight => 'N',
            Role::Bishop => 'B',
            Role::Rook => 'R',
            Role::Queen => 'Q',
            Role::King => 'K',
        }
    }

    /// Returns the role for an ASCII piece character, case-insensitive.
    #[must_use]
    pub const fn from_char(ch: char) -> Option<Role> {
        match ch {
            'P' | 'p' => Some(Role::Pawn),
            'N' | 'n' => Some(Role::Knight),
            'B' | 'b' => Some(Role::Bishop),
            'R' | 'r' => Some(Role::Rook),
            'Q' | 'q' => Some(Role::Queen),
            'K' | 'k' => Some(Role::King),
            _ => None,
        }
    }
}

impl From<Role> for cozy_chess::Piece {
    fn from(role: Role) -> Self {
        match role {
            Role::Pawn => cozy_chess::Piece::Pawn,
            Role::Knight => cozy_chess::Piece::Knight,
            Role::Bishop => cozy_chess::Piece::Bishop,
            Role::Rook => cozy_chess::Piece::Rook,
            Role::Queen => cozy_chess::Piece::Queen,
            Role::King => cozy_chess::Piece::King,
        }
    }
}

impl From<cozy_chess::Piece> for Role {
    fn from(piece: cozy_chess::Piece) -> Self {
        match piece {
            cozy_chess::Piece::Pawn => Role::Pawn,
            cozy_chess::Piece::Knight => Role::Knight,
            cozy_chess::Piece::Bishop => Role::Bishop,
            cozy_chess::Piece::Rook => Role::Rook,
            cozy_chess::Piece::Queen => Role::Queen,
            cozy_chess::Piece::King => Role::King,
        }
    }
}

/// A piece: a [`Role`] with a [`Color`].
///
/// # Examples
///
/// ```
/// use brett::chess::{Color, Piece, Role};
///
/// assert_eq!(Piece::new(Color::White, Role::Knight).char(), 'N');
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Piece {
    /// The color of the piece.
    pub color: Color,
    /// The role of the piece.
    pub role: Role,
}

impl Piece {
    /// Creates a piece with the given color and role.
    #[must_use]
    pub const fn new(color: Color, role: Role) -> Piece {
        Piece { color, role }
    }

    /// Returns the SAN/PGN character: uppercase for white, lowercase for
    /// black.
    #[must_use]
    pub const fn char(self) -> char {
        match self.color {
            Color::White => self.role.char(),
            Color::Black => match self.role {
                Role::Pawn => 'p',
                Role::Knight => 'n',
                Role::Bishop => 'b',
                Role::Rook => 'r',
                Role::Queen => 'q',
                Role::King => 'k',
            },
        }
    }
}

/// A file on the chess board, from [`File::A`] to [`File::H`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct File(u8);

impl File {
    /// The first file.
    pub const A: File = File(0);
    /// The second file.
    pub const B: File = File(1);
    /// The third file.
    pub const C: File = File(2);
    /// The fourth file.
    pub const D: File = File(3);
    /// The fifth file.
    pub const E: File = File(4);
    /// The sixth file.
    pub const F: File = File(5);
    /// The seventh file.
    pub const G: File = File(6);
    /// The eighth file.
    pub const H: File = File(7);

    /// All files, from A to H.
    pub const ALL: [File; 8] = [
        File::A,
        File::B,
        File::C,
        File::D,
        File::E,
        File::F,
        File::G,
        File::H,
    ];

    /// Creates a file from its index in `0..8`.
    ///
    /// # Panics
    ///
    /// Panics if `index >= 8`.
    #[must_use]
    pub const fn new(index: u32) -> File {
        assert!(index < 8, "file index out of range");
        File(index as u8)
    }

    /// Returns the file index in `0..8`.
    #[must_use]
    pub const fn to_u32(self) -> u32 {
        self.0 as u32
    }

    /// Mirrors the file, as if viewed from the other side of the board.
    #[must_use]
    pub const fn flip_horizontal(self) -> File {
        File(7 - self.0)
    }
}

impl fmt::Display for File {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", (b'a' + self.0) as char)
    }
}

impl From<File> for u32 {
    fn from(file: File) -> Self {
        file.0 as u32
    }
}

impl From<File> for usize {
    fn from(file: File) -> Self {
        file.0 as usize
    }
}

/// A rank on the chess board, from [`Rank::First`] to [`Rank::Eighth`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Rank(u8);

#[allow(non_upper_case_globals)]
impl Rank {
    /// The first rank.
    pub const First: Rank = Rank(0);
    /// The second rank.
    pub const Second: Rank = Rank(1);
    /// The third rank.
    pub const Third: Rank = Rank(2);
    /// The fourth rank.
    pub const Fourth: Rank = Rank(3);
    /// The fifth rank.
    pub const Fifth: Rank = Rank(4);
    /// The sixth rank.
    pub const Sixth: Rank = Rank(5);
    /// The seventh rank.
    pub const Seventh: Rank = Rank(6);
    /// The eighth rank.
    pub const Eighth: Rank = Rank(7);

    /// All ranks, from first to eighth.
    pub const ALL: [Rank; 8] = [
        Rank::First,
        Rank::Second,
        Rank::Third,
        Rank::Fourth,
        Rank::Fifth,
        Rank::Sixth,
        Rank::Seventh,
        Rank::Eighth,
    ];

    /// Creates a rank from its index in `0..8`.
    ///
    /// # Panics
    ///
    /// Panics if `index >= 8`.
    #[must_use]
    pub const fn new(index: u32) -> Rank {
        assert!(index < 8, "rank index out of range");
        Rank(index as u8)
    }

    /// Returns the rank index in `0..8`.
    #[must_use]
    pub const fn to_u32(self) -> u32 {
        self.0 as u32
    }

    /// Mirrors the rank, as if viewed from the other side of the board.
    #[must_use]
    pub const fn flip_vertical(self) -> Rank {
        Rank(7 - self.0)
    }
}

impl fmt::Display for Rank {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", (b'1' + self.0) as char)
    }
}

impl From<Rank> for u32 {
    fn from(rank: Rank) -> Self {
        rank.0 as u32
    }
}

impl From<Rank> for usize {
    fn from(rank: Rank) -> Self {
        rank.0 as usize
    }
}

/// A square on the chess board, from [`Square::A1`] to [`Square::H8`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Square(u8);

macro_rules! squares {
    ($($name:ident = $file:ident, $rank:ident;)*) => {
        impl Square {
            $(
                #[doc = concat!("The [`Square`] ", stringify!($name), ".")]
                pub const $name: Square = Square(
                    File::$file.0 + Rank::$rank.0 * 8,
                );
            )*
        }
    };
}

squares! {
    A1 = A, First;
    B1 = B, First;
    C1 = C, First;
    D1 = D, First;
    E1 = E, First;
    F1 = F, First;
    G1 = G, First;
    H1 = H, First;
    A2 = A, Second;
    B2 = B, Second;
    C2 = C, Second;
    D2 = D, Second;
    E2 = E, Second;
    F2 = F, Second;
    G2 = G, Second;
    H2 = H, Second;
    A3 = A, Third;
    B3 = B, Third;
    C3 = C, Third;
    D3 = D, Third;
    E3 = E, Third;
    F3 = F, Third;
    G3 = G, Third;
    H3 = H, Third;
    A4 = A, Fourth;
    B4 = B, Fourth;
    C4 = C, Fourth;
    D4 = D, Fourth;
    E4 = E, Fourth;
    F4 = F, Fourth;
    G4 = G, Fourth;
    H4 = H, Fourth;
    A5 = A, Fifth;
    B5 = B, Fifth;
    C5 = C, Fifth;
    D5 = D, Fifth;
    E5 = E, Fifth;
    F5 = F, Fifth;
    G5 = G, Fifth;
    H5 = H, Fifth;
    A6 = A, Sixth;
    B6 = B, Sixth;
    C6 = C, Sixth;
    D6 = D, Sixth;
    E6 = E, Sixth;
    F6 = F, Sixth;
    G6 = G, Sixth;
    H6 = H, Sixth;
    A7 = A, Seventh;
    B7 = B, Seventh;
    C7 = C, Seventh;
    D7 = D, Seventh;
    E7 = E, Seventh;
    F7 = F, Seventh;
    G7 = G, Seventh;
    H7 = H, Seventh;
    A8 = A, Eighth;
    B8 = B, Eighth;
    C8 = C, Eighth;
    D8 = D, Eighth;
    E8 = E, Eighth;
    F8 = F, Eighth;
    G8 = G, Eighth;
    H8 = H, Eighth;
}

impl Square {
    /// All squares, from A1 to H8.
    pub const ALL: [Square; 64] = [
        Square::A1,
        Square::B1,
        Square::C1,
        Square::D1,
        Square::E1,
        Square::F1,
        Square::G1,
        Square::H1,
        Square::A2,
        Square::B2,
        Square::C2,
        Square::D2,
        Square::E2,
        Square::F2,
        Square::G2,
        Square::H2,
        Square::A3,
        Square::B3,
        Square::C3,
        Square::D3,
        Square::E3,
        Square::F3,
        Square::G3,
        Square::H3,
        Square::A4,
        Square::B4,
        Square::C4,
        Square::D4,
        Square::E4,
        Square::F4,
        Square::G4,
        Square::H4,
        Square::A5,
        Square::B5,
        Square::C5,
        Square::D5,
        Square::E5,
        Square::F5,
        Square::G5,
        Square::H5,
        Square::A6,
        Square::B6,
        Square::C6,
        Square::D6,
        Square::E6,
        Square::F6,
        Square::G6,
        Square::H6,
        Square::A7,
        Square::B7,
        Square::C7,
        Square::D7,
        Square::E7,
        Square::F7,
        Square::G7,
        Square::H7,
        Square::A8,
        Square::B8,
        Square::C8,
        Square::D8,
        Square::E8,
        Square::F8,
        Square::G8,
        Square::H8,
    ];

    /// Creates a square from its index in `0..64`, with A1 = 0.
    ///
    /// # Panics
    ///
    /// Panics if `index >= 64`.
    #[must_use]
    pub const fn new(index: u32) -> Square {
        assert!(index < 64, "square index out of range");
        Square(index as u8)
    }

    /// Creates a square from a file and a rank.
    #[must_use]
    pub const fn from_coords(file: File, rank: Rank) -> Square {
        Square(file.0 + rank.0 * 8)
    }

    /// Returns the file of the square.
    #[must_use]
    pub const fn file(self) -> File {
        File(self.0 % 8)
    }

    /// Returns the rank of the square.
    #[must_use]
    pub const fn rank(self) -> Rank {
        Rank(self.0 / 8)
    }

    /// Returns the square index in `0..64`, with A1 = 0.
    #[must_use]
    pub const fn to_u32(self) -> u32 {
        self.0 as u32
    }
}

impl fmt::Display for Square {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.file(), self.rank())
    }
}

impl From<Square> for u32 {
    fn from(square: Square) -> Self {
        square.0 as u32
    }
}

impl From<Square> for usize {
    fn from(square: Square) -> Self {
        square.0 as usize
    }
}

impl FromStr for Square {
    type Err = ParseSquareError;

    fn from_str(s: &str) -> Result<Square, ParseSquareError> {
        let mut chars = s.chars();
        match (chars.next(), chars.next(), chars.next()) {
            (Some(file), Some(rank), None) => {
                if file.is_ascii_lowercase()
                    && rank.is_ascii_digit()
                    && (b'a'..=b'h').contains(&(file as u8))
                    && (b'1'..=b'8').contains(&(rank as u8))
                {
                    Ok(Square(file as u8 - b'a' + (rank as u8 - b'1') * 8))
                } else {
                    Err(ParseSquareError)
                }
            }
            _ => Err(ParseSquareError),
        }
    }
}

impl From<Square> for cozy_chess::Square {
    fn from(square: Square) -> Self {
        cozy_chess::Square::try_index(square.0 as usize).expect("valid square")
    }
}

impl From<cozy_chess::Square> for Square {
    fn from(square: cozy_chess::Square) -> Self {
        Square(square as u8)
    }
}

/// A set of squares as a 64-bit bitmask, bit `i` set for [`Square`] `i`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Bitboard(u64);

impl Bitboard {
    /// A bitboard with no squares.
    pub const EMPTY: Bitboard = Bitboard(0);

    /// A bitboard containing only the given square.
    #[must_use]
    pub const fn from_square(square: Square) -> Bitboard {
        Bitboard(1 << square.0)
    }

    /// Returns whether the bitboard contains the square.
    #[must_use]
    pub const fn contains(self, square: Square) -> bool {
        self.0 & (1 << square.0) != 0
    }

    /// Returns whether the bitboard contains no squares.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns the underlying bitmask, bit `i` for [`Square`] `i`.
    #[must_use]
    pub const fn to_u64(self) -> u64 {
        self.0
    }
}

impl BitOr for Bitboard {
    type Output = Bitboard;

    fn bitor(self, rhs: Bitboard) -> Bitboard {
        Bitboard(self.0 | rhs.0)
    }
}

impl BitOrAssign for Bitboard {
    fn bitor_assign(&mut self, rhs: Bitboard) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for Bitboard {
    type Output = Bitboard;

    fn bitand(self, rhs: Bitboard) -> Bitboard {
        Bitboard(self.0 & rhs.0)
    }
}

impl BitAndAssign for Bitboard {
    fn bitand_assign(&mut self, rhs: Bitboard) {
        self.0 &= rhs.0;
    }
}

impl Not for Bitboard {
    type Output = Bitboard;

    fn not(self) -> Bitboard {
        Bitboard(!self.0)
    }
}

impl From<Bitboard> for cozy_chess::BitBoard {
    fn from(bitboard: Bitboard) -> Self {
        cozy_chess::BitBoard(bitboard.0)
    }
}

impl From<cozy_chess::BitBoard> for Bitboard {
    fn from(bitboard: cozy_chess::BitBoard) -> Self {
        Bitboard(bitboard.0)
    }
}
