//! Windows-specific window chrome: rounded corners and acrylic blur.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// Ask DWM for Windows 11 rounded corners on our undecorated window.
pub fn round_corners(window: &impl HasWindowHandle) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Graphics::Dwm::{
            DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
        };
        let Ok(handle) = window.window_handle() else { return };
        if let RawWindowHandle::Win32(h) = handle.as_raw() {
            let pref = DWMWCP_ROUND;
            // SAFETY: valid HWND from winit; pointer/size describe a live i32.
            unsafe {
                DwmSetWindowAttribute(
                    h.hwnd.get() as _,
                    DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                    (&pref as *const i32).cast(),
                    std::mem::size_of::<i32>() as u32,
                );
            }
        }
    }
    #[cfg(not(windows))]
    let _ = window;
}

/// Turns the acrylic backdrop on or off. Returns whether it is active.
///
/// Windows 11 renders the system backdrop (DWMSBT) only inside the DWM frame, so
/// the frame is extended over the whole client area; our GL surface is
/// per-pixel transparent on top of it. `SetWindowCompositionAttribute` blur is
/// not used: with an OpenGL surface it makes DWM treat the window as opaque.
pub fn set_blur(window: &impl HasWindowHandle, on: bool) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Graphics::Dwm::{
            DWMSBT_NONE, DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
            DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
        };
        use windows_sys::Win32::UI::Controls::MARGINS;
        let Some(hwnd) = hwnd(window) else { return false };
        let m = if on { -1 } else { 0 };
        let margins = MARGINS { cxLeftWidth: m, cxRightWidth: m, cyTopHeight: m, cyBottomHeight: m };
        let dark: i32 = 1;
        let backdrop = if on { DWMSBT_TRANSIENTWINDOW } else { DWMSBT_NONE };
        // SAFETY: valid HWND from winit; pointers reference live locals of the stated size.
        let ok = unsafe {
            DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, (&dark as *const i32).cast(), 4);
            let a = DwmExtendFrameIntoClientArea(hwnd, &margins);
            let b = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE as u32, (&backdrop as *const i32).cast(), 4);
            a == 0 && b == 0
        };
        if on {
            hide_caption_buttons(window);
        }
        if on && !ok {
            log::warn!("acrylic backdrop unavailable (needs Windows 11 22H2+)");
        }
        on && ok
    }
    #[cfg(not(windows))]
    {
        let _ = (window, on);
        false
    }
}

/// DWM draws the system caption buttons into an extended frame when the window
/// has a system menu; we draw our own, so drop the style bit. Cheap to call
/// every frame (winit may restore the style on maximize/restore).
pub fn hide_caption_buttons(window: &impl HasWindowHandle) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GWL_STYLE, GetWindowLongPtrW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
            SetWindowLongPtrW, SetWindowPos, WS_SYSMENU,
        };
        let Some(hwnd) = hwnd(window) else { return };
        // SAFETY: valid HWND owned by this thread's event loop.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
            if style & WS_SYSMENU as isize != 0 {
                SetWindowLongPtrW(hwnd, GWL_STYLE, style & !(WS_SYSMENU as isize));
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }
    #[cfg(not(windows))]
    let _ = window;
}

#[cfg(windows)]
fn hwnd(window: &impl HasWindowHandle) -> Option<windows_sys::Win32::Foundation::HWND> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as _),
        _ => None,
    }
}

/// Exclude a window with the specified title from screen capture (OBS, Discord, ShareX).
#[allow(dead_code)]
pub fn exclude_from_capture_by_title(title: &str) -> bool {
    setup_overlay_window(title, Option::<&eframe::Frame>::None)
}

/// Sets capture exclusion on the overlay window and binds it to the main window as owner
/// so it follows the main window's z-order, stays hidden when minimized, and is never captured.
pub fn setup_overlay_window(overlay_title: &str, main_window: Option<&impl HasWindowHandle>) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            FindWindowW, GWLP_HWNDPARENT, SetWindowDisplayAffinity, SetWindowLongPtrW,
            WDA_EXCLUDEFROMCAPTURE,
        };
        let wide: Vec<u16> = overlay_title.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: FindWindowW called with valid null-terminated wide string.
        unsafe {
            let overlay_hwnd = FindWindowW(std::ptr::null(), wide.as_ptr());
            if !overlay_hwnd.is_null() {
                SetWindowDisplayAffinity(overlay_hwnd, WDA_EXCLUDEFROMCAPTURE);
                if let Some(main) = main_window {
                    if let Some(main_hwnd) = hwnd(main) {
                        SetWindowLongPtrW(overlay_hwnd, GWLP_HWNDPARENT, main_hwnd as isize);
                    }
                }
                return true;
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (overlay_title, main_window);
    }
    false
}

