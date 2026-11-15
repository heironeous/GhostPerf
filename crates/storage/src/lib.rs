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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSample {
    pub pid: u32,
    pub name: String,
    pub cpu_pct: f32,
    pub mem_mb: f32,
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
    pub top_processes: Vec<ProcessSample>,
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
            "#,
        )?;
        Ok(())
    }

    pub fn start_session(&self, started_at: i64, label: Option<&str>, host_info: Option<&str>) -> Result<i64> {
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
                    disk_read_bps, disk_write_bps, net_rx_bps, net_tx_bps, top_processes_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                    top_json,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
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
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
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
                    disk_write_bps, net_rx_bps, net_tx_bps, top_processes_json
             FROM metrics WHERE session_id = ?1 ORDER BY ts ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            let cpu_per_core_json: String = row.get(2)?;
            let top_json: String = row.get(9)?;
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

    pub fn delete_session(&self, session_id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM metrics WHERE session_id = ?1", params![session_id])?;
        self.conn.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_stop_and_roundtrip_metrics() {
        let store = Store::open(":memory:").unwrap();
        let id = store.start_session(1000, Some("test"), Some("host")).unwrap();

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
            top_processes: vec![ProcessSample {
                pid: 42,
                name: "example.exe".into(),
                cpu_pct: 9.0,
                mem_mb: 128.0,
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
    }
}
