mod core;
mod providers;

use core::window::RooqApp;
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let initial_path = args.get(1).map(PathBuf::from);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 700.0])
            .with_min_inner_size([300.0, 200.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Rooq",
        native_options,
        Box::new(move |cc| {
            let mut app = RooqApp::new(cc);
            if let Some(path) = initial_path {
                if path.exists() {
                    app.open_path(&cc.egui_ctx, path);
                } else {
                    eprintln!("warning: path does not exist: {}", path.display());
                }
            }
            Ok(Box::new(app))
        }),
    )
}
