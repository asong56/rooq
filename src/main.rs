// Suppresses the console window in release builds: this is a background
// daemon, not a CLI tool, so no console should flash on launch.
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

    // Explicit path argument: open it directly and exit when the window
    // closes, same as any ordinary "open this file" viewer. Useful for
    // testing and for wiring into a right-click "Open with Rooq" entry.
    if let Some(path) = args.get(1) {
        return run_single_file(PathBuf::from(path));
    }

    run_daemon();
    Ok(())
}

fn run_single_file(path: PathBuf) -> eframe::Result<()> {
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
            if path.exists() {
                app.open_path(&cc.egui_ctx, path);
            } else {
                eprintln!("warning: path does not exist: {}", path.display());
            }
            Ok(Box::new(app))
        }),
    )
}

/// No-window background mode: install the tray icon and the Space-key
/// watcher, then wait. Each toggle-on opens a preview window for whatever
/// file is currently selected in the foreground Explorer window; toggle-off
/// (a second Space press) closes it.
///
/// Known gap: `hotkey::install`'s Explorer check looks at whatever window
/// is currently in the foreground. If the preview window takes foreground
/// focus when it opens, a second Space press won't be recognized as a
/// toggle-off (since the foreground window is then Rooq's own preview, not
/// Explorer) until focus returns to Explorer. `with_always_on_top()` below
/// keeps the window visible without deliberately stealing focus, which
/// should keep Explorer foregrounded in the common case, but this hasn't
/// been verified against a real Explorer window.
fn run_daemon() {
    // COM apartment state is per-thread; held for the lifetime of this
    // function since `selected_file_in_foreground_explorer` (called below,
    // on this same thread) makes COM calls.
    let _com = match daemon::selection::ComGuard::init() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("failed to initialize COM: {e:?}");
            return;
        }
    };

    let (toggle_tx, toggle_rx) = mpsc::channel::<()>();

    // WH_KEYBOARD_LL must live on a thread that pumps messages, and so
    // does the tray icon (Shell_NotifyIconW delivers its events through
    // the owning thread's message queue). Both run together on this one
    // dedicated thread so only one message loop is needed.
    let hotkey_thread = std::thread::spawn(move || {
        if let Err(e) = daemon::run_message_loop(toggle_tx) {
            eprintln!("daemon message loop failed: {e:?}");
        }
    });

    // State for the currently-open preview window, if any. `close_flag` is
    // polled by that window's RooqApp each frame (see
    // core::window::RooqApp::with_close_signal); setting it asks that
    // window to close on its own thread rather than reaching across
    // threads into eframe's internals.
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
        let native_options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([900.0, 700.0])
                .with_min_inner_size([300.0, 200.0])
                .with_always_on_top(),
            ..Default::default()
        };

        let result = eframe::run_native(
            "Rooq",
            native_options,
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
