use std::sync::Arc;

use brett::app;
use eframe::egui;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let icon: Arc<egui::IconData> = image::load_from_memory(include_bytes!("../assets/brett.webp"))
        .map(|img| {
            Arc::new(egui::IconData {
                rgba: img.to_rgba8().into_raw(),
                width: img.width(),
                height: img.height(),
            })
        })
        .unwrap_or_else(|_| {
            Arc::new(egui::IconData {
                rgba: vec![0; 4],
                width: 1,
                height: 1,
            })
        });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 640.0])
            .with_title("Brett")
            .with_app_id("brett")
            .with_icon(icon),
        ..Default::default()
    };

    eframe::run_native(
        "Brett",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
