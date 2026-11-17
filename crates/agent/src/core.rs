use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use storage::{MetricSample, NewEvent, Store};

use crate::eventlog;
use crate::now_millis;
use agent_config::AgentConfig;

pub enum CoreMsg {
    Start {
        label: Option<String>,
        reply: Sender<anyhow::Result<i64>>,
    },
    Stop {
        reply: Sender<anyhow::Result<Option<i64>>>,
    },
    Status {
        reply: Sender<StatusSnapshot>,
    },
    Sample(MetricSample),
    /// A stall, threshold spike, or (eventually) a correlated OS event —
    /// anything timestamped that isn't a regular metric sample.
    Event {
        ts: i64,
        kind: String,
        value: f64,
        detail: Option<String>,
    },
    Shutdown,
}

#[derive(Debug, Clone)]
pub struct StatusSnapshot {
    pub recording: bool,
    pub session_id: Option<i64>,
    pub started_at: Option<i64>,
    pub samples_buffered: usize,
}

/// Runs on a dedicated thread and is the sole owner of the SQLite `Store`.
/// All reads/writes to session state funnel through `rx` so there's never
/// cross-thread contention on the connection itself.
pub fn run(
    store: Store,
    rx: Receiver<CoreMsg>,
    config: AgentConfig,
    recording_flag: Arc<AtomicBool>,
) {
    let mut buffer: Vec<MetricSample> = Vec::with_capacity(config.flush_batch_size);
    let mut events_buffer: Vec<NewEvent> = Vec::new();
    let mut current_session: Option<(i64, i64)> = None; // (id, started_at)
    let flush_interval = Duration::from_millis(config.flush_interval_ms);
    // The time-based flush is tracked against a deadline rather than relying on
    // `recv_timeout` alone: while recording, samples arrive every
    // `sample_interval_ms`, so a plain `recv_timeout(flush_interval)` would
    // never time out and only the size-based flush would ever fire.
    let mut last_flush = Instant::now();

    loop {
        let wait = flush_interval.saturating_sub(last_flush.elapsed());
        match rx.recv_timeout(wait) {
            Ok(CoreMsg::Start { label, reply }) => {
                let result = if current_session.is_some() {
                    Err(anyhow::anyhow!("a session is already recording"))
                } else {
                    let started_at = now_millis();
                    store
                        .start_session(started_at, label.as_deref(), Some(host_info().as_str()))
                        .map_err(anyhow::Error::from)
                        .map(|id| {
                            current_session = Some((id, started_at));
                            recording_flag.store(true, Ordering::SeqCst);
                            id
                        })
                };
                let _ = reply.send(result);
            }
            Ok(CoreMsg::Stop { reply }) => {
                let result = if let Some((id, started_at)) = current_session.take() {
                    recording_flag.store(false, Ordering::SeqCst);
                    let ended_at = now_millis();
                    if config.eventlog_correlation {
                        events_buffer.extend(eventlog::correlate(started_at, ended_at));
                    }
                    flush(&store, id, &mut buffer, &mut events_buffer);
                    store
                        .stop_session(id, ended_at)
                        .map(|_| Some(id))
                        .map_err(Into::into)
                } else {
                    Ok(None)
                };
                let _ = reply.send(result);
            }
            Ok(CoreMsg::Status { reply }) => {
                let snapshot = StatusSnapshot {
                    recording: current_session.is_some(),
                    session_id: current_session.map(|(id, _)| id),
                    started_at: current_session.map(|(_, started_at)| started_at),
                    samples_buffered: buffer.len(),
                };
                let _ = reply.send(snapshot);
            }
            Ok(CoreMsg::Sample(sample)) => {
                if current_session.is_some() {
                    check_thresholds(&sample, &config, &mut events_buffer);
                    buffer.push(sample);
                    if buffer.len() >= config.flush_batch_size {
                        if let Some((id, _)) = current_session {
                            flush(&store, id, &mut buffer, &mut events_buffer);
                        }
                        last_flush = Instant::now();
                    }
                }
            }
            Ok(CoreMsg::Event {
                ts,
                kind,
                value,
                detail,
            }) => {
                if current_session.is_some() {
                    events_buffer.push(NewEvent {
                        ts,
                        kind,
                        value,
                        detail,
                    });
                }
            }
            Ok(CoreMsg::Shutdown) => {
                if let Some((id, _)) = current_session.take() {
                    flush(&store, id, &mut buffer, &mut events_buffer);
                    let _ = store.stop_session(id, now_millis());
                }
                recording_flag.store(false, Ordering::SeqCst);
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if last_flush.elapsed() >= flush_interval {
            if let Some((id, _)) = current_session {
                flush(&store, id, &mut buffer, &mut events_buffer);
            }
            last_flush = Instant::now();
        }
    }
}

/// A sample at or above either threshold is recorded as a spike event
/// alongside the regular metric row, so the UI can flag it on a timeline
/// without having to re-scan every sample for threshold crossings.
fn check_thresholds(
    sample: &MetricSample,
    config: &AgentConfig,
    events_buffer: &mut Vec<NewEvent>,
) {
    if sample.cpu_pct >= config.cpu_spike_threshold_pct {
        events_buffer.push(NewEvent {
            ts: sample.ts,
            kind: "cpu_spike".to_string(),
            value: sample.cpu_pct as f64,
            detail: None,
        });
    }

    let mem_total = sample.mem_used_mb + sample.mem_available_mb;
    if mem_total > 0.0 {
        let mem_pct = sample.mem_used_mb / mem_total * 100.0;
        if mem_pct >= config.mem_spike_threshold_pct {
            events_buffer.push(NewEvent {
                ts: sample.ts,
                kind: "mem_spike".to_string(),
                value: mem_pct as f64,
                detail: None,
            });
        }
    }
}

fn flush(
    store: &Store,
    session_id: i64,
    buffer: &mut Vec<MetricSample>,
    events_buffer: &mut Vec<NewEvent>,
) {
    if !buffer.is_empty() {
        if let Err(e) = store.insert_metrics_batch(session_id, buffer) {
            eprintln!("ghostperf-agent: failed to flush metrics batch: {e}");
        }
        buffer.clear();
    }
    if !events_buffer.is_empty() {
        if let Err(e) = store.insert_events_batch(session_id, events_buffer) {
            eprintln!("ghostperf-agent: failed to flush events batch: {e}");
        }
        events_buffer.clear();
    }
}

fn host_info() -> String {
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn sample(ts: i64) -> MetricSample {
        MetricSample {
            ts,
            cpu_pct: 1.0,
            cpu_per_core_pct: vec![1.0],
            mem_used_mb: 1.0,
            mem_available_mb: 1.0,
            disk_read_bps: 0.0,
            disk_write_bps: 0.0,
            net_rx_bps: 0.0,
            net_tx_bps: 0.0,
            context_switches_per_sec: 0.0,
            page_faults_per_sec: 0.0,
            gpu_pct: None,
            top_processes: Vec::new(),
        }
    }

    /// Regression: samples arriving faster than `flush_interval_ms` used to
    /// keep resetting `recv_timeout`, so the time-based flush never fired and
    /// data only reached disk once `flush_batch_size` was hit.
    #[test]
    fn time_based_flush_fires_while_samples_keep_arriving() {
        let db_path = std::env::temp_dir().join(format!(
            "ghostperf-core-test-{}-{}.db",
            std::process::id(),
            now_millis()
        ));
        let store = Store::open(&db_path).unwrap();
        let config = AgentConfig {
            flush_batch_size: 10_000, // never reached in this test
            flush_interval_ms: 100,
            ..AgentConfig::default()
        };

        let (tx, rx) = channel();
        let flag = Arc::new(AtomicBool::new(false));
        let core = std::thread::spawn(move || run(store, rx, config, flag));

        let (reply, reply_rx) = channel();
        tx.send(CoreMsg::Start { label: None, reply }).unwrap();
        let session_id = reply_rx.recv().unwrap().unwrap();

        // Send a sample every 20ms for ~400ms: always well inside the 100ms
        // flush interval, so only a deadline-based flush can write anything.
        for i in 0..20 {
            tx.send(CoreMsg::Sample(sample(i))).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }

        // Read through a second connection while the session is still recording.
        let reader = Store::open(&db_path).unwrap();
        let persisted = reader.metrics_for_session(session_id).unwrap().len();
        assert!(persisted > 0, "no samples flushed by the time-based flush");

        tx.send(CoreMsg::Shutdown).unwrap();
        core.join().unwrap();
        drop(reader);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db_path.display()));
        }
    }

    #[test]
    fn check_thresholds_flags_cpu_and_mem_spikes() {
        let config = AgentConfig {
            cpu_spike_threshold_pct: 90.0,
            mem_spike_threshold_pct: 80.0,
            ..AgentConfig::default()
        };

        let mut below = sample(1);
        below.cpu_pct = 50.0;
        below.mem_used_mb = 10.0;
        below.mem_available_mb = 90.0; // 10% used
        let mut events = Vec::new();
        check_thresholds(&below, &config, &mut events);
        assert!(events.is_empty());

        let mut above = sample(2);
        above.cpu_pct = 95.0;
        above.mem_used_mb = 85.0;
        above.mem_available_mb = 15.0; // 85% used
        let mut events = Vec::new();
        check_thresholds(&above, &config, &mut events);
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|e| e.kind == "cpu_spike"));
        assert!(events.iter().any(|e| e.kind == "mem_spike"));
    }

    /// End-to-end through the core thread: a spiking sample should produce
    /// both a metrics row and an events row for the same session.
    #[test]
    fn spiking_sample_persists_an_event_alongside_the_metric() {
        let db_path = std::env::temp_dir().join(format!(
            "ghostperf-core-spike-test-{}-{}.db",
            std::process::id(),
            now_millis()
        ));
        let store = Store::open(&db_path).unwrap();
        let config = AgentConfig {
            flush_batch_size: 1, // flush immediately so the test doesn't need to wait
            cpu_spike_threshold_pct: 90.0,
            ..AgentConfig::default()
        };

        let (tx, rx) = channel();
        let flag = Arc::new(AtomicBool::new(false));
        let core = std::thread::spawn(move || run(store, rx, config, flag));

        let (reply, reply_rx) = channel();
        tx.send(CoreMsg::Start { label: None, reply }).unwrap();
        let session_id = reply_rx.recv().unwrap().unwrap();

        let mut spike = sample(1);
        spike.cpu_pct = 99.0;
        tx.send(CoreMsg::Sample(spike)).unwrap();

        let (reply, reply_rx) = channel();
        tx.send(CoreMsg::Stop { reply }).unwrap();
        reply_rx.recv().unwrap().unwrap();

        tx.send(CoreMsg::Shutdown).unwrap();
        core.join().unwrap();

        let reader = Store::open(&db_path).unwrap();
        let metrics = reader.metrics_for_session(session_id).unwrap();
        let events = reader.events_for_session(session_id).unwrap();
        assert_eq!(metrics.len(), 1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "cpu_spike");

        drop(reader);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db_path.display()));
        }
    }

    #[test]
    fn explicit_core_event_is_buffered_and_flushed() {
        let db_path = std::env::temp_dir().join(format!(
            "ghostperf-core-event-test-{}-{}.db",
            std::process::id(),
            now_millis()
        ));
        let store = Store::open(&db_path).unwrap();
        let config = AgentConfig {
            flush_batch_size: 1,
            ..AgentConfig::default()
        };

        let (tx, rx) = channel();
        let flag = Arc::new(AtomicBool::new(false));
        let core = std::thread::spawn(move || run(store, rx, config, flag));

        let (reply, reply_rx) = channel();
        tx.send(CoreMsg::Start { label: None, reply }).unwrap();
        let session_id = reply_rx.recv().unwrap().unwrap();

        tx.send(CoreMsg::Event {
            ts: 123,
            kind: "stall".into(),
            value: 77.0,
            detail: Some("x".into()),
        })
        .unwrap();
        // Events alone don't trigger the size-based flush (only samples do),
        // so stop the session, which flushes unconditionally.
        let (reply, reply_rx) = channel();
        tx.send(CoreMsg::Stop { reply }).unwrap();
        reply_rx.recv().unwrap().unwrap();

        tx.send(CoreMsg::Shutdown).unwrap();
        core.join().unwrap();

        let reader = Store::open(&db_path).unwrap();
        let events = reader.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "stall");
        assert_eq!(events[0].value, 77.0);

        drop(reader);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db_path.display()));
        }
    }
}
