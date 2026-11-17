// Windowless console on Windows: the egui window is the only window this
// process should show (same reasoning as agent/tray — see crates/logging).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod data;
mod export;
mod format;
mod process_agg;
mod views;

fn main() -> eframe::Result {
    let log_file = logging::init(&agent_config::log_dir(), "ui");
    log::info!(
        "starting (pid {}, log {})",
        std::process::id(),
        log_file.map_or_else(|| "unavailable".into(), |p| p.display().to_string())
    );

    let config = agent_config::AgentConfig::load_or_default(
        agent_config::AgentConfig::default_config_path(),
    )
    .unwrap_or_default();
    let db_path = config.db_path.clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1150.0, 820.0]),
        ..Default::default()
    };

    eframe::run_native(
        "GhostPerf",
        options,
        Box::new(move |_cc| Ok(Box::new(app::GhostPerfApp::new(db_path)))),
    )
}
