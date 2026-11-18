//! One-time "GhostPerf is running in the tray" notification.
//!
//! The tray has no window, so starting it otherwise gives no sign that
//! anything happened: the icon may even be tucked into the notification
//! area's overflow (Windows 11 hides new icons there by default). This shows
//! a notification balloon from the tray icon on the first run per data
//! folder (so once for an install, once for each portable copy), then never
//! again. Showing it at every start would mean one at every sign-in with
//! "Start with Windows" on.
//!
//! The marker file is written only after the balloon was actually accepted.
//! At sign-in the taskbar may not exist yet when the tray starts, and then
//! the next start tries again rather than the notice being lost.

#[cfg(not(target_os = "windows"))]
mod generic_impl;
#[cfg(target_os = "windows")]
mod windows_impl;

#[cfg(not(target_os = "windows"))]
use generic_impl::show_balloon;
#[cfg(target_os = "windows")]
use windows_impl::show_balloon;

use std::path::Path;

use tray_icon::TrayIcon;

const MARKER_FILE: &str = "tray-welcome-shown";

/// Fits Win32's `szInfoTitle` (64 UTF-16 units including the terminator).
const TITLE: &str = "GhostPerf is running";
/// Fits Win32's `szInfo` (256 UTF-16 units including the terminator).
/// Windows 11 hides new tray icons in the overflow, hence the second sentence.
const TEXT: &str = "GhostPerf lives in the notification area. Click its icon to start \
                    recording or open the session viewer. Don't see it? Click the ^ \
                    arrow next to the clock.";

pub fn show_on_first_run(tray: &TrayIcon) {
    let marker = agent_config::data_dir().join(MARKER_FILE);
    if !is_first_run(&marker) {
        return;
    }
    match show_balloon(tray, TITLE, TEXT) {
        Ok(()) => {
            log::info!("showed the first-run notification");
            if let Err(e) = mark_shown(&marker) {
                // Worst case it's shown once more next start; not worth more.
                log::warn!("couldn't write {}: {e}", marker.display());
            }
        }
        Err(e) => log::warn!("first-run notification not shown, will retry next start: {e:#}"),
    }
}

fn is_first_run(marker: &Path) -> bool {
    !marker.exists()
}

fn mark_shown(marker: &Path) -> std::io::Result<()> {
    if let Some(dir) = marker.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        marker,
        "Delete this file to see GhostPerf's first-run tray notification again.\n",
    )
}

/// A NUL-terminated UTF-16 copy of `s` for a fixed-size Win32 string field,
/// truncated if needed so the terminator always fits.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))] // used by windows_impl
fn to_wide_fixed<const N: usize>(s: &str) -> [u16; N] {
    let mut out = [0u16; N];
    for (slot, unit) in out[..N - 1].iter_mut().zip(s.encode_utf16()) {
        *slot = unit;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_until_marked() {
        let dir =
            std::env::temp_dir().join(format!("ghostperf-welcome-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // The data folder may not exist yet; marking must create it.
        let marker = dir.join("nested").join(MARKER_FILE);

        assert!(is_first_run(&marker));
        mark_shown(&marker).unwrap();
        assert!(!is_first_run(&marker));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn message_fits_the_win32_fields_untruncated() {
        assert!(TITLE.encode_utf16().count() < 64);
        assert!(TEXT.encode_utf16().count() < 256);
        let wide: [u16; 256] = to_wide_fixed(TEXT);
        let len = wide.iter().position(|&u| u == 0).unwrap();
        assert_eq!(String::from_utf16(&wide[..len]).unwrap(), TEXT);
    }

    #[test]
    fn to_wide_fixed_truncates_and_keeps_the_terminator() {
        let wide: [u16; 4] = to_wide_fixed("abcdef");
        assert_eq!(wide, [b'a' as u16, b'b' as u16, b'c' as u16, 0]);
    }
}
