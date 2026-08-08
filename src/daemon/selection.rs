//! Reads the currently-selected file from the foreground Windows Explorer
//! window via Shell Automation COM (`ShellWindows` -> matching `HWND` ->
//! `Document.SelectedItems()`). This is the same mechanism Explorer's own
//! "Copy as path" and similar shell tools use; no Explorer-side extension
//! or hook is needed.
//!
//! CAUTION: this file has not been verified against a real compile (no
//! Windows/Rust toolchain in the environment it was written in). The
//! `IShellWindows`/`IShellFolderViewDual` automation surface used below
//! comes from `windows-rs`'s IDispatch-derived bindings, which have proven
//! easy to get wrong blind — `IShellWindows::Item` and
//! `FolderItems::Item` both failed to resolve as written in an earlier
//! draft (see project history). The VARIANT construction below follows a
//! pattern confirmed from a working windows-rs 0.6x sample; the `Item`
//! calls' exact signatures are the most likely remaining source of
//! compile errors and should be checked against whatever `windows`
//! version is actually pinned in Cargo.lock.

use std::path::PathBuf;
use windows::core::Interface;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_LOCAL_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
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

/// Builds a `VT_I4` VARIANT wrapping a plain integer index, for calls like
/// `IShellWindows::Item` that take a VARIANT-typed index. Wrapped in its
/// own function since VARIANT's union layout is otherwise easy to get
/// wrong (see module doc).
fn int_variant(value: i32) -> VARIANT {
    let mut variant = VARIANT::default();
    unsafe {
        (*variant.Anonymous.Anonymous).vt = VT_I4.0 as u16;
        (*variant.Anonymous.Anonymous).Anonymous.lVal = value;
    }
    variant
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
        let index_variant = int_variant(i);
        let item = match unsafe { shell_windows.Item(&index_variant) } {
            Ok(item) => item,
            Err(_) => continue,
        };

        let browser: IWebBrowserApp = match item.cast() {
            Ok(b) => b,
            Err(_) => continue,
        };

        // IWebBrowserApp::HWND returns SHANDLE_PTR (a pointer-sized
        // integer wrapper, not a raw pointer); its inner value needs to
        // go through an explicit conversion rather than a bare `as` cast
        // between unrelated types.
        let hwnd = match unsafe { browser.HWND() } {
            Ok(h) => HWND(h.0 as *mut core::ffi::c_void),
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

        let first_variant = int_variant(0);
        let first = unsafe { selected.Item(&first_variant).ok()? };
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

    #[test]
    fn int_variant_roundtrips_the_value() {
        let v = int_variant(42);
        let read_back = unsafe { (*v.Anonymous.Anonymous).Anonymous.lVal };
        assert_eq!(read_back, 42);
    }
}
