//! Background daemon: tray icon + Space-key watcher, both driven by one
//! Win32 message loop on a dedicated thread (see `main.rs::run_daemon`).

pub mod hotkey;
pub mod selection;

use std::sync::mpsc::Sender;
use std::time::Duration;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
};

/// Installs the tray icon and the keyboard hook, then pumps messages for
/// both until the tray's "Exit" item is clicked. Each Space press detected
/// by the hook (see `hotkey.rs`) is forwarded on `toggle_tx`.
pub fn run_message_loop(toggle_tx: Sender<()>) -> windows::core::Result<()> {
    let (_tray_icon, exit_id) = build_tray_icon();

    // WH_KEYBOARD_LL is set up here (same thread, same message loop) rather
    // than in a `hotkey::run` that owns its own `GetMessage` loop, so one
    // loop below can service both the hook and the tray icon.
    let hook = hotkey::install(toggle_tx)?;

    let mut msg = MSG::default();
    loop {
        let has_message = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool();
        if has_message {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        if let Ok(event) = MenuEvent::receiver().try_recv() {
            if Some(&event.id) == exit_id.as_ref() {
                break;
            }
        }

        if !has_message {
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    hotkey::uninstall(hook);
    Ok(())
}

fn build_tray_icon() -> (Option<tray_icon::TrayIcon>, Option<MenuId>) {
    let exit_item = MenuItem::new("Exit", true, None);
    let exit_id = exit_item.id().clone();

    let menu = Menu::new();
    if menu.append(&exit_item).is_err() {
        eprintln!("warning: failed to build tray menu; Exit item unavailable");
    }

    let icon = match tray_icon_image() {
        Ok(icon) => icon,
        Err(e) => {
            eprintln!("warning: failed to build tray icon image: {e}");
            return (None, Some(exit_id));
        }
    };

    let tray = match TrayIconBuilder::new()
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_tooltip("Rooq — press Space in Explorer to preview")
        .build()
    {
        Ok(tray) => Some(tray),
        Err(e) => {
            eprintln!("warning: failed to create tray icon: {e}");
            None
        }
    };

    (tray, Some(exit_id))
}

/// A plain 16x16 filled square rather than a bundled .ico: this is a
/// placeholder good enough to make the tray icon visible and clickable.
/// Swap in a real icon asset if/when one exists.
fn tray_icon_image() -> Result<Icon, tray_icon::BadIcon> {
    const SIZE: u32 = 16;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for _ in 0..(SIZE * SIZE) {
        rgba.extend_from_slice(&[70, 130, 200, 255]);
    }
    Icon::from_rgba(rgba, SIZE, SIZE)
}
