//! Diagnostic helper: write a PGN file as a SCID4 database.
//!
//! Usage: cargo run --example scid_export -- input.pgn output.si4
//!
//! This is useful for validating the native writer with independent SCID
//! tools (`scidpgn`, `scidt`, or the SCID 4.x GUI).

use std::path::PathBuf;

use brett::{domain, scid};

fn main() {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        eprintln!("usage: scid_export input.pgn output.si4");
        std::process::exit(2);
    };

    let games = match domain::parse_pgn_file(&input) {
        Ok(games) => games,
        Err(err) => {
            eprintln!("{}: {err:#}", input.display());
            std::process::exit(1);
        }
    };
    let description = format!(
        "Brett export: {}",
        output
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "SCID".to_string())
    );
    match scid::write_database(&output, &description, games.into_iter().map(Ok)) {
        Ok(report) => println!(
            "{}: {} games, {} bytes",
            output.display(),
            report.games,
            report.bytes
        ),
        Err(err) => {
            eprintln!("{}: {err:#}", output.display());
            std::process::exit(1);
        }
    }
}
