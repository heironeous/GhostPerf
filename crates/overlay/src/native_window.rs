//! Win32 window tweaks for the overlay (Windows only; no-ops elsewhere), on
//! top of what egui's `ViewportBuilder` already gives it (`transparent`,
//! `mouse_passthrough`, `taskbar(false)`, `always_on_top` — see
//! `app.rs::viewport`), plus the one question about the window egui can't
//! answer: whether it's on any monitor at all ([`NativeWindow::on_screen`]).
//! Was `noactivate.rs` until it grew past that one flag.
//!
//! `mouse_passthrough` already means clicks can't land on this window at
//! all, and `with_active(false)` (also set in `app.rs`) covers *initial*
//! focus, but nothing in egui's cross-platform `ViewportBuilder` covers
//! "never take activation/focus, ever" — which is exactly what
//! `WS_EX_NOACTIVATE` is for, and exactly what TODO.md Phase 6 names
//! explicitly. Same raw-`HWND`-via-`raw_window_handle` approach as
//! `crates/ui/src/icon.rs`, for the same reason: no safe eframe/egui API
//! reaches this.
//!
//! The extended style is re-checked every frame ([`NativeWindow::ensure`])
//! rather than set once: winit rewrites `GWL_EXSTYLE` wholesale from its own
//! flags whenever one of them changes (`WindowFlags::apply_diff` in winit
//! 0.30), and it has no flag for these bits. Set once at creation, the
//! first Windows run found `WS_EX_NOACTIVATE` already gone by the time the
//! window was on screen, and a hotkey hide/show would drop it again.

pub struct NativeWindow {
    #[cfg(windows)]
    hwnd: Option<windows_impl::Hwnd>,
}

impl NativeWindow {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        #[cfg(windows)]
        {
            let hwnd = windows_impl::hwnd(cc)
                .inspect_err(|e| {
                    log::warn!(
                        "couldn't get the overlay's window handle ({e:#}); \
                         it may take focus despite mouse-passthrough"
                    )
                })
                .ok();
            if let Some(hwnd) = hwnd {
                windows_impl::drop_dwm_frame(hwnd);
            }
            let keeper = Self { hwnd };
            keeper.ensure(false);
            keeper
        }
        #[cfg(not(windows))]
        {
            let _ = cc;
            Self {}
        }
    }

    /// Puts the overlay's extended style back if winit has reset it, or
    /// switches it for edit mode (see [`overlay_ex_style`]). One
    /// `GetWindowLongPtrW` per frame when nothing changed.
    pub fn ensure(&self, editing: bool) {
        #[cfg(windows)]
        if let Some(hwnd) = self.hwnd {
            windows_impl::ensure_ex_style(hwnd, editing);
        }
        #[cfg(not(windows))]
        let _ = editing;
    }

    /// Moves the overlay back above other always-on-top windows. Being
    /// topmost only puts a window in the topmost band; within it, the
    /// window activated last is on top. On the first Windows run, a
    /// topmost fullscreen window (as many games use) activated after the
    /// overlay covered it completely.
    pub fn raise(&self) {
        #[cfg(windows)]
        if let Some(hwnd) = self.hwnd {
            windows_impl::raise(hwnd);
        }
    }

    /// Whether any part of the window is on a monitor. `None` where that
    /// can't be told (not Windows, or no window handle). egui only knows
    /// the size of the monitor the window is on, not where monitors are, so
    /// it can't say that a remembered position has become unreachable.
    pub fn on_screen(&self) -> Option<bool> {
        #[cfg(windows)]
        if let Some(hwnd) = self.hwnd {
            return Some(windows_impl::on_screen(hwnd));
        }
        None
    }
}

/// The pure part of [`windows_impl::ensure_ex_style`]: the extended style
/// the overlay should have, given what's set now.
///
/// - `NOACTIVATE`: never take focus from the game (see the module doc),
///   except in edit mode. egui only starts a window drag if the window has
///   focus (`ViewportCommand::StartDrag` in egui-winit 0.37), so with
///   `NOACTIVATE` a drag was silently ignored. While editing, the user is
///   deliberately clicking on the overlay anyway.
/// - `TOOLWINDOW` instead of `APPWINDOW`: keeps it out of Alt-Tab. egui's
///   `taskbar(false)` only removes the taskbar button (`ITaskbarList`), and
///   winit still sets `APPWINDOW`, which puts a window in Alt-Tab whatever
///   else is set.
#[cfg_attr(not(windows), allow(dead_code))] // only called by windows_impl; kept compiled for its test
fn overlay_ex_style(current: u32, editing: bool) -> u32 {
    const APPWINDOW: u32 = 0x0004_0000;
    const TOOLWINDOW: u32 = 0x0000_0080;
    const NOACTIVATE: u32 = 0x0800_0000;
    let style = (current | TOOLWINDOW) & !APPWINDOW;
    if editing {
        style & !NOACTIVATE
    } else {
        style | NOACTIVATE
    }
}

#[cfg(windows)]
mod windows_impl {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE,
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWM_WINDOW_CORNER_PREFERENCE,
    };
    use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONULL};
    use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_RUNNING_D3D_FULL_SCREEN};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST,
        SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
    };

    /// The raw handle as an integer: `HWND` wraps a pointer, so it isn't
    /// `Send`, which `eframe::App` requires of the app holding it.
    #[derive(Clone, Copy)]
    pub struct Hwnd(isize);

    impl Hwnd {
        fn get(self) -> HWND {
            HWND(self.0 as *mut _)
        }
    }

    pub fn hwnd(cc: &eframe::CreationContext<'_>) -> anyhow::Result<Hwnd> {
        let RawWindowHandle::Win32(handle) = cc.window_handle()?.as_raw() else {
            anyhow::bail!("not a Win32 window");
        };
        Ok(Hwnd(handle.hwnd.get()))
    }

    pub fn ensure_ex_style(hwnd: Hwnd, editing: bool) {
        // SAFETY: plain Win32 calls on our own window, adjusting individual
        // bits of its existing extended style rather than replacing it.
        unsafe {
            let current = GetWindowLongPtrW(hwnd.get(), GWL_EXSTYLE) as u32;
            let wanted = super::overlay_ex_style(current, editing);
            if wanted == current {
                return;
            }
            SetWindowLongPtrW(hwnd.get(), GWL_EXSTYLE, wanted as isize);
            // Cached frame data only picks up a style change after this.
            let _ = SetWindowPos(
                hwnd.get(),
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            log::debug!("overlay extended style set: {current:#x} -> {wanted:#x}");
        }
    }

    pub fn on_screen(hwnd: Hwnd) -> bool {
        // SAFETY: a query on our own window; the handle it returns isn't kept.
        !unsafe { MonitorFromWindow(hwnd.get(), MONITOR_DEFAULTTONULL) }.is_invalid()
    }

    pub fn raise(hwnd: Hwnd) {
        // Not over an exclusive-mode Direct3D fullscreen app: the overlay
        // can't be drawn there anyway (see TODO.md Phase 6), and a window
        // covering one can make the game leave fullscreen.
        // SAFETY: no arguments; returns a plain enum value.
        if unsafe { SHQueryUserNotificationState() } == Ok(QUNS_RUNNING_D3D_FULL_SCREEN) {
            return;
        }
        // SAFETY: z-order change only on our own window, without moving,
        // sizing or activating it.
        unsafe {
            let _ = SetWindowPos(
                hwnd.get(),
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }

    /// Windows 11 draws a 1 px border and rounds the corners of every
    /// top-level window, decorations or not. On a transparent overlay that
    /// showed as a grey outline around the whole window, bigger than the
    /// readout's own background. Best-effort: these attributes don't exist
    /// before Windows 11, where there's no border to remove.
    pub fn drop_dwm_frame(hwnd: Hwnd) {
        // SAFETY: each call passes a pointer to a local of exactly the size
        // it states, on our own window.
        unsafe {
            let none = DWMWA_COLOR_NONE;
            let _ = DwmSetWindowAttribute(
                hwnd.get(),
                DWMWA_BORDER_COLOR,
                (&raw const none).cast(),
                size_of_val(&none) as u32,
            );
            let square: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_DONOTROUND;
            let _ = DwmSetWindowAttribute(
                hwnd.get(),
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&raw const square).cast(),
                size_of_val(&square) as u32,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_style_adds_noactivate_and_toolwindow_and_drops_appwindow() {
        // What the first Windows run found winit had left on the window:
        // APPWINDOW | LAYERED | WINDOWEDGE | TRANSPARENT | ACCEPTFILES | TOPMOST.
        let from_winit = 0x000C_0138;
        assert_eq!(overlay_ex_style(from_winit, false), 0x0808_01B8);
    }

    #[test]
    fn an_already_correct_style_is_left_alone() {
        let done = overlay_ex_style(0x000C_0138, false);
        assert_eq!(overlay_ex_style(done, false), done);
    }

    #[test]
    fn edit_mode_drops_noactivate_and_locking_restores_it() {
        let locked = overlay_ex_style(0x000C_0138, false);
        let editing = overlay_ex_style(locked, true);
        assert_eq!(editing, 0x0008_01B8);
        assert_eq!(overlay_ex_style(editing, false), locked);
    }
}
