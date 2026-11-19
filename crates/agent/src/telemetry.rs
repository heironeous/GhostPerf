//! Live CPU/RAM/GPU readout for the overlay (Phase 6), published by the
//! sampler thread and read by the `live` control command — no session, no
//! SQLite, nothing persisted. Deliberately not routed through `core::run`:
//! unlike `start`/`stop`/`status`/`reload`, it never touches session state
//! or `AgentConfig`, so `ipc::dispatch` reads it directly.
//!
//! The overlay needs "reuse the agent's existing sampler … rather than
//! sampling a second time" (TODO.md Phase 6). The sampler thread already
//! samples once per tick for a recording session; [`Telemetry::publish`] is
//! called with that same result regardless of whether anything is
//! recording, so there's no second sampler and no extra sampling work when
//! a session already happens to be active. What's new is that the sampler
//! thread now *also* ticks when nothing is recording but the overlay is
//! polling — see [`Telemetry::wants_samples`], which `main.rs` OR's into
//! `sampling_loop::run`'s active condition alongside the recording flag.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use storage::MetricSample;

/// How long after the last `live` poll to keep sampling before falling back
/// to idle. Generous relative to any sane overlay refresh rate (hundreds of
/// ms to a few seconds) so a missed poll or two doesn't re-trigger a prime,
/// but short enough that a crashed/closed overlay stops costing anything
/// within a few seconds.
const LEASE_MS: i64 = 3000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveSnapshot {
    pub ts: i64,
    pub cpu_pct: f32,
    pub mem_used_pct: Option<f32>,
    pub gpu_pct: Option<f32>,
}

impl LiveSnapshot {
    fn from_sample(sample: &MetricSample) -> Self {
        Self {
            ts: sample.ts,
            cpu_pct: sample.cpu_pct,
            mem_used_pct: sample.mem_used_pct(),
            gpu_pct: sample.gpu_pct,
        }
    }
}

pub struct Telemetry {
    latest: Mutex<Option<LiveSnapshot>>,
    last_poll_ms: AtomicI64,
}

impl Telemetry {
    pub fn new() -> Self {
        Self {
            latest: Mutex::new(None),
            // Far in the past, not 0: `wants_samples` compares against
            // `now_millis() - last_poll_ms`, and millis-since-epoch minus 0
            // is already huge, but this is explicit about the intent (no
            // poll has ever happened) rather than relying on that.
            last_poll_ms: AtomicI64::new(i64::MIN / 2),
        }
    }

    /// Called by the sampler thread every tick, whether or not a session is
    /// recording.
    pub fn publish(&self, sample: &MetricSample) {
        *self.latest.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(LiveSnapshot::from_sample(sample));
    }

    /// Called by the sampler thread while it's idle: drops the latest
    /// snapshot, since nothing will update it until sampling resumes. When
    /// it was kept, the first Windows run found an overlay opened half a minute after
    /// the last one closed showing that old reading as current until the
    /// sampler had ticked again; `poll` now answers "no data yet" instead.
    pub fn sampler_idle(&self) {
        *self.latest.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Called by the `live` command handler: returns the latest snapshot
    /// (`None` if the sampler hasn't ticked since this call started
    /// sampling — ask again shortly) and refreshes the lease so the sampler
    /// keeps running.
    pub fn poll(&self, now_ms: i64) -> Option<LiveSnapshot> {
        self.last_poll_ms.store(now_ms, Ordering::Relaxed);
        *self.latest.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether a `live` poll happened recently enough that the sampler
    /// should keep ticking even with no session recording.
    pub fn wants_samples(&self, now_ms: i64) -> bool {
        now_ms - self.last_poll_ms.load(Ordering::Relaxed) < LEASE_MS
    }
}

impl Default for Telemetry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: i64, cpu_pct: f32) -> MetricSample {
        MetricSample {
            ts,
            cpu_pct,
            cpu_per_core_pct: vec![cpu_pct],
            mem_used_mb: 4000.0,
            mem_available_mb: 12_000.0,
            disk_read_bps: 0.0,
            disk_write_bps: 0.0,
            net_rx_bps: 0.0,
            net_tx_bps: 0.0,
            context_switches_per_sec: 0.0,
            page_faults_per_sec: 0.0,
            gpu_pct: Some(10.0),
            disk_queue_len: None,
            top_processes: Vec::new(),
        }
    }

    #[test]
    fn nothing_published_yet_is_none() {
        let t = Telemetry::new();
        assert_eq!(t.poll(1000), None);
    }

    #[test]
    fn poll_returns_the_latest_published_sample() {
        let t = Telemetry::new();
        t.publish(&sample(1000, 12.5));
        let snap = t.poll(1000).unwrap();
        assert_eq!(snap.ts, 1000);
        assert_eq!(snap.cpu_pct, 12.5);
        assert_eq!(snap.gpu_pct, Some(10.0));
        assert!(snap.mem_used_pct.unwrap() > 0.0);

        t.publish(&sample(2000, 90.0));
        assert_eq!(t.poll(2000).unwrap().cpu_pct, 90.0);
    }

    #[test]
    fn an_idle_sampler_leaves_no_reading_to_serve() {
        let t = Telemetry::new();
        t.publish(&sample(1000, 12.5));
        t.sampler_idle();
        assert_eq!(t.poll(40_000), None);
        t.publish(&sample(41_000, 30.0));
        assert_eq!(t.poll(41_000).unwrap().cpu_pct, 30.0);
    }

    #[test]
    fn wants_samples_only_within_the_lease_after_a_poll() {
        let t = Telemetry::new();
        assert!(!t.wants_samples(0), "no poll yet");

        t.poll(10_000);
        assert!(t.wants_samples(10_000));
        assert!(t.wants_samples(10_000 + LEASE_MS - 1));
        assert!(!t.wants_samples(10_000 + LEASE_MS));
    }

    #[test]
    fn polling_refreshes_the_lease() {
        let t = Telemetry::new();
        t.poll(0);
        t.poll(LEASE_MS - 1);
        // Still within the lease *from the second poll*, well past the
        // first poll's deadline alone.
        assert!(t.wants_samples(LEASE_MS - 1 + LEASE_MS - 1));
    }
}
