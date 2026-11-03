use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use brett::chess::{Chess, Position, SanPlus};
use brett::domain;
use brett::library::{GameLibrary, PgnLibrary};
use brett::position::{
    BuildOptions, PositionIndex, RatingFilter, decode_move, encode_move, position_key,
    reference_cache_path,
};

const FOUR_GAMES: &str = r#"[Event "Alpha"]
[Site "Test"]
[Date "2024.01.01"]
[White "Alice"]
[Black "Bob"]
[WhiteElo "2500"]
[BlackElo "2400"]
[Result "1-0"]

1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 1-0

[Event "Beta"]
[Site "Test"]
[Date "2023.05.10"]
[White "Carol"]
[Black "Dave"]
[WhiteElo "2600"]
[BlackElo "2550"]
[Result "1/2-1/2"]

1. e4 c5 2. Nf3 d6 1/2-1/2

[Event "Gamma"]
[Site "Test"]
[Date "2025.12.31"]
[White "Eve"]
[Black "Frank"]
[WhiteElo "2400"]
[BlackElo "2450"]
[Result "0-1"]

1. d4 d5 2. c4 e6 0-1

[Event "Delta"]
[Site "Test"]
[Date "2022.07.04"]
[White "Gina"]
[Black "Hank"]
[WhiteElo "2300"]
[BlackElo "2350"]
[Result "1-0"]

1. c4 e5 2. Nc3 Nf6 1-0
"#;

fn temp_path(name: &str, extension: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "brett-position-{name}-{}-{unique}.{extension}",
        std::process::id()
    ))
}

fn build(name: &str, pgn: &str, options: BuildOptions) -> (PathBuf, PathBuf, PositionIndex) {
    let pgn_path = temp_path(name, "pgn");
    let index_path = temp_path(name, "brettpos");
    std::fs::write(&pgn_path, pgn).unwrap();
    let report = PositionIndex::build_to(&pgn_path, &index_path, options, |_| {}).unwrap();
    assert_eq!(report.games, 4);
    let index = PositionIndex::open(&index_path).unwrap();
    (pgn_path, index_path, index)
}

fn after(mut pos: Chess, sans: &[&str]) -> Chess {
    for san in sans {
        let san = SanPlus::from_ascii(san.as_bytes()).unwrap();
        let mv = san.san.to_move(&pos).unwrap();
        pos = pos.play(mv).unwrap();
    }
    pos
}

fn move_sans(index: &PositionIndex, pos: &Chess) -> Vec<String> {
    let mut moves: Vec<_> = index
        .moves(position_key(pos))
        .into_iter()
        .filter_map(|stat| stat.san(pos).map(|san| (san, stat)))
        .collect();
    moves.sort_by(|a, b| b.1.games.cmp(&a.1.games).then(a.0.cmp(&b.0)));
    moves.into_iter().map(|(san, _)| san).collect()
}

#[test]
fn start_position_aggregates_games_elo_dates_and_moves() {
    let (pgn_path, index_path, index) = build("start", FOUR_GAMES, BuildOptions::default());
    assert_eq!(index.game_count(), 4);
    assert_eq!(index.position_count(), 14);

    let start = Chess::default();
    let stats = index.stats(position_key(&start)).unwrap();
    assert_eq!(stats.games, 4);
    assert_eq!(stats.white, 2);
    assert_eq!(stats.draw, 1);
    assert_eq!(stats.black, 1);
    assert_eq!(stats.elo_count, 8);
    assert!((stats.elo_avg().unwrap() - 2443.75).abs() < f64::EPSILON);
    assert_eq!(stats.first_date, 2022_0704);
    assert_eq!(stats.last_date, 2025_1231);

    assert_eq!(move_sans(&index, &start), ["e4", "c4", "d4"]);

    let e4 = index
        .moves(position_key(&start))
        .into_iter()
        .find(|stat| stat.san(&start).as_deref() == Some("e4"))
        .unwrap();
    assert_eq!(e4.games, 2);
    assert_eq!(e4.white, 1);
    assert_eq!(e4.draw, 1);
    assert_eq!(e4.black, 0);
    assert!((e4.score_white() - 0.75).abs() < f32::EPSILON);
    assert!((e4.elo_avg().unwrap() - 2512.5).abs() < f64::EPSILON);

    std::fs::remove_file(pgn_path).unwrap();
    std::fs::remove_file(index_path).unwrap();
}

#[test]
fn child_positions_and_score_are_queryable() {
    let (pgn_path, index_path, index) = build("children", FOUR_GAMES, BuildOptions::default());

    let after_e4 = after(Chess::default(), &["e4"]);
    assert_eq!(move_sans(&index, &after_e4), ["c5", "e5"]);
    let stats = index.stats(position_key(&after_e4)).unwrap();
    assert_eq!(stats.games, 2);
    assert_eq!(stats.white, 1);
    assert_eq!(stats.draw, 1);

    let after_e4_e5 = after(Chess::default(), &["e4", "e5"]);
    assert_eq!(move_sans(&index, &after_e4_e5), ["Nf3"]);

    let after_d4 = after(Chess::default(), &["d4"]);
    assert_eq!(move_sans(&index, &after_d4), ["d5"]);

    std::fs::remove_file(pgn_path).unwrap();
    std::fs::remove_file(index_path).unwrap();
}

#[test]
fn only_mainlines_are_counted() {
    let pgn_path = temp_path("opera", "pgn");
    std::fs::write(&pgn_path, brett::test_data::OPERA_PGN).unwrap();
    let index_path = temp_path("opera", "brettpos");
    PositionIndex::build_to(&pgn_path, &index_path, BuildOptions::default(), |_| {}).unwrap();
    let index = PositionIndex::open(&index_path).unwrap();

    let games = domain::parse_pgn_file(&pgn_path).unwrap();
    let game = &games[0];
    let d4 = game
        .nodes
        .iter()
        .find(|node| node.san() == Some("d4"))
        .unwrap();
    let moves = index.moves(position_key(&d4.pos));

    assert_eq!(moves.len(), 1, "the 3...exd4 variation must not be counted");
    assert_eq!(moves[0].san(&d4.pos).as_deref(), Some("Bg4"));
    assert_eq!(moves[0].games, 1);

    std::fs::remove_file(pgn_path).unwrap();
    std::fs::remove_file(index_path).unwrap();
}

#[test]
fn max_plies_stops_the_index() {
    let (pgn_path, index_path, index) = build(
        "depth",
        FOUR_GAMES,
        BuildOptions {
            max_plies: 2,
            ..BuildOptions::default()
        },
    );
    let start = Chess::default();
    assert_eq!(index.stats(position_key(&start)).unwrap().games, 4);
    let after_e4_e5 = after(start, &["e4", "e5"]);
    assert!(index.stats(position_key(&after_e4_e5)).is_none());

    std::fs::remove_file(pgn_path).unwrap();
    std::fs::remove_file(index_path).unwrap();
}

#[test]
fn move_codes_round_trip_through_legal_moves() {
    let pos = Chess::default();
    for mv in pos.legal_moves() {
        let code = encode_move(mv).unwrap();
        assert_eq!(decode_move(&pos, code), Some(mv));
    }
}

#[test]
fn index_survives_reopen() {
    let (pgn_path, index_path, index) = build("reopen", FOUR_GAMES, BuildOptions::default());
    let expected = index.stats(position_key(&Chess::default())).unwrap();
    drop(index);

    let reopened = PositionIndex::open(&index_path).unwrap();
    assert_eq!(
        reopened.stats(position_key(&Chess::default())).unwrap(),
        expected
    );
    assert_eq!(reopened.game_count(), 4);
    assert_eq!(reopened.edge_count(), 17);

    std::fs::remove_file(pgn_path).unwrap();
    std::fs::remove_file(index_path).unwrap();
}

#[test]
fn missing_positions_return_none() {
    let (pgn_path, index_path, index) = build("missing", FOUR_GAMES, BuildOptions::default());
    assert!(index.stats(0xDEAD_BEEF_DEAD_BEEF).is_none());
    assert!(index.moves(0xDEAD_BEEF_DEAD_BEEF).is_empty());
    std::fs::remove_file(pgn_path).unwrap();
    std::fs::remove_file(index_path).unwrap();
}

#[test]
fn reference_cache_is_separate_and_reopens() {
    let pgn_path = temp_path("reference-cache", "pgn");
    std::fs::write(&pgn_path, FOUR_GAMES).unwrap();
    let library = Arc::new(PgnLibrary::build(pgn_path.clone()).unwrap()) as Arc<dyn GameLibrary>;
    let filter = RatingFilter::around(2425, 50);
    let index_path = reference_cache_path(&pgn_path, Some(filter));

    assert!(
        PositionIndex::open_cached_for_at(library.as_ref(), &index_path)
            .unwrap()
            .is_none()
    );
    PositionIndex::build_cached_for_at(
        library.clone(),
        &index_path,
        BuildOptions {
            rating_filter: Some(filter),
            ..BuildOptions::default()
        },
        |_| {},
    )
    .unwrap();
    let index = PositionIndex::open_cached_for_at(library.as_ref(), &index_path)
        .unwrap()
        .unwrap();
    assert_eq!(index.game_count(), 2);
    assert_eq!(
        index.stats(position_key(&Chess::default())).unwrap().games,
        2
    );

    std::fs::remove_file(pgn_path).unwrap();
    std::fs::remove_file(index_path).unwrap();
}

#[test]
fn source_path_is_never_used_for_the_index_file() {
    let pgn_path = Path::new("/tmp/opencode/does-not-matter.pgn");
    let cache = brett::position::cache_path(pgn_path);
    assert_eq!(cache.extension().unwrap(), "brettpos");
    assert!(
        cache.starts_with(
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cache"))
        )
    );
}
