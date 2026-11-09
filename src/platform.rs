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
        const DWMWA_CAPTION_COLOR: u32 = 35;
        const DWMWA_BORDER_COLOR: u32 = 34;
        let dark_color: u32 = 0x000a0a0a;
        let backdrop = if on { DWMSBT_TRANSIENTWINDOW } else { DWMSBT_NONE };
        // SAFETY: valid HWND from winit; pointers reference live locals of the stated size.
        let ok = unsafe {
            DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, (&dark as *const i32).cast(), 4);
            DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, (&dark_color as *const u32).cast(), 4);
            DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, (&dark_color as *const u32).cast(), 4);
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

/// Sets whole-window opacity using Windows layered window attributes (0.0 to 1.0).
/// When opacity < 1.0, DWM composites the window over the desktop with true alpha,
/// without washing out dark backgrounds to white.
pub fn set_window_opacity(window: &impl HasWindowHandle, opacity: f32) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes,
            SetWindowLongPtrW, WS_EX_LAYERED,
        };
        let Some(hwnd) = hwnd(window) else { return };
        let opacity = opacity.clamp(0.2, 1.0);
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            if opacity >= 0.999 {
                if style & WS_EX_LAYERED as isize != 0 {
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style & !(WS_EX_LAYERED as isize));
                }
            } else {
                if style & WS_EX_LAYERED as isize == 0 {
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_LAYERED as isize);
                }
                let alpha = (opacity * 255.0).round() as u8;
                SetLayeredWindowAttributes(hwnd, 0, alpha, LWA_ALPHA);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (window, opacity);
    }
}

/// Exclude a window with the specified title from screen capture (OBS, Discord, ShareX).
#[allow(dead_code)]
pub fn exclude_from_capture_by_title(title: &str) -> bool {
    setup_overlay_window(title, Option::<&eframe::Frame>::None, 1.0)
}

/// Sets capture exclusion on the overlay window and binds it to the main window as owner
/// so it follows the main window's z-order, stays hidden when minimized, and is never captured.
pub fn setup_overlay_window(
    overlay_title: &str,
    main_window: Option<&impl HasWindowHandle>,
    opacity: f32,
) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            FindWindowW, GWLP_HWNDPARENT, GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA,
            SetLayeredWindowAttributes, SetWindowDisplayAffinity, SetWindowLongPtrW,
            WDA_EXCLUDEFROMCAPTURE, WS_EX_LAYERED, WS_EX_TRANSPARENT,
        };
        let wide: Vec<u16> = overlay_title.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: FindWindowW called with valid null-terminated wide string.
        unsafe {
            let overlay_hwnd = FindWindowW(std::ptr::null(), wide.as_ptr());
            if !overlay_hwnd.is_null() {
                SetWindowDisplayAffinity(overlay_hwnd, WDA_EXCLUDEFROMCAPTURE);
                let ex_style = GetWindowLongPtrW(overlay_hwnd, GWL_EXSTYLE);
                if ex_style & (WS_EX_TRANSPARENT as isize | WS_EX_LAYERED as isize)
                    != (WS_EX_TRANSPARENT as isize | WS_EX_LAYERED as isize)
                {
                    SetWindowLongPtrW(
                        overlay_hwnd,
                        GWL_EXSTYLE,
                        ex_style | WS_EX_TRANSPARENT as isize | WS_EX_LAYERED as isize,
                    );
                }
                let alpha = (opacity.clamp(0.2, 1.0) * 255.0).round() as u8;
                SetLayeredWindowAttributes(overlay_hwnd, 0, alpha, LWA_ALPHA);
                if let Some(main) = main_window
                    && let Some(main_hwnd) = hwnd(main) {
                        SetWindowLongPtrW(overlay_hwnd, GWLP_HWNDPARENT, main_hwnd as isize);
                    }
                return true;
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (overlay_title, main_window, opacity);
    }
    false
}

/// Month of the PC's local date, 1..=12.
#[cfg(windows)]
pub fn local_month() -> u16 {
    let mut st = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::SYSTEMTIME>() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut st) };
    st.wMonth
}

/// Without a local-time API this falls back to the UTC month (off by a few hours at worst).
#[cfg(not(windows))]
pub fn local_month() -> u16 {
    let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Howard Hinnant's civil_from_days, month only.
    let z = days + 719_468;
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    (if mp < 10 { mp + 3 } else { mp - 9 }) as u16
}

/// Snow season: December to February.
pub fn is_winter() -> bool {
    matches!(local_month(), 12 | 1 | 2)
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(windows)]
    fn test_layered_window_and_dwm() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, DefWindowProcW, RegisterClassW, WNDCLASSW,
            WS_POPUP, GWL_EXSTYLE, WS_EX_LAYERED, SetWindowLongPtrW, SetLayeredWindowAttributes, LWA_ALPHA,
        };
        use windows_sys::Win32::Graphics::Dwm::{
            DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
            DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
        };
        use windows_sys::Win32::UI::Controls::MARGINS;

        unsafe {
            let class_name: Vec<u16> = "TestDwmClass\0".encode_utf16().collect();
            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(DefWindowProcW),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: std::ptr::null_mut(),
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
            };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                WS_POPUP,
                100, 100, 200, 200,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            assert!(!hwnd.is_null());

            let dark: i32 = 1;
            let res_dark = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, (&dark as *const i32).cast(), 4);
            let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
            let res_margins = DwmExtendFrameIntoClientArea(hwnd, &m);
            let backdrop = DWMSBT_TRANSIENTWINDOW;
            let res_backdrop = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE as u32, (&backdrop as *const i32).cast(), 4);

            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, WS_EX_LAYERED as isize);
            let res_layered = SetLayeredWindowAttributes(hwnd, 0, 200, LWA_ALPHA);
            assert_ne!(res_layered, 0);

            println!("res_dark: {res_dark:#x}, res_margins: {res_margins:#x}, res_backdrop: {res_backdrop:#x}");
            DestroyWindow(hwnd);
        }
    }
}
