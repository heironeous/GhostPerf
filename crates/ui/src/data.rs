//! Thin wrapper around `storage::Store` for the UI: mostly reads (list
//! sessions, fetch metrics/events for one), plus the handful of writes the
//! UI itself needs (rename, delete) — see TODO.md Phase 4.

use storage::{Event, MetricSample, Session, SessionSummary, Store};

pub struct SessionRow {
    pub session: Session,
    pub summary: SessionSummary,
}

pub struct Db {
    store: Store,
}

impl Db {
    pub fn open(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        Ok(Self {
            store: Store::open(path)?,
        })
    }

    pub fn list_session_rows(&self) -> anyhow::Result<Vec<SessionRow>> {
        self.store
            .list_sessions()?
            .into_iter()
            .map(|session| {
                let summary = self.store.session_summary(session.id)?;
                Ok(SessionRow { session, summary })
            })
            .collect()
    }

    pub fn session(&self, id: i64) -> anyhow::Result<Option<Session>> {
        Ok(self.store.get_session(id)?)
    }

    pub fn metrics(&self, id: i64) -> anyhow::Result<Vec<MetricSample>> {
        Ok(self.store.metrics_for_session(id)?)
    }

    pub fn events(&self, id: i64) -> anyhow::Result<Vec<Event>> {
        Ok(self.store.events_for_session(id)?)
    }

    pub fn rename(&self, id: i64, label: Option<&str>) -> anyhow::Result<()> {
        Ok(self.store.rename_session(id, label)?)
    }

    pub fn delete(&self, id: i64) -> anyhow::Result<()> {
        Ok(self.store.delete_session(id)?)
    }
}
