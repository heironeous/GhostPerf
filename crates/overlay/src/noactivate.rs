//! `WS_EX_NOACTIVATE` on the overlay window (Windows only), on top of what
//! egui's `ViewportBuilder` already gives it (`transparent`,
//! `mouse_passthrough`, `taskbar(false)`, `always_on_top` — see
//! `app.rs::viewport`).
//!
//! `mouse_passthrough` already means clicks can't land on this window at
//! all, and `with_active(false)` (also set in `app.rs`) covers *initial*
//! focus, but nothing in egui's cross-platform `ViewportBuilder` covers
//! "never take activation/focus, ever" — which is exactly what
//! `WS_EX_NOACTIVATE` is for, and exactly what TODO.md Phase 6 names
//! explicitly. Same raw-`HWND`-via-`raw_window_handle` approach as
//! `crates/ui/src/icon.rs`, for the same reason: no safe eframe/egui API
//! reaches this.

pub fn apply(cc: &eframe::CreationContext<'_>) {
    #[cfg(windows)]
    if let Err(e) = windows_impl::set_noactivate(cc) {
        log::warn!(
            "couldn't set WS_EX_NOACTIVATE on the overlay window ({e:#}); \
             it may steal focus despite mouse-passthrough"
        );
    }
    #[cfg(not(windows))]
    let _ = cc;
}

#[cfg(windows)]
mod windows_impl {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE,
    };

    pub fn set_noactivate(cc: &eframe::CreationContext<'_>) -> anyhow::Result<()> {
        let RawWindowHandle::Win32(handle) = cc.window_handle()?.as_raw() else {
            anyhow::bail!("not a Win32 window");
        };
        let hwnd = HWND(handle.hwnd.get() as *mut _);
        // SAFETY: plain Win32 calls on our own window, OR-ing one bit into
        // its existing extended style rather than replacing it wholesale.
        unsafe {
            let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, current | WS_EX_NOACTIVATE.0 as isize);
        }
        Ok(())
    }
}
