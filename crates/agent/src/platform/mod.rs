//! OS-specific counters that `sysinfo` doesn't expose: system-wide context
//! switches/page faults, GPU utilization, and per-process handle/thread
//! counts. Real implementations exist for Windows (the shipping target,
//! using PDH perf counters) and Linux (`/proc`, used only so this crate
//! builds and is smoke-testable in a non-Windows dev environment).

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
mod generic_impl;
#[cfg(target_os = "linux")]
mod linux_impl;
#[cfg(target_os = "windows")]
mod windows_impl;

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub use generic_impl::PlatformCounters;
#[cfg(target_os = "linux")]
pub use linux_impl::PlatformCounters;
#[cfg(target_os = "windows")]
pub use windows_impl::PlatformCounters;

/// System-wide counters sampled once per tick, alongside cpu/mem/disk/net.
#[derive(Debug, Clone, Default)]
pub struct SystemCounters {
    pub context_switches_per_sec: f64,
    pub page_faults_per_sec: f64,
    /// `None` when no GPU counter could be read on this machine.
    pub gpu_pct: Option<f32>,
}
