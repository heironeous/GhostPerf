//! The UI window's icon, from `assets/icon/` like every other GhostPerf
//! binary's (see `assets/README.md`: the violet ghost with the amber
//! magnifying glass).
//!
//! Without this, eframe gives the window its own default icon (the egui
//! logo), which is what showed in the title bar, taskbar and Alt-Tab. Only
//! the exe file itself (in Explorer) had the GhostPerf icon, via `build.rs`.
//!
//! On Windows the window icons are loaded from that same embedded `.ico`
//! (resource 1) at the exact sizes Windows asks for: the small icon (title
//! bar, 24 px at 150% scaling) and the big one (Alt-Tab and taskbar, 48 px).
//! That way the title bar gets the hand-drawn small frames, which use a
//! simplified ghost at 32 px and below. eframe's own icon setter can't do
//! that: it takes one RGBA image and resamples it to both sizes. So eframe
//! is given an *empty* icon, which makes it, and winit, set none, and
//! [`apply`] sets both with `WM_SETICON` once the window exists. The tray
//! loads its notification-area icons the same way (`crates/tray/src/icons.rs`).
//!
//! On other platforms, or if loading the resource fails, the 256 px PNG
//! compiled into the binary is used through eframe instead.

use std::sync::Arc;

use egui::IconData;

const PNG: &[u8] = include_bytes!("../../../assets/icon/png/ghostperf-ui-256.png");

fn png_icon() -> Arc<IconData> {
    // A compile-time constant checked by the test below, so a failure here is
    // a build problem, not a runtime condition.
    Arc::new(eframe::icon_data::from_png_bytes(PNG).expect("embedded UI icon PNG is valid"))
}

/// The icon for `NativeOptions`' viewport. Empty on Windows, where [`apply`]
/// sets the real one from the exe's resources.
pub fn viewport_icon() -> Arc<IconData> {
    if cfg!(windows) {
        Arc::new(IconData::default())
    } else {
        png_icon()
    }
}

/// Call from the app creator, once the window exists.
pub fn apply(cc: &eframe::CreationContext<'_>) {
    #[cfg(windows)]
    if let Err(e) = windows_impl::set_from_resource(cc) {
        log::warn!(
            "couldn't set the window icon from the exe's resources ({e:#}); using the embedded PNG"
        );
        cc.egui_ctx
            .send_viewport_cmd(egui::ViewportCommand::Icon(Some(png_icon())));
    }
    #[cfg(not(windows))]
    let _ = cc;
}

#[cfg(windows)]
mod windows_impl {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows::Win32::UI::WindowsAndMessaging::{
        LoadImageW, SendMessageW, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXICON,
        SM_CXSMICON, SM_CYICON, SM_CYSMICON, WM_SETICON,
    };

    /// `build.rs` embeds `ghostperf-ui.ico` with `set_icon`, which is
    /// resource ID 1 (as for the tray's idle icon).
    const RESOURCE_ID: usize = 1;

    pub fn set_from_resource(cc: &eframe::CreationContext<'_>) -> anyhow::Result<()> {
        let RawWindowHandle::Win32(handle) = cc.window_handle()?.as_raw() else {
            anyhow::bail!("not a Win32 window");
        };
        let hwnd = HWND(handle.hwnd.get() as *mut _);
        // SAFETY: plain Win32 calls on our own window and module. The icons
        // aren't LR_SHARED, so they're ours to keep, and the window uses them
        // for the life of the process, so they're never destroyed.
        unsafe {
            let module = GetModuleHandleW(None)?;
            // The window's own DPI, not the process default: with per-monitor
            // DPI awareness the two differ on a mixed-DPI multi-monitor setup.
            let dpi = GetDpiForWindow(hwnd);
            for (which, cx, cy) in [
                (ICON_SMALL, SM_CXSMICON, SM_CYSMICON),
                (ICON_BIG, SM_CXICON, SM_CYICON),
            ] {
                let (w, h) = (
                    GetSystemMetricsForDpi(cx, dpi),
                    GetSystemMetricsForDpi(cy, dpi),
                );
                let icon = LoadImageW(
                    Some(HINSTANCE(module.0)),
                    // MAKEINTRESOURCEW: a small integer ID passed as a pointer.
                    PCWSTR(RESOURCE_ID as *const u16),
                    IMAGE_ICON,
                    w,
                    h,
                    LR_DEFAULTCOLOR,
                )?;
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(which as usize)),
                    Some(LPARAM(icon.0 as isize)),
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_png_decodes_to_a_square_icon() {
        let icon = png_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }

    #[test]
    fn eframe_gets_an_empty_icon_only_on_windows() {
        // Empty is what makes eframe and winit skip setting their own icon
        // (and so not override ours); see the module docs.
        assert_eq!(viewport_icon().is_empty(), cfg!(windows));
    }
}
