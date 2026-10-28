use std::num::NonZeroU32;
use std::str::FromStr;

use brett::chess::{
    Bitboard, Board, CastlingMode, CastlingSide, Chess, Color, File, Piece, Rank, Role, Setup,
    Square,
};

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

#[test]
fn board_and_custom_start_round_trip() {
    let mut board = Board::empty();
    assert!(board.piece_at(Square::E4).is_none());
    board.set_piece_at(Square::E4, Piece::new(Color::White, Role::Knight));
    assert_eq!(
        board.piece_at(Square::E4),
        Some(Piece::new(Color::White, Role::Knight))
    );
    assert_eq!(board.role_at(Square::E4), Some(Role::Knight));
    assert!(board.king_of(Color::White).is_none());

    let mut board = Board::startpos();
    board.remove_piece_at(Square::E2);
    board.set_piece_at(Square::E1, Piece::new(Color::White, Role::King));
    assert_eq!(board.king_of(Color::White), Some(Square::E1));
}

#[test]
fn setup_default_and_swap_turn() {
    let setup = Setup::default();
    assert_eq!(setup.turn, Color::White);
    assert_eq!(setup.fullmoves, NonZeroU32::new(1).unwrap());
    assert_eq!(
        setup.board.piece_at(Square::E1),
        Some(Piece::new(Color::White, Role::King))
    );
    assert_eq!(
        setup.board.piece_at(Square::D1),
        Some(Piece::new(Color::White, Role::Queen))
    );

    let mut setup = Setup::empty();
    setup.turn = Color::Black;
    setup.ep_square = Some(Square::E3);
    setup.swap_turn();
    assert_eq!(setup.turn, Color::White);
    assert_eq!(setup.ep_square, None);
}

/// DFRC start: king on G1, rooks on F1 and H1.
fn dfrc_setup() -> Setup {
    let mut board = Board::empty();
    for (square, role) in [
        (Square::A1, Role::Bishop),
        (Square::B1, Role::Bishop),
        (Square::C1, Role::Queen),
        (Square::D1, Role::Knight),
        (Square::E1, Role::Knight),
        (Square::F1, Role::Rook),
        (Square::G1, Role::King),
        (Square::H1, Role::Rook),
    ] {
        board.set_piece_at(square, Piece::new(Color::White, role));
    }
    for file in File::ALL {
        board.set_piece_at(
            Square::from_coords(file, Rank::Second),
            Piece::new(Color::White, Role::Pawn),
        );
    }
    // A black king is required for the position to validate; this test only
    // inspects White's rights.
    board.set_piece_at(Square::E8, Piece::new(Color::Black, Role::King));
    let mut rights = Bitboard::EMPTY;
    rights |= Bitboard::from_square(Square::F1);
    rights |= Bitboard::from_square(Square::H1);
    Setup {
        board,
        turn: Color::White,
        castling_rights: rights,
        ep_square: None,
        halfmoves: 0,
        fullmoves: NonZeroU32::new(1).unwrap(),
    }
}

#[test]
fn chess_from_setup_standard_and_chess960() {
    let chess = Chess::default();
    assert_eq!(chess.turn(), Color::White);
    assert_eq!(
        chess.board().piece_at(Square::E1),
        Some(Piece::new(Color::White, Role::King))
    );
    assert_eq!(
        chess.castles().rook(Color::White, CastlingSide::KingSide),
        Some(Square::H1)
    );
    assert_eq!(
        chess.castles().rook(Color::White, CastlingSide::QueenSide),
        Some(Square::A1)
    );
    assert_eq!(chess.fullmoves(), NonZeroU32::new(1).unwrap());

    let chess960 = Chess::from_setup(dfrc_setup(), CastlingMode::Chess960).unwrap();
    assert_eq!(
        chess960
            .castles()
            .rook(Color::White, CastlingSide::KingSide),
        Some(Square::H1)
    );
    assert_eq!(
        chess960
            .castles()
            .rook(Color::White, CastlingSide::QueenSide),
        Some(Square::F1)
    );
}

#[test]
fn standard_mode_drops_nonstandard_castling_rights() {
    let standard = Chess::from_setup(dfrc_setup(), CastlingMode::Standard).unwrap();
    assert!(!standard.castles().has(Color::White, CastlingSide::KingSide));
    assert!(
        !standard
            .castles()
            .has(Color::White, CastlingSide::QueenSide)
    );
    assert!(Chess::from_setup(Setup::empty(), CastlingMode::Standard).is_err());
}
