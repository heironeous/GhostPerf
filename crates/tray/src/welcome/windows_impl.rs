//! The balloon itself: `Shell_NotifyIconW(NIM_MODIFY)` with `NIF_INFO` on the
//! icon `tray-icon` already added. Windows 10/11 show it as a toast.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_INFO, NIIF_INFO, NIIF_LARGE_ICON, NIIF_USER, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, LoadImageW, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXICON, SM_CYICON,
};

use tray_icon::TrayIcon;

use super::to_wide_fixed;

/// The idle icon's resource ID (see `build.rs` and `icons.rs`).
const ICON_RESOURCE_ID: usize = 1;

/// A shell icon is addressed by (window, ID). `tray-icon` exposes the window
/// but not the ID, which it takes from a process-wide counter starting at 1
/// (shared with `TrayIconId` generation, so it isn't simply 1). The window is
/// created for this one icon, so the first ID the shell accepts is ours;
/// NIM_MODIFY on an ID that isn't registered fails without side effects.
const MAX_ICON_ID: u32 = 16;

pub fn show_balloon(tray: &TrayIcon, title: &str, text: &str) -> anyhow::Result<()> {
    // With notifications switched off system-wide, the shell still accepts
    // the balloon and silently drops it, and the caller would then mark the
    // notice as shown. Fail instead, so it appears on the first start after
    // notifications are turned back on. (Per-app and Do Not Disturb settings
    // can't be checked as simply; a balloon suppressed by those is lost.)
    if toasts_disabled() {
        anyhow::bail!("notifications are turned off in Windows settings");
    }
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(tray.window_handle() as *mut _),
        uFlags: NIF_INFO,
        szInfoTitle: to_wide_fixed(title),
        szInfo: to_wide_fixed(text),
        dwInfoFlags: NIIF_INFO,
        ..Default::default()
    };
    if let Some(icon) = large_app_icon() {
        nid.hBalloonIcon = icon;
        nid.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON;
    }
    for id in 1..=MAX_ICON_ID {
        nid.uID = id;
        // SAFETY: `nid` is fully initialized and outlives the call.
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) }.as_bool() {
            return Ok(());
        }
    }
    anyhow::bail!("the shell didn't accept the notification for any icon ID up to {MAX_ICON_ID} (taskbar not ready?)")
}

/// Settings > System > Notifications > "Notifications" off. The value is
/// absent until the user first changes it, and absent means on.
fn toasts_disabled() -> bool {
    let subkey = windows::core::w!(r"Software\Microsoft\Windows\CurrentVersion\PushNotifications");
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: `value` and `size` describe a valid DWORD-sized buffer.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            windows::core::w!("ToastEnabled"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    r.is_ok() && value == 0
}

/// The GhostPerf icon at the system's large-icon size, for the balloon's
/// image; without it Windows shows a generic "info" glyph. Deliberately never
/// destroyed: the shell may still be using it after the call returns, and it's
/// loaded once per data folder, ever.
fn large_app_icon() -> Option<HICON> {
    // SAFETY: plain Win32 calls on our own module's resources.
    unsafe {
        let module = GetModuleHandleW(None).ok()?;
        let icon = LoadImageW(
            Some(HINSTANCE(module.0)),
            // MAKEINTRESOURCEW: a small integer ID passed as a pointer.
            PCWSTR(ICON_RESOURCE_ID as *const u16),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXICON),
            GetSystemMetrics(SM_CYICON),
            LR_DEFAULTCOLOR,
        )
        .ok()?;
        Some(HICON(icon.0))
    }
}
