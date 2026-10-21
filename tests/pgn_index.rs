use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use brett::domain;
use brett::library::PgnLibrary;

const THREE_GAMES: &str = r#"[Event "Alpha"]
[Site "Test"]
[Date "2024.01.01"]
[White "Alice"]
[Black "Bob"]
[Result "1-0"]

{ Comment with a fake tag [Event "Not a game"] and a blank line.

Still the same comment. }
1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 1-0

[Event "Beta"]
[Site "Test"]
[Date "2024.02.02"]
[White "Carol"]
[Black "Dave"]
[Result "1/2-1/2"]

1. d4 d5 2. c4 e6 1/2-1/2

[Event "Gamma"]
[Site "Test"]
[Date "2024.03.03"]
[White "Eve"]
[Black "Frank"]
[Result "0-1"]

1. c4 e5 2. Nc3 Nf6 0-1
"#;

fn write_temp_pgn(name: &str, contents: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "brett-test-{name}-{}-{unique}.pgn",
        std::process::id()
    ));
    std::fs::write(&path, contents).unwrap();
    path
}

fn mainline_sans(game: &domain::Game) -> Vec<String> {
    game.mainline()
        .into_iter()
        .map(|node| game.nodes[node].san().unwrap_or("--").to_string())
        .collect()
}

#[test]
fn builds_offsets_and_lazily_loads_each_game() {
    let path = write_temp_pgn("three-games", THREE_GAMES);
    let library = PgnLibrary::build(&path).unwrap();

    assert_eq!(library.len(), 3);
    assert_eq!(library.file_len(), std::fs::metadata(&path).unwrap().len());

    let mut previous_end = 0;
    for (index, entry) in library.games().iter().enumerate() {
        let (start, end) = entry.byte_range();
        assert!(start < end, "game {index} must have a non-empty range");
        assert!(
            start >= previous_end,
            "game {index} overlaps its predecessor"
        );
        assert!(end <= library.file_len());
        previous_end = end;
    }

    let alpha = library.load(0).unwrap();
    assert_eq!(alpha.white(), "Alice");
    assert_eq!(alpha.black(), "Bob");
    assert_eq!(alpha.result(), "1-0");
    assert_eq!(alpha.mainline_len(), 6);
    assert_eq!(
        mainline_sans(&alpha),
        ["e4", "e5", "Nf3", "Nc6", "Bb5", "a6"]
    );

    let beta = library.load(1).unwrap();
    assert_eq!(beta.white(), "Carol");
    assert_eq!(beta.result(), "1/2-1/2");
    assert_eq!(mainline_sans(&beta), ["d4", "d5", "c4", "e6"]);

    let gamma = library.load(2).unwrap();
    assert_eq!(gamma.white(), "Eve");
    assert_eq!(mainline_sans(&gamma), ["c4", "e5", "Nc3", "Nf6"]);

    std::fs::remove_file(path).unwrap();
}

#[test]
fn filter_is_case_insensitive_and_uses_headers() {
    let path = write_temp_pgn("filter", THREE_GAMES);
    let library = PgnLibrary::build(&path).unwrap();

    assert_eq!(library.filter(""), vec![0, 1, 2]);
    assert_eq!(library.filter("alice"), vec![0]);
    assert_eq!(library.filter("DAVE"), vec![1]);
    assert_eq!(library.filter("gamma"), vec![2]);
    assert_eq!(library.filter("test"), vec![0, 1, 2]);
    assert!(library.filter("nobody").is_empty());

    std::fs::remove_file(path).unwrap();
}

#[test]
fn index_matches_full_parse_on_local_fixtures() {
    for name in [
        "fischer.pgn",
        "prep_teufl.pgn",
        "candidates_2026_openings.pgn",
    ] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/local")
            .join(name);
        if !path.exists() {
            continue;
        }

        let library = PgnLibrary::build(&path).unwrap();
        let expected = domain::parse_pgn_file(&path).unwrap();
        assert_eq!(library.len(), expected.len(), "{name}: game count");

        let mut indices = vec![0, expected.len() / 2, expected.len() - 1];
        indices.sort_unstable();
        indices.dedup();
        for index in indices {
            let loaded = library.load(index).unwrap();
            let expected = &expected[index];
            assert_eq!(loaded.white(), expected.white(), "{name} #{index}");
            assert_eq!(loaded.black(), expected.black(), "{name} #{index}");
            assert_eq!(loaded.result(), expected.result(), "{name} #{index}");
            assert_eq!(
                mainline_sans(&loaded),
                mainline_sans(expected),
                "{name} #{index}: mainline"
            );
        }
    }
}

#[test]
fn large_export_indexes_and_samples_load() {
    let path = PathBuf::from("/tmp/opencode/scid-fixtures/frese/frese.pgn");
    if !path.exists() {
        return;
    }

    let library = PgnLibrary::build(&path).unwrap();
    assert_eq!(library.len(), 49_780);

    for index in [0, 1, 12_345, 24_890, 49_778, 49_779] {
        let game = library.load(index).unwrap();
        assert!(
            !game.mainline().is_empty(),
            "game {index} should have moves"
        );
        assert_eq!(game.white(), library.get(index).unwrap().white());
    }
}

#[test]
#[ignore = "full 49,780-game lazy-load pass; run manually on the SCID corpus"]
fn every_indexed_game_loads_from_its_byte_range() {
    let path = PathBuf::from("/tmp/opencode/scid-fixtures/frese/frese.pgn");
    if !path.exists() {
        return;
    }

    let library = PgnLibrary::build(&path).unwrap();
    for index in 0..library.len() {
        let entry = library.get(index).unwrap();
        let game = library
            .load(index)
            .unwrap_or_else(|err| panic!("game {index} failed: {err:#}"));
        assert_eq!(game.white(), entry.white(), "game {index}");
        assert_eq!(game.black(), entry.black(), "game {index}");
        assert_eq!(game.result(), entry.result(), "game {index}");
        // Some database games are result-only (forfeits), so empty movetext is
        // legitimate; the header match is what proves the byte range is right.
    }
}
