use std::path::PathBuf;

use agent_config::AgentConfig;

pub struct SettingsView {
    config_path: PathBuf,
    config: AgentConfig,
    /// `AgentConfig::retention_max_db_mb` split into a checkbox and a
    /// value, so disabling it doesn't lose the number the user had typed
    /// (re-enabling restores it instead of resetting to the default).
    max_db_cap_enabled: bool,
    max_db_cap_mb: u64,
    status: Option<String>,
}

const DEFAULT_MAX_DB_CAP_MB: u64 = 500;

impl SettingsView {
    pub fn load() -> Self {
        let config_path = AgentConfig::default_config_path();
        let config = AgentConfig::load_or_default(&config_path).unwrap_or_default();
        let max_db_cap_enabled = config.retention_max_db_mb.is_some();
        let max_db_cap_mb = config.retention_max_db_mb.unwrap_or(DEFAULT_MAX_DB_CAP_MB);
        Self {
            config_path,
            config,
            max_db_cap_enabled,
            max_db_cap_mb,
            status: None,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<crate::app::Action> {
        ui.heading("Settings");
        ui.label(format!("Config file: {}", self.config_path.display()));
        ui.weak("Saving applies the changes to the running agent right away, including to a recording in progress.");
        ui.separator();

        egui::Grid::new("settings_grid")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                ui.label("Sample interval (ms)");
                ui.add(
                    egui::DragValue::new(&mut self.config.sample_interval_ms).range(100..=60_000),
                );
                ui.end_row();

                ui.label("Top N processes");
                ui.add(egui::DragValue::new(&mut self.config.top_n_processes).range(1..=50));
                ui.end_row();

                ui.label("Retention (days)");
                ui.add(egui::DragValue::new(&mut self.config.retention_days).range(1..=3650));
                ui.end_row();

                ui.label("Max database size");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.max_db_cap_enabled, "cap at");
                    ui.add_enabled(
                        self.max_db_cap_enabled,
                        egui::DragValue::new(&mut self.max_db_cap_mb)
                            .range(10..=1_000_000)
                            .suffix(" MB"),
                    );
                });
                ui.end_row();

                ui.label("CPU spike threshold (%)");
                ui.add(
                    egui::DragValue::new(&mut self.config.cpu_spike_threshold_pct)
                        .range(1.0..=100.0),
                );
                ui.end_row();

                ui.label("Memory spike threshold (%)");
                ui.add(
                    egui::DragValue::new(&mut self.config.mem_spike_threshold_pct)
                        .range(1.0..=100.0),
                );
                ui.end_row();

                ui.label("Windows Event Log correlation");
                ui.checkbox(&mut self.config.eventlog_correlation, "enabled");
                ui.end_row();

                ui.label("Database location");
                ui.label(self.config.resolved_db_path().display().to_string());
                ui.end_row();
            });

        ui.separator();
        if ui.button("Save").clicked() {
            self.config.retention_max_db_mb = self.max_db_cap_enabled.then_some(self.max_db_cap_mb);
            self.status = Some(match self.config.save(&self.config_path) {
                Ok(()) => reload_status(tell_agent_to_reload()),
                Err(e) => format!("Save failed: {e:#}"),
            });
        }
        if let Some(status) = &self.status {
            ui.label(status);
        }
        None
    }
}

/// Sends `reload` to the running agent. `None` if there's no agent to tell
/// (no `agent.port`); an error if one is advertised but didn't answer, which
/// is usually a stale port file left by a crash.
fn tell_agent_to_reload() -> Option<anyhow::Result<String>> {
    let port = agent_config::read_port_file(&agent_config::port_file_path())?;
    let reply = agent_config::send_command(port, "reload");
    match &reply {
        Ok(r) => log::info!("settings saved; agent replied to reload: {r}"),
        Err(e) => log::warn!("settings saved; reload to agent on port {port} failed: {e:#}"),
    }
    Some(reply)
}

/// The message under the Save button for each outcome of the reload.
fn reload_status(reply: Option<anyhow::Result<String>>) -> String {
    const NOT_RUNNING: &str =
        "Saved. The agent isn't running; it will use these settings when it starts.";
    match reply {
        None | Some(Err(_)) => NOT_RUNNING.to_string(),
        Some(Ok(r)) if r.starts_with("OK") => "Saved and applied to the running agent.".to_string(),
        // The agent re-reads the file itself, so a refusal means the file it
        // read isn't what was just saved, or doesn't parse.
        Some(Ok(r)) => {
            format!("Saved, but the agent didn't apply it ({r}). It will on its next start.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_status_covers_each_outcome() {
        assert!(reload_status(Some(Ok("OK reloaded changes=2".into()))).contains("applied"));
        assert!(reload_status(None).contains("isn't running"));
        assert!(reload_status(Some(Err(anyhow::anyhow!("refused")))).contains("isn't running"));
        let refused = reload_status(Some(Ok("ERR agent.toml: bad".into())));
        assert!(refused.contains("didn't apply") && refused.contains("ERR agent.toml: bad"));
    }
}
