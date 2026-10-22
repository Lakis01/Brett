//! Font setup.
//!
//! egui's bundled fonts do not contain chess figurines or most chess
//! annotation symbols. DejaVu Sans is embedded as a low-priority fallback so
//! normal text keeps the stock egui font, while `♘`, `♕`, `⩲`, `∞`, … render
//! correctly in the move list and tree.

use eframe::egui;

/// Install the fallback font that supplies chess symbols.
pub fn install(ctx: &egui::Context) {
    ctx.add_font(egui::FontInsert::new(
        "dejavu-chess-symbols",
        egui::FontData::from_static(include_bytes!("../../assets/fonts/DejaVuSans.ttf")),
        vec![
            egui::InsertFontFamily {
                family: egui::FontFamily::Proportional,
                priority: egui::FontPriority::Lowest,
            },
            egui::InsertFontFamily {
                family: egui::FontFamily::Monospace,
                priority: egui::FontPriority::Lowest,
            },
        ],
    ));
}
