//! Build sidecar position index for correspondence database.

use brett::position::{BuildOptions, PositionIndex};
use brett::scid::ScidLibrary;
use std::path::Path;

fn main() -> anyhow::Result<()> {
    let source_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "databases/iccf/Correspondence_2350.si4".to_string());
    let source = Path::new(&source_path);
    let sidecar = source.with_extension("brettpos");
    let lib = ScidLibrary::open(source)?;
    let options = BuildOptions {
        max_plies: 40,
        ..Default::default()
    };
    println!(
        "Indexing {} into {}...",
        source.display(),
        sidecar.display()
    );
    let report = PositionIndex::build_library_to(&lib, &sidecar, options, |p| {
        if p.games > 0 && p.games % 20_000 == 0 {
            println!("  {} games, {:.1}s", p.games, p.elapsed.as_secs_f64());
        }
    })?;
    println!(
        "Done. {} games, {} positions, {:.1} MiB index in {:.1}s.",
        report.games,
        report.positions,
        report.bytes as f64 / (1024.0 * 1024.0),
        report.elapsed.as_secs_f64()
    );
    Ok(())
}
