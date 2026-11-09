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

/// Turns the acrylic backdrop on or off. Returns whether blur is active.
pub fn set_blur(window: &impl HasWindowHandle, on: bool) -> bool {
    if on {
        match window_vibrancy::apply_acrylic(window, Some((0, 0, 0, 0))) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("acrylic blur unavailable: {e}");
                false
            }
        }
    } else {
        let _ = window_vibrancy::clear_acrylic(window);
        false
    }
}
