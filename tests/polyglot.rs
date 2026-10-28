use brett::book::polyglot::position_key;
use brett::chess::{CastlingMode, Chess, EnPassantMode, Fen, Position, SanPlus};

fn position(fen: &str) -> Chess {
    Fen::from_ascii(fen.as_bytes())
        .unwrap()
        .into_position(CastlingMode::Standard)
        .unwrap()
}

fn after(mut pos: Chess, sans: &[&str]) -> Chess {
    for san in sans {
        let san = SanPlus::from_ascii(san.as_bytes()).unwrap();
        let mv = san.san.to_move(&pos).unwrap();
        pos = pos.play(mv).unwrap();
    }
    pos
}

#[test]
fn polyglot_start_position_key() {
    assert_eq!(position_key(&Chess::default()), 0x463b_9618_1691_fc9c);
    assert_eq!(
        position_key(&after(Chess::default(), &["e4"])),
        0x823c_9b50_fd11_4196
    );
    assert_eq!(
        position_key(&after(Chess::default(), &["e4", "c5"])),
        0x644d_4afe_0256_4aeb
    );
}

#[test]
fn polyglot_kiwipete_and_castling_keys() {
    let kiwipete = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
    assert_eq!(position_key(&position(kiwipete)), 0xc3ce_103f_01d1_5e1d);

    let castles = "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1";
    assert_eq!(position_key(&position(castles)), 0xfda2_39cc_692a_6053);

    let bare = "8/2P5/8/8/8/8/8/K6k w - - 0 1";
    assert_eq!(position_key(&position(bare)), 0xce58_c3b3_356f_fdac);
}

#[test]
fn polyglot_ep_includes_adjacent_pawn_regardless_of_legality() {
    let legal = "rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq d6 0 3";
    assert_eq!(position_key(&position(legal)), 0x8260_57c0_f644_3c7c);

    let pinned = "4r1k1/8/8/3pP3/8/8/8/4K3 w - d6 0 1";
    let no_ep = "4r1k1/8/8/3pP3/8/8/8/4K3 w - - 0 1";
    assert_eq!(position_key(&position(pinned)), 0x7ddd_0eef_bfc8_435a);
    assert_eq!(position_key(&position(no_ep)), 0x6144_d03c_8370_d3fb);
    assert_eq!(position(pinned).ep_square(EnPassantMode::Legal), None);
}
