//! Developer helper: build a position index and print opening-tree statistics.
//!
//! ```sh
//! cargo run --release --example position_report -- big.pgn
//! ```

use std::time::Instant;

use brett::position::{BuildOptions, PositionIndex, position_key};
use shakmaty::Chess;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: position_report <file.pgn>");
        std::process::exit(2);
    };

    let index_path = std::env::temp_dir().join(format!(
        "brett-position-report-{}.brettpos",
        std::process::id()
    ));
    let started = Instant::now();
    let report =
        match PositionIndex::build_to(&path, &index_path, BuildOptions::default(), |progress| {
            if progress.games > 0 && progress.games % 10_000 == 0 {
                eprintln!(
                    "  {} games, {} moves, {:.1}s",
                    progress.games,
                    progress.edges,
                    progress.elapsed.as_secs_f64()
                );
            }
        }) {
            Ok(report) => report,
            Err(err) => {
                eprintln!("error: {err:#}");
                std::process::exit(1);
            }
        };
    let index = match PositionIndex::open(&index_path) {
        Ok(index) => index,
        Err(err) => {
            eprintln!("error opening index: {err:#}");
            std::process::exit(1);
        }
    };

    println!("games       {}", report.games);
    println!("positions   {}", report.positions);
    println!("edges       {}", report.edges);
    println!("build       {:?}", report.elapsed);
    println!(
        "index       {:.1} MiB",
        report.bytes as f64 / (1024.0 * 1024.0)
    );
    println!("wall        {:?}", started.elapsed());

    let start = Chess::default();
    if let Some(stats) = index.stats(position_key(&start)) {
        println!(
            "start       {} games, W/D/B {}/{}/{}, avg Elo {}",
            stats.games,
            stats.white,
            stats.draw,
            stats.black,
            stats
                .elo_avg()
                .map(|elo| format!("{elo:.0}"))
                .unwrap_or_else(|| "—".to_string())
        );
        let mut moves = index.moves(position_key(&start));
        moves.sort_by_key(|stat| std::cmp::Reverse(stat.games));
        for stat in moves.iter().take(8) {
            println!(
                "  {:<6} {:>8} games  {:>5.1}%  {}/{}/{}",
                stat.san(&start).unwrap_or_else(|| "?".to_string()),
                stat.games,
                stat.score_white() * 100.0,
                stat.white,
                stat.draw,
                stat.black
            );
        }
    }

    let _ = std::fs::remove_file(index_path);
}
