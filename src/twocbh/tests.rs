//! Synthetic 2CBH databases.  Every byte is written by the test, so no
//! personal or copyrighted database is needed in the tree.

use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::chess::{
    Board, CastlingMode, Chess, Color, EnPassantMode, Fen, Piece, Position, Role, Setup, Square,
};
use crate::domain::Game;
use crate::library::GameLibrary;

use super::*;

const RECORD_SIZE: usize = 192;
const LID_FIRST: usize = 0xB8;
const LID_STRIDE: usize = 0x108A;
const CBG_HEADER: usize = 12;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("brett-2cbh-test-{}-{unique}", std::process::id()));
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

struct GameSpec {
    kind: u8,
    white_id: u32,
    black_id: u32,
    event_id: u32,
    source_id: u32,
    flags: u32,
    date: u32,
    result: u16,
    round: u16,
    subround: u32,
    white_elo: u32,
    black_elo: u32,
    game_id: u64,
    beauty: u64,
    start_id: Option<u16>,
    tokens: Vec<u16>,
}

impl Default for GameSpec {
    fn default() -> Self {
        Self {
            kind: 1,
            white_id: 0,
            black_id: 1,
            event_id: 0,
            source_id: 0,
            flags: 0,
            date: packed_date(2025, 10, 12),
            result: 2,
            round: 1,
            subround: 0,
            white_elo: 0,
            black_elo: 0,
            game_id: 0,
            beauty: 0,
            start_id: None,
            tokens: Vec::new(),
        }
    }
}

struct Database {
    _dir: TempDir,
    stem: PathBuf,
    records: Vec<[u8; RECORD_SIZE]>,
    cbg: Vec<u8>,
    cba: Vec<u8>,
}

impl Database {
    fn new() -> Self {
        let dir = TempDir::new();
        let stem = dir.path().join("synthetic");
        let mut cbg = vec![0u8; CBG_HEADER];
        cbg[8..10].copy_from_slice(&(CBG_HEADER as u16).to_le_bytes());
        cbg[10..12].copy_from_slice(&0x0500u16.to_le_bytes());
        let mut cba = vec![0u8; CBG_HEADER];
        cba[8..10].copy_from_slice(&(CBG_HEADER as u16).to_le_bytes());
        let db = Self {
            _dir: dir,
            stem,
            records: Vec::new(),
            cbg,
            cba,
        };
        write_lid(&db.stem);
        db
    }

    fn add_game(&mut self, game: GameSpec) -> u32 {
        let id = 1 + self.records.len() as u32;
        let moves_offset = self.cbg.len() as u64;
        let content = payload(game.start_id, &game.tokens);
        append_record(&mut self.cbg, &content);
        let annotations_offset = if game.flags & 0x40 != 0 {
            let at = self.cba.len() as u64;
            append_record(&mut self.cba, &[0, 0, 0, 0]);
            at
        } else {
            CBG_HEADER as u64
        };

        let mut record = [0u8; RECORD_SIZE];
        record[0] = game.kind;
        record[1..4].copy_from_slice(&[0, 1, 1]);
        record[8..16].copy_from_slice(&moves_offset.to_le_bytes());
        record[16..24].copy_from_slice(&annotations_offset.to_le_bytes());
        record[24..28].copy_from_slice(&game.white_id.to_le_bytes());
        record[32..36].copy_from_slice(&game.black_id.to_le_bytes());
        record[40..44].copy_from_slice(&game.event_id.to_le_bytes());
        record[48..52].copy_from_slice(&game.flags.to_le_bytes());
        record[56..60].copy_from_slice(&game.source_id.to_le_bytes());
        record[0x58..0x5c]
            .copy_from_slice(&(((game.round as u32) << 16) | u32::from(game.result)).to_le_bytes());
        record[0x5c..0x60].copy_from_slice(&game.subround.to_le_bytes());
        record[0x60..0x64].copy_from_slice(&game.white_elo.to_le_bytes());
        record[0x70..0x74].copy_from_slice(&game.black_elo.to_le_bytes());
        if let Some(start_id) = game.start_id {
            record[0x80..0x84].copy_from_slice(&(0xFC40u32 + u32::from(start_id)).to_le_bytes());
        }
        record[0x98..0xa0].copy_from_slice(&game.game_id.to_le_bytes());
        record[0xb0..0xb8].copy_from_slice(&game.beauty.to_le_bytes());
        record[0xbc..0xc0].copy_from_slice(&game.date.to_le_bytes());
        self.records.push(record);
        id
    }

    fn add_text(&mut self) -> u32 {
        let id = 1 + self.records.len() as u32;
        let mut record = [0u8; RECORD_SIZE];
        record[0] = 0x03;
        self.records.push(record);
        id
    }

    fn finish(&self) -> PathBuf {
        let mut cbg = self.cbg.clone();
        let cbg_len = cbg.len() as u64;
        cbg[0..8].copy_from_slice(&cbg_len.to_le_bytes());
        fs::write(self.stem.with_extension("2cbg"), cbg).unwrap();

        let mut cba = self.cba.clone();
        let cba_len = cba.len() as u64;
        cba[0..8].copy_from_slice(&cba_len.to_le_bytes());
        fs::write(self.stem.with_extension("2cba"), cba).unwrap();

        let mut cbh = vec![0u8; RECORD_SIZE];
        cbh[8] = 0x26;
        cbh[10..12].copy_from_slice(&(RECORD_SIZE as u16).to_le_bytes());
        let count = (self.records.len() as u32 + 1).to_le_bytes();
        cbh[0x10..0x14].copy_from_slice(&count);
        for record in &self.records {
            cbh.extend_from_slice(record);
        }
        let path = self.stem.with_extension("2cbh");
        fs::write(&path, cbh).unwrap();
        path
    }
}

fn payload(start_id: Option<u16>, tokens: &[u16]) -> Vec<u8> {
    let mut words = Vec::with_capacity(tokens.len() + 4);
    match start_id {
        Some(id) => {
            words.push(2);
            words.push(0xfffb);
            words.push(id);
            words.push(0xfffc);
        }
        None => {
            words.push(1);
            words.push(0xfffc);
        }
    }
    words.extend_from_slice(tokens);
    let mut bytes = Vec::with_capacity(words.len() * 2);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes
}

fn append_record(file: &mut Vec<u8>, content: &[u8]) {
    let a = content.len() as u32;
    let b = 0u32;
    let total = 34 + u64::from(a) + u64::from(b);
    file.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
    file.extend_from_slice(&a.to_le_bytes());
    file.extend_from_slice(&b.to_le_bytes());
    file.extend_from_slice(&[0u8; 8]);
    file.extend_from_slice(content);
    file.extend_from_slice(&[0xff, 0xff]);
    file.extend(std::iter::repeat_n(0u8, b as usize));
    file.extend_from_slice(&total.to_le_bytes());
}

fn write_lid(stem: &Path) {
    let players = [
        ("Doe", "Jane", 123456u32),
        ("Roe", "Richard", u32::MAX),
        ("Rathanvel", "VS", 789012),
        ("", "", u32::MAX),
    ];
    let events = [("Testville", "Test Open"), ("Elsewhere", "Second Event")];
    let sources = [("Test Source", "Test Publisher"), ("", "")];
    let blocks = players.len().max(events.len()).max(sources.len());
    let mut bytes = vec![0u8; LID_FIRST + blocks * LID_STRIDE];
    for index in 0..blocks {
        let base = LID_FIRST + index * LID_STRIDE;
        if let Some((last, first, fide)) = players.get(index) {
            write_player(&mut bytes[base..base + LID_STRIDE], last, first, *fide);
        }
        if let Some((place, title)) = events.get(index) {
            write_event(
                &mut bytes[base + 0x400..base + 0x400 + LID_STRIDE],
                place,
                title,
            );
        }
        if let Some((title, publisher)) = sources.get(index) {
            write_source(
                &mut bytes[base + 0x860..base + 0x860 + LID_STRIDE],
                title,
                publisher,
            );
        }
    }
    fs::write(stem.with_extension("2lid"), bytes).unwrap();
}

fn write_player(block: &mut [u8], last: &str, first: &str, fide: u32) {
    let total = 32 + last.len() + first.len();
    block[0..4].copy_from_slice(&(total as u32).to_le_bytes());
    let mut at = 4;
    for text in [last, first] {
        block[at..at + 4].copy_from_slice(&(text.len() as u32).to_le_bytes());
        at += 4;
        block[at..at + text.len()].copy_from_slice(text.as_bytes());
        at += text.len();
    }
    block[at..at + 8].fill(0);
    block[at + 8..at + 12].copy_from_slice(&1u32.to_le_bytes());
    block[at + 12..at + 16].copy_from_slice(&8u32.to_le_bytes());
    block[at + 16..at + 20].copy_from_slice(&fide.to_le_bytes());
}

fn write_event(block: &mut [u8], place: &str, title: &str) {
    let total = 32 + place.len() + title.len() + 72;
    block[0..4].copy_from_slice(&(total as u32).to_le_bytes());
    let mut at = 4;
    for text in [place, title] {
        block[at..at + 4].copy_from_slice(&(text.len() as u32).to_le_bytes());
        at += 4;
        block[at..at + text.len()].copy_from_slice(text.as_bytes());
        at += text.len();
    }
}

fn write_source(block: &mut [u8], title: &str, publisher: &str) {
    let total = 20 + title.len() + publisher.len();
    block[0..4].copy_from_slice(&(total as u32).to_le_bytes());
    let mut at = 4;
    for text in [title, publisher] {
        block[at..at + 4].copy_from_slice(&(text.len() as u32).to_le_bytes());
        at += 4;
        block[at..at + text.len()].copy_from_slice(text.as_bytes());
        at += text.len();
    }
    block[at..at + 4].copy_from_slice(&0xfd368u32.to_le_bytes());
    block[at + 4..at + 8].copy_from_slice(&0xfd368u32.to_le_bytes());
}

fn packed_date(year: u32, month: u32, day: u32) -> u32 {
    (year << 9) | (month << 5) | day
}

fn game_tokens() -> Vec<u16> {
    // `append_record` supplies the final `0xffff`, matching real 2CBG files.
    vec![44351, 45101, 12895, 33643, 15463, 44731]
}

#[test]
fn token_table_reproduces_known_entries_and_boundaries() {
    let table = tokens::table();
    assert_eq!(table[44351], [33, 35, 6, 0, 0, 0, 0, 0]);
    assert_eq!(table[44913], [22, 20, 14, 0, 0, 0, 0, 0]);
    assert_eq!(table[45353], [32, 16, 1, 0, 0, 24, 5, 0]);
    assert_eq!(table[45354], [32, 48, 1, 0, 56, 40, 5, 0]);
    assert_eq!(table[45355], [39, 23, 9, 0, 7, 31, 13, 0]);
    assert_eq!(table[45356], [39, 55, 9, 0, 63, 47, 13, 0]);
    assert_eq!(table[47430], [32, 48, 1, 0, 56, 40, 5, 0]);
    assert_eq!(table[49196], [15, 55, 9, 0, 23, 47, 13, 0]);
    assert_eq!(table[49197], [0; 8]);
    assert_eq!(table.len(), 65_536);
}

#[test]
fn headers_and_standard_moves_come_from_the_container() {
    let mut db = Database::new();
    db.add_game(GameSpec {
        white_elo: 2789,
        black_elo: 2661,
        game_id: 2232578210357655,
        beauty: 8248753166932,
        flags: 0x02,
        tokens: game_tokens(),
        ..GameSpec::default()
    });
    let path = db.finish();

    let library = TwoCbhLibrary::open(&path).unwrap();
    assert_eq!(library.len(), 1);
    assert_eq!(library.filter("testville"), vec![0]);
    assert!(library.source_meta().unwrap().len > 0);

    let indexed = library.get(0).unwrap();
    assert_eq!(indexed.event(), "Test Open");
    assert_eq!(indexed.header("Site"), Some("Testville"));
    assert_eq!(indexed.date(), "2025.10.12");
    assert_eq!(indexed.header("Round"), Some("1"));
    assert_eq!(indexed.white(), "Doe, Jane");
    assert_eq!(indexed.black(), "Roe, Richard");
    assert_eq!(indexed.result(), "1-0");
    assert_eq!(indexed.header("WhiteElo"), Some("2789"));
    assert_eq!(indexed.header("BlackElo"), Some("2661"));
    assert_eq!(indexed.header("WhiteFideId"), Some("123456"));
    assert_eq!(indexed.header("BlackFideId"), None);
    assert_eq!(indexed.header("GameId"), Some("2232578210357655"));
    assert_eq!(indexed.header("Beauty"), Some("8248753166932"));
    assert_eq!(indexed.header("SourceTitle"), Some("Test Source"));
    assert_eq!(indexed.header("Source"), Some("Test Publisher"));
    assert_eq!(indexed.cbh_record_id(), Some(1));
    assert!(!indexed.cbh_deleted());

    let game = library.load(0).unwrap();
    assert!(game.errors.is_empty(), "{:?}", game.errors);
    assert_eq!(
        game.mainline()
            .iter()
            .map(|&id| game.nodes[id].mv.as_ref().unwrap().san.as_str())
            .collect::<Vec<_>>(),
        ["e4", "e5", "Nf3", "Nc6", "Bb5", "a6"]
    );
}

#[test]
fn headers_use_initials_period_round_subround_and_omit_zero_elo() {
    let mut db = Database::new();
    db.add_game(GameSpec {
        white_id: 2,
        black_id: 3,
        event_id: 1,
        source_id: 1,
        date: packed_date(2024, 0, 0),
        result: 1,
        round: 3,
        subround: 2,
        tokens: game_tokens(),
        ..GameSpec::default()
    });
    let path = db.finish();
    let library = TwoCbhLibrary::open(&path).unwrap();
    let indexed = library.get(0).unwrap();

    assert_eq!(indexed.white(), "Rathanvel, VS.");
    assert_eq!(indexed.black(), "?");
    assert_eq!(indexed.header("Round"), Some("3.2"));
    assert_eq!(indexed.date(), "2024.??.??");
    assert_eq!(indexed.result(), "1/2-1/2");
    assert_eq!(indexed.event(), "Second Event");
    assert_eq!(indexed.header("Site"), Some("Elsewhere"));
    assert_eq!(indexed.header("WhiteElo"), None);
    assert_eq!(indexed.header("BlackElo"), None);
    assert_eq!(indexed.header("WhiteFideId"), Some("789012"));
    assert_eq!(indexed.header("SourceTitle"), None);
    assert_eq!(indexed.header("Source"), None);
}

#[test]
fn text_records_are_skipped_and_deleted_games_stay_readable() {
    let mut db = Database::new();
    db.add_text();
    db.add_game(GameSpec {
        tokens: game_tokens(),
        ..GameSpec::default()
    });
    db.add_game(GameSpec {
        kind: 0x81,
        tokens: game_tokens(),
        ..GameSpec::default()
    });
    let path = db.finish();
    let library = TwoCbhLibrary::open(&path).unwrap();

    assert_eq!(library.len(), 2);
    assert_eq!(library.get(0).unwrap().cbh_record_id(), Some(2));
    assert!(!library.get(0).unwrap().cbh_deleted());
    assert!(library.get(1).unwrap().cbh_deleted());
    assert!(library.load(0).unwrap().errors.is_empty());
    assert!(
        library
            .load(1)
            .unwrap()
            .errors
            .iter()
            .any(|error| error.contains("deleted"))
    );
}

#[test]
fn nested_variations_attach_to_the_parent_of_the_replaced_move() {
    let mut db = Database::new();
    db.add_game(GameSpec {
        tokens: vec![
            44351, 45101, 12895, 33643, 15463, 44731, 0xfffd, 0xffff, 35113, 45354, 0xfffd, 0xffff,
            44258,
        ],
        ..GameSpec::default()
    });
    let path = db.finish();
    let library = TwoCbhLibrary::open(&path).unwrap();
    let game = library.load(0).unwrap();
    assert!(game.errors.is_empty(), "{:?}", game.errors);

    let mainline = game.mainline();
    let sans: Vec<&str> = mainline
        .iter()
        .map(|&id| game.nodes[id].mv.as_ref().unwrap().san.as_str())
        .collect();
    assert_eq!(sans, ["e4", "e5", "Nf3", "Nc6", "Bb5", "a6"]);

    // The variation replaces a6, so it hangs off a6's parent (Bb5).
    let bb5 = mainline[4];
    assert_eq!(game.nodes[bb5].children.len(), 2);
    let nf6 = game.nodes[bb5].children[1];
    assert_eq!(game.nodes[nf6].mv.as_ref().unwrap().san, "Nf6");
    assert_eq!(game.nodes[nf6].children.len(), 2);
    let castle = game.nodes[nf6].children[0];
    let d3 = game.nodes[nf6].children[1];
    assert_eq!(game.nodes[castle].mv.as_ref().unwrap().san, "O-O");
    assert_eq!(game.nodes[d3].mv.as_ref().unwrap().san, "d3");
}

#[test]
fn chess960_start_fen_and_castling_use_the_960_rook() {
    let mut db = Database::new();
    db.add_game(GameSpec {
        start_id: Some(518),
        tokens: vec![44351, 45101, 12895, 33643, 15457, 37867, 47430],
        ..GameSpec::default()
    });
    let path = db.finish();
    let library = TwoCbhLibrary::open(&path).unwrap();
    let game = library.load(0).unwrap();
    assert!(game.errors.is_empty(), "{:?}", game.errors);

    assert_eq!(game.header("Variant"), Some("Chess960"));
    assert_eq!(game.header("SetUp"), Some("1"));
    assert_eq!(
        game.header("FEN"),
        Some("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1")
    );
    let last = *game.mainline().last().unwrap();
    assert_eq!(game.nodes[last].mv.as_ref().unwrap().san, "O-O");
    assert_eq!(game.nodes[last].mv.as_ref().unwrap().uci, "e1h1");
}

#[test]
fn chess960_start_ids_cover_the_boundaries() {
    for (id, expected) in [
        (
            0,
            "bbqnnrkr/pppppppp/8/8/8/8/PPPPPPPP/BBQNNRKR w KQkq - 0 1",
        ),
        (
            959,
            "rkrnnqbb/pppppppp/8/8/8/8/PPPPPPPP/RKRNNQBB w KQkq - 0 1",
        ),
    ] {
        let position = moves::start_position_960(id).unwrap();
        let fen = Fen::from_position(&position, EnPassantMode::Legal).to_string();
        assert_eq!(fen, expected, "start id {id}");
    }
}

#[test]
fn promotion_and_en_passant_tokens_decode() {
    let mut game = Game::default();
    let mut setup = Setup::empty();
    setup
        .board
        .set_piece_at(square(0, 6), Piece::new(Color::White, Role::Pawn));
    setup
        .board
        .set_piece_at(square(4, 0), Piece::new(Color::White, Role::King));
    setup
        .board
        .set_piece_at(square(4, 7), Piece::new(Color::Black, Role::King));
    setup.fullmoves = NonZeroU32::new(1).unwrap();
    game.nodes[0].pos = Chess::from_setup(setup, CastlingMode::Standard).unwrap();
    moves::decode_moves(&mut game, &[44049, 0xffff], false).unwrap();
    let last = *game.mainline().last().unwrap();
    let ply = game.nodes[last].mv.as_ref().unwrap();
    assert_eq!(ply.san, "a8=Q+");
    assert_eq!(ply.uci, "a7a8q");

    let mut game = Game::default();
    let mut setup = Setup::empty();
    setup
        .board
        .set_piece_at(square(4, 4), Piece::new(Color::White, Role::Pawn));
    setup
        .board
        .set_piece_at(square(3, 4), Piece::new(Color::Black, Role::Pawn));
    setup
        .board
        .set_piece_at(square(4, 0), Piece::new(Color::White, Role::King));
    setup
        .board
        .set_piece_at(square(4, 7), Piece::new(Color::Black, Role::King));
    setup.ep_square = Some(square(3, 5));
    setup.fullmoves = NonZeroU32::new(1).unwrap();
    game.nodes[0].pos = Chess::from_setup(setup, CastlingMode::Standard).unwrap();
    moves::decode_moves(&mut game, &[44391, 0xffff], false).unwrap();
    let last = *game.mainline().last().unwrap();
    let ply = game.nodes[last].mv.as_ref().unwrap();
    assert_eq!(ply.san, "exd6");
    assert_eq!(ply.uci, "e5d6");
    assert_eq!(
        game.nodes[last].pos.board().piece_at(square(3, 4)),
        None,
        "the captured pawn is removed"
    );
}

#[test]
fn malformed_move_record_is_reported_without_panicking() {
    let mut db = Database::new();
    db.add_game(GameSpec {
        tokens: game_tokens(),
        ..GameSpec::default()
    });
    let path = db.finish();
    let cbg_path = path.with_extension("2cbg");
    let mut bytes = fs::read(&cbg_path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&cbg_path, bytes).unwrap();

    let library = TwoCbhLibrary::open(&path).unwrap();
    let error = library.load(0).unwrap_err().to_string();
    assert!(error.contains("record"), "{error}");
}

#[test]
#[ignore = "analysis-only token dump; set BRETT_2CBH_TOKEN_DUMP"]
fn generated_token_table_matches_the_runtime_dump() {
    let Some(path) = std::env::var_os("BRETT_2CBH_TOKEN_DUMP") else {
        return;
    };
    let bytes = fs::read(path).unwrap();
    assert_eq!(bytes.len(), 65_536 * 8);
    let table = tokens::table();
    for (index, expected) in bytes.as_chunks::<8>().0.iter().enumerate() {
        let expected: [u16; 8] = expected
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        assert_eq!(
            table[index], expected,
            "token {index} differs from the runtime table"
        );
    }
}

fn square(file: u8, rank: u8) -> Square {
    Square::new(u32::from(rank) * 8 + u32::from(file))
}

#[test]
fn board_helper_is_rank_major() {
    assert_eq!(square(0, 0), Square::A1);
    assert_eq!(square(4, 3), Square::E4);
    let board = Board::startpos();
    assert_eq!(
        board.piece_at(Square::E1),
        Some(Piece::new(Color::White, Role::King))
    );
}
