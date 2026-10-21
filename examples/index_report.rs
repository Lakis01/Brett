//! Developer helper: time a PGN index build and a lazy game load.
//!
//! ```sh
//! cargo run --example index_report -- big.pgn
//! ```

use std::time::Instant;

use brett::library::PgnLibrary;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: index_report <file.pgn>");
        std::process::exit(2);
    };

    let started = Instant::now();
    let library = match PgnLibrary::build(path) {
        Ok(library) => library,
        Err(err) => {
            eprintln!("error: {err:#}");
            std::process::exit(1);
        }
    };
    let elapsed = started.elapsed();

    let header_bytes: usize = library
        .games()
        .iter()
        .flat_map(|game| game.headers().iter())
        .map(|(key, value)| key.len() + value.len() + 2 * std::mem::size_of::<String>())
        .sum();

    println!("games       {}", library.len());
    println!(
        "file        {:.1} MiB",
        library.file_len() as f64 / (1024.0 * 1024.0)
    );
    println!("indexed     {elapsed:?}");
    if elapsed.as_secs_f64() > 0.0 {
        println!(
            "throughput  {:.0} games/s",
            library.len() as f64 / elapsed.as_secs_f64()
        );
    }
    println!(
        "headers     ~{:.1} MiB",
        header_bytes as f64 / (1024.0 * 1024.0)
    );

    for index in [0, library.len() / 2, library.len().saturating_sub(1)] {
        let started = Instant::now();
        let game = library.load(index).expect("indexed game must load");
        println!(
            "load #{index:<6} {} – {}, {} plies, {:?}",
            game.white(),
            game.black(),
            game.mainline_len(),
            started.elapsed()
        );
    }
}
