//! Manual validation sample for the 2CBH writer.
//!
//! Usage: cargo run --example twocbh_sample -- [output.2cbh]
//!
//! Writes six games covering metadata, variations, promotion, Chess960 on the
//! standard board, a deleted game, and a null move, then reopens the result
//! and checks the game count. The output path defaults to
//! `target/twocbh-sample.2cbh`.

use std::path::PathBuf;

use brett::domain::parse_pgn;
use brett::twocbh::{
    TwoCbhLibrary, TwoCbhWriteOptions, core_companion_files, write_database_with_options,
};

const SAMPLE: &str = r#"
[Event "Brett 2CBH sample"]
[Site "Brett"]
[Date "2026.10.31"]
[Round "1"]
[White "Normal, Nora"]
[Black "Black, Ben"]
[Result "1-0"]
[WhiteElo "2500"]
[BlackElo "2400"]
[WhiteFideId "123456"]
[BlackFideId "654321"]
[GameId "1001"]
[Beauty "5"]
[SourceTitle "Brett sample"]
[Source "Brett"]

1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 *

[Event "Brett 2CBH sample"]
[Site "Brett"]
[Date "2026.10.31"]
[Round "2"]
[White "Variations, Vera"]
[Black "Black, Ben"]
[Result "1/2-1/2"]

1. e4 e5 (1... c5 2. Nf3 (2. Nc3 Nc6) 2... d6) 2. Nf3 Nc6 (2... Nf6) 3. Bb5 *

[Event "Brett 2CBH sample"]
[Site "Brett"]
[Date "2026.10.31"]
[Round "3"]
[White "Promotion, Petra"]
[Black "Black, Ben"]
[Result "1-0"]

1. e4 a6 2. e5 d5 3. exd6 Nf6 4. dxc7 Nbd7 5. cxd8=Q *

[Event "Brett 2CBH sample"]
[Site "Brett"]
[Date "2026.10.31"]
[Round "4"]
[White "Castle, Carl"]
[Black "Black, Ben"]
[Result "1-0"]
[Variant "Chess960"]
[FEN "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"]

1. e4 e5 2. Nf3 Nc6 3. Bc4 Bc5 4. O-O *

[Event "Brett 2CBH sample"]
[Site "Brett"]
[Date "2026.10.31"]
[Round "5"]
[White "Deleted, Dana"]
[Black "Black, Ben"]
[Result "0-1"]
[Deleted "1"]

1. d4 d5 *

[Event "Brett 2CBH sample"]
[Site "Brett"]
[Date "2026.10.31"]
[Round "6"]
[White "Null, Nina"]
[Black "Black, Ben"]
[Result "*"]

1. e4 e5 2. -- *
"#;

const CHECKLIST: [&str; 9] = [
    "ChessBase opens the .2cbh without a repair prompt.",
    "The game list shows names, event, site, date, round, result, Elo, FIDE ids, game id, and beauty values.",
    "The variation game opens with the correct mainline and variation tree.",
    "Castling, promotion, en passant, and Chess960 castling replay correctly.",
    "The deleted game is marked deleted.",
    "The null-move game opens or fails cleanly, and the result is recorded.",
    "The annotation pane is empty but does not crash.",
    "ChessBase creates .2lcd/.2lgd indexes without errors.",
    "Saving or closing in ChessBase does not corrupt the database.",
];

fn main() -> anyhow::Result<()> {
    let output = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/twocbh-sample.2cbh"));

    let games = parse_pgn(SAMPLE.as_bytes())?;
    assert_eq!(games.len(), 6, "the sample PGN should contain six games");

    let report = write_database_with_options(
        &output,
        TwoCbhWriteOptions {
            allow_annotation_loss: true,
        },
        games.into_iter().map(Ok),
    )?;

    let library = TwoCbhLibrary::open(&output)?;
    assert_eq!(
        library.len(),
        6,
        "the reopened 2CBH database should hold six games"
    );

    let path = std::fs::canonicalize(library.path())?;
    for companion in core_companion_files(&path) {
        assert!(
            companion.is_file(),
            "expected 2CBH core file {}",
            companion.display()
        );
    }

    println!("{}: {} games", path.display(), report.games);
    println!();
    println!("ChessBase manual checklist:");
    for item in CHECKLIST {
        println!("  - {item}");
    }
    Ok(())
}
