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

use brett::chess::{Move, San, SanPlus, UciMove};

fn play_san(pos: Chess, san: &str) -> Chess {
    let san = SanPlus::from_ascii(san.as_bytes()).unwrap();
    let mv = san.san.to_move(&pos).unwrap();
    pos.play(mv).unwrap()
}

#[test]
fn legal_moves_and_play_round_trip() {
    let start = Chess::default();
    assert_eq!(start.legal_moves().len(), 20);
    let e4 = SanPlus::from_ascii(b"e4")
        .unwrap()
        .san
        .to_move(&start)
        .unwrap();
    assert_eq!(
        e4,
        Move::Normal {
            role: Role::Pawn,
            from: Square::E2,
            capture: None,
            to: Square::E4,
            promotion: None,
        }
    );
    let after = start.clone().play(e4).unwrap();
    assert_eq!(after.turn(), Color::Black);
    assert_eq!(
        after.board().piece_at(Square::E4),
        Some(Piece::new(Color::White, Role::Pawn))
    );
}

#[test]
fn san_round_trip_and_checkmate_suffix() {
    let start = Chess::default();
    let e4 = SanPlus::from_ascii(b"e4")
        .unwrap()
        .san
        .to_move(&start)
        .unwrap();
    assert_eq!(SanPlus::from_move(start.clone(), e4).to_string(), "e4");

    let mut pos = start;
    for san in ["e4", "e5", "Bc4", "Nc6", "Qh5", "Nf6"] {
        pos = play_san(pos, san);
    }
    let mate = SanPlus::from_ascii(b"Qxf7#")
        .unwrap()
        .san
        .to_move(&pos)
        .unwrap();
    assert_eq!(SanPlus::from_move(pos.clone(), mate).to_string(), "Qxf7#");
    assert!(pos.clone().play(mate).unwrap().is_checkmate());
}

#[test]
fn uci_standard_castling_round_trip() {
    let mut pos = Chess::default();
    for san in ["e4", "e5", "Nf3", "Nc6", "Bc4", "Bc5"] {
        pos = play_san(pos, san);
    }
    let uci = UciMove::from_ascii(b"e1g1").unwrap();
    let mv = uci.to_move(&pos).unwrap();
    assert_eq!(
        mv,
        Move::Castle {
            king: Square::E1,
            rook: Square::H1
        }
    );
    assert_eq!(
        UciMove::from_move(mv, CastlingMode::Standard).to_string(),
        "e1g1"
    );
    assert_eq!(SanPlus::from_move(pos, mv).to_string(), "O-O");
}

#[test]
fn en_passant_move_is_recognized() {
    let mut pos = Chess::default();
    for san in ["e4", "a6", "e5", "d5"] {
        pos = play_san(pos, san);
    }
    let mv = UciMove::from_ascii(b"e5d6").unwrap().to_move(&pos).unwrap();
    assert_eq!(
        mv,
        Move::EnPassant {
            from: Square::E5,
            to: Square::D6
        }
    );
    let after = pos.play(mv).unwrap();
    assert!(after.board().piece_at(Square::D5).is_none());
}

#[test]
fn illegal_san_is_an_error() {
    let start = Chess::default();
    let san = SanPlus::from_ascii(b"Kf3").unwrap();
    assert!(san.san.to_move(&start).is_err());
    assert!(SanPlus::from_ascii(b"").is_err());
    assert!(matches!(SanPlus::from_ascii(b"--").unwrap().san, San::Null));
}

/// An FRC position where both castles are legal: the White king on B1 with its
/// rooks on A1 and H1, the Black king on E8, and rights only on those rook
/// squares. Unlike the start position the king is not on the E file, so
/// castling there can only round trip under `CastlingMode::Chess960`.
fn frc_castling_setup() -> Setup {
    let mut board = Board::empty();
    board.set_piece_at(Square::A1, Piece::new(Color::White, Role::Rook));
    board.set_piece_at(Square::B1, Piece::new(Color::White, Role::King));
    board.set_piece_at(Square::H1, Piece::new(Color::White, Role::Rook));
    board.set_piece_at(Square::E8, Piece::new(Color::Black, Role::King));
    let mut rights = Bitboard::EMPTY;
    rights |= Bitboard::from_square(Square::A1);
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
fn chess960_castling_san_uci_round_trip() {
    let pos = Chess::from_setup(frc_castling_setup(), CastlingMode::Chess960).unwrap();
    assert_eq!(
        pos.castles().rook(Color::White, CastlingSide::KingSide),
        Some(Square::H1)
    );
    assert_eq!(
        pos.castles().rook(Color::White, CastlingSide::QueenSide),
        Some(Square::A1)
    );

    // Kingside: king B1 -> G1, h1 rook -> F1.
    let oo = SanPlus::from_ascii(b"O-O")
        .unwrap()
        .san
        .to_move(&pos)
        .unwrap();
    assert_eq!(
        oo,
        Move::Castle {
            king: Square::B1,
            rook: Square::H1
        }
    );
    assert_eq!(
        UciMove::from_move(oo, CastlingMode::Chess960).to_string(),
        "b1h1"
    );
    assert_eq!(
        UciMove::from_ascii(b"b1h1").unwrap().to_move(&pos).unwrap(),
        oo
    );
    assert_eq!(
        UciMove::from_move(oo, CastlingMode::Standard).to_string(),
        "b1g1"
    );
    assert_eq!(SanPlus::from_move(pos.clone(), oo).to_string(), "O-O");
    let after_oo = pos.clone().play(oo).unwrap();
    assert_eq!(
        after_oo.board().piece_at(Square::G1),
        Some(Piece::new(Color::White, Role::King))
    );
    assert_eq!(
        after_oo.board().piece_at(Square::F1),
        Some(Piece::new(Color::White, Role::Rook))
    );

    // Queenside: king B1 -> C1, a1 rook -> D1.
    let ooo = SanPlus::from_ascii(b"O-O-O")
        .unwrap()
        .san
        .to_move(&pos)
        .unwrap();
    assert_eq!(
        ooo,
        Move::Castle {
            king: Square::B1,
            rook: Square::A1
        }
    );
    assert_eq!(
        UciMove::from_move(ooo, CastlingMode::Chess960).to_string(),
        "b1a1"
    );
    assert_eq!(
        UciMove::from_ascii(b"b1a1").unwrap().to_move(&pos).unwrap(),
        ooo
    );
    assert_eq!(
        UciMove::from_move(ooo, CastlingMode::Standard).to_string(),
        "b1c1"
    );
    assert_eq!(SanPlus::from_move(pos.clone(), ooo).to_string(), "O-O-O");
    let after_ooo = pos.play(ooo).unwrap();
    assert_eq!(
        after_ooo.board().piece_at(Square::C1),
        Some(Piece::new(Color::White, Role::King))
    );
    assert_eq!(
        after_ooo.board().piece_at(Square::D1),
        Some(Piece::new(Color::White, Role::Rook))
    );
}

use brett::chess::{EnPassantMode, Fen, Position};

#[test]
fn fen_round_trips_start_position() {
    let start = Chess::default();
    let fen = Fen::from_position(&start, EnPassantMode::Legal).to_string();
    assert_eq!(
        fen,
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"
    );
    let parsed = Fen::from_ascii(fen.as_bytes()).unwrap();
    let pos = parsed.into_position(CastlingMode::Standard).unwrap();
    assert_eq!(
        pos.board().piece_at(Square::E1),
        Some(Piece::new(Color::White, Role::King))
    );
}

#[test]
fn fen_resolves_xfen_kq_to_outermost_rooks() {
    let fen = "bbqnnrkr/pppppppp/8/8/8/8/PPPPPPPP/BBQNNRKR w KQkq - 0 1";
    let setup = Fen::from_ascii(fen.as_bytes()).unwrap().into_setup();
    assert!(setup.castling_rights.contains(Square::F1));
    assert!(setup.castling_rights.contains(Square::H1));
    let pos = Fen::from_ascii(fen.as_bytes())
        .unwrap()
        .into_position(CastlingMode::Chess960)
        .unwrap();
    assert_eq!(
        Fen::from_position(&pos, EnPassantMode::Legal).to_string(),
        fen
    );
}

#[test]
fn fen_writes_file_letters_for_inner_rooks() {
    let mut board = Board::empty();
    board.set_piece_at(Square::G1, Piece::new(Color::White, Role::King));
    board.set_piece_at(Square::A1, Piece::new(Color::White, Role::Rook));
    board.set_piece_at(Square::D1, Piece::new(Color::White, Role::Rook));
    board.set_piece_at(Square::E8, Piece::new(Color::Black, Role::King));
    let mut rights = Bitboard::EMPTY;
    rights |= Bitboard::from_square(Square::D1);
    let setup = Setup {
        board,
        turn: Color::White,
        castling_rights: rights,
        ep_square: None,
        halfmoves: 0,
        fullmoves: NonZeroU32::new(1).unwrap(),
    };
    let pos = Chess::from_setup(setup, CastlingMode::Chess960).unwrap();
    let fen = Fen::from_position(&pos, EnPassantMode::Legal).to_string();
    assert!(
        fen.contains(" w D "),
        "expected inner-rook file letter, got {fen}"
    );
}

#[test]
fn position_hash_is_stable_and_uses_legal_ep_only() {
    assert_eq!(Chess::default().position_hash(), 0x33ab_452b_8705_6e0f);

    let legal = "rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq d6 0 3";
    let no_ep = "rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq - 0 3";
    let legal = Fen::from_ascii(legal.as_bytes())
        .unwrap()
        .into_position(CastlingMode::Standard)
        .unwrap();
    let no_ep = Fen::from_ascii(no_ep.as_bytes())
        .unwrap()
        .into_position(CastlingMode::Standard)
        .unwrap();
    assert_ne!(legal.position_hash(), no_ep.position_hash());

    let pinned = "4r1k1/8/8/3pP3/8/8/8/4K3 w - d6 0 1";
    let pinned_no_ep = "4r1k1/8/8/3pP3/8/8/8/4K3 w - - 0 1";
    let pinned = Fen::from_ascii(pinned.as_bytes())
        .unwrap()
        .into_position(CastlingMode::Standard)
        .unwrap();
    let pinned_no_ep = Fen::from_ascii(pinned_no_ep.as_bytes())
        .unwrap()
        .into_position(CastlingMode::Standard)
        .unwrap();
    assert_eq!(pinned.ep_square(EnPassantMode::Legal), None);
    assert_eq!(pinned.position_hash(), pinned_no_ep.position_hash());
}

#[test]
fn swap_turn_clears_ep_and_keeps_clocks() {
    let fen = "rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq d6 0 3";
    let pos = Fen::from_ascii(fen.as_bytes())
        .unwrap()
        .into_position(CastlingMode::Standard)
        .unwrap();
    let swapped = pos.swap_turn().unwrap();
    assert_eq!(swapped.turn(), Color::Black);
    assert_eq!(swapped.maybe_ep_square(), None);
    assert_eq!(swapped.halfmoves(), 0);
    assert_eq!(swapped.fullmoves(), NonZeroU32::new(3).unwrap());
}
