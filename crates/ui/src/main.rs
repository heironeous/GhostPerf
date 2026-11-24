// Windowless console on Windows: the egui window is the only window this
// process should show (same reasoning as agent/tray — see crates/logging).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod data;
mod export;
mod format;
mod icon;
mod process_agg;
mod temps;
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
    let db_path = config.resolved_db_path();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1150.0, 820.0])
            .with_icon(icon::viewport_icon()),
        wgpu_options: wgpu_options(),
        ..Default::default()
    };

    eframe::run_native(
        "GhostPerf",
        options,
        Box::new(move |cc| {
            icon::apply(cc);
            Ok(Box::new(app::GhostPerfApp::new(db_path)))
        }),
    )
}

/// DX12 (GL as a fallback) on Windows instead of wgpu's default, which
/// tries Vulkan first. Some Intel iGPU Vulkan drivers (`igvk64.dll`
/// 30.0.101.1122, Intel UHD Graphics device 0x9B41) segfault while the surface is being
/// created. That native crash kills the process before any Rust panic hook can log it, so
/// the window just never appears. DX12 is the native API and is present on
/// every supported Windows. `WGPU_BACKEND` still overrides this, e.g. to
/// compare against Vulkan. The overlay does the same (`crates/overlay/src/main.rs`).
fn wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup};
    use eframe::wgpu;

    let mut config = WgpuConfiguration::default();
    if cfg!(windows) && wgpu::Backends::from_env().is_none() {
        if let WgpuSetup::CreateNew(create_new) = &mut config.wgpu_setup {
            create_new.instance_descriptor.backends = wgpu::Backends::DX12 | wgpu::Backends::GL;
        }
    }
    config
}
