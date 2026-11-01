//! Local 2CBH validation against databases that are not redistributed.
//!
//! Each test is a no-op unless its environment variable points at the local
//! database.  This keeps the public suite self-contained while making the
//! real-file checks reproducible on the development machine.

use std::collections::HashSet;
use std::path::PathBuf;

use brett::domain::{self, Game};
use brett::twocbh::{TwoCbhFile, TwoCbhLibrary};

fn env_path(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(name)?);
    path.exists().then_some(path)
}

fn uci(game: &Game) -> Vec<String> {
    game.mainline()
        .iter()
        .map(|&id| game.nodes[id].mv.as_ref().unwrap().uci.clone())
        .collect()
}

fn tree(game: &Game, node: usize) -> String {
    let mut lines: Vec<String> = game.nodes[node]
        .children
        .iter()
        .map(|&child| {
            let uci = &game.nodes[child].mv.as_ref().unwrap().uci;
            format!("({uci} {})", tree(game, child))
        })
        .collect();
    lines.sort();
    lines.join("")
}

/// The full move tree in stored child order, so variation ordering is compared.
fn tree_ordered(game: &Game, node: usize) -> String {
    game.nodes[node]
        .children
        .iter()
        .map(|&child| {
            let uci = &game.nodes[child].mv.as_ref().unwrap().uci;
            format!("({uci} {})", tree_ordered(game, child))
        })
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn writer_round_trips_update01_sample() {
    let Some(cbh) = env_path("BRETT_2CBH_UPDATE01") else {
        return;
    };
    let source = TwoCbhLibrary::open(&cbh).unwrap();
    // Update01 has no FEN or Variant tags, but filter out any unsupported
    // start so this stays a round-trip test if the local sample changes.
    let take = 50;
    let games: Vec<Game> = (0..source.len())
        .filter_map(|index| source.load(index).ok())
        .filter(|game| {
            game.header("FEN").is_none()
                && game.header("Variant").is_none()
                && ["White", "Black", "Event", "Site", "Date", "Round", "Result"]
                    .iter()
                    .all(|key| game.header(key).is_some())
                && game
                    .errors
                    .iter()
                    .all(|error| error.contains("marked deleted"))
        })
        .take(take)
        .collect();
    assert_eq!(games.len(), take, "not enough writable games in the sample");
    // The ordered-tree comparison must not be vacuous: the sample has to
    // contain at least one branching variation for sibling order to matter.
    assert!(
        games
            .iter()
            .any(|game| game.nodes.iter().any(|node| node.children.len() > 1)),
        "sample has no branching variation to order-check"
    );

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("brett-2cbh-roundtrip-{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("roundtrip.2cbh");

    let report = brett::twocbh::write_database_with_options(
        &target,
        brett::twocbh::TwoCbhWriteOptions {
            allow_annotation_loss: true,
        },
        games.iter().cloned().map(Ok),
    )
    .unwrap();
    assert_eq!(report.games, take);

    let written = TwoCbhLibrary::open(&target).unwrap();
    assert_eq!(written.len(), take);
    for (index, want) in games.iter().enumerate() {
        let got = written.load(index).unwrap();
        assert!(
            got.errors
                .iter()
                .all(|error| error.contains("marked deleted")),
            "game {index}: {:?}",
            got.errors
        );
        assert_eq!(uci(&got), uci(want), "game {index} mainline");
        assert_eq!(tree(&got, 0), tree(want, 0), "game {index} tree");
        assert_eq!(
            tree_ordered(&got, 0),
            tree_ordered(want, 0),
            "game {index} ordered tree"
        );
        for key in [
            "White",
            "Black",
            "Event",
            "Site",
            "Date",
            "Round",
            "Result",
            "WhiteElo",
            "BlackElo",
            "WhiteFideId",
            "BlackFideId",
            "GameId",
            "Beauty",
            "SourceTitle",
            "Source",
        ] {
            assert_eq!(
                got.header(key).unwrap_or(""),
                want.header(key).unwrap_or(""),
                "game {index} {key}"
            );
        }
    }

    for companion in brett::twocbh::existing_companion_files(&target) {
        let _ = std::fs::remove_file(companion);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn update01_matches_the_pgn_mainlines() {
    let Some(cbh) = env_path("BRETT_2CBH_UPDATE01") else {
        return;
    };
    let library = TwoCbhLibrary::open(&cbh).unwrap();
    let pgn = cbh.with_file_name("New Database.pgn");
    let games: Vec<Game> = domain::parse_pgn_file(&pgn)
        .unwrap()
        .into_iter()
        .filter(|game| game.mainline_len() > 0)
        .collect();
    assert_eq!(library.len(), games.len());

    let mut move_mismatches = 0;
    let mut header_mismatches = 0;
    for (index, want) in games.iter().enumerate() {
        let got = library.load(index).unwrap();
        assert!(
            got.errors.is_empty(),
            "game {index} ({} – {}) had errors: {:?}",
            got.white(),
            got.black(),
            got.errors
        );
        if uci(&got) != uci(want) {
            move_mismatches += 1;
            if move_mismatches <= 3 {
                let got = uci(&got);
                let want = uci(want);
                let first = got
                    .iter()
                    .zip(&want)
                    .position(|(a, b)| a != b)
                    .unwrap_or(got.len().min(want.len()));
                eprintln!(
                    "move mismatch game {index}: first ply {} got {:?} want {:?}",
                    first + 1,
                    got.get(first),
                    want.get(first)
                );
            }
        }
        for key in [
            "White", "Black", "Event", "Site", "Date", "Round", "Result", "WhiteElo", "BlackElo",
        ] {
            let got_value = got.header(key).unwrap_or("");
            let want_value = want.header(key).unwrap_or("");
            if got_value != want_value {
                header_mismatches += 1;
                if header_mismatches <= 12 {
                    eprintln!(
                        "header mismatch game {index} {key}: got {got_value:?} want {want_value:?}"
                    );
                }
            }
        }
    }
    eprintln!(
        "Update01: {} games, {move_mismatches} move mismatches, {header_mismatches} header mismatches",
        games.len()
    );
    assert_eq!(move_mismatches, 0);
    assert_eq!(header_mismatches, 0);
}

#[test]
fn chess960_database_matches_the_pgn_mainlines() {
    let Some(cbh) = env_path("BRETT_2CBH_9601") else {
        return;
    };
    let library = TwoCbhLibrary::open(&cbh).unwrap();
    // 9601.2cbh is the GMT-M event.  GMT-GC is a separate tournament with
    // the same seven start positions, so it is not the oracle for this file.
    let pgn = cbh.with_file_name("GMT-M_accentus_chess960.pgn");
    let games = domain::parse_pgn_file(&pgn).unwrap();
    assert_eq!(library.len(), games.len());

    let mut starts = HashSet::new();
    for (index, want) in games.iter().enumerate() {
        let got = library.load(index).unwrap();
        assert!(
            got.errors.is_empty(),
            "960 game {index} had errors: {:?}",
            got.errors
        );
        assert_eq!(got.header("Variant"), Some("Chess960"));
        assert_eq!(uci(&got), uci(want), "960 game {index}");
        let fen = got.header("FEN").unwrap().to_string();
        starts.insert(fen.clone());

        // Castling spelling may be KQkq or file letters; the position is the
        // same.  Compare the board, side to move, en-passant field and clocks.
        let mut got_fields = fen.split(' ');
        let mut want_fields = want.header("FEN").unwrap().split(' ');
        assert_eq!(
            got_fields.next(),
            want_fields.next(),
            "960 game {index} board"
        );
        assert_eq!(
            got_fields.next(),
            want_fields.next(),
            "960 game {index} side"
        );
        got_fields.next();
        want_fields.next();
        assert_eq!(
            got_fields.next(),
            want_fields.next(),
            "960 game {index} en passant"
        );
        assert_eq!(
            got_fields.next(),
            want_fields.next(),
            "960 game {index} halfmove"
        );
        assert_eq!(
            got_fields.next(),
            want_fields.next(),
            "960 game {index} fullmove"
        );
    }
    eprintln!(
        "9601: {} games, {} distinct starts",
        games.len(),
        starts.len()
    );
    assert_eq!(starts.len(), 7);
}

#[test]
fn nested_variation_game_matches_the_pgn_tree() {
    let Some(cbh) = env_path("BRETT_2CBH_NESTED") else {
        return;
    };
    let library = TwoCbhLibrary::open(&cbh).unwrap();
    let pgn = cbh.with_file_name("New Database.pgn");
    let games: Vec<Game> = domain::parse_pgn_file(&pgn)
        .unwrap()
        .into_iter()
        .filter(|game| game.mainline_len() > 0)
        .collect();
    let want = games.last().unwrap();
    let got = library.load(library.len() - 1).unwrap();
    assert!(got.errors.is_empty(), "{:?}", got.errors);
    assert_eq!(got.header("GameId"), Some("2371187592116069"));
    assert_eq!(uci(&got), uci(want));
    assert_eq!(tree(&got, 0), tree(want, 0));
}

#[test]
fn mega_record_three_loads_without_indexing_the_database() {
    let Some(cbh) = env_path("BRETT_2CBH_MEGA") else {
        return;
    };
    let file = TwoCbhFile::open(&cbh).unwrap();
    assert_eq!(file.slot_count(), 11_743_084);
    let game = file.load_game(3).unwrap();
    eprintln!(
        "Mega gid 3: {} – {}, {}, {} plies",
        game.white(),
        game.black(),
        game.date(),
        game.mainline_len()
    );
    assert!(game.errors.is_empty(), "{:?}", game.errors);
    assert_eq!(game.header("GameId"), Some("2235863507857418"));
    // The record stores the year only; month and day are zero in the file.
    assert_eq!(game.date(), "1475.??.??");
    assert!(game.mainline_len() > 20);
}
