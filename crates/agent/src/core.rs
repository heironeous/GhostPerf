use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use storage::{MetricSample, NewEvent, Store};

use crate::eventlog;
use crate::live::LiveSettings;
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
    /// Apply a freshly loaded `agent.toml` while running (the `reload`
    /// command, sent by the UI after saving its settings). Replies with the
    /// changes it applied.
    Reload {
        config: Box<AgentConfig>,
        reply: Sender<Vec<String>>,
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
    mut config: AgentConfig,
    recording_flag: Arc<AtomicBool>,
    live: Arc<LiveSettings>,
) {
    let mut buffer: Vec<MetricSample> = Vec::with_capacity(config.flush_batch_size);
    let mut events_buffer: Vec<NewEvent> = Vec::new();
    let mut current_session: Option<(i64, i64)> = None; // (id, started_at)
    let mut flush_interval = Duration::from_millis(config.flush_interval_ms);
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
                            log::info!("session {id} started (label: {label:?})");
                            id
                        })
                };
                if let Err(e) = &result {
                    log::warn!("start refused: {e:#}");
                }
                let _ = reply.send(result);
            }
            Ok(CoreMsg::Stop { reply }) => {
                let result = if let Some((id, started_at)) = current_session.take() {
                    recording_flag.store(false, Ordering::SeqCst);
                    finish_session(
                        &store,
                        &config,
                        id,
                        started_at,
                        &mut buffer,
                        &mut events_buffer,
                    )
                    .map(|_| Some(id))
                } else {
                    Ok(None)
                };
                if let Err(e) = &result {
                    log::error!("failed to stop session: {e:#}");
                }
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
            Ok(CoreMsg::Reload {
                config: new_config,
                reply,
            }) => {
                let new_config = keep_db_path(&config, *new_config);
                let changes = config.changed_settings(&new_config);
                config = new_config;
                live.apply(&config);
                flush_interval = Duration::from_millis(config.flush_interval_ms);
                if changes.is_empty() {
                    log::info!("reload: no settings changed");
                } else {
                    log::info!("reload applied: {}", changes.join(", "));
                }
                // Reply before pruning: a size-cap prune ends in a VACUUM,
                // which on a large database can outlast the client's 2 s
                // read timeout, and the settings are already in effect.
                let _ = reply.send(changes);
                prune_by_retention(&store, &config);
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
                log::info!("shutdown requested");
                recording_flag.store(false, Ordering::SeqCst);
                if let Some((id, started_at)) = current_session.take() {
                    if let Err(e) = finish_session(
                        &store,
                        &config,
                        id,
                        started_at,
                        &mut buffer,
                        &mut events_buffer,
                    ) {
                        // Nobody is waiting on a reply here, so this is the
                        // only trace. The session isn't lost either way: the
                        // next agent start closes it as orphaned.
                        log::error!("failed to close session {id} on shutdown: {e:#}");
                    }
                }
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

/// `new` with `db_path` reset to the running one. The store stays open on
/// the database the agent started with, so a changed `db_path` (only
/// editable by hand; the UI shows it read-only) applies at the next start.
fn keep_db_path(running: &AgentConfig, mut new: AgentConfig) -> AgentConfig {
    if new.db_path != running.db_path {
        log::warn!(
            "reload: db_path changed to {}; it takes effect when the agent restarts",
            new.db_path.display()
        );
        new.db_path = running.db_path.clone();
    }
    new
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

    if let Some(mem_pct) = sample.mem_used_pct() {
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

/// Everything that ends a session, shared by an explicit `stop` and by
/// `shutdown` (the tray's Quit) so the two can't drift apart — they already
/// did once: shutdown used to skip Event Log correlation, so a session
/// ended by quitting the tray had none.
fn finish_session(
    store: &Store,
    config: &AgentConfig,
    id: i64,
    started_at: i64,
    buffer: &mut Vec<MetricSample>,
    events_buffer: &mut Vec<NewEvent>,
) -> anyhow::Result<()> {
    let ended_at = now_millis();
    let mut correlated = 0;
    if config.eventlog_correlation {
        let events = eventlog::correlate(started_at, ended_at);
        correlated = events.len();
        events_buffer.extend(events);
    }
    flush(store, id, buffer, events_buffer);
    store.stop_session(id, ended_at)?;
    log::info!(
        "session {id} stopped after {:.1}s ({correlated} Event Log entries correlated)",
        (ended_at - started_at) as f64 / 1000.0
    );
    prune_by_retention(store, config);
    Ok(())
}

/// Runs retention (Phase 5): deletes completed sessions older than
/// `retention_days`, then, if `retention_max_db_mb` is set and the
/// database is still over that size, the oldest remaining ones too. Called
/// after every session ends (here) and once at agent startup
/// (`crates/agent/src/main.rs`) — the former keeps a long-running agent's
/// database in check between restarts, the latter catches a config edited
/// while the agent wasn't running.
pub(crate) fn prune_by_retention(store: &Store, config: &AgentConfig) {
    let cutoff = now_millis() - config.retention_days as i64 * 86_400_000;
    let max_bytes = config.retention_max_db_mb.map(|mb| mb * 1024 * 1024);
    match store.prune(cutoff, max_bytes) {
        Ok(0) => {}
        Ok(n) => log::info!(
            "pruned {n} session(s) past retention ({} days{})",
            config.retention_days,
            max_bytes.map_or_else(String::new, |b| format!(", {} MB cap", b / 1024 / 1024))
        ),
        Err(e) => log::warn!("retention prune failed: {e:#}"),
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
            log::error!("failed to flush metrics batch: {e}");
        }
        buffer.clear();
    }
    if !events_buffer.is_empty() {
        if let Err(e) = store.insert_events_batch(session_id, events_buffer) {
            log::error!("failed to flush events batch: {e}");
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

    fn run_core(store: Store, rx: Receiver<CoreMsg>, config: AgentConfig, flag: Arc<AtomicBool>) {
        let live = LiveSettings::new(&config);
        run(store, rx, config, flag, live)
    }

    fn temp_db(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "ghostperf-core-{name}-test-{}-{}.db",
            std::process::id(),
            now_millis()
        ))
    }

    fn remove_db(path: &std::path::Path) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    /// `reload` mid-session: the core applies new thresholds to the very
    /// next sample, updates the threads' live settings, keeps the running
    /// database, and reports what changed.
    #[test]
    fn reload_applies_new_settings_mid_session() {
        let db_path = temp_db("reload");
        let store = Store::open(&db_path).unwrap();
        let config = AgentConfig {
            flush_batch_size: 1,
            cpu_spike_threshold_pct: 90.0,
            eventlog_correlation: false,
            ..AgentConfig::default()
        };
        let live = LiveSettings::new(&config);
        let (tx, rx) = channel();
        let flag = Arc::new(AtomicBool::new(false));
        let core = {
            let live = live.clone();
            std::thread::spawn(move || run(store, rx, config, flag, live))
        };

        let (reply, reply_rx) = channel();
        tx.send(CoreMsg::Start { label: None, reply }).unwrap();
        let session_id = reply_rx.recv().unwrap().unwrap();

        let mut busy = sample(1);
        busy.cpu_pct = 60.0;
        tx.send(CoreMsg::Sample(busy.clone())).unwrap(); // under 90: no spike

        let (reply, reply_rx) = channel();
        tx.send(CoreMsg::Reload {
            config: Box::new(AgentConfig {
                cpu_spike_threshold_pct: 50.0,
                sample_interval_ms: 250,
                db_path: "somewhere-else.db".into(),
                flush_batch_size: 1,
                eventlog_correlation: false,
                ..AgentConfig::default()
            }),
            reply,
        })
        .unwrap();
        let changes = reply_rx.recv().unwrap();
        assert_eq!(
            changes,
            [
                "cpu_spike_threshold_pct: 90.0 -> 50.0",
                "sample_interval_ms: 1000 -> 250"
            ],
            "db_path must not be reported (or applied) as changed"
        );
        assert_eq!(live.sample_interval(), Duration::from_millis(250));

        busy.ts = 2;
        tx.send(CoreMsg::Sample(busy)).unwrap(); // over 50: spike

        let (reply, reply_rx) = channel();
        tx.send(CoreMsg::Stop { reply }).unwrap();
        reply_rx.recv().unwrap().unwrap();
        tx.send(CoreMsg::Shutdown).unwrap();
        core.join().unwrap();

        let reader = Store::open(&db_path).unwrap();
        assert_eq!(reader.metrics_for_session(session_id).unwrap().len(), 2);
        let events = reader.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!((events[0].kind.as_str(), events[0].ts), ("cpu_spike", 2));
        drop(reader);
        remove_db(&db_path);
    }

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
            disk_queue_len: None,
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
        let core = std::thread::spawn(move || run_core(store, rx, config, flag));

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

    /// Retention (Phase 5) must actually run as part of ending a session,
    /// not just exist as a `Store` method nobody calls.
    #[test]
    fn stopping_a_session_prunes_old_sessions_past_retention() {
        let db_path = std::env::temp_dir().join(format!(
            "ghostperf-core-retention-test-{}-{}.db",
            std::process::id(),
            now_millis()
        ));
        let ten_days_ms = 10 * 86_400_000;
        let old_id = {
            let store = Store::open(&db_path).unwrap();
            let started = now_millis() - ten_days_ms;
            let id = store.start_session(started, None, None).unwrap();
            store.stop_session(id, started + 1000).unwrap();
            id
        };

        let store = Store::open(&db_path).unwrap();
        // Cutoff lands a day ago: well after the 10-day-old session above,
        // safely before the brand new one this test is about to record.
        let config = AgentConfig {
            retention_days: 1,
            eventlog_correlation: false,
            ..AgentConfig::default()
        };
        let (tx, rx) = channel();
        let flag = Arc::new(AtomicBool::new(false));
        let core = std::thread::spawn(move || run_core(store, rx, config, flag));

        let (reply, reply_rx) = channel();
        tx.send(CoreMsg::Start { label: None, reply }).unwrap();
        let new_id = reply_rx.recv().unwrap().unwrap();
        let (reply, reply_rx) = channel();
        tx.send(CoreMsg::Stop { reply }).unwrap();
        reply_rx.recv().unwrap().unwrap();
        tx.send(CoreMsg::Shutdown).unwrap();
        core.join().unwrap();

        let reader = Store::open(&db_path).unwrap();
        assert!(
            reader.get_session(old_id).unwrap().is_none(),
            "10-day-old completed session should have been pruned"
        );
        assert!(
            reader.get_session(new_id).unwrap().is_some(),
            "the session that was just recorded must survive its own retention check"
        );

        drop(reader);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db_path.display()));
        }
    }

    /// The tray's Quit sends `shutdown` while a session may still be
    /// recording: buffered samples and events must reach disk and the
    /// session must be closed, exactly as an explicit `stop` would.
    #[test]
    fn shutdown_while_recording_flushes_and_closes_the_session() {
        let db_path = std::env::temp_dir().join(format!(
            "ghostperf-core-shutdown-test-{}-{}.db",
            std::process::id(),
            now_millis()
        ));
        let store = Store::open(&db_path).unwrap();
        let config = AgentConfig {
            flush_batch_size: 10_000, // nothing reaches disk before shutdown
            flush_interval_ms: 60_000,
            // Real Event Log contents would make the event count
            // machine-dependent; correlation itself is covered separately.
            eventlog_correlation: false,
            ..AgentConfig::default()
        };

        let (tx, rx) = channel();
        let flag = Arc::new(AtomicBool::new(false));
        let core = {
            let flag = flag.clone();
            std::thread::spawn(move || run_core(store, rx, config, flag))
        };

        let (reply, reply_rx) = channel();
        tx.send(CoreMsg::Start { label: None, reply }).unwrap();
        let session_id = reply_rx.recv().unwrap().unwrap();
        for i in 0..3 {
            tx.send(CoreMsg::Sample(sample(i))).unwrap();
        }
        tx.send(CoreMsg::Event {
            ts: 1,
            kind: "stall".into(),
            value: 60.0,
            detail: None,
        })
        .unwrap();

        tx.send(CoreMsg::Shutdown).unwrap();
        core.join().unwrap();
        assert!(!flag.load(Ordering::SeqCst), "sampling should be stopped");

        let reader = Store::open(&db_path).unwrap();
        let session = reader.get_session(session_id).unwrap().unwrap();
        assert!(session.ended_at.is_some(), "session left open by shutdown");
        assert_eq!(reader.metrics_for_session(session_id).unwrap().len(), 3);
        assert_eq!(reader.events_for_session(session_id).unwrap().len(), 1);

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
        let core = std::thread::spawn(move || run_core(store, rx, config, flag));

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
        let core = std::thread::spawn(move || run_core(store, rx, config, flag));

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
