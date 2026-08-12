// Shell COM chain from Raymond Chen's "get info from the foreground Explorer window" sample (Old New Thing, 2004-07-20) — no Explorer-side extension needed.
// IShellFolderViewDual's IDispatch-only automation API is avoided: windows-rs only generates caller-side methods for vtable interfaces, and everything below is vtable-based.

use std::path::PathBuf;
use windows::core::Interface;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IServiceProvider, CLSCTX_LOCAL_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Variant::{VARENUM, VARIANT, VT_I4};
use windows::Win32::UI::Shell::{
    IShellBrowser, IShellItemArray, IShellWindows, IWebBrowserApp, ShellWindows,
    SID_STopLevelBrowser, SIGDN_FILESYSPATH, SVGIO_SELECTION,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

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

fn int_variant(value: i32) -> VARIANT {
    let mut variant = VARIANT::default();
    unsafe {
        (*variant.Anonymous.Anonymous).vt = VARENUM(VT_I4.0 as u16);
        (*variant.Anonymous.Anonymous).Anonymous.lVal = value;
    }
    variant
}

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

        let browser_app: IWebBrowserApp = match item.cast() {
            Ok(b) => b,
            Err(_) => continue,
        };

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
        // PWSTR doesn't free its Shell-allocated memory on drop.
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
