use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use brett::library::GameLibrary;
use brett::players::PlayerDirectory;
use brett::position::{BuildOptions, PositionIndex, position_key};
use brett::scid::ScidLibrary;
use shakmaty::Chess;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/scid")
        .join(name)
}

fn temp_path(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "brett-scid-{name}-{}-{unique}.brettpos",
        std::process::id()
    ))
}

#[test]
fn scid_database_implements_the_shared_library_interface() {
    let library = ScidLibrary::open(fixture("opera.si4")).unwrap();
    let database: &dyn GameLibrary = &library;
    assert_eq!(database.len(), 1);
    assert!(!database.is_empty());
    assert_eq!(database.filter("morphy"), [0]);
    assert_eq!(database.filter("isouard"), [0]);
    assert!(database.filter("nobody").is_empty());

    let game = database.load(0).unwrap();
    assert_eq!(game.white(), "Paul Morphy");
    assert!(game.mainline_len() > 30);
}

#[test]
fn players_are_built_from_scid_headers() {
    let library = ScidLibrary::open(fixture("opera.si4")).unwrap();
    let players = PlayerDirectory::build(&library);
    let index = players.find_key("morphy paul").unwrap();
    let player = players.get(index).unwrap();
    assert_eq!(player.name, "Paul Morphy");
    assert_eq!(player.games(), 1);
    assert_eq!(player.game_indices, [0]);
}

#[test]
fn position_index_is_built_from_scid_games() {
    let library = ScidLibrary::open(fixture("opera.si4")).unwrap();
    let path = temp_path("positions");
    let report =
        PositionIndex::build_library_to(&library, &path, BuildOptions::default(), |_| {}).unwrap();
    assert_eq!(report.games, 1);
    assert!(report.edges > 30);

    let index = PositionIndex::open(&path).unwrap();
    let start = Chess::default();
    let stats = index.stats(position_key(&start)).unwrap();
    assert_eq!(stats.games, 1);
    assert_eq!(stats.white, 1);
    let moves = index.moves(position_key(&start));
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0].san(&start).as_deref(), Some("e4"));

    std::fs::remove_file(path).unwrap();
}
