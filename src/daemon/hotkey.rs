// RegisterHotKey without a modifier grabs that key system-wide, breaking Space in every other app — hence the low-level hook, gated on Explorer being foreground.

use std::sync::mpsc::Sender;
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetWindowThreadProcessId, SetWindowsHookExW,
    UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

const VK_SPACE: u32 = 0x20;

static TOGGLE_SENDER: std::sync::OnceLock<Sender<()>> = std::sync::OnceLock::new();

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

// Process name, not window class: also covers the desktop (explorer.exe) and is more stable across Windows versions.
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
