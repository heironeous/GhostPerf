use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

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

    loop {
        match rx.recv_timeout(flush_interval) {
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
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some((id, _)) = current_session {
                    flush(&store, id, &mut buffer);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
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
