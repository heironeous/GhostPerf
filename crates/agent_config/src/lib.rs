//! `AgentConfig`, the `agent.toml` load/default path, and the agent's
//! runtime control-port file — split out of the `agent` crate so other
//! processes (the tray, later the UI) agree on where these live and how
//! they're formatted without duplicating that logic.

mod control_port;

pub use control_port::{
    port_file_path, read_port_file, remove_port_file, send_command, write_port_file,
};

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Directory holding `agent.toml`, the default database, and the agent's
/// runtime files (`agent.port`, `agent.lock`).
///
/// Portable mode (Phase 5) takes priority: if a `data` folder already
/// exists next to the running executable, everything lives there instead
/// of `%APPDATA%\GhostPerf` — no `--portable` flag and no separate
/// portable build, since all three binaries make this same check. A
/// portable `.zip` distribution ships with that empty `data` folder
/// alongside the exes; an installed copy doesn't have one, so it falls
/// through to `%APPDATA%`. `%APPDATA%` itself doesn't exist on non-Windows
/// dev builds, so that path falls back to the OS temp dir.
pub fn data_dir() -> PathBuf {
    if let Some(portable) = portable_data_dir() {
        return portable;
    }
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("GhostPerf")
}

fn portable_data_dir() -> Option<PathBuf> {
    portable_data_dir_in(&std::env::current_exe().ok()?)
}

/// Pure half of [`portable_data_dir`], taking the executable path as an
/// argument instead of calling [`std::env::current_exe`] itself, so it's
/// testable without actually controlling what that returns.
fn portable_data_dir_in(exe_path: &Path) -> Option<PathBuf> {
    let dir = exe_path.parent()?.join("data");
    dir.is_dir().then_some(dir)
}

/// Where each binary writes its log file (`agent.log`, `tray.log`, …): the
/// binaries are windowless, so there's no console to print to.
pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

const DEFAULT_DB_FILE: &str = "ghostperf.db";

/// Pure half of [`AgentConfig::resolved_db_path`]. `Path::join` with an
/// absolute path returns that path unchanged, so an absolute `db_path` (one
/// set by hand, or saved by an older version) is used as is.
fn resolve_db_path(data_dir: &Path, db_path: &Path) -> PathBuf {
    data_dir.join(db_path)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
// Fields missing from an existing agent.toml (e.g. one written by an older
// version before a setting existed) fall back to `Default` instead of failing
// to parse and stopping the agent from starting. Unknown fields are ignored
// rather than rejected (no `deny_unknown_fields`) for the same reason in
// reverse: settings that have since been removed, like `control_port`, may
// still be sitting in an older file.
#[serde(default)]
pub struct AgentConfig {
    /// How often to sample metrics while a session is recording.
    pub sample_interval_ms: u64,
    /// How many samples to buffer in memory before flushing to SQLite.
    pub flush_batch_size: usize,
    /// Max time a batch can sit in memory before being flushed, even if
    /// `flush_batch_size` hasn't been reached yet.
    pub flush_interval_ms: u64,
    /// Number of top CPU-consuming processes to snapshot each sample.
    pub top_n_processes: usize,
    /// The SQLite database. A relative path (the default, `ghostperf.db`) is
    /// relative to [`data_dir`]; read it through [`AgentConfig::resolved_db_path`].
    /// It used to default to an absolute path, which `load_or_default` then
    /// saved into `agent.toml` on first run, so a portable copy that was later
    /// moved or copied elsewhere kept pointing at its *old* location: the
    /// agent failed to start, or recorded into the original copy's database
    /// if that still existed.
    pub db_path: PathBuf,
    /// How many days of completed sessions to retain before auto-pruning.
    pub retention_days: u32,
    /// If set, the database is additionally capped at this size: when over
    /// it, the oldest completed sessions are deleted (regardless of
    /// `retention_days`) until it's back under the cap. `None` (the
    /// default) means no size cap, only the age-based rule above.
    #[serde(default)]
    pub retention_max_db_mb: Option<u64>,
    /// How often the stall detector's timing thread wakes up and measures
    /// how late it is. Shorter intervals catch shorter stalls but cost more
    /// (harmless) wakeups.
    pub stall_check_interval_ms: u64,
    /// A wakeup running this many ms later than `stall_check_interval_ms`
    /// scheduled it is recorded as a stall event.
    pub stall_threshold_ms: u64,
    /// A sample with `cpu_pct` at or above this is recorded as a `cpu_spike` event.
    pub cpu_spike_threshold_pct: f32,
    /// A sample with memory utilization (`mem_used_mb` / total) at or above
    /// this is recorded as a `mem_spike` event.
    pub mem_spike_threshold_pct: f32,
    /// Whether to correlate Windows Event Log entries (System/Application,
    /// Warning and above) into a session's events when it stops. No-op on
    /// non-Windows builds.
    pub eventlog_correlation: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            sample_interval_ms: 1000,
            flush_batch_size: 30,
            flush_interval_ms: 10_000,
            top_n_processes: 5,
            db_path: PathBuf::from(DEFAULT_DB_FILE),
            retention_days: 30,
            retention_max_db_mb: None,
            stall_check_interval_ms: 15,
            stall_threshold_ms: 50,
            cpu_spike_threshold_pct: 90.0,
            mem_spike_threshold_pct: 90.0,
            eventlog_correlation: true,
        }
    }
}

impl AgentConfig {
    pub fn load_or_default(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            let text = std::fs::read_to_string(path)?;
            Ok(toml::from_str(&text)?)
        } else {
            let cfg = Self::default();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, toml::to_string_pretty(&cfg)?)?;
            Ok(cfg)
        }
    }

    pub fn default_config_path() -> PathBuf {
        data_dir().join("agent.toml")
    }

    /// Where the database actually is: `db_path` resolved against [`data_dir`].
    pub fn resolved_db_path(&self) -> PathBuf {
        resolve_db_path(&data_dir(), &self.db_path)
    }

    /// Writes this config back to `path` (e.g. after the UI's settings
    /// screen edits it). The agent doesn't watch the file: a running agent
    /// picks the change up when sent the `reload` control command, which
    /// the settings screen does right after saving.
    pub fn save(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// `name: old -> new` for each setting that differs between `self` and
    /// `other`, in name order, for the agent to log what a reload changed.
    /// Compared through the TOML form, so it covers every field, including
    /// ones added later, without listing them here.
    pub fn changed_settings(&self, other: &Self) -> Vec<String> {
        let (Ok(toml::Value::Table(old)), Ok(toml::Value::Table(new))) =
            (toml::Value::try_from(self), toml::Value::try_from(other))
        else {
            return Vec::new();
        };
        // An `Option` field that's `None` is left out of the TOML entirely.
        let show =
            |v: Option<&toml::Value>| v.map_or_else(|| "unset".to_string(), |v| v.to_string());
        let names: std::collections::BTreeSet<&String> = old.keys().chain(new.keys()).collect();
        names
            .into_iter()
            .filter(|name| old.get(*name) != new.get(*name))
            .map(|name| format!("{name}: {} -> {}", show(old.get(name)), show(new.get(name))))
            .collect()
    }
}

/// Whether this copy runs in portable mode (a `data` folder next to the
/// exe; see [`data_dir`]).
pub fn is_portable() -> bool {
    portable_data_dir().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_older_version_missing_new_fields_still_loads() {
        // Exactly what a Phase 1 agent wrote: none of the stall/spike/eventlog fields.
        let old = r#"
sample_interval_ms = 500
flush_batch_size = 30
flush_interval_ms = 10000
top_n_processes = 5
db_path = 'C:\GhostPerf\ghostperf.db'
control_port = 40000
retention_days = 30
"#;
        let cfg: AgentConfig = toml::from_str(old).expect("old config should still parse");
        let defaults = AgentConfig::default();
        assert_eq!(cfg.sample_interval_ms, 500);
        assert_eq!(
            cfg.stall_check_interval_ms,
            defaults.stall_check_interval_ms
        );
        assert_eq!(cfg.stall_threshold_ms, defaults.stall_threshold_ms);
        assert_eq!(
            cfg.cpu_spike_threshold_pct,
            defaults.cpu_spike_threshold_pct
        );
        assert_eq!(cfg.eventlog_correlation, defaults.eventlog_correlation);
    }

    #[test]
    fn config_with_removed_control_port_field_still_loads() {
        // Every agent.toml auto-created before the control port became
        // OS-assigned has this line; it must be ignored, not fatal.
        let old = "sample_interval_ms = 750\ncontrol_port = 38631\n";
        let cfg: AgentConfig = toml::from_str(old).expect("stale control_port should be ignored");
        assert_eq!(cfg.sample_interval_ms, 750);
    }

    #[test]
    fn config_missing_retention_max_db_mb_defaults_to_unlimited() {
        let old = "sample_interval_ms = 500\n";
        let cfg: AgentConfig = toml::from_str(old).unwrap();
        assert_eq!(cfg.retention_max_db_mb, None);
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ghostperf-agent-config-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn portable_data_dir_is_used_when_a_data_folder_sits_next_to_the_exe() {
        let dir = scratch_dir("portable-present");
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let exe = dir.join("ghostperf-agent.exe");
        assert_eq!(portable_data_dir_in(&exe), Some(dir.join("data")));
    }

    #[test]
    fn no_data_folder_next_to_the_exe_means_not_portable() {
        let dir = scratch_dir("portable-absent");
        let exe = dir.join("ghostperf-agent.exe");
        assert_eq!(portable_data_dir_in(&exe), None);
    }

    #[test]
    fn a_file_named_data_does_not_count_as_portable_mode() {
        // Only a *directory* called `data` is the marker; a stray file with
        // that name (unlikely, but cheap to guard) must not trigger it.
        let dir = scratch_dir("portable-file-not-dir");
        std::fs::write(dir.join("data"), b"not a directory").unwrap();
        let exe = dir.join("ghostperf-agent.exe");
        assert_eq!(portable_data_dir_in(&exe), None);
    }

    #[test]
    fn default_db_path_is_saved_relative_so_a_moved_data_dir_follows() {
        // What `load_or_default` writes on first run must not pin the
        // database to the folder it was first run from.
        let saved = toml::to_string_pretty(&AgentConfig::default()).unwrap();
        let cfg: AgentConfig = toml::from_str(&saved).unwrap();
        assert!(cfg.db_path.is_relative());
        let moved = Path::new(r"E:\GhostPerf\data");
        assert_eq!(
            resolve_db_path(moved, &cfg.db_path),
            moved.join("ghostperf.db")
        );
    }

    #[test]
    fn an_absolute_db_path_is_used_as_is() {
        let abs = std::env::temp_dir().join("elsewhere").join("ghostperf.db");
        assert_eq!(resolve_db_path(Path::new("unused"), &abs), abs);
    }

    #[test]
    fn changed_settings_lists_only_what_differs() {
        let old = AgentConfig::default();
        assert!(old.changed_settings(&old.clone()).is_empty());

        let new = AgentConfig {
            sample_interval_ms: 500,
            retention_max_db_mb: Some(200),
            ..old.clone()
        };
        assert_eq!(
            old.changed_settings(&new),
            [
                "retention_max_db_mb: unset -> 200",
                "sample_interval_ms: 1000 -> 500"
            ]
        );
    }

    #[test]
    fn save_then_load_or_default_roundtrips_edited_fields() {
        let path = std::env::temp_dir().join(format!(
            "ghostperf-agent-config-save-test-{}.toml",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let mut cfg = AgentConfig::default();
        cfg.sample_interval_ms = 2500;
        cfg.retention_days = 7;
        cfg.save(&path).unwrap();

        let loaded = AgentConfig::load_or_default(&path).unwrap();
        assert_eq!(loaded.sample_interval_ms, 2500);
        assert_eq!(loaded.retention_days, 7);

        std::fs::remove_file(&path).unwrap();
    }
}
