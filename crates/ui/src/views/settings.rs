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
        ui.weak("Changes apply the next time the agent starts — it doesn't watch this file.");
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
                ui.label(self.config.db_path.display().to_string());
                ui.end_row();
            });

        ui.separator();
        if ui.button("Save").clicked() {
            self.config.retention_max_db_mb = self.max_db_cap_enabled.then_some(self.max_db_cap_mb);
            self.status = Some(match self.config.save(&self.config_path) {
                Ok(()) => "Saved. Restart the agent for changes to take effect.".to_string(),
                Err(e) => format!("Save failed: {e:#}"),
            });
        }
        if let Some(status) = &self.status {
            ui.label(status);
        }
        None
    }
}
