//! The settings the sampler and stall threads read on every tick, so a
//! `reload` can change them without restarting those threads.
//!
//! The core thread owns the rest of `AgentConfig` (thresholds, flushing,
//! retention, Event Log correlation) and simply replaces its copy on
//! `reload`. The other two threads each got their settings once, as plain
//! values, when they were spawned; they now read them from here instead.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agent_config::AgentConfig;

pub struct LiveSettings {
    sample_interval_ms: AtomicU64,
    top_n_processes: AtomicUsize,
    stall_check_interval_ms: AtomicU64,
    stall_threshold_ms: AtomicU64,
}

impl LiveSettings {
    pub fn new(config: &AgentConfig) -> Arc<Self> {
        let live = Self {
            sample_interval_ms: AtomicU64::new(0),
            top_n_processes: AtomicUsize::new(0),
            stall_check_interval_ms: AtomicU64::new(0),
            stall_threshold_ms: AtomicU64::new(0),
        };
        live.apply(config);
        Arc::new(live)
    }

    /// Each value is independent of the others, so relaxed stores are
    /// enough: a reader may briefly see some new and some old values, which
    /// is harmless here.
    pub fn apply(&self, config: &AgentConfig) {
        self.sample_interval_ms
            .store(config.sample_interval_ms, Ordering::Relaxed);
        self.top_n_processes
            .store(config.top_n_processes, Ordering::Relaxed);
        self.stall_check_interval_ms
            .store(config.stall_check_interval_ms, Ordering::Relaxed);
        self.stall_threshold_ms
            .store(config.stall_threshold_ms, Ordering::Relaxed);
    }

    pub fn sample_interval(&self) -> Duration {
        Duration::from_millis(self.sample_interval_ms.load(Ordering::Relaxed))
    }

    pub fn top_n_processes(&self) -> usize {
        self.top_n_processes.load(Ordering::Relaxed)
    }

    /// `(check_interval, threshold)` for the stall detector.
    pub fn stall_timing(&self) -> (Duration, Duration) {
        (
            Duration::from_millis(self.stall_check_interval_ms.load(Ordering::Relaxed)),
            Duration::from_millis(self.stall_threshold_ms.load(Ordering::Relaxed)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_replaces_every_value() {
        let live = LiveSettings::new(&AgentConfig::default());
        live.apply(&AgentConfig {
            sample_interval_ms: 250,
            top_n_processes: 9,
            stall_check_interval_ms: 7,
            stall_threshold_ms: 80,
            ..AgentConfig::default()
        });
        assert_eq!(live.sample_interval(), Duration::from_millis(250));
        assert_eq!(live.top_n_processes(), 9);
        assert_eq!(
            live.stall_timing(),
            (Duration::from_millis(7), Duration::from_millis(80))
        );
    }
}
