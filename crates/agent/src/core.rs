use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use storage::{MetricSample, Store};

use crate::config::AgentConfig;
use crate::now_millis;

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
                let result = if let Some((id, _)) = current_session.take() {
                    recording_flag.store(false, Ordering::SeqCst);
                    flush(&store, id, &mut buffer);
                    let ended_at = now_millis();
                    store.stop_session(id, ended_at).map(|_| Some(id)).map_err(Into::into)
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
                if let Some((id, _)) = current_session {
                    buffer.push(sample);
                    if buffer.len() >= config.flush_batch_size {
                        flush(&store, id, &mut buffer);
                        last_flush = Instant::now();
                    }
                }
            }
            Ok(CoreMsg::Shutdown) => {
                if let Some((id, _)) = current_session.take() {
                    flush(&store, id, &mut buffer);
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
                flush(&store, id, &mut buffer);
            }
            last_flush = Instant::now();
        }
    }
}

fn flush(store: &Store, session_id: i64, buffer: &mut Vec<MetricSample>) {
    if buffer.is_empty() {
        return;
    }
    if let Err(e) = store.insert_metrics_batch(session_id, buffer) {
        eprintln!("ghostperf-agent: failed to flush metrics batch: {e}");
    }
    buffer.clear();
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
}
