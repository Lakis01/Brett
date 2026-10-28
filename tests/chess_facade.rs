use std::str::FromStr;

use brett::chess::{Bitboard, Color, File, Piece, Rank, Role, Square};

#[test]
fn scalar_types_round_trip() {
    assert_eq!(Square::new(12), Square::E2);
    assert_eq!(Square::from_str("e4").unwrap(), Square::E4);
    assert_eq!(Square::E4.to_string(), "e4");
    assert_eq!(Square::from_coords(File::E, Rank::Fourth), Square::E4);
    assert_eq!(Square::E4.file(), File::E);
    assert_eq!(Square::E4.rank(), Rank::Fourth);
    assert_eq!(File::new(4), File::E);
    assert_eq!(Rank::new(0), Rank::First);
    assert_eq!(File::A.flip_horizontal(), File::H);
    assert_eq!(Rank::First.flip_vertical(), Rank::Eighth);
    assert_eq!(Color::White.other(), Color::Black);
    assert_eq!(Piece::new(Color::White, Role::Knight).char(), 'N');
    assert_eq!(Piece::new(Color::Black, Role::Queen).char(), 'q');
    assert!(Bitboard::from_square(Square::E4).contains(Square::E4));
    assert!(!Bitboard::EMPTY.contains(Square::E4));
    assert_eq!(
        (Bitboard::from_square(Square::A1) | Bitboard::from_square(Square::H8)).to_u64(),
        0x8000_0000_0000_0001
    );
}

#[test]
fn square_parsing_rejects_bad_input() {
    assert!(Square::from_str("").is_err());
    assert!(Square::from_str("i4").is_err());
    assert!(Square::from_str("e9").is_err());
    assert!(Square::from_str("e44").is_err());
}
