//! Finds the UI process's main window and brings it to the front.
//!
//! Searched by process rather than by `FindWindowW` with the title: the
//! title is just "GhostPerf", which isn't unique to this window, and the
//! process ID is what the caller already has.

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    SetForegroundWindow, ShowWindow, GW_OWNER, SW_RESTORE,
};

struct Search {
    pid: u32,
    found: Option<HWND>,
}

/// `EnumWindows` callback: stops at the first visible, unowned top-level
/// window of `Search::pid`. Owned windows are dialogs and popups; hidden
/// ones are helper windows (winit and wgpu create some).
unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `&mut Search` passed to `EnumWindows` below,
    // which outlives the enumeration.
    let search = unsafe { &mut *(lparam.0 as *mut Search) };
    let mut pid = 0u32;
    // SAFETY: plain queries on a window handle the system just gave us.
    let is_main = unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid == search.pid && IsWindowVisible(hwnd).as_bool() && GetWindow(hwnd, GW_OWNER).is_err()
    };
    if is_main {
        search.found = Some(hwnd);
        return BOOL(0); // stop enumerating
    }
    BOOL(1)
}

/// `true` if `pid` has a main window, which is then restored if minimized
/// and brought to the front.
pub fn bring_to_front(pid: u32) -> bool {
    let mut search = Search { pid, found: None };
    // SAFETY: `visit` only uses `lparam` as the `Search` it points to. The
    // result is an error when `visit` stops the enumeration early, which is
    // the success case here, so it's ignored in favor of `search.found`.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize)) };
    let Some(hwnd) = search.found else {
        return false;
    };
    // SAFETY: `hwnd` is a live top-level window found just above.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        // Allowed here: Windows lets the process that received the last
        // input event set the foreground window, and the tray just handled
        // the menu click. If it's refused anyway, Windows flashes the
        // window's taskbar button instead, which still points at it.
        if !SetForegroundWindow(hwnd).as_bool() {
            log::warn!("Windows refused to bring the UI window forward; it flashes instead");
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Machine-dependent: needs a running `ghostperf-ui`, ideally minimized
    /// first. Run with
    /// `$env:GP_UI_PID = <pid>; cargo test -p tray -- --ignored brings_a_running_ui_forward`.
    #[test]
    #[ignore]
    fn brings_a_running_ui_forward() {
        let pid: u32 = std::env::var("GP_UI_PID")
            .expect("set GP_UI_PID to a running ghostperf-ui's process ID")
            .parse()
            .unwrap();
        assert!(bring_to_front(pid), "no main window found for pid {pid}");
        let mut search = Search { pid, found: None };
        // SAFETY: as in `bring_to_front`.
        let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize)) };
        let hwnd = search.found.unwrap();
        assert!(!unsafe { IsIconic(hwnd) }.as_bool(), "still minimized");
    }

    #[test]
    fn a_process_without_windows_is_not_found() {
        // This test process has no top-level windows of its own.
        assert!(!bring_to_front(std::process::id()));
    }
}
