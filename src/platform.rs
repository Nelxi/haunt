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
    civil_from_days(days).1 as u16
}

/// (year, month, day) of a day count since 1970-01-01 (Howard Hinnant's algorithm).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Day count since 1970-01-01 of a civil date (inverse of [`civil_from_days`]).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Seconds to add to UTC to get the PC's local time, right now.
#[cfg(windows)]
pub fn utc_offset_secs() -> i64 {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    let secs = |st: &SYSTEMTIME| {
        days_from_civil(st.wYear as i64, st.wMonth as u32, st.wDay as u32) * 86_400
            + st.wHour as i64 * 3600
            + st.wMinute as i64 * 60
            + st.wSecond as i64
    };
    // SAFETY: both fill a plain struct we own.
    let (mut local, mut utc) = unsafe { (std::mem::zeroed::<SYSTEMTIME>(), std::mem::zeroed::<SYSTEMTIME>()) };
    unsafe {
        GetLocalTime(&mut local);
        GetSystemTime(&mut utc);
    }
    // The two calls can straddle a second; offsets are whole minutes anyway.
    ((secs(&local) - secs(&utc)) as f64 / 60.0).round() as i64 * 60
}

#[cfg(not(windows))]
pub fn utc_offset_secs() -> i64 {
    0
}

/// "2026-11-09 10:32" in local time for a Unix timestamp.
pub fn format_local(unix: i64) -> String {
    let t = unix + utc_offset_secs();
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", s / 3600, s % 3600 / 60)
}

#[cfg(windows)]
fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Moves files and folders to the Recycle Bin, so a slip can be undone.
#[cfg(windows)]
pub fn to_recycle_bin(paths: &[std::path::PathBuf]) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::{FO_DELETE, SHFILEOPSTRUCTW, SHFileOperationW};
    if paths.is_empty() {
        return Ok(());
    }
    // A list of NUL-terminated paths, ended by an extra NUL.
    let mut from: Vec<u16> = paths.iter().flat_map(|p| wide(p.as_os_str())).collect();
    from.push(0);
    const FOF_SILENT: u16 = 0x4;
    const FOF_NOCONFIRMATION: u16 = 0x10;
    const FOF_ALLOWUNDO: u16 = 0x40;
    const FOF_NOERRORUI: u16 = 0x400;
    // SAFETY: the struct is zeroed (null handles and strings) and `from` outlives the call.
    let mut op = unsafe { std::mem::zeroed::<SHFILEOPSTRUCTW>() };
    op.wFunc = FO_DELETE;
    op.pFrom = from.as_ptr();
    op.fFlags = FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI;
    let r = unsafe { SHFileOperationW(&mut op) };
    if r != 0 {
        return Err(format!("0x{r:x}"));
    }
    if op.fAnyOperationsAborted != 0 {
        return Err("aborted".into());
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn to_recycle_bin(paths: &[std::path::PathBuf]) -> Result<(), String> {
    for p in paths {
        let r = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
        r.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Opens a file with its default app (or a folder in Explorer).
#[cfg(windows)]
pub fn open_path(path: &std::path::Path) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let verb = wide(std::ffi::OsStr::new("open"));
    let file = wide(path.as_os_str());
    // SAFETY: NUL-terminated strings that live through the call; null for the rest.
    unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), 1) };
}

#[cfg(not(windows))]
pub fn open_path(path: &std::path::Path) {
    let _ = std::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" }).arg(path).spawn();
}

/// Snow season: December to February.
pub fn is_winter() -> bool {
    matches!(local_month(), 12 | 1 | 2)
}

#[cfg(test)]
mod tests {
    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(super::civil_from_days(0), (1970, 1, 1));
        assert_eq!(super::civil_from_days(19_782), (2024, 2, 29));
        for d in (-800_000..800_000).step_by(997) {
            let (y, m, dd) = super::civil_from_days(d);
            assert_eq!(super::days_from_civil(y, m, dd), d);
        }
    }

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
