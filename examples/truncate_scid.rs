//! Truncate every game in a SCID4 database at a fixed ply limit.
//!
//! Usage: cargo run --release --example truncate_scid -- input.si4 [max_plies] [output.si4]
//!
//! `output.si4` defaults to the input path, so the triple is replaced in place
//! through the writer's rollback-safe rename. The command is idempotent: when
//! every branch is already within the limit it reports that and does not
//! rewrite the database.

use std::path::PathBuf;

use brett::domain::Game;
use brett::scid::ScidLibrary;
use brett::scid::write_database;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let Some(input) = args.next() else {
        eprintln!("usage: truncate_scid input.si4 [max_plies] [output.si4]");
        std::process::exit(2);
    };
    let max_plies = match args.next() {
        Some(value) => value
            .to_string_lossy()
            .parse::<usize>()
            .map_err(|_| anyhow::anyhow!("max_plies must be a whole number"))?,
        None => 70,
    };
    let output = args.next().unwrap_or_else(|| input.clone());
    if max_plies == 0 {
        anyhow::bail!("max_plies must be greater than zero");
    }

    let library = ScidLibrary::open(&input)?;
    let total = library.len();
    let mut deepest = 0usize;
    for index in 0..total {
        let game = library.load(index)?;
        deepest = deepest.max(tree_depth(&game));
    }
    println!(
        "{}: {} games, deepest branch {} plies",
        input.display(),
        total,
        deepest
    );
    if deepest <= max_plies {
        println!("Already within {max_plies} plies; nothing to do.");
        return Ok(());
    }

    let description =
        format!("ICCF correspondence reference (mainlines truncated to {max_plies} plies)");
    let report = write_database(
        &output,
        &description,
        (0..total).map(|index| {
            let mut game = library.load(index)?;
            game.truncate_mainline(max_plies);
            Ok(game)
        }),
    )?;
    println!(
        "Wrote {}: {} games, {} bytes (limit {max_plies} plies)",
        output.display(),
        report.games,
        report.bytes
    );
    Ok(())
}

/// Deepest stored variation, counting a move as one ply from the root.
fn tree_depth(game: &Game) -> usize {
    fn walk(game: &Game, node: usize, depth: usize) -> usize {
        game.nodes[node]
            .children
            .iter()
            .map(|&child| walk(game, child, depth + 1))
            .max()
            .unwrap_or(depth)
    }
    walk(game, 0, 0)
}
