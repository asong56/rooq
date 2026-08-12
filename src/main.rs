// Not a CLI tool, so no console should flash on launch.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod core;
mod daemon;
mod providers;

use core::window::RooqApp;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if let Some(path) = args.get(1) {
        return run_single_file(PathBuf::from(path));
    }

    run_daemon();
    Ok(())
}

fn native_options(always_on_top: bool) -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([900.0, 700.0])
        .with_min_inner_size([300.0, 200.0]);
    if always_on_top {
        viewport = viewport.with_always_on_top();
    }
    eframe::NativeOptions {
        viewport,
        ..Default::default()
    }
}

fn run_single_file(path: PathBuf) -> eframe::Result<()> {
    eframe::run_native(
        "Rooq",
        native_options(false),
        Box::new(move |cc| {
            let mut app = RooqApp::new(cc);
            if path.exists() {
                app.open_path(&cc.egui_ctx, path);
            } else {
                eprintln!("warning: path does not exist: {}", path.display());
            }
            Ok(Box::new(app))
        }),
    )
}

fn run_daemon() {
    // COM apartment state is per-thread; selection queries below run on this same thread.
    let _com = match daemon::selection::ComGuard::init() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("failed to initialize COM: {e:?}");
            return;
        }
    };

    let (toggle_tx, toggle_rx) = mpsc::channel::<()>();

    let hotkey_thread = std::thread::spawn(move || {
        if let Err(e) = daemon::run_message_loop(toggle_tx) {
            eprintln!("daemon message loop failed: {e:?}");
        }
    });

    let mut open_preview: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)> = None;

    for () in toggle_rx {
        match open_preview.take() {
            Some((close_flag, handle)) => {
                close_flag.store(true, Ordering::SeqCst);
                let _ = handle.join();
            }
            None => {
                let Some(path) = daemon::selection::selected_file_in_foreground_explorer() else {
                    continue;
                };
                let close_flag = Arc::new(AtomicBool::new(false));
                let handle = spawn_preview_window(path, Arc::clone(&close_flag));
                open_preview = Some((close_flag, handle));
            }
        }
    }

    let _ = hotkey_thread.join();
}

fn spawn_preview_window(
    path: PathBuf,
    close_flag: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let result = eframe::run_native(
            "Rooq",
            native_options(true),
            Box::new(move |cc| {
                let mut app = RooqApp::with_close_signal(cc, Some(close_flag));
                app.open_path(&cc.egui_ctx, path);
                Ok(Box::new(app))
            }),
        );

        if let Err(e) = result {
            eprintln!("preview window failed: {e:?}");
        }
    })
}
