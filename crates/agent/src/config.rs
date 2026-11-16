use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
// Fields missing from an existing agent.toml (e.g. one written by an older
// version before a setting existed) fall back to `Default` instead of failing
// to parse and stopping the agent from starting.
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
    pub db_path: PathBuf,
    /// Loopback-only TCP port used for the local start/stop/status control channel.
    pub control_port: u16,
    /// How many days of completed sessions to retain before auto-pruning.
    pub retention_days: u32,
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
            db_path: default_db_path(),
            // Kept below 44620: on some Windows machines (WSL/Hyper-V/VPN
            // drivers) a block of loopback ports from 44620 to 48715 is held
            // without appearing in `excludedportrange`, and the old default
            // 47631 fell inside it.
            control_port: 38631,
            retention_days: 30,
            stall_check_interval_ms: 15,
            stall_threshold_ms: 50,
            cpu_spike_threshold_pct: 90.0,
            mem_spike_threshold_pct: 90.0,
            eventlog_correlation: true,
        }
    }
}

fn default_db_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("GhostPerf").join("ghostperf.db")
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
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        base.join("GhostPerf").join("agent.toml")
    }
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
        assert_eq!(cfg.control_port, 40000);
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
}
