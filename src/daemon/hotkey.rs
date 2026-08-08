//! Global Space-key toggle via a `WH_KEYBOARD_LL` hook. Space is only
//! treated as a toggle when the foreground window belongs to Explorer, so
//! an ordinary Space press elsewhere (a text field, a game) passes through
//! untouched. `RegisterHotKey` was deliberately not used: a hotkey
//! registered without a modifier key intercepts that key everywhere on the
//! system, which would break typing Space in every other application.
//!
//! `WH_KEYBOARD_LL` requires a message loop running on the thread that
//! installed it; `install`/`uninstall` only set up and tear down the hook,
//! the pump itself lives in `daemon::run_message_loop` so it can also
//! service the tray icon.

use std::sync::mpsc::Sender;
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetWindowThreadProcessId, SetWindowsHookExW,
    UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

const VK_SPACE: u32 = 0x20;

/// Set by `install`, read from the hook procedure. The hook procedure is a
/// plain `extern "system" fn` (Windows calls it directly; no closure
/// capture is possible), so the sender has to reach it through a static.
static TOGGLE_SENDER: std::sync::OnceLock<Sender<()>> = std::sync::OnceLock::new();

/// Installs the hook. Each Space key-down (not key-up — one toggle per
/// press is what "press once to show, press again to hide" needs) while
/// Explorer is the foreground window sends `()` on `toggle_tx`.
pub fn install(toggle_tx: Sender<()>) -> windows::core::Result<HHOOK> {
    TOGGLE_SENDER
        .set(toggle_tx)
        .expect("hotkey::install must only be called once");

    unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(hook_proc),
            Some(HINSTANCE::default()),
            0,
        )
    }
}

pub fn uninstall(hook: HHOOK) {
    unsafe {
        let _ = UnhookWindowsHookEx(hook);
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }

    let is_keydown = wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN;
    if is_keydown {
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if kb.vkCode == VK_SPACE && foreground_window_is_explorer() {
            if let Some(tx) = TOGGLE_SENDER.get() {
                let _ = tx.send(());
            }
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Checking the owning process name (rather than matching a window class
/// like `CabinetWClass`) is more robust across Windows versions and covers
/// both File Explorer windows and the desktop, which is also owned by
/// explorer.exe.
fn foreground_window_is_explorer() -> bool {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return false;
    }

    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return false;
    }

    process_name_by_pid(pid)
        .map(|name| name.eq_ignore_ascii_case("explorer.exe"))
        .unwrap_or(false)
}

fn process_name_by_pid(pid: u32) -> Option<String> {
    use windows::Win32::System::ProcessStatus::K32GetModuleBaseNameW;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };

    unsafe {
        let handle =
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, false, pid).ok()?;
        let mut buf = [0u16; 260];
        let len = K32GetModuleBaseNameW(handle, None, &mut buf);
        if len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}
