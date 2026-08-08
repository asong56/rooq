//! Reads the currently-selected file from the foreground Windows Explorer
//! window via Shell Automation COM (`ShellWindows` -> matching `HWND` ->
//! `Document.SelectedItems()`). This is the same mechanism Explorer's own
//! "Copy as path" and similar shell tools use; no Explorer-side extension
//! or hook is needed.

use std::path::PathBuf;
use windows::core::{Interface, Variant};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_LOCAL_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{IShellFolderViewDual, IShellWindows, IWebBrowserApp, ShellWindows};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

/// RAII guard for `CoInitializeEx`/`CoUninitialize` pairing on the calling
/// thread. COM apartment state is per-thread, so this must be held for the
/// lifetime of any COM calls made from that thread.
pub struct ComGuard;

impl ComGuard {
    pub fn init() -> windows::core::Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
        Ok(Self)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// Returns the path of the first selected item in the foreground Explorer
/// window, or `None` if the foreground window isn't Explorer, nothing is
/// selected, or the COM calls fail for any reason (missing Explorer window,
/// permissions, etc. — all treated as "no selection" rather than an error
/// the caller needs to handle differently).
pub fn selected_file_in_foreground_explorer() -> Option<PathBuf> {
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.is_invalid() {
        return None;
    }

    let shell_windows: IShellWindows =
        unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER).ok()? };

    let count = unsafe { shell_windows.Count().ok()? };
    for i in 0..count {
        // NOTE: this VARIANT construction is the one call in this file not
        // verified against a real compile (no Windows/Rust toolchain in
        // the environment this was written in). If `Variant::from(i32)`
        // doesn't satisfy `IShellWindows::Item`'s `&VARIANT` parameter,
        // build a raw `windows::Win32::System::Variant::VARIANT` with
        // `vt: VT_I4` and the value in the `Anonymous.Anonymous.Anonymous`
        // union instead — see the VARIANT structure docs.
        let index_variant = Variant::from(i);
        let item = match unsafe { shell_windows.Item(&index_variant) } {
            Ok(item) => item,
            Err(_) => continue,
        };

        let browser: IWebBrowserApp = match item.cast() {
            Ok(b) => b,
            Err(_) => continue,
        };

        let hwnd = match unsafe { browser.HWND() } {
            Ok(h) => HWND(h as isize as *mut core::ffi::c_void),
            Err(_) => continue,
        };
        if hwnd != foreground {
            continue;
        }

        let document = unsafe { browser.Document().ok()? };
        let view: IShellFolderViewDual = document.cast().ok()?;
        let selected = unsafe { view.SelectedItems().ok()? };

        let selected_count = unsafe { selected.Count().ok()? };
        if selected_count <= 0 {
            return None;
        }

        let first = unsafe { selected.Item(&Variant::from(0i32)).ok()? };
        let path = unsafe { first.Path().ok()? };
        return Some(PathBuf::from(path.to_string()));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn com_guard_inits_and_drops_without_panicking() {
        let _guard = ComGuard::init().expect("CoInitializeEx should succeed on a fresh thread");
    }
}
