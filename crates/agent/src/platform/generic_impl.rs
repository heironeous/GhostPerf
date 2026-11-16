//! Fallback for platforms that are neither the shipping target (Windows)
//! nor the dev-smoke-test target (Linux): everything reports unavailable.

use super::SystemCounters;

pub struct PlatformCounters;

impl PlatformCounters {
    pub fn new() -> Self {
        Self
    }

    pub fn sample_system(&mut self, _elapsed_secs: f64) -> SystemCounters {
        SystemCounters::default()
    }

    pub fn handle_count(&self, _pid: u32) -> Option<u32> {
        None
    }

    pub fn thread_count(&self, _pid: u32) -> Option<u32> {
        None
    }
}
