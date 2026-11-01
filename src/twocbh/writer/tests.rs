use super::*;
use crate::domain;
use crate::domain::Game;
use crate::twocbh::TwoCbhFile;
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
    for pgn in [
        // En passant, then a capture promotion.
        "1. e4 a6 2. e5 d5 3. exd6 Nf6 4. dxc7 Nbd7 5. cxd8=Q *",
        // Quiet promotion (a8 is empty after ...Rb8).
        "1. a4 b5 2. axb5 a6 3. bxa6 Nc6 4. a7 Rb8 5. a8=Q *",
        "1. e4 e5 2. Nf3 Nc6 3. Bc4 Bc5 4. O-O *",
        "1. d4 d5 2. Nc3 Nc6 3. Bf4 Bf5 4. Qd2 Qd7 5. O-O-O *",
        "1. e4 e5 2. -- *",
    ] {
        let game = parse_one(pgn);
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
