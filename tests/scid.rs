use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use brett::chess::Chess;
use brett::library::GameLibrary;
use brett::players::PlayerDirectory;
use brett::position::{BuildOptions, PositionIndex, position_key};
use brett::scid::ScidLibrary;

struct OperaScid {
    dir: PathBuf,
    path: PathBuf,
}

impl OperaScid {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("brett-scid-opera-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("opera.si4");
        let games = brett::domain::parse_pgn(brett::test_data::OPERA_PGN.as_bytes()).unwrap();
        brett::scid::write_database(&path, "Opera Game", games.into_iter().map(Ok)).unwrap();
        Self { dir, path }
    }
}

impl Drop for OperaScid {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
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
    let database = OperaScid::new();
    let library = ScidLibrary::open(&database.path).unwrap();
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
    let database = OperaScid::new();
    let library = ScidLibrary::open(&database.path).unwrap();
    let players = PlayerDirectory::build(&library);
    let index = players.find_key("morphy paul").unwrap();
    let player = players.get(index).unwrap();
    assert_eq!(player.name, "Paul Morphy");
    assert_eq!(player.games(), 1);
    assert_eq!(player.game_indices, [0]);
}

#[test]
fn position_index_is_built_from_scid_games() {
    let database = OperaScid::new();
    let library = ScidLibrary::open(&database.path).unwrap();
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
