//! Synthetic classic-CBH databases.  The fixture is written byte by byte by
//! the test, so no personal or copyrighted database is needed in the tree.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::chess::{CastlingSide, Chess, Color, File, Position, Rank, Role, Square};

use super::tables;
use super::*;
use crate::library::GameLibrary;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("brett-cbh-test-{}-{unique}", std::process::id()));
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

#[derive(Default)]
struct GameSpec {
    cbh_flags: u8,
    cbg_flags: u8,
    body: Vec<u8>,
    annotation: Option<Vec<u8>>,
    date: u32,
    result: u8,
    round: u8,
    subround: u8,
    white_elo: u16,
    black_elo: u16,
    eco: u16,
    mainline_moves: u8,
}

struct Database {
    _dir: TempDir,
    stem: PathBuf,
    games: Vec<[u8; RECORD_SIZE]>,
    cbg: Vec<u8>,
    cba: Vec<u8>,
}

impl Database {
    fn new() -> Self {
        let dir = TempDir::new();
        let stem = dir.path().join("synthetic");
        // Classic `.cbg` and `.cba` files have a short file header; the first
        // record is not at offset zero.
        let cbg = vec![0u8; 10];
        let cba = vec![0u8; 26];
        let db = Self {
            _dir: dir,
            stem,
            games: Vec::new(),
            cbg,
            cba,
        };
        write_entities(&db.stem);
        db
    }

    fn add(&mut self, game: GameSpec) -> u32 {
        let id = 1 + self.games.len() as u32;
        let moves_offset = self.cbg.len() as u32;
        let size = 4 + game.body.len();
        self.cbg.push(game.cbg_flags);
        self.cbg.push((size >> 16) as u8);
        self.cbg.push((size >> 8) as u8);
        self.cbg.push(size as u8);
        self.cbg.extend_from_slice(&game.body);

        let annotations_offset = match &game.annotation {
            Some(items) => {
                let at = self.cba.len() as u32;
                let total = 14 + items.len();
                self.cba.extend_from_slice(&u24(id));
                self.cba.extend_from_slice(&[1, 0, 0x0e, 0x0e]);
                let count = annotation_item_count(items) + 1;
                self.cba.extend_from_slice(&u24(count));
                self.cba.extend_from_slice(&(total as u32).to_be_bytes());
                self.cba.extend_from_slice(items);
                at
            }
            None => 0,
        };

        let mut record = [0u8; RECORD_SIZE];
        record[0] = game.cbh_flags;
        put_u32(&mut record[1..5], moves_offset);
        put_u32(&mut record[5..9], annotations_offset);
        put_u24(&mut record[9..12], 0); // White (entity ids are 0-based)
        put_u24(&mut record[12..15], 1); // Black
        put_u24(&mut record[15..18], 0); // Tournament
        put_u24(&mut record[18..21], 0); // Annotator
        put_u24(&mut record[21..24], 0); // Source
        put_u24(&mut record[24..27], game.date);
        record[27] = game.result;
        record[29] = game.round;
        record[30] = game.subround;
        record[31..33].copy_from_slice(&game.white_elo.to_be_bytes());
        record[33..35].copy_from_slice(&game.black_elo.to_be_bytes());
        record[35..37].copy_from_slice(&game.eco.to_be_bytes());
        record[45] = game.mainline_moves;
        self.games.push(record);
        id
    }

    fn finish(&self) -> PathBuf {
        fs::write(self.stem.with_extension("cbg"), &self.cbg).unwrap();
        fs::write(self.stem.with_extension("cba"), &self.cba).unwrap();
        let mut cbh = vec![0u8; RECORD_SIZE];
        cbh[0] = 0;
        cbh[2] = 0x2c;
        cbh[3..5].copy_from_slice(&(RECORD_SIZE as u16).to_be_bytes());
        let count = (self.games.len() as u32 + 1).to_be_bytes();
        cbh[6..10].copy_from_slice(&count);
        for game in &self.games {
            cbh.extend_from_slice(game);
        }
        let path = self.stem.with_extension("cbh");
        fs::write(&path, cbh).unwrap();
        path
    }
}

fn write_entities(stem: &Path) {
    let players = vec![
        player_record("Doe", "Jane"),
        player_record("Roe", "Richard"),
    ];
    write_entity(&stem.with_extension("cbp"), 50, &players);
    write_entity(
        &stem.with_extension("cbt"),
        74,
        &[tournament_record("Test Open", "Testville")],
    );
    write_entity(
        &stem.with_extension("cbc"),
        45,
        &[text_record("Analyst, Ann", 45)],
    );
    write_entity(
        &stem.with_extension("cbs"),
        25,
        &[text_record("Test Source", 25)],
    );
}

fn write_entity(path: &Path, data_size: usize, records: &[Vec<u8>]) {
    let mut bytes = vec![0u8; 28];
    bytes[8..12].copy_from_slice(&ENTITY_MAGIC.to_le_bytes());
    bytes[12..16].copy_from_slice(&(data_size as i32).to_le_bytes());
    bytes[24..28].copy_from_slice(&0i32.to_le_bytes());
    for data in records {
        assert_eq!(data.len(), data_size);
        bytes.extend_from_slice(&0i32.to_le_bytes());
        bytes.extend_from_slice(&0i32.to_le_bytes());
        bytes.push(0);
        bytes.extend_from_slice(data);
    }
    fs::write(path, bytes).unwrap();
}

#[test]
fn classic_player_records_have_no_title_field() {
    let dir = TempDir::new();
    let stem = dir.path().join("titles");
    write_entity(
        &stem.with_extension("cbp"),
        50,
        &[player_record("Doe", "GM Jane")],
    );
    let entities = Entities::open(&stem).unwrap();
    assert_eq!(
        entities.player_name(0),
        "Doe, GM Jane",
        "a title-looking given name stays part of the name; the 50-byte player record has no title field"
    );
}

fn player_record(last: &str, first: &str) -> Vec<u8> {
    let mut data = vec![0u8; 50];
    data[..last.len()].copy_from_slice(last.as_bytes());
    data[30..30 + first.len()].copy_from_slice(first.as_bytes());
    data
}

fn tournament_record(title: &str, place: &str) -> Vec<u8> {
    let mut data = vec![0u8; 74];
    data[..title.len()].copy_from_slice(title.as_bytes());
    data[40..40 + place.len()].copy_from_slice(place.as_bytes());
    data
}

fn text_record(text: &str, width: usize) -> Vec<u8> {
    let mut data = vec![0u8; width];
    data[..text.len()].copy_from_slice(text.as_bytes());
    data
}

fn annotation_item_count(items: &[u8]) -> u32 {
    let mut at = 0usize;
    let mut count = 0u32;
    while at < items.len() {
        count += 1;
        at += u16::from_be_bytes([items[at + 4], items[at + 5]]) as usize;
    }
    count
}

fn annotation_item(position: i32, kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut item = Vec::with_capacity(6 + payload.len());
    item.extend_from_slice(&u24(position as u32 & 0x00ff_ffff));
    item.push(kind);
    item.extend_from_slice(&((6 + payload.len()) as u16).to_be_bytes());
    item.extend_from_slice(payload);
    item
}

fn text_annotation(position: i32, before: bool, text: &str) -> Vec<u8> {
    let mut payload = vec![0, 0];
    payload.extend_from_slice(text.as_bytes());
    annotation_item(position, if before { 0x82 } else { 0x02 }, &payload)
}

fn nag_annotation(position: i32, on_move: u8, on_position: u8, prefix: u8) -> Vec<u8> {
    annotation_item(position, 0x03, &[on_move, on_position, prefix])
}

fn u24(value: u32) -> [u8; 3] {
    [(value >> 16) as u8, (value >> 8) as u8, value as u8]
}

fn put_u24(target: &mut [u8], value: u32) {
    target.copy_from_slice(&u24(value));
}

fn put_u32(target: &mut [u8], value: u32) {
    target.copy_from_slice(&value.to_be_bytes());
}

/// ChessBase square number: file-major, `a1 = 0`, `a2 = 1`.
fn cb_square(square: &str) -> u8 {
    let bytes = square.as_bytes();
    (bytes[0] - b'a') * 8 + (bytes[1] - b'1')
}

fn move_word(from: &str, to: &str, promotion: u16) -> u16 {
    u16::from(cb_square(from)) | (u16::from(cb_square(to)) << 6) | (promotion << 12)
}

/// A compact (mode 0/4/10) stream writer.
struct Compact {
    table: &'static [u8; 256],
    pre: bool,
    stream: Vec<u8>,
    ply: u32,
}

impl Compact {
    fn new(table: &'static [u8; 256], pre: bool) -> Self {
        Self {
            table,
            pre,
            stream: Vec::new(),
            ply: 0,
        }
    }

    fn encoded(&self, canonical: u8) -> u8 {
        let ply = self.ply as u8;
        for stored in 0u16..=255 {
            let index = stored as u8;
            let value = if self.pre {
                self.table[index.wrapping_sub(ply) as usize]
            } else {
                self.table[index as usize].wrapping_sub(ply)
            };
            if value == canonical {
                return index;
            }
        }
        panic!("canonical byte {canonical} is absent from the table");
    }

    fn code(&mut self, canonical: u8) {
        let byte = self.encoded(canonical);
        self.stream.push(byte);
    }

    fn byte(&mut self, canonical: u8) {
        self.code(canonical);
    }

    fn mv(&mut self, from: &str, to: &str) {
        self.two_byte(move_word(from, to, 0));
    }

    fn two_byte(&mut self, word: u16) {
        self.code(235);
        self.byte((word >> 8) as u8);
        self.byte(word as u8);
        self.ply += 1;
    }

    fn start_variation(&mut self) {
        self.code(254);
    }

    fn end_line(&mut self) {
        self.code(255);
    }

    fn finish(mut self) -> Vec<u8> {
        self.end_line();
        self.stream
    }
}

struct Simple {
    stream: Vec<u8>,
    ply: u32,
    table: &'static [u8; 256],
}

impl Simple {
    fn new() -> Self {
        Self {
            stream: Vec::new(),
            ply: 0,
            table: &tables::MODE_5,
        }
    }

    fn encoded(&self, canonical: u8) -> u8 {
        let ply = self.ply as u8;
        for stored in 0u16..=255 {
            let index = stored as u8;
            if self.table[index as usize].wrapping_sub(ply) == canonical {
                return index;
            }
        }
        panic!("canonical byte {canonical} is absent from mode 5");
    }

    fn word(&mut self, word: u16) {
        self.stream.push(self.encoded((word >> 8) as u8));
        self.stream.push(self.encoded(word as u8));
        self.ply += 1;
    }

    fn mv(&mut self, from: &str, to: &str, flags: u16) {
        self.word(move_word(from, to, 0) | flags);
    }

    fn finish(mut self, last: (&str, &str)) -> Vec<u8> {
        self.mv(last.0, last.1, 0x4000);
        self.stream
    }
}

fn standard_start(mode: u8) -> Vec<u8> {
    let board = Chess::default();
    let mut bits = BitWriter::default();
    for cb in 0..64u8 {
        // ChessBase numbers squares by file first: a1, a2, ... a8, b1.
        let file = cb / 8;
        let rank = cb % 8;
        let square = Square::from_coords(File::new(u32::from(file)), Rank::new(u32::from(rank)));
        match board.board().piece_at(square) {
            Some(piece) => {
                bits.push(1, 1);
                bits.push(u8::from(piece.color == Color::Black), 1);
                let code = match piece.role {
                    Role::King => 1,
                    Role::Queen => 2,
                    Role::Knight => 3,
                    Role::Bishop => 4,
                    Role::Rook => 5,
                    Role::Pawn => 6,
                };
                bits.push(code, 3);
            }
            None => bits.push(0, 1),
        }
    }
    let mut start = vec![0u8, 0, 0x0f, 1];
    start.extend_from_slice(&bits.finish());
    if mode == 10 {
        start.extend_from_slice(&[
            0x20, 0x27, // white king e1, black king e8
            0x07, 0x00, // white O-O rook h1, white O-O-O rook a1
            0x3f, 0x38, // black O-O rook h8, black O-O-O rook a8
            0x02, 0x06, // Chess960 position 518
        ]);
    }
    start
}

#[derive(Default)]
struct BitWriter {
    bits: Vec<bool>,
}

impl BitWriter {
    fn push(&mut self, value: u8, width: usize) {
        for bit in (0..width).rev() {
            self.bits.push((value >> bit) & 1 != 0);
        }
    }

    fn finish(self) -> [u8; 24] {
        let mut out = [0u8; 24];
        for (index, bit) in self.bits.into_iter().enumerate() {
            if bit {
                out[index / 8] |= 0x80 >> (index % 8);
            }
        }
        out
    }
}

fn date(year: u32, month: u32, day: u32) -> u32 {
    (year << 9) | (month << 5) | day
}

#[test]
fn headers_entities_tree_variations_comments_and_nags() {
    let mut db = Database::new();
    let mut moves = Compact::new(&tables::MODE_0, true);
    moves.mv("e2", "e4");
    moves.mv("e7", "e5");
    moves.mv("g1", "f3");
    moves.mv("b8", "c6");
    moves.start_variation();
    moves.mv("f1", "b5");
    moves.mv("a7", "a6");
    moves.mv("b5", "a4");
    moves.mv("g8", "f6");
    moves.end_line();
    moves.mv("f1", "c4");
    moves.mv("f8", "c5");
    let body = moves.finish();

    let mut annotation = Vec::new();
    annotation.extend(text_annotation(-1, false, "Opening survey."));
    annotation.extend(text_annotation(0, false, "King pawn."));
    annotation.extend(annotation_item(0, 0x07, &[0, 32, 14, 0])); // 0:32:14
    annotation.extend(annotation_item(0, 0x21, &[0xc8, 0x01, 0, 0, 0, 0])); // 456
    annotation.extend(annotation_item(1, 0x22, &[0, 0, 0, 32])); // medal
    annotation.extend(text_annotation(2, true, "Develop."));
    annotation.extend(nag_annotation(3, 5, 0, 1));
    annotation.extend(annotation_item(4, 0x04, &[2, 37])); // green e5
    annotation.extend(annotation_item(4, 0x05, &[2, 64, 63])); // green h8-h7
    annotation.extend(text_annotation(4, false, "Spanish."));
    annotation.extend(text_annotation(8, true, "Italian."));
    annotation.extend(text_annotation(100, false, "Past the last move."));
    db.add(GameSpec {
        cbh_flags: 0x01,
        cbg_flags: 0x00,
        body,
        annotation: Some(annotation),
        date: date(2024, 1, 2),
        result: 1,
        round: 3,
        subround: 2,
        white_elo: 2500,
        black_elo: 2400,
        eco: 221 * 128,
        mainline_moves: 8,
    });
    let path = db.finish();

    let library = CbhLibrary::open(&path).unwrap();
    assert_eq!(library.len(), 1);
    assert_eq!(
        library.filter("testville"),
        vec![0],
        "{:?}",
        library.games()[0].headers()
    );
    assert!(library.source_meta().unwrap().len > 0);
    let indexed = library.get(0).unwrap();
    assert_eq!(indexed.event(), "Test Open");
    assert_eq!(indexed.header("Site"), Some("Testville"));
    assert_eq!(indexed.date(), "2024.01.02");
    assert_eq!(indexed.header("Round"), Some("3.2"));
    assert_eq!(indexed.white(), "Doe, Jane");
    assert_eq!(indexed.black(), "Roe, Richard");
    assert_eq!(indexed.result(), "1/2-1/2");
    assert_eq!(indexed.header("ECO"), Some("C20"));
    assert_eq!(indexed.header("WhiteElo"), Some("2500"));
    assert_eq!(indexed.cbh_record_id(), Some(1));
    assert!(indexed.cbh_annotations_offset().is_some());

    let directory = crate::players::PlayerDirectory::build(&library);
    let doe_key = crate::players::player_key("Doe, Jane").expect("Doe has a player key");
    let doe = directory.find_key(&doe_key).expect("Doe is indexed");
    assert_eq!(directory.get(doe).unwrap().games(), 1);
    assert_eq!(directory.get(doe).unwrap().white.games, 1);

    let game = library.load(0).unwrap();
    assert!(game.errors.is_empty(), "{:?}", game.errors);
    assert_eq!(game.header("Annotator"), Some("Analyst, Ann"));
    assert_eq!(game.header("Source"), Some("Test Source"));

    let mainline: Vec<&str> = game
        .mainline()
        .iter()
        .map(|&id| game.nodes[id].mv.as_ref().unwrap().san.as_str())
        .collect();
    assert_eq!(
        mainline,
        ["e4", "e5", "Nf3", "Nc6", "Bb5", "a6", "Ba4", "Nf6"]
    );
    let nc6 = game.nodes[0].children[0];
    let nc6 = game.nodes[nc6].children[0];
    let nc6 = game.nodes[nc6].children[0];
    let nc6 = game.nodes[nc6].children[0];
    assert_eq!(game.nodes[nc6].mv.as_ref().unwrap().san, "Nc6");
    assert_eq!(game.nodes[nc6].children.len(), 2);
    let variation = game.nodes[nc6].children[1];
    assert_eq!(game.nodes[variation].mv.as_ref().unwrap().san, "Bc4");
    assert_eq!(
        game.nodes[game.nodes[variation].children[0]]
            .mv
            .as_ref()
            .unwrap()
            .san,
        "Bc5"
    );

    assert_eq!(game.nodes[0].comment, "Opening survey.");
    assert_eq!(
        game.nodes[1].comment,
        "[%eval 456,0] [%emt 0:32:14] King pawn."
    );
    assert_eq!(game.nodes[2].comment, "[%mdl 32]");
    assert_eq!(game.nodes[3].comment, "Develop.");
    assert_eq!(game.nodes[4].nags, [1, 5]);
    assert_eq!(game.nodes[5].comment, "Spanish.");
    assert!(
        game.nodes[5]
            .shapes
            .contains(&crate::domain::Shape::Square {
                color: crate::domain::ShapeColor::Green,
                square: Square::E5,
            })
    );
    assert!(game.nodes[5].shapes.contains(&crate::domain::Shape::Arrow {
        color: crate::domain::ShapeColor::Green,
        from: Square::H8,
        to: Square::H7,
    }));
    assert_eq!(game.nodes[9].comment, "Italian.");
    // Position 100 is past the last stored ply; ChessBase's export writes it
    // after the main line's last move.
    assert_eq!(game.nodes[8].comment, "Past the last move.");
}

#[test]
fn mode_four_compact_encoding() {
    let mut db = Database::new();
    let mut moves = Compact::new(&tables::MODE_4, false);
    moves.mv("d2", "d4");
    moves.mv("g8", "f6");
    moves.mv("c2", "c4");
    moves.mv("e7", "e6");
    db.add(GameSpec {
        cbh_flags: 0x01,
        cbg_flags: 0x04,
        body: moves.finish(),
        date: date(2019, 6, 1),
        result: 2,
        round: 2,
        subround: 0,
        white_elo: 2100,
        black_elo: 2110,
        eco: 0,
        mainline_moves: 4,
        ..GameSpec::default()
    });
    let path = db.finish();
    let library = CbhLibrary::open(&path).unwrap();
    let game = library.load(0).unwrap();
    assert!(game.errors.is_empty(), "{:?}", game.errors);
    let mainline: Vec<&str> = game
        .mainline()
        .iter()
        .map(|&id| game.nodes[id].mv.as_ref().unwrap().san.as_str())
        .collect();
    assert_eq!(mainline, ["d4", "Nf6", "c4", "e6"]);
}

#[test]
fn mode_five_simple_encoding_and_variation() {
    let mut db = Database::new();
    let mut moves = Simple::new();
    moves.mv("e2", "e4", 0x8000);
    moves.mv("e7", "e5", 0x4000);
    moves.mv("d2", "d4", 0);
    let body = moves.finish(("d7", "d5"));
    db.add(GameSpec {
        cbh_flags: 0x01,
        cbg_flags: 0x05,
        body,
        date: date(2020, 0, 0),
        result: 2,
        round: 0,
        subround: 0,
        white_elo: 0,
        black_elo: 0,
        eco: 0,
        mainline_moves: 2,
        ..GameSpec::default()
    });
    let path = db.finish();
    let library = CbhLibrary::open(&path).unwrap();
    let game = library.load(0).unwrap();
    assert!(game.errors.is_empty(), "{:?}", game.errors);
    let mainline: Vec<&str> = game
        .mainline()
        .iter()
        .map(|&id| game.nodes[id].mv.as_ref().unwrap().san.as_str())
        .collect();
    assert_eq!(mainline, ["e4", "e5"]);
    let root = &game.nodes[0];
    assert_eq!(root.children.len(), 2);
    assert_eq!(game.nodes[root.children[1]].mv.as_ref().unwrap().san, "d4");
    assert_eq!(game.nodes[0].comment, "");
    assert_eq!(library.get(0).unwrap().date(), "2020.??.??");
    assert_eq!(library.get(0).unwrap().result(), "1-0");
}

#[test]
fn mode_ten_explicit_start_and_castling() {
    let mut db = Database::new();
    let mut moves = Compact::new(&tables::MODE_10, false);
    moves.mv("e2", "e4");
    moves.mv("e7", "e5");
    moves.mv("g1", "f3");
    moves.mv("b8", "c6");
    moves.mv("f1", "c4");
    moves.mv("f8", "c5");
    moves.code(9); // O-O, one-byte compact code
    moves.ply += 1;
    let mut body = standard_start(10);
    body.extend(moves.finish());
    db.add(GameSpec {
        cbh_flags: 0x01,
        cbg_flags: 0x4a,
        body,
        date: date(2011, 10, 30),
        result: 1,
        round: 1,
        subround: 0,
        white_elo: 2400,
        black_elo: 2410,
        eco: 0,
        mainline_moves: 7,
        ..GameSpec::default()
    });
    let path = db.finish();
    let library = CbhLibrary::open(&path).unwrap();
    let game = library.load(0).unwrap();
    assert!(game.errors.is_empty(), "{:?}", game.errors);
    assert_eq!(game.header("SetUp"), Some("1"));
    assert_eq!(game.header("Variant"), Some("Chess960"));
    assert_eq!(
        game.header("FEN"),
        Some("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1")
    );
    let mainline: Vec<&str> = game
        .mainline()
        .iter()
        .map(|&id| game.nodes[id].mv.as_ref().unwrap().san.as_str())
        .collect();
    assert_eq!(mainline.last(), Some(&"O-O"));
    assert!(
        !game
            .final_position()
            .castles()
            .has(Color::White, CastlingSide::KingSide)
    );
}

#[test]
fn deleted_unsupported_and_truncated_games_are_visible() {
    let mut db = Database::new();
    let mut deleted = Compact::new(&tables::MODE_0, true);
    deleted.mv("e2", "e4");
    db.add(GameSpec {
        cbh_flags: 0x81,
        cbg_flags: 0x00,
        body: deleted.finish(),
        date: date(2020, 1, 1),
        result: 1,
        round: 1,
        subround: 0,
        white_elo: 0,
        black_elo: 0,
        eco: 0,
        mainline_moves: 1,
        ..GameSpec::default()
    });

    let mut unsupported = standard_start(10);
    unsupported.extend_from_slice(&[0xff]);
    db.add(GameSpec {
        cbh_flags: 0x01,
        cbg_flags: 0x4b,
        body: unsupported,
        date: date(2020, 1, 1),
        result: 1,
        round: 1,
        subround: 0,
        white_elo: 0,
        black_elo: 0,
        eco: 0,
        mainline_moves: 0,
        ..GameSpec::default()
    });

    let mut truncated = Compact::new(&tables::MODE_0, true);
    truncated.mv("e2", "e4");
    let body = truncated.stream;
    db.add(GameSpec {
        cbh_flags: 0x01,
        cbg_flags: 0x00,
        body,
        date: date(2020, 1, 1),
        result: 1,
        round: 1,
        subround: 0,
        white_elo: 0,
        black_elo: 0,
        eco: 0,
        mainline_moves: 1,
        ..GameSpec::default()
    });
    let path = db.finish();

    let library = CbhLibrary::open(&path).unwrap();
    assert_eq!(library.len(), 3);
    assert!(library.get(0).unwrap().cbh_deleted());
    let deleted = library.load(0).unwrap();
    assert!(deleted.errors.iter().any(|error| error.contains("deleted")));
    assert_eq!(deleted.nodes.len(), 2);

    let unsupported = library.load(1).unwrap();
    assert!(
        unsupported
            .errors
            .iter()
            .any(|error| error.contains("mode 11"))
    );
    assert_eq!(unsupported.nodes.len(), 1);

    let truncated = library.load(2).unwrap();
    assert!(
        truncated
            .errors
            .iter()
            .any(|error| error.contains("not terminated")),
        "{:?}",
        truncated.errors
    );
    assert_eq!(truncated.nodes.len(), 2);
}

#[test]
fn an_empty_move_record_gets_ply_count_zero() {
    let mut db = Database::new();
    db.add(GameSpec {
        cbh_flags: 0x01,
        cbg_flags: 0x00,
        body: Vec::new(),
        date: date(2013, 1, 7),
        result: 1,
        round: 0,
        subround: 0,
        white_elo: 2436,
        black_elo: 2438,
        eco: 0,
        mainline_moves: 0,
        ..GameSpec::default()
    });
    let path = db.finish();
    let library = CbhLibrary::open(&path).unwrap();
    let game = library.load(0).unwrap();
    assert!(game.errors.is_empty(), "{:?}", game.errors);
    assert_eq!(game.nodes.len(), 1);
    assert_eq!(game.header("PlyCount"), Some("0"));
}

#[test]
fn position_index_works_through_the_library_interface() {
    let mut db = Database::new();
    let mut moves = Compact::new(&tables::MODE_0, true);
    moves.mv("e2", "e4");
    moves.mv("e7", "e5");
    db.add(GameSpec {
        cbh_flags: 0x01,
        cbg_flags: 0x00,
        body: moves.finish(),
        date: date(2024, 1, 2),
        result: 2,
        round: 1,
        subround: 0,
        white_elo: 2500,
        black_elo: 2400,
        eco: 221 * 128,
        mainline_moves: 2,
        ..GameSpec::default()
    });
    let path = db.finish();
    let library = CbhLibrary::open(&path).unwrap();
    let index_path = db._dir.path().join("cbh.brettpos");
    crate::position::PositionIndex::build_library_to(
        &library,
        &index_path,
        crate::position::BuildOptions::default(),
        |_| {},
    )
    .unwrap();
    let index = crate::position::PositionIndex::open(&index_path).unwrap();
    assert_eq!(index.game_count(), 1);

    let game = library.load(0).unwrap();
    let start = crate::position::position_key(&game.nodes[0].pos);
    let moves = index.moves(start);
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0].games, 1);
    assert_eq!(moves[0].white, 1);

    let after_e4 = crate::position::position_key(&game.nodes[1].pos);
    let moves = index.moves(after_e4);
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0].games, 1);
    assert_eq!(moves[0].white, 1);
}

#[test]
fn wide_offsets_are_checked_against_the_short_header() {
    let dir = TempDir::new();
    let stem = dir.path().join("wide");
    let mut bytes = vec![0u8; 32];
    bytes[4..8].copy_from_slice(&0x26i32.to_le_bytes());
    bytes[8..12].copy_from_slice(&1i32.to_le_bytes());
    let mut record = [0u8; 0x26];
    let annotations = 0x1_0000_9abcu64;
    let moves = 0x1_2345_6789u64;
    record[0x0c..0x14].copy_from_slice(&(annotations as i64).to_be_bytes());
    record[0x1e..0x26].copy_from_slice(&(moves as i64).to_be_bytes());
    bytes.extend_from_slice(&record);
    fs::write(stem.with_extension("cbj"), bytes).unwrap();

    let wide = Wide::open_if_needed(&stem, 1u64 << 32, 0)
        .unwrap()
        .expect("a wide index is required over 4 GiB");
    let short = (0x2345_6789u32, 0x9abcu32);
    assert_eq!(wide.offsets(1, short).unwrap(), (moves, annotations));
    assert_eq!(
        wide.offsets(0, short).unwrap(),
        (0x2345_6789, 0x9abc),
        "id zero falls back to the short header"
    );
    assert!(wide.offsets(1, (0x2345_6788, 0x9abc)).is_err());
}

#[test]
fn non_ascii_entity_text_is_preserved() {
    assert_eq!(decode_text(b"Ji\xed\0"), "Jií");
    assert_eq!(decode_text(b"Ne\xdf"), "Neß");
    assert_eq!(decode_text("Müller".as_bytes()), "Müller");
}

#[test]
fn classic_guiding_text_header_parses_titles_and_html_blocks() {
    // total_len | [3,0] | title_count le_u16 | titles | 0x01 | block_count le_u32 | blocks
    let title = "Ostend 1907";
    let html = "<p>GOETZ drew with JANOWSKI.</p>";
    let mut body = Vec::new();
    body.extend_from_slice(&[3, 0]);
    body.extend_from_slice(&1u16.to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes()); // lang
    body.extend_from_slice(&(title.len() as u16).to_le_bytes());
    body.extend_from_slice(title.as_bytes());
    body.push(0x01);
    body.extend_from_slice(&1u32.to_le_bytes());
    body.push(0); // lang
    body.extend_from_slice(&(html.len() as u32).to_le_bytes());
    body.extend_from_slice(html.as_bytes());
    let total_len = body.len() as u32;
    let mut record = Vec::new();
    record.extend_from_slice(&total_len.to_be_bytes());
    record.extend_from_slice(&body);
    let (titles, blocks) = super::parse_classic_guiding_record(&record, 0).unwrap();
    assert_eq!(titles, vec![title.to_string()]);
    assert_eq!(blocks, vec![html.to_string()]);
    let doc = crate::guiding::parse_guiding_html(&blocks[0]);
    assert!(
        doc.blocks
            .iter()
            .any(|block| matches!(block, crate::guiding::GuidingBlock::Paragraph(_)))
    );
}

#[test]
fn rejects_a_header_file_that_is_not_classic_cbh() {
    let dir = TempDir::new();
    let stem = dir.path().join("bad");
    fs::write(stem.with_extension("cbg"), vec![0u8; 10]).unwrap();
    fs::write(stem.with_extension("cbh"), vec![0u8; 45]).unwrap();
    let error = CbhLibrary::open(stem.with_extension("cbh"))
        .err()
        .expect("a 45-byte header file must be rejected");
    assert!(format!("{error:#}").contains("46 plus whole records"));

    let mut wrong_size = vec![0u8; RECORD_SIZE * 2];
    wrong_size[3..5].copy_from_slice(&45u16.to_be_bytes());
    fs::write(stem.with_extension("cbh"), wrong_size).unwrap();
    let error = CbhLibrary::open(stem.with_extension("cbh"))
        .err()
        .expect("a header file stating 45-byte records must be rejected");
    assert!(format!("{error:#}").contains("states a 45-byte game record"));
}

#[test]
fn chess960_start_keeps_outer_rook_castling_rights() {
    let mut board = crate::chess::Board::empty();
    for (square, role) in [
        (crate::chess::Square::A1, Role::Rook),
        (crate::chess::Square::B1, Role::Knight),
        (crate::chess::Square::C1, Role::Bishop),
        (crate::chess::Square::D1, Role::Queen),
        (crate::chess::Square::E1, Role::King),
        (crate::chess::Square::F1, Role::Bishop),
        (crate::chess::Square::G1, Role::Knight),
        (crate::chess::Square::H1, Role::Rook),
        (crate::chess::Square::E8, Role::King),
    ] {
        let color = if square.rank() == crate::chess::Rank::Eighth {
            crate::chess::Color::Black
        } else {
            crate::chess::Color::White
        };
        board.set_piece_at(square, crate::chess::Piece::new(color, role));
    }
    let mut rights = crate::chess::Bitboard::EMPTY;
    rights |= crate::chess::Bitboard::from_square(crate::chess::Square::A1);
    rights |= crate::chess::Bitboard::from_square(crate::chess::Square::H1);
    let setup = crate::chess::Setup {
        board,
        turn: crate::chess::Color::White,
        castling_rights: rights,
        ep_square: None,
        halfmoves: 0,
        fullmoves: std::num::NonZeroU32::new(1).unwrap(),
    };
    let pos = Chess::from_setup(setup, crate::chess::CastlingMode::Chess960).unwrap();
    assert!(pos.castles().has(
        crate::chess::Color::White,
        crate::chess::CastlingSide::KingSide
    ));
    assert!(pos.castles().has(
        crate::chess::Color::White,
        crate::chess::CastlingSide::QueenSide
    ));
}

fn entity_file_with(data_size: usize, rows: &[(i32, i32, Vec<u8>)], root: i32) -> EntityFile {
    let dir = TempDir::new();
    let path = dir.path().join("synthetic.cbp");
    let mut bytes = vec![0u8; 28];
    bytes[8..12].copy_from_slice(&ENTITY_MAGIC.to_le_bytes());
    bytes[12..16].copy_from_slice(&(data_size as i32).to_le_bytes());
    bytes[16..20].copy_from_slice(&root.to_le_bytes());
    bytes[24..28].copy_from_slice(&0i32.to_le_bytes());
    for (left, right, data) in rows {
        assert_eq!(data.len(), data_size);
        bytes.extend_from_slice(&left.to_le_bytes());
        bytes.extend_from_slice(&right.to_le_bytes());
        bytes.push(0);
        bytes.extend_from_slice(data);
    }
    fs::write(&path, &bytes).unwrap();
    EntityFile::open(&path, data_size as i32)
        .unwrap()
        .expect("entity file opens")
}

fn player_data(last: &str, first: &str, game_count: u32, first_game: u32) -> Vec<u8> {
    let mut data = vec![0u8; 50];
    data[..last.len()].copy_from_slice(last.as_bytes());
    data[30..30 + first.len()].copy_from_slice(first.as_bytes());
    data[42..46].copy_from_slice(&game_count.to_le_bytes());
    data[46..50].copy_from_slice(&first_game.to_le_bytes());
    data
}

#[test]
fn entity_game_counts_and_first_game_come_from_the_record_tail() {
    let file = entity_file_with(
        50,
        &[
            (-1, -1, player_data("Doe", "Jane", 7, 3)),
            (-1, -1, player_data("Roe", "Richard", 0, 0)),
        ],
        0,
    );
    assert_eq!(file.game_count(0), 7);
    assert_eq!(file.first_game_id(0), 3);
    assert_eq!(file.game_count(1), 0);
    assert_eq!(file.first_game_id(1), 0);
    assert_eq!(file.game_count(99), 0, "unknown ids read as zero");
}

#[test]
fn avl_search_walks_left_and_right_from_the_root() {
    // BST over the last-name field: root "M" (id 1), left "A" (id 0),
    // right "Z" (id 2).
    let file = entity_file_with(
        50,
        &[
            (-1, -1, player_data("A", "", 1, 10)),
            (0, 2, player_data("M", "", 2, 20)),
            (-1, -1, player_data("Z", "", 3, 30)),
        ],
        1,
    );
    assert_eq!(file.root(), 1);
    assert_eq!(file.left(1), Some(0));
    assert_eq!(file.right(1), Some(2));
    let by_last = |want: &str| {
        file.avl_search(|data| {
            let name = decode_text(data.get(0..30).unwrap_or(&[]))
                .trim_end_matches('\0')
                .to_lowercase();
            name.cmp(&want.to_string())
        })
    };
    assert_eq!(by_last("m"), Some(1));
    assert_eq!(by_last("a"), Some(0));
    assert_eq!(by_last("z"), Some(2));
    assert_eq!(by_last("q"), None, "a missing key misses");
}

#[test]
fn player_entity_lookup_uses_avl_with_linear_fallback() {
    let mut db = Database::new();
    db.add(GameSpec {
        cbh_flags: 1,
        ..GameSpec::default()
    });
    let path = db.finish();
    let library = CbhLibrary::open(&path).unwrap();
    // The fixture writes flat left/right links, so "Roe, Richard" can only
    // resolve through the linear fallback while "Doe, Jane" hits the root.
    assert_eq!(
        library.player_entity_stats("Doe, Jane").map(|stat| stat.0),
        Some(0)
    );
    assert_eq!(
        library
            .player_entity_stats("Roe, Richard")
            .map(|stat| stat.0),
        Some(1)
    );
    assert_eq!(library.player_entity_stats("Nobody, N"), None);
    assert_eq!(
        library.event_entity_stats("Test Open").map(|stat| stat.0),
        Some(0)
    );
}
