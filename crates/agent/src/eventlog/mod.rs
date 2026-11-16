//! Correlates Windows Event Log entries (System/Application channels,
//! Warning and above) into a session's events when it stops, so a crash or
//! a service failure that happened during the recording window shows up
//! next to the metrics. No-op on non-Windows builds — there's no
//! equivalent log to correlate against.

mod format;
#[cfg(not(target_os = "windows"))]
mod generic_impl;
#[cfg(target_os = "windows")]
mod windows_impl;

#[cfg(not(target_os = "windows"))]
pub use generic_impl::correlate;
#[cfg(target_os = "windows")]
pub use windows_impl::correlate;
