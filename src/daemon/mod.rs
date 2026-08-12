pub mod hotkey;
pub mod selection;

use std::sync::mpsc::Sender;
use std::time::Duration;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
};

pub fn run_message_loop(toggle_tx: Sender<()>) -> windows::core::Result<()> {
    let (_tray_icon, exit_id) = build_tray_icon();

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

fn tray_icon_image() -> Result<Icon, tray_icon::BadIcon> {
    const SIZE: u32 = 16;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for _ in 0..(SIZE * SIZE) {
        rgba.extend_from_slice(&[70, 130, 200, 255]);
    }
    Icon::from_rgba(rgba, SIZE, SIZE)
}
