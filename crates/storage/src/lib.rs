use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: i64,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub label: Option<String>,
    pub host_info: Option<String>,
}

/// Cheap per-session aggregate stats for a list-view row — see
/// `Store::session_summary`. `None` fields mean the session has no samples
/// yet (e.g. started and stopped with nothing recorded in between).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub sample_count: i64,
    pub avg_cpu_pct: Option<f64>,
    pub peak_cpu_pct: Option<f64>,
    pub peak_mem_used_mb: Option<f64>,
    /// Highest memory-in-use percentage over the session, as defined by
    /// `MetricSample::mem_used_pct`.
    pub peak_mem_used_pct: Option<f64>,
    /// `(kind, count)`, one row per distinct `events.kind` for the session.
    pub event_counts: Vec<(String, i64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSample {
    pub pid: u32,
    pub name: String,
    pub cpu_pct: f32,
    pub mem_mb: f32,
    /// Windows HANDLE count (leak detection). `None` where unavailable (e.g.
    /// not sampled this tick, or unsupported on the platform). On the Linux
    /// dev build this is approximated with the open-fd count, which is not
    /// the same metric but exercises the same plumbing.
    #[serde(default)]
    pub handle_count: Option<u32>,
    #[serde(default)]
    pub thread_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSample {
    pub ts: i64,
    pub cpu_pct: f32,
    pub cpu_per_core_pct: Vec<f32>,
    pub mem_used_mb: f32,
    pub mem_available_mb: f32,
    pub disk_read_bps: f64,
    pub disk_write_bps: f64,
    pub net_rx_bps: f64,
    pub net_tx_bps: f64,
    #[serde(default)]
    pub context_switches_per_sec: f64,
    #[serde(default)]
    pub page_faults_per_sec: f64,
    /// `None` when no GPU counter is available on this machine.
    #[serde(default)]
    pub gpu_pct: Option<f32>,
    pub top_processes: Vec<ProcessSample>,
}

impl MetricSample {
    /// Total memory as seen by this sample: used + available. Not stored as
    /// its own column; this sum is constant across a session in practice
    /// (65 329 MB on the 64 GB test machine) and is the same total the
    /// agent's `mem_spike` threshold has always used.
    pub fn mem_total_mb(&self) -> f32 {
        self.mem_used_mb + self.mem_available_mb
    }

    /// Memory in use as a percentage of [`Self::mem_total_mb`]; `None` for a
    /// sample with no memory figures (total 0) rather than dividing by zero.
    /// The one definition shared by the agent's `mem_spike` threshold and the
    /// UI's memory chart, so a spike marker lines up with the curve that
    /// crosses the threshold. `Store::session_summary`'s SQL mirrors it.
    pub fn mem_used_pct(&self) -> Option<f32> {
        let total = self.mem_total_mb();
        (total > 0.0).then(|| self.mem_used_mb / total * 100.0)
    }
}

/// A point-in-time annotation alongside a session's metrics: a detected
/// stall, a threshold spike, or a correlated OS event-log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub session_id: i64,
    pub ts: i64,
    pub kind: String,
    pub value: f64,
    pub detail: Option<String>,
}

/// Event data not yet assigned an id, for batched inserts.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub ts: i64,
    pub kind: String,
    pub value: f64,
    pub detail: Option<String>,
}

/// Owns the SQLite connection. Not `Sync` — callers that need the store from
/// multiple threads should own it on a single writer thread and talk to it
/// over a channel (see `agent`'s writer thread).
pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at  INTEGER NOT NULL,
                ended_at    INTEGER,
                label       TEXT,
                host_info   TEXT
            );

            CREATE TABLE IF NOT EXISTS metrics (
                session_id          INTEGER NOT NULL REFERENCES sessions(id),
                ts                   INTEGER NOT NULL,
                cpu_pct              REAL NOT NULL,
                cpu_per_core_json   TEXT NOT NULL,
                mem_used_mb          REAL NOT NULL,
                mem_available_mb     REAL NOT NULL,
                disk_read_bps        REAL NOT NULL,
                disk_write_bps       REAL NOT NULL,
                net_rx_bps           REAL NOT NULL,
                net_tx_bps           REAL NOT NULL,
                top_processes_json  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_metrics_session_ts
                ON metrics(session_id, ts);

            CREATE TABLE IF NOT EXISTS events (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id  INTEGER NOT NULL REFERENCES sessions(id),
                ts          INTEGER NOT NULL,
                kind        TEXT NOT NULL,
                value       REAL NOT NULL,
                detail      TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_events_session_ts
                ON events(session_id, ts);
            "#,
        )?;

        // `metrics` predates these columns; add them in place for existing
        // DBs rather than requiring a fresh file (sqlite can't add multiple
        // columns in one ALTER TABLE statement, so these run individually
        // and are each guarded by a column-existence check for idempotency).
        self.ensure_column(
            "metrics",
            "context_switches_per_sec",
            "REAL NOT NULL DEFAULT 0",
        )?;
        self.ensure_column("metrics", "page_faults_per_sec", "REAL NOT NULL DEFAULT 0")?;
        self.ensure_column("metrics", "gpu_pct", "REAL")?;

        Ok(())
    }

    fn ensure_column(&self, table: &str, column: &str, decl: &str) -> Result<()> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let exists = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .iter()
            .any(|name| name == column);
        if !exists {
            self.conn.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
                [],
            )?;
        }
        Ok(())
    }

    pub fn start_session(
        &self,
        started_at: i64,
        label: Option<&str>,
        host_info: Option<&str>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO sessions (started_at, label, host_info) VALUES (?1, ?2, ?3)",
            params![started_at, label, host_info],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn stop_session(&self, session_id: i64, ended_at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET ended_at = ?1 WHERE id = ?2",
            params![ended_at, session_id],
        )?;
        Ok(())
    }

    /// Closes any session left open (e.g. after an unclean shutdown) by
    /// stamping it with `ended_at`, so the UI never shows a session that
    /// looks like it's still running when the agent isn't.
    pub fn close_orphaned_sessions(&self, ended_at: i64) -> Result<usize> {
        let n = self.conn.execute(
            "UPDATE sessions SET ended_at = ?1 WHERE ended_at IS NULL",
            params![ended_at],
        )?;
        Ok(n)
    }

    pub fn insert_metrics_batch(&self, session_id: i64, samples: &[MetricSample]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO metrics (
                    session_id, ts, cpu_pct, cpu_per_core_json, mem_used_mb, mem_available_mb,
                    disk_read_bps, disk_write_bps, net_rx_bps, net_tx_bps,
                    context_switches_per_sec, page_faults_per_sec, gpu_pct, top_processes_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            )?;
            for s in samples {
                let cpu_per_core_json = serde_json::to_string(&s.cpu_per_core_pct)?;
                let top_json = serde_json::to_string(&s.top_processes)?;
                stmt.execute(params![
                    session_id,
                    s.ts,
                    s.cpu_pct,
                    cpu_per_core_json,
                    s.mem_used_mb,
                    s.mem_available_mb,
                    s.disk_read_bps,
                    s.disk_write_bps,
                    s.net_rx_bps,
                    s.net_tx_bps,
                    s.context_switches_per_sec,
                    s.page_faults_per_sec,
                    s.gpu_pct,
                    top_json,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn insert_events_batch(&self, session_id: i64, events: &[NewEvent]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO events (session_id, ts, kind, value, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for e in events {
                stmt.execute(params![session_id, e.ts, e.kind, e.value, e.detail])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn events_for_session(&self, session_id: i64) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, ts, kind, value, detail FROM events WHERE session_id = ?1 ORDER BY ts ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(Event {
                id: row.get(0)?,
                session_id: row.get(1)?,
                ts: row.get(2)?,
                kind: row.get(3)?,
                value: row.get(4)?,
                detail: row.get(5)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, started_at, ended_at, label, host_info FROM sessions ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Session {
                id: row.get(0)?,
                started_at: row.get(1)?,
                ended_at: row.get(2)?,
                label: row.get(3)?,
                host_info: row.get(4)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn get_session(&self, session_id: i64) -> Result<Option<Session>> {
        self.conn
            .query_row(
                "SELECT id, started_at, ended_at, label, host_info FROM sessions WHERE id = ?1",
                params![session_id],
                |row| {
                    Ok(Session {
                        id: row.get(0)?,
                        started_at: row.get(1)?,
                        ended_at: row.get(2)?,
                        label: row.get(3)?,
                        host_info: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn metrics_for_session(&self, session_id: i64) -> Result<Vec<MetricSample>> {
        let mut stmt = self.conn.prepare(
            "SELECT ts, cpu_pct, cpu_per_core_json, mem_used_mb, mem_available_mb, disk_read_bps,
                    disk_write_bps, net_rx_bps, net_tx_bps, context_switches_per_sec,
                    page_faults_per_sec, gpu_pct, top_processes_json
             FROM metrics WHERE session_id = ?1 ORDER BY ts ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            let cpu_per_core_json: String = row.get(2)?;
            let top_json: String = row.get(12)?;
            Ok((
                MetricSample {
                    ts: row.get(0)?,
                    cpu_pct: row.get(1)?,
                    cpu_per_core_pct: Vec::new(),
                    mem_used_mb: row.get(3)?,
                    mem_available_mb: row.get(4)?,
                    disk_read_bps: row.get(5)?,
                    disk_write_bps: row.get(6)?,
                    net_rx_bps: row.get(7)?,
                    net_tx_bps: row.get(8)?,
                    context_switches_per_sec: row.get(9)?,
                    page_faults_per_sec: row.get(10)?,
                    gpu_pct: row.get(11)?,
                    top_processes: Vec::new(),
                },
                cpu_per_core_json,
                top_json,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (mut sample, cpu_per_core_json, top_json) = row?;
            sample.cpu_per_core_pct = serde_json::from_str(&cpu_per_core_json)?;
            sample.top_processes = serde_json::from_str(&top_json)?;
            out.push(sample);
        }
        Ok(out)
    }

    pub fn rename_session(&self, session_id: i64, label: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET label = ?1 WHERE id = ?2",
            params![label, session_id],
        )?;
        Ok(())
    }

    /// Cheap aggregate stats for a session's list-view row, computed in SQL
    /// so the UI doesn't have to load every sample just to show a summary.
    pub fn session_summary(&self, session_id: i64) -> Result<SessionSummary> {
        // The percentage mirrors `MetricSample::mem_used_pct`; NULLIF turns a
        // zero total into NULL (ignored by MAX) instead of a division by zero.
        let (sample_count, avg_cpu_pct, peak_cpu_pct, peak_mem_used_mb, peak_mem_used_pct) =
            self.conn.query_row(
                "SELECT COUNT(*), AVG(cpu_pct), MAX(cpu_pct), MAX(mem_used_mb), \
                 MAX(mem_used_mb * 100.0 / NULLIF(mem_used_mb + mem_available_mb, 0)) \
                 FROM metrics WHERE session_id = ?1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<f64>>(1)?,
                        row.get::<_, Option<f64>>(2)?,
                        row.get::<_, Option<f64>>(3)?,
                        row.get::<_, Option<f64>>(4)?,
                    ))
                },
            )?;

        let mut stmt = self
            .conn
            .prepare("SELECT kind, COUNT(*) FROM events WHERE session_id = ?1 GROUP BY kind")?;
        let event_counts = stmt
            .query_map(params![session_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(SessionSummary {
            sample_count,
            avg_cpu_pct,
            peak_cpu_pct,
            peak_mem_used_mb,
            peak_mem_used_pct,
            event_counts,
        })
    }

    pub fn delete_session(&self, session_id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM metrics WHERE session_id = ?1",
            params![session_id],
        )?;
        self.conn.execute(
            "DELETE FROM events WHERE session_id = ?1",
            params![session_id],
        )?;
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
        Ok(())
    }

    /// Deletes completed sessions older than `cutoff_ts`, then — if
    /// `max_db_bytes` is given and the database is still over that size —
    /// keeps deleting the oldest remaining completed session until it's
    /// under the cap or none are left. A session still recording
    /// (`ended_at IS NULL`) is never pruned by either rule, however old or
    /// however far over a size cap the database is. Returns the number of
    /// sessions deleted by either rule.
    pub fn prune(&self, cutoff_ts: i64, max_db_bytes: Option<u64>) -> Result<usize> {
        let mut deleted = self.prune_older_than(cutoff_ts)?;
        if let Some(max_bytes) = max_db_bytes {
            deleted += self.prune_to_max_size(max_bytes)?;
        }
        Ok(deleted)
    }

    pub fn prune_older_than(&self, cutoff_ts: i64) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM metrics WHERE session_id IN \
             (SELECT id FROM sessions WHERE started_at < ?1 AND ended_at IS NOT NULL)",
            params![cutoff_ts],
        )?;
        tx.execute(
            "DELETE FROM events WHERE session_id IN \
             (SELECT id FROM sessions WHERE started_at < ?1 AND ended_at IS NOT NULL)",
            params![cutoff_ts],
        )?;
        let n = tx.execute(
            "DELETE FROM sessions WHERE started_at < ?1 AND ended_at IS NOT NULL",
            params![cutoff_ts],
        )?;
        tx.commit()?;
        Ok(n)
    }

    /// Deletes the oldest completed sessions, one at a time, until
    /// [`Self::db_size_bytes`] reports at or under `max_bytes` or there are
    /// no more completed sessions to delete. `VACUUM`s once at the end if
    /// anything was deleted, so the file on disk actually shrinks to match
    /// rather than just freeing pages for SQLite to reuse internally.
    pub fn prune_to_max_size(&self, max_bytes: u64) -> Result<usize> {
        let mut deleted = 0usize;
        while self.db_size_bytes()? > max_bytes {
            let oldest: Option<i64> = self
                .conn
                .query_row(
                    "SELECT id FROM sessions WHERE ended_at IS NOT NULL \
                     ORDER BY started_at ASC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(id) = oldest else { break };
            self.delete_session(id)?;
            deleted += 1;
        }
        if deleted > 0 {
            self.conn.execute_batch("VACUUM")?;
        }
        Ok(deleted)
    }

    /// Approximate on-disk database size: `(total pages − free pages) ×
    /// page size`, not just `page_count × page_size` — without a `VACUUM`
    /// (which `prune_to_max_size` only does once it's finished deleting,
    /// not after every single delete), pages freed by `DELETE` sit on
    /// SQLite's internal freelist and still count toward `page_count` even
    /// though they hold no data, which would make size-cap pruning delete
    /// every session before ever observing a smaller size.
    pub fn db_size_bytes(&self) -> Result<u64> {
        let page_count: i64 = self
            .conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let freelist_count: i64 = self
            .conn
            .query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
        let page_size: i64 = self
            .conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))?;
        Ok(((page_count - freelist_count).max(0) * page_size) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_stop_and_roundtrip_metrics() {
        let store = Store::open(":memory:").unwrap();
        let id = store
            .start_session(1000, Some("test"), Some("host"))
            .unwrap();

        let sample = MetricSample {
            ts: 1001,
            cpu_pct: 12.5,
            cpu_per_core_pct: vec![10.0, 15.0],
            mem_used_mb: 2048.0,
            mem_available_mb: 4096.0,
            disk_read_bps: 100.0,
            disk_write_bps: 50.0,
            net_rx_bps: 10.0,
            net_tx_bps: 5.0,
            context_switches_per_sec: 2500.0,
            page_faults_per_sec: 75.0,
            gpu_pct: Some(33.0),
            top_processes: vec![ProcessSample {
                pid: 42,
                name: "example.exe".into(),
                cpu_pct: 9.0,
                mem_mb: 128.0,
                handle_count: Some(200),
                thread_count: Some(12),
            }],
        };
        store.insert_metrics_batch(id, &[sample]).unwrap();
        store.stop_session(id, 2000).unwrap();

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].ended_at, Some(2000));

        let metrics = store.metrics_for_session(id).unwrap();
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].top_processes[0].name, "example.exe");
        assert_eq!(metrics[0].top_processes[0].handle_count, Some(200));
        assert_eq!(metrics[0].context_switches_per_sec, 2500.0);
        assert_eq!(metrics[0].gpu_pct, Some(33.0));
    }

    #[test]
    fn close_orphaned_sessions_closes_only_still_open_ones() {
        let store = Store::open(":memory:").unwrap();
        let clean = store.start_session(1000, None, None).unwrap();
        store.stop_session(clean, 2000).unwrap();
        // Simulates what a crash/unclean-shutdown leaves behind: a session
        // row with no `ended_at`, exactly what `Store::open` on the next
        // agent start must find and close (see crates/agent/src/main.rs).
        let orphaned = store.start_session(3000, None, None).unwrap();

        let closed = store.close_orphaned_sessions(9999).unwrap();
        assert_eq!(closed, 1, "only the orphaned session should be touched");

        assert_eq!(
            store.get_session(clean).unwrap().unwrap().ended_at,
            Some(2000),
            "an already-closed session's end time must not be overwritten"
        );
        assert_eq!(
            store.get_session(orphaned).unwrap().unwrap().ended_at,
            Some(9999)
        );

        // Idempotent: running it again (e.g. a second restart) with
        // nothing left open closes nothing.
        assert_eq!(store.close_orphaned_sessions(99999).unwrap(), 0);
    }

    #[test]
    fn rename_session_updates_label() {
        let store = Store::open(":memory:").unwrap();
        let id = store.start_session(1000, Some("first"), None).unwrap();

        store.rename_session(id, Some("renamed")).unwrap();
        assert_eq!(
            store.get_session(id).unwrap().unwrap().label.as_deref(),
            Some("renamed")
        );

        store.rename_session(id, None).unwrap();
        assert_eq!(store.get_session(id).unwrap().unwrap().label, None);
    }

    #[test]
    fn session_summary_aggregates_metrics_and_events() {
        let store = Store::open(":memory:").unwrap();
        let id = store.start_session(1000, None, None).unwrap();

        let sample = |ts: i64, cpu: f32, mem: f32| MetricSample {
            ts,
            cpu_pct: cpu,
            cpu_per_core_pct: vec![cpu],
            mem_used_mb: mem,
            mem_available_mb: 1024.0,
            disk_read_bps: 0.0,
            disk_write_bps: 0.0,
            net_rx_bps: 0.0,
            net_tx_bps: 0.0,
            context_switches_per_sec: 0.0,
            page_faults_per_sec: 0.0,
            gpu_pct: None,
            top_processes: Vec::new(),
        };
        store
            .insert_metrics_batch(id, &[sample(1001, 10.0, 100.0), sample(1002, 90.0, 300.0)])
            .unwrap();
        store
            .insert_events_batch(
                id,
                &[
                    NewEvent {
                        ts: 1002,
                        kind: "cpu_spike".into(),
                        value: 90.0,
                        detail: None,
                    },
                    NewEvent {
                        ts: 1003,
                        kind: "cpu_spike".into(),
                        value: 95.0,
                        detail: None,
                    },
                    NewEvent {
                        ts: 1004,
                        kind: "stall".into(),
                        value: 60.0,
                        detail: None,
                    },
                ],
            )
            .unwrap();

        let summary = store.session_summary(id).unwrap();
        assert_eq!(summary.sample_count, 2);
        assert_eq!(summary.avg_cpu_pct, Some(50.0));
        assert_eq!(summary.peak_cpu_pct, Some(90.0));
        assert_eq!(summary.peak_mem_used_mb, Some(300.0));
        // The SQL percentage must agree with `MetricSample::mem_used_pct`.
        let expected = sample(1002, 90.0, 300.0).mem_used_pct().unwrap() as f64;
        let got = summary.peak_mem_used_pct.unwrap();
        assert!((got - expected).abs() < 1e-3, "{got} vs {expected}");
        let mut counts = summary.event_counts.clone();
        counts.sort();
        assert_eq!(
            counts,
            vec![("cpu_spike".to_string(), 2), ("stall".to_string(), 1)]
        );
    }

    #[test]
    fn session_summary_of_empty_session_has_none_stats() {
        let store = Store::open(":memory:").unwrap();
        let id = store.start_session(1000, None, None).unwrap();
        let summary = store.session_summary(id).unwrap();
        assert_eq!(summary.sample_count, 0);
        assert_eq!(summary.avg_cpu_pct, None);
        assert_eq!(summary.peak_cpu_pct, None);
        assert_eq!(summary.peak_mem_used_mb, None);
        assert_eq!(summary.peak_mem_used_pct, None);
        assert!(summary.event_counts.is_empty());
    }

    #[test]
    fn mem_used_pct_is_used_over_used_plus_available() {
        let mut s = MetricSample {
            ts: 0,
            cpu_pct: 0.0,
            cpu_per_core_pct: Vec::new(),
            mem_used_mb: 16_000.0,
            mem_available_mb: 48_000.0,
            disk_read_bps: 0.0,
            disk_write_bps: 0.0,
            net_rx_bps: 0.0,
            net_tx_bps: 0.0,
            context_switches_per_sec: 0.0,
            page_faults_per_sec: 0.0,
            gpu_pct: None,
            top_processes: Vec::new(),
        };
        assert_eq!(s.mem_total_mb(), 64_000.0);
        assert_eq!(s.mem_used_pct(), Some(25.0));
        // No memory figures at all: no percentage, and no division by zero,
        // in Rust or in the summary SQL.
        s.mem_used_mb = 0.0;
        s.mem_available_mb = 0.0;
        assert_eq!(s.mem_used_pct(), None);
        let store = Store::open(":memory:").unwrap();
        let id = store.start_session(0, None, None).unwrap();
        store.insert_metrics_batch(id, &[s]).unwrap();
        assert_eq!(store.session_summary(id).unwrap().peak_mem_used_pct, None);
    }

    fn minimal_sample(ts: i64) -> MetricSample {
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

    #[test]
    fn prune_older_than_deletes_old_completed_sessions_but_keeps_recent_and_recording() {
        let store = Store::open(":memory:").unwrap();

        let old = store.start_session(1_000, None, None).unwrap();
        store
            .insert_metrics_batch(old, &[minimal_sample(1_000)])
            .unwrap();
        store.stop_session(old, 1_500).unwrap();

        let recent = store.start_session(50_000, None, None).unwrap();
        store.stop_session(recent, 50_500).unwrap();

        let still_recording = store.start_session(500, None, None).unwrap();

        let deleted = store.prune_older_than(10_000).unwrap();
        assert_eq!(deleted, 1);

        let remaining: Vec<i64> = store
            .list_sessions()
            .unwrap()
            .iter()
            .map(|s| s.id)
            .collect();
        assert!(
            !remaining.contains(&old),
            "old completed session should be pruned"
        );
        assert!(remaining.contains(&recent), "recent session must survive");
        assert!(
            remaining.contains(&still_recording),
            "a recording session must never be pruned, however old its started_at is"
        );
        assert!(
            store.metrics_for_session(old).unwrap().is_empty(),
            "pruning a session must also remove its metrics"
        );
    }

    #[test]
    fn prune_to_max_size_deletes_oldest_completed_sessions_first() {
        let store = Store::open(":memory:").unwrap();
        let mut ids = Vec::new();
        for i in 0..20 {
            let id = store.start_session(i * 1000, None, None).unwrap();
            let samples: Vec<_> = (0..50).map(|j| minimal_sample(i * 1000 + j)).collect();
            store.insert_metrics_batch(id, &samples).unwrap();
            store.stop_session(id, i * 1000 + 999).unwrap();
            ids.push(id);
        }

        let before = store.db_size_bytes().unwrap();
        assert!(before > 0);

        // A cap smaller than the current size must shrink it, by deleting
        // the oldest sessions (lowest `started_at`) first.
        let cap = before / 2;
        let deleted = store.prune_to_max_size(cap).unwrap();
        assert!(deleted > 0, "some sessions should have been deleted");
        assert!(store.db_size_bytes().unwrap() <= cap);

        // Oldest-first, not just "some were deleted": no surviving session
        // may be older than any deleted one. (Not asserting an exact
        // deleted-count/size-fraction relationship — SQLite pages can hold
        // rows from more than one session, so freed space per session
        // isn't perfectly uniform.)
        let remaining: Vec<i64> = store
            .list_sessions()
            .unwrap()
            .iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(remaining.len(), ids.len() - deleted);
        let max_deleted_start = ids
            .iter()
            .enumerate()
            .filter(|(_, id)| !remaining.contains(id))
            .map(|(i, _)| i as i64 * 1000)
            .max()
            .unwrap();
        let min_remaining_start = ids
            .iter()
            .enumerate()
            .filter(|(_, id)| remaining.contains(id))
            .map(|(i, _)| i as i64 * 1000)
            .min()
            .unwrap();
        assert!(
            max_deleted_start < min_remaining_start,
            "every deleted session must be older than every surviving one"
        );
    }

    #[test]
    fn prune_to_max_size_never_deletes_a_recording_session() {
        let store = Store::open(":memory:").unwrap();
        let id = store.start_session(0, None, None).unwrap();
        let samples: Vec<_> = (0..200).map(minimal_sample).collect();
        store.insert_metrics_batch(id, &samples).unwrap();
        // Never stopped: still "recording".

        let deleted = store.prune_to_max_size(1).unwrap();
        assert_eq!(deleted, 0);
        assert!(store.get_session(id).unwrap().is_some());
    }

    #[test]
    fn prune_combines_age_and_size_rules() {
        let store = Store::open(":memory:").unwrap();
        let old = store.start_session(0, None, None).unwrap();
        store
            .insert_metrics_batch(old, &[minimal_sample(0)])
            .unwrap();
        store.stop_session(old, 100).unwrap();

        let newer = store.start_session(100_000, None, None).unwrap();
        let samples: Vec<_> = (0..200).map(|i| minimal_sample(100_000 + i)).collect();
        store.insert_metrics_batch(newer, &samples).unwrap();
        store.stop_session(newer, 200_000).unwrap();

        let deleted = store.prune(50_000, Some(1)).unwrap();
        // `old` is pruned by age, `newer` by the size cap (it's the only
        // one left once `old` is gone, and 1 byte is smaller than anything).
        assert_eq!(deleted, 2);
        assert!(store.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn events_roundtrip() {
        let store = Store::open(":memory:").unwrap();
        let id = store.start_session(1000, None, None).unwrap();

        store
            .insert_events_batch(
                id,
                &[
                    NewEvent {
                        ts: 1010,
                        kind: "stall".into(),
                        value: 120.0,
                        detail: None,
                    },
                    NewEvent {
                        ts: 1020,
                        kind: "cpu_spike".into(),
                        value: 97.5,
                        detail: Some("exceeded 90%".into()),
                    },
                ],
            )
            .unwrap();

        let events = store.events_for_session(id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "stall");
        assert_eq!(events[1].detail.as_deref(), Some("exceeded 90%"));
    }

    #[test]
    fn migrate_adds_missing_columns_to_existing_metrics_table() {
        let db_path = std::env::temp_dir().join(format!(
            "ghostperf-storage-migrate-test-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        {
            // Simulate a pre-Phase-2 DB: only the original columns exist.
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE sessions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, started_at INTEGER NOT NULL,
                    ended_at INTEGER, label TEXT, host_info TEXT
                );
                CREATE TABLE metrics (
                    session_id INTEGER NOT NULL, ts INTEGER NOT NULL, cpu_pct REAL NOT NULL,
                    cpu_per_core_json TEXT NOT NULL, mem_used_mb REAL NOT NULL,
                    mem_available_mb REAL NOT NULL, disk_read_bps REAL NOT NULL,
                    disk_write_bps REAL NOT NULL, net_rx_bps REAL NOT NULL,
                    net_tx_bps REAL NOT NULL, top_processes_json TEXT NOT NULL
                );
                "#,
            )
            .unwrap();
        }

        // Opening via `Store::open` should migrate the old table in place.
        let store = Store::open(&db_path).unwrap();
        let id = store.start_session(1, None, None).unwrap();
        let sample = MetricSample {
            ts: 2,
            cpu_pct: 1.0,
            cpu_per_core_pct: vec![1.0],
            mem_used_mb: 1.0,
            mem_available_mb: 1.0,
            disk_read_bps: 0.0,
            disk_write_bps: 0.0,
            net_rx_bps: 0.0,
            net_tx_bps: 0.0,
            context_switches_per_sec: 9.0,
            page_faults_per_sec: 3.0,
            gpu_pct: None,
            top_processes: Vec::new(),
        };
        store.insert_metrics_batch(id, &[sample]).unwrap();
        let metrics = store.metrics_for_session(id).unwrap();
        assert_eq!(metrics[0].context_switches_per_sec, 9.0);

        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db_path.display()));
        }
    }
}
