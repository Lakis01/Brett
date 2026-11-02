use super::*;
use crate::domain;
use crate::domain::Game;
use crate::twocbh::{TwoCbhFile, TwoCbhLibrary};
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("brett-2cbh-writer-{}-{unique}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn writes_a_readable_empty_database() {
    let dir = TempDir::new();
    let path = dir.path().join("empty.2cbh");
    let report = write_database(&path, Vec::<Result<Game>>::new()).unwrap();
    assert_eq!(report.games, 0);
    assert!(report.bytes > 0);

    let file = TwoCbhFile::open(&path).unwrap();
    assert_eq!(file.slot_count(), 1);
    assert_eq!(file.game_count(), 0);

    let cbh = std::fs::read(&path).unwrap();
    assert_eq!(cbh.len(), 192);
    assert_eq!(cbh[8], 0x26);
    assert_eq!(u16::from_le_bytes([cbh[0x0a], cbh[0x0b]]), 192);
    assert_eq!(u16::from_le_bytes([cbh[0x0c], cbh[0x0d]]), 0x0500);
    assert_eq!(u32::from_le_bytes(cbh[0x10..0x14].try_into().unwrap()), 1);

    let cbg = std::fs::read(path.with_extension("2cbg")).unwrap();
    assert_eq!(cbg.len(), 12);
    assert_eq!(u64::from_le_bytes(cbg[0..8].try_into().unwrap()), 12);
    assert_eq!(u16::from_le_bytes([cbg[8], cbg[9]]), 12);
    assert_eq!(u16::from_le_bytes([cbg[10], cbg[11]]), 0x0500);

    let cba = std::fs::read(path.with_extension("2cba")).unwrap();
    assert_eq!(cba.len(), 12);
    assert_eq!(u16::from_le_bytes([cba[10], cba[11]]), 0);

    let lid = std::fs::read(path.with_extension("2lid")).unwrap();
    assert_eq!(lid.len(), 0xB8 + 0x108A);
    let be = |at: usize| u32::from_be_bytes(lid[at..at + 4].try_into().unwrap());
    assert_eq!(be(0x00), 0xB8);
    assert_eq!(be(0x04), 6);
    assert_eq!(be(0x08), 0x400);
    assert_eq!(be(0x0c), 0);
    assert_eq!(be(0x10), 2);
    assert_eq!(be(0x14), u32::MAX);
    assert_eq!(be(0x18), u32::MAX);
    assert_eq!(be(0x1c), 0x460);
    assert_eq!(be(0x24), 0);
    assert_eq!(be(0x38), 2);
    assert_eq!(be(0x44), 0x400);
    assert_eq!(be(0x74), 1);
    assert_eq!(be(0x80), u32::MAX);
}

#[test]
fn normalizes_companion_extensions_and_rejects_other_targets() {
    let dir = TempDir::new();
    let path = dir.path().join("db.2cbg");
    write_database(&path, Vec::<Result<Game>>::new()).unwrap();
    assert!(dir.path().join("db.2cbh").exists());
    assert!(dir.path().join("db.2cbg").exists());

    let err = write_database(&dir.path().join("db.txt"), Vec::<Result<Game>>::new()).unwrap_err();
    assert!(format!("{err:#}").contains("must end in .2cbh"));

    let err = write_database(
        &dir.path().join("missing/db.2cbh"),
        Vec::<Result<Game>>::new(),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("does not exist"));
}

#[test]
fn removes_stale_derived_indexes_and_preserves_unrelated_files() {
    let dir = TempDir::new();
    let path = dir.path().join("replace.2cbh");
    write_database(&path, Vec::<Result<Game>>::new()).unwrap();
    std::fs::write(path.with_extension("2lcd"), b"stale").unwrap();
    std::fs::write(path.with_extension("2lgd"), b"stale").unwrap();
    std::fs::write(path.with_extension("ini"), b"keep").unwrap();

    write_database(&path, Vec::<Result<Game>>::new()).unwrap();

    assert!(!path.with_extension("2lcd").exists());
    assert!(!path.with_extension("2lgd").exists());
    assert_eq!(std::fs::read(path.with_extension("ini")).unwrap(), b"keep");
}

fn parse_one(pgn: &str) -> Game {
    domain::parse_pgn(pgn.as_bytes()).unwrap().remove(0)
}

fn ordered_tree(game: &Game, node: usize) -> String {
    game.nodes[node]
        .children
        .iter()
        .map(|&child| {
            let uci = &game.nodes[child].mv.as_ref().unwrap().uci;
            format!("({uci} {})", ordered_tree(game, child))
        })
        .collect::<Vec<_>>()
        .join("")
}

fn uci_line(game: &Game) -> Vec<String> {
    game.mainline()
        .iter()
        .map(|&id| game.nodes[id].mv.as_ref().unwrap().uci.clone())
        .collect()
}

fn decode(encoded: &EncodedMoves, start_id: Option<u16>) -> Game {
    let mut decoded = Game::default();
    if let Some(id) = start_id {
        decoded.nodes[0].pos = crate::twocbh::moves::start_position_960(id).unwrap();
    }
    crate::twocbh::moves::decode_moves(&mut decoded, &encoded.tokens, start_id.is_some()).unwrap();
    decoded
}

#[test]
fn encodes_and_decodes_a_standard_mainline() {
    let game = parse_one("1. e4 e5 2. Nf3 Nc6 3. Bb5 *");
    let encoded = encode_game_moves(&game).unwrap();
    assert_eq!(encoded.start_id, None);
    assert_eq!(encoded.tokens.last(), Some(&0xffff));

    let decoded = decode(&encoded, None);
    assert_eq!(uci_line(&decoded), uci_line(&game));
    assert_eq!(ordered_tree(&decoded, 0), ordered_tree(&game, 0));
}

#[test]
fn variation_markers_follow_the_replaced_move_and_round_trip() {
    let game = parse_one(
        "1. e4 (1. d4 d5) (1. c4 e5) e5 (1... c5 2. Nf3 (2. Nc3 Nc6)) \
         2. Nf3 Nc6 (2... Nf6) 3. Bb5 *",
    );
    let encoded = encode_game_moves(&game).unwrap();
    // Two root alternatives: both markers follow the first mainline move.
    assert_eq!(encoded.tokens[1], 0xfffd);
    assert_eq!(encoded.tokens[2], 0xfffd);

    let decoded = decode(&encoded, None);
    assert_eq!(uci_line(&decoded), uci_line(&game));
    assert_eq!(ordered_tree(&decoded, 0), ordered_tree(&game, 0));
}

#[test]
fn special_moves_round_trip() {
    const BLACK_EN_PASSANT: &str = "1. e4 e5 2. Nf3 Nc6 3. Bc4 Bc5 4. c3 Nf6 5. b4 Bb6 6. a4 a6 7. O-O O-O \
         8. Bb2 d6 9. h3 Be6 10. Bxe6 fxe6 11. Qe2 Qe7 12. Na3 Rad8 13. Rad1 d5 \
         14. Bc1 dxe4 15. d4 exd3 *";
    for pgn in [
        // En passant, then a capture promotion.
        "1. e4 a6 2. e5 d5 3. exd6 Nf6 4. dxc7 Nbd7 5. cxd8=Q *",
        // Quiet promotion (a8 is empty after ...Rb8).
        "1. a4 b5 2. axb5 a6 3. bxa6 Nc6 4. a7 Rb8 5. a8=Q *",
        "1. e4 e5 2. Nf3 Nc6 3. Bc4 Bc5 4. O-O *",
        "1. d4 d5 2. Nc3 Nc6 3. Bf4 Bf5 4. Qd2 Qd7 5. O-O-O *",
        // Black queenside castling.
        "1. d4 d5 2. Nc3 Nc6 3. Bf4 Bf5 4. Qd2 Qd7 5. O-O-O O-O-O *",
        // Black en passant (...exd3 after 15. d4).
        BLACK_EN_PASSANT,
        "1. e4 e5 2. -- *",
    ] {
        let game = parse_one(pgn);
        assert!(game.errors.is_empty(), "{pgn}: {:?}", game.errors);
        if pgn == BLACK_EN_PASSANT {
            let line = uci_line(&game);
            assert_eq!(line.len(), 30, "{pgn}");
            assert_eq!(line.last().map(String::as_str), Some("e4d3"), "{pgn}");
        }
        let encoded = encode_game_moves(&game).unwrap();
        let decoded = decode(&encoded, None);
        assert!(decoded.errors.is_empty(), "{pgn}: {:?}", decoded.errors);
        assert_eq!(uci_line(&decoded), uci_line(&game), "{pgn}");
        assert_eq!(ordered_tree(&decoded, 0), ordered_tree(&game, 0), "{pgn}");
    }
}

#[test]
fn chess960_start_and_castling_round_trip() {
    let standard_board = parse_one(
        "[Variant \"Chess960\"]\n\
         [FEN \"rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1\"]\n\n\
         1. e4 e5 2. Nf3 Nc6 3. Bc4 Bc5 4. O-O *",
    );
    let encoded = encode_game_moves(&standard_board).unwrap();
    assert_eq!(encoded.start_id, Some(518));
    let decoded = decode(&encoded, Some(518));
    assert_eq!(ordered_tree(&decoded, 0), ordered_tree(&standard_board, 0));

    let non_standard = parse_one(
        "[Variant \"Chess960\"]\n\
         [FEN \"bbqnnrkr/pppppppp/8/8/8/8/PPPPPPPP/BBQNNRKR w KQkq - 0 1\"]\n\n\
         1. f4 f5 2. Rf2 d5 3. O-O *",
    );
    let encoded = encode_game_moves(&non_standard).unwrap();
    assert_eq!(encoded.start_id, Some(0));
    let decoded = decode(&encoded, Some(0));
    assert_eq!(ordered_tree(&decoded, 0), ordered_tree(&non_standard, 0));
    assert_eq!(
        uci_line(&decoded).last().map(String::as_str),
        Some("g1h1"),
        "SP0 kingside castling must survive as Chess960 castling"
    );
}

#[test]
fn rejects_custom_fen_and_unsupported_variants() {
    let custom = parse_one("[FEN \"8/8/8/8/8/8/4K3/7k w - - 0 1\"]\n\n*");
    let err = encode_game_moves(&custom).unwrap_err();
    assert!(format!("{err:#}").contains("standard chess or Chess960"));

    let crazyhouse = parse_one("[Variant \"Crazyhouse\"]\n\n1. e4 *");
    let err = encode_game_moves(&crazyhouse).unwrap_err();
    assert!(format!("{err:#}").contains("variant"));
}

#[test]
fn rejects_unresolvable_moves() {
    let mut game = parse_one("1. e4 e5 *");
    let node = game.mainline()[0];
    game.nodes[node].mv.as_mut().unwrap().uci = "e2e5".to_string();
    game.nodes[node].mv.as_mut().unwrap().san = "Z9".to_string();
    let err = encode_game_moves(&game).unwrap_err();
    assert!(format!("{err:#}").contains("cannot resolve move"));
}

#[test]
fn writes_and_reopens_metadata_and_names() {
    let dir = TempDir::new();
    let path = dir.path().join("meta.2cbh");
    let game = parse_one(
        "[Event \"Test Event\"]\n\
         [Site \"Test City\"]\n\
         [Date \"2024.01.02\"]\n\
         [Round \"3.2\"]\n\
         [White \"Doe, Jane\"]\n\
         [Black \"Roe, Richard\"]\n\
         [Result \"1-0\"]\n\
         [WhiteElo \"2500\"]\n\
         [BlackElo \"2400\"]\n\
         [WhiteFideId \"123456\"]\n\
         [BlackFideId \"654321\"]\n\
         [GameId \"42\"]\n\
         [Beauty \"7\"]\n\
         [SourceTitle \"Test Source\"]\n\
         [Source \"Publisher\"]\n\
         [Deleted \"1\"]\n\n\
         1. e4 e5 *",
    );

    let report = write_database(&path, vec![Ok(game)]).unwrap();
    assert_eq!(report.games, 1);

    let cbh = std::fs::read(&path).unwrap();
    let record = &cbh[192..384];
    assert_eq!(record[0], 0x81);
    assert_eq!(&record[1..4], &[0, 1, 1]);
    assert_eq!(
        u32::from_le_bytes(record[0x18..0x1c].try_into().unwrap()),
        0
    );
    assert_eq!(
        u32::from_le_bytes(record[0x20..0x24].try_into().unwrap()),
        1
    );
    assert_eq!(
        u32::from_le_bytes(record[0x28..0x2c].try_into().unwrap()),
        0
    );
    assert_eq!(
        u32::from_le_bytes(record[0x38..0x3c].try_into().unwrap()),
        0
    );
    assert_eq!(
        u32::from_le_bytes(record[0x30..0x34].try_into().unwrap()),
        2
    );
    assert_eq!(&record[0x40..0x50], &[0xff; 16]);
    assert_eq!(
        u32::from_le_bytes(record[0x58..0x5c].try_into().unwrap()),
        (3 << 16) | 2
    );
    assert_eq!(
        u32::from_le_bytes(record[0x5c..0x60].try_into().unwrap()),
        2
    );
    assert_eq!(
        u32::from_le_bytes(record[0x60..0x64].try_into().unwrap()),
        2500
    );
    assert_eq!(
        u32::from_le_bytes(record[0x70..0x74].try_into().unwrap()),
        2400
    );
    assert_eq!(
        u32::from_le_bytes(record[0x80..0x84].try_into().unwrap()),
        0
    );
    assert_eq!(
        u64::from_le_bytes(record[0x98..0xa0].try_into().unwrap()),
        42
    );
    assert_eq!(
        u64::from_le_bytes(record[0xb0..0xb8].try_into().unwrap()),
        7
    );
    assert_eq!(
        u32::from_le_bytes(record[0xbc..0xc0].try_into().unwrap()),
        (2024 << 9) | (1 << 5) | 2
    );

    let library = TwoCbhLibrary::open(&path).unwrap();
    assert_eq!(library.len(), 1);
    let got = library.load(0).unwrap();
    assert_eq!(got.white(), "Doe, Jane");
    assert_eq!(got.black(), "Roe, Richard");
    assert_eq!(got.header("Event"), Some("Test Event"));
    assert_eq!(got.header("Site"), Some("Test City"));
    assert_eq!(got.date(), "2024.01.02");
    assert_eq!(got.header("Round"), Some("3.2"));
    assert_eq!(got.result(), "1-0");
    assert_eq!(got.header("WhiteElo"), Some("2500"));
    assert_eq!(got.header("BlackElo"), Some("2400"));
    assert_eq!(got.header("WhiteFideId"), Some("123456"));
    assert_eq!(got.header("BlackFideId"), Some("654321"));
    assert_eq!(got.header("GameId"), Some("42"));
    assert_eq!(got.header("Beauty"), Some("7"));
    assert_eq!(got.header("SourceTitle"), Some("Test Source"));
    assert_eq!(got.header("Source"), Some("Publisher"));
    assert!(got.errors.iter().any(|e| e.contains("marked deleted")));

    let lid = std::fs::read(path.with_extension("2lid")).unwrap();
    assert_eq!(u32::from_be_bytes(lid[0x10..0x14].try_into().unwrap()), 3);
    assert_eq!(u32::from_be_bytes(lid[0x24..0x28].try_into().unwrap()), 1);

    let block = 0xB8;
    let player_len = u32::from_le_bytes(lid[block..block + 4].try_into().unwrap());
    assert_eq!(player_len as usize, 32 + "Doe".len() + "Jane".len());
    let event_at = block + 0x400;
    let event_len = u32::from_le_bytes(lid[event_at..event_at + 4].try_into().unwrap());
    assert_eq!(
        event_len as usize,
        12 + "Test City".len() + "Test Event".len() + 104
    );
    let source_at = block + 0x860;
    let source_len = u32::from_le_bytes(lid[source_at..source_at + 4].try_into().unwrap());
    assert_eq!(
        source_len as usize,
        20 + "Test Source".len() + "Publisher".len()
    );
}

#[test]
fn writes_and_reopens_an_empty_game() {
    let dir = TempDir::new();
    let path = dir.path().join("empty-game.2cbh");
    let game = parse_one("[White \"Empty\"]\n[Black \"Game\"]\n[Result \"*\"]\n\n*");

    write_database(&path, vec![Ok(game)]).unwrap();

    let library = TwoCbhLibrary::open(&path).unwrap();
    assert_eq!(library.len(), 1);
    let got = library.load(0).unwrap();
    assert!(got.errors.is_empty(), "{:?}", got.errors);
    assert_eq!(got.mainline_len(), 0);
    assert_eq!(got.header("PlyCount"), Some("0"));
}

#[test]
fn keeps_duplicate_names_with_distinct_fide_ids() {
    let dir = TempDir::new();
    let path = dir.path().join("dupes.2cbh");
    let make = |fide: &str| {
        let mut game = parse_one("1. e4 e5 *");
        game.set_header("White", "Doe, Jane".to_string());
        game.set_header("WhiteFideId", fide.to_string());
        game
    };
    write_database(&path, vec![Ok(make("111")), Ok(make("222"))]).unwrap();

    let library = TwoCbhLibrary::open(&path).unwrap();
    assert_eq!(library.load(0).unwrap().header("WhiteFideId"), Some("111"));
    assert_eq!(library.load(1).unwrap().header("WhiteFideId"), Some("222"));
}

#[test]
fn preserves_utf8_names() {
    let dir = TempDir::new();
    let path = dir.path().join("utf8.2cbh");
    let mut game = parse_one("1. e4 e5 *");
    game.set_header("White", "Müller, Jörg".to_string());
    game.set_header("Black", "Ābols, Jānis".to_string());
    write_database(&path, vec![Ok(game)]).unwrap();

    let library = TwoCbhLibrary::open(&path).unwrap();
    let got = library.load(0).unwrap();
    assert_eq!(got.white(), "Müller, Jörg");
    assert_eq!(got.black(), "Ābols, Jānis");
}

#[test]
fn rejects_overlong_names() {
    let dir = TempDir::new();
    let path = dir.path().join("long.2cbh");
    let mut game = parse_one("1. e4 e5 *");
    game.set_header("White", "A".repeat(0x400));
    let err = write_database(&path, vec![Ok(game)]).unwrap_err();
    assert!(format!("{err:#}").contains("too long"));
    assert!(!path.exists());
}

#[test]
fn annotation_loss_is_blocked_by_default() {
    let dir = TempDir::new();
    let path = dir.path().join("annotated.2cbh");
    let game = parse_one("[White \"Annotated\"]\n\n1. e4 {comment} e5 *");
    assert!(crate::twocbh::has_unsupported_annotations(&game));

    let err = write_database(&path, vec![Ok(game.clone())]).unwrap_err();
    let message = format!("{err:#}");
    assert!(message.contains("comment"));
    assert!(message.contains("allow_annotation_loss"));
    assert!(message.contains("game 0 (Annotated"), "{message}");
    assert!(message.contains("at node 1"), "{message}");
    assert!(!path.exists());

    let report = write_database_with_options(
        &path,
        TwoCbhWriteOptions {
            allow_annotation_loss: true,
        },
        vec![Ok(game)],
    )
    .unwrap();
    assert_eq!(report.games, 1);
    let got = TwoCbhLibrary::open(&path).unwrap().load(0).unwrap();
    assert!(got.nodes.iter().all(|node| node.comment.is_empty()));
}

#[test]
fn every_annotation_kind_is_guarded() {
    for (pgn, feature) in [
        ("1. e4! e5 *", "NAG"),
        ("1. e4 {[%clk 0:05:00]} e5 *", "clock"),
        ("1. e4 {[%eval 0.3]} e5 *", "evaluation"),
    ] {
        let dir = TempDir::new();
        let path = dir.path().join("guarded.2cbh");
        let game = parse_one(pgn);
        let err = write_database(&path, vec![Ok(game)]).unwrap_err();
        assert!(format!("{err:#}").contains(feature), "{pgn}");
    }
}

#[test]
fn records_are_aligned_and_framed() {
    let dir = TempDir::new();
    let path = dir.path().join("aligned.2cbh");
    write_database(
        &path,
        vec![Ok(parse_one("1. e4 e5 *")), Ok(parse_one("1. d4 d5 *"))],
    )
    .unwrap();

    let cbh = std::fs::read(&path).unwrap();
    let cbg = std::fs::read(path.with_extension("2cbg")).unwrap();
    let cba = std::fs::read(path.with_extension("2cba")).unwrap();
    assert_eq!(cbh.len(), 192 * 3);

    for id in 1..=2usize {
        let record = &cbh[id * 192..(id + 1) * 192];
        let cbg_offset = u64::from_le_bytes(record[8..16].try_into().unwrap());
        let cba_offset = u64::from_le_bytes(record[16..24].try_into().unwrap());
        // The file header is 12 bytes, so the first record starts at 12;
        // each record's end is aligned so the next record starts 8-aligned.
        if id == 1 {
            assert_eq!(cbg_offset, 12);
            assert_eq!(cba_offset, 12);
        }

        let a = u32::from_le_bytes(
            cbg[cbg_offset as usize + 8..cbg_offset as usize + 12]
                .try_into()
                .unwrap(),
        ) as u64;
        let b = u32::from_le_bytes(
            cbg[cbg_offset as usize + 12..cbg_offset as usize + 16]
                .try_into()
                .unwrap(),
        ) as u64;
        assert_eq!((cbg_offset + 34 + a + b) % 8, 0);
        let ca = u32::from_le_bytes(
            cba[cba_offset as usize + 8..cba_offset as usize + 12]
                .try_into()
                .unwrap(),
        ) as u64;
        let cb = u32::from_le_bytes(
            cba[cba_offset as usize + 12..cba_offset as usize + 16]
                .try_into()
                .unwrap(),
        ) as u64;
        assert_eq!((cba_offset + 34 + ca + cb) % 8, 0);
        assert_eq!(
            &cba[cba_offset as usize + 24..cba_offset as usize + 28],
            &[0x00, 0x20, 0xff, 0xff]
        );
        assert_eq!(
            &cba[cba_offset as usize + 28..cba_offset as usize + 30],
            &[0xff, 0x7f]
        );
    }
}
