//! Smoke tests against real-world PGN files that are not redistributed.
//!
//! The files live in `fixtures/local/`, which is git-ignored. Each test is a
//! no-op when its fixture is missing so the suite stays green on a fresh clone.

use std::path::PathBuf;

use brett::domain;

fn fixture(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/local")
        .join(name);
    path.exists().then_some(path)
}

#[test]
fn scid_export_of_annotated_games_parses_cleanly() {
    let Some(path) = fixture("fischer.pgn") else {
        return;
    };
    let games = domain::parse_pgn_file(&path).unwrap();
    assert_eq!(games.len(), 5);
    let annotated_starts = games
        .iter()
        .filter(|game| !game.nodes[0].comment.is_empty())
        .count();
    assert!(
        annotated_starts >= 4,
        "expected most games to start with an annotation, got {annotated_starts}"
    );
    for (i, game) in games.iter().enumerate() {
        assert!(
            game.errors.is_empty(),
            "game {i} ({} – {}) had errors: {:?}",
            game.white(),
            game.black(),
            game.errors
        );
        assert!(game.mainline_len() > 10, "game {i} looks truncated");
    }
    let with_variations = games
        .iter()
        .filter(|game| game.nodes.iter().any(|n| n.children.len() > 1))
        .count();
    assert!(
        with_variations >= 4,
        "expected variations in most games, got {with_variations}"
    );
}

#[test]
fn chessbase_style_prep_file_parses_cleanly() {
    let Some(path) = fixture("prep_teufl.pgn") else {
        return;
    };
    let games = domain::parse_pgn_file(&path).unwrap();
    assert!(!games.is_empty());
    for game in &games {
        assert!(game.errors.is_empty(), "{:?}", game.errors);
    }
    assert!(
        games
            .iter()
            .any(|g| g.nodes.iter().any(|n| n.children.len() > 1)),
        "expected variations in the prep file"
    );
}

#[test]
fn candidate_openings_parse_cleanly() {
    let Some(path) = fixture("candidates_2026_openings.pgn") else {
        return;
    };
    let games = domain::parse_pgn_file(&path).unwrap();
    assert!(!games.is_empty());
    for game in &games {
        assert!(game.errors.is_empty(), "{:?}", game.errors);
    }
}
