use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
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
