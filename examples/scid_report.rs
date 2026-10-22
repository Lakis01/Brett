//! Diagnostic helper: open SCID databases and decode every game.
//!
//! Usage: cargo run --example scid_report -- "/path/to/database.si4" …

use brett::scid::ScidLibrary;

fn main() {
    let mut failed = false;
    for path in std::env::args_os().skip(1) {
        match ScidLibrary::open(&path) {
            Ok(library) => {
                println!(
                    "{}: {} games, {} bytes, indexed in {:.2}s",
                    library.path().display(),
                    library.len(),
                    library.file_len(),
                    library.indexed_in().as_secs_f64()
                );
                let mut errors = 0usize;
                let mut moves = 0usize;
                for index in 0..library.len() {
                    match library.load(index) {
                        Ok(game) => moves += game.mainline_len(),
                        Err(err) => {
                            errors += 1;
                            if errors <= 10 {
                                eprintln!("  game {}: {err:#}", index + 1);
                            }
                        }
                    }
                }
                println!(
                    "  decoded {} moves, {} errors{}",
                    moves,
                    errors,
                    if errors > 10 { " (first 10 shown)" } else { "" }
                );
                failed |= errors > 0;
            }
            Err(err) => {
                eprintln!("{}: {err:#}", std::path::Path::new(&path).display());
                failed = true;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}
