//! Reads the currently-selected file from the foreground Windows Explorer
//! window via Shell COM interfaces — the same mechanism behind Raymond
//! Chen's canonical "get info from the foreground Explorer window" sample
//! (Old New Thing, 2004-07-20) and tools like QuickLook. No Explorer-side
//! extension or hook is needed.
//!
//! Chain: `IShellWindows` (enumerate open Explorer windows) -> match the
//! one whose `HWND` is the foreground window -> `IServiceProvider` ->
//! `QueryService(SID_STopLevelBrowser)` -> `IShellBrowser` ->
//! `QueryActiveShellView` -> `IShellView` -> `GetItemObject::<IShellItemArray>
//! (SVGIO_SELECTION)` -> `GetItemAt(0)` -> `IShellItem::GetDisplayName`.
//!
//! Deliberately not used: `IShellFolderViewDual`/`FolderItems` (the
//! `IDispatch`-only "OLE automation" side of the Shell API). An earlier
//! draft of this file used that path and failed to compile — `windows-rs`
//! 0.62 does not generate caller-side inherent methods for pure
//! `IDispatch`-derived interfaces, only the `_Impl` trait used to
//! *implement* one. Everything from `IShellBrowser` onward above is a
//! vtable-based interface instead, which `windows-rs` does bind normally;
//! see e.g. `IShellView::GetItemObject`, confirmed against the crate's
//! published docs.
//!
//! CAUTION: still not verified against a real compile (no Windows/Rust
//! toolchain available in the environment this was written in). The one
//! remaining risk is `IShellWindows::Item` — its `_Impl` trait signature
//! (`fn Item(&self, index: &VARIANT) -> Result<IDispatch>`) matches the
//! call below exactly, and `IShellWindows` is declared `dual` (not
//! IDispatch-only) in the Windows SDK's IDL, so it should get an inherent
//! method the same way `IShellBrowser`/`IShellView` do — but a prior draft
//! hit a "method not found" error on this exact call that repeated
//! searching couldn't fully explain. If this specific line fails again,
//! that's the one to paste back.

use std::path::PathBuf;
use windows::core::Interface;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IServiceProvider, CLSCTX_LOCAL_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Shell::{
    IShellBrowser, IShellItemArray, IShellWindows, IWebBrowserApp, ShellWindows,
    SID_STopLevelBrowser, SIGDN_FILESYSPATH, SVGIO_SELECTION,
};
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

/// Builds a `VT_I4` VARIANT wrapping a plain integer index, for
/// `IShellWindows::Item`'s VARIANT-typed index parameter.
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
/// selected, or any COM call fails for any reason (missing Explorer
/// window, permissions, etc. — all treated as "no selection" rather than
/// an error the caller needs to handle differently).
pub fn selected_file_in_foreground_explorer() -> Option<PathBuf> {
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.is_invalid() {
        return None;
    }

    let shell_windows: IShellWindows =
        unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER).ok()? };

    // IShellWindows::Count returns i32 (a signed COM automation LONG), not
    // u32 — confirmed against IShellWindows_Impl's trait signature.
    let count = unsafe { shell_windows.Count().ok()? };
    for i in 0..count {
        let index_variant = int_variant(i);
        let item = match unsafe { shell_windows.Item(&index_variant) } {
            Ok(item) => item,
            Err(_) => continue,
        };

        let browser_app: IWebBrowserApp = match item.cast() {
            Ok(b) => b,
            Err(_) => continue,
        };

        // IWebBrowserApp::HWND returns SHANDLE_PTR, a pointer-sized
        // integer wrapper (not a raw pointer); unwrap its inner value
        // before converting, rather than casting between unrelated types.
        let hwnd = match unsafe { browser_app.HWND() } {
            Ok(h) => HWND(h.0 as *mut core::ffi::c_void),
            Err(_) => continue,
        };
        if hwnd != foreground {
            continue;
        }

        let service_provider: IServiceProvider = match browser_app.cast() {
            Ok(sp) => sp,
            Err(_) => continue,
        };
        let shell_browser: IShellBrowser =
            unsafe { service_provider.QueryService(&SID_STopLevelBrowser).ok()? };
        let shell_view = unsafe { shell_browser.QueryActiveShellView().ok()? };

        let selection: IShellItemArray =
            unsafe { shell_view.GetItemObject(SVGIO_SELECTION).ok()? };

        let selected_count = unsafe { selection.GetCount().ok()? };
        if selected_count == 0 {
            return None;
        }

        let first = unsafe { selection.GetItemAt(0).ok()? };
        let display_name = unsafe { first.GetDisplayName(SIGDN_FILESYSPATH).ok()? };
        let path_string = unsafe { display_name.to_string().ok()? };
        // display_name (a PWSTR) owns Shell-allocated memory that must be
        // freed with CoTaskMemFree; PWSTR itself doesn't do this on drop.
        unsafe {
            windows::Win32::System::Com::CoTaskMemFree(Some(
                display_name.as_ptr() as *mut _ as *const _
            ))
        };

        return Some(PathBuf::from(path_string));
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
