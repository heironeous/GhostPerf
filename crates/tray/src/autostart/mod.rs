//! "Start with Windows" toggle. Real implementations exist for Windows (a
//! `HKCU\...\Run` value, the shipping mechanism) and Linux (an XDG
//! `~/.config/autostart/*.desktop` entry, used only so this crate builds
//! and the toggle logic is testable in this non-Windows dev container).

mod format;
#[cfg(not(target_os = "windows"))]
mod linux_impl;
#[cfg(target_os = "windows")]
mod windows_impl;

#[cfg(not(target_os = "windows"))]
pub use linux_impl::{disable, enable, is_enabled};
#[cfg(target_os = "windows")]
pub use windows_impl::{disable, enable, is_enabled};
