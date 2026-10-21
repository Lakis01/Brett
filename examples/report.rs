//! Developer helper: parse a PGN file and print statistics.
//!
//! ```sh
//! cargo run --example report -- path/to/file.pgn
//! ```

use std::time::Instant;

use brett::domain;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: report <file.pgn>");
        std::process::exit(2);
    };

    let started = Instant::now();
    let games = match domain::parse_pgn_file(path.as_ref()) {
        Ok(games) => games,
        Err(err) => {
            eprintln!("error: {err:#}");
            std::process::exit(1);
        }
    };
    let elapsed = started.elapsed();

    let nodes: usize = games.iter().map(|g| g.nodes.len()).sum();
    let plies: usize = games.iter().map(|g| g.mainline_len()).sum();
    let variations: usize = games
        .iter()
        .map(|g| g.nodes.iter().filter(|n| n.children.len() > 1).count())
        .sum();
    let comments = games
        .iter()
        .filter(|g| !g.nodes[0].comment.is_empty())
        .count();
    let errors: Vec<(usize, &str)> = games
        .iter()
        .enumerate()
        .flat_map(|(i, g)| g.errors.iter().map(move |e| (i, e.as_str())))
        .collect();

    println!("file        {path}");
    println!("games       {}", games.len());
    println!("plies       {plies}");
    println!("nodes       {nodes}");
    println!(
        "node size   {} bytes (~{} MiB retained)",
        std::mem::size_of::<domain::Node>(),
        nodes * std::mem::size_of::<domain::Node>() / (1024 * 1024)
    );
    println!("variations  {variations}");
    println!("comments    {comments} games start with a comment");
    println!("errors      {}", errors.len());
    for (game, err) in errors.iter().take(10) {
        println!("  game {game}: {err}");
    }
    println!("elapsed     {elapsed:?}");
}
