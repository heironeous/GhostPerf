// Windowless console on Windows: the overlay's own window is deliberately
// invisible to Alt-Tab/taskbar (see app.rs::viewport), and there should be
// no console behind it either — same reasoning as agent/tray/ui, see
// crates/logging.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod commands;
mod live_reply;
mod native_window;

use global_hotkey::hotkey::HotKey;
use global_hotkey::GlobalHotKeyManager;

fn main() -> eframe::Result {
    let log_file = logging::init(&agent_config::log_dir(), "overlay");
    log::info!(
        "starting (pid {}, log {})",
        std::process::id(),
        log_file.map_or_else(|| "unavailable".into(), |p| p.display().to_string())
    );

    let config_path = agent_config::OverlayConfig::default_config_path();
    let config = agent_config::OverlayConfig::load_or_default(&config_path).unwrap_or_default();

    // Must be created on this thread, before `run_native` starts pumping
    // it: on Windows the hotkey is delivered as a message on whichever
    // thread's event loop is running when it's registered (see
    // `global_hotkey`'s crate docs). Kept alive for the process's
    // lifetime by moving it into the app below, not dropped here.
    let hotkey = hotkey_from_config(&config.hotkey);
    let manager = hotkey.as_ref().and_then(|hk| {
        let manager = GlobalHotKeyManager::new()
            .inspect_err(|e| log::warn!("couldn't create the global hotkey manager: {e}"))
            .ok()?;
        manager
            .register(*hk)
            .inspect_err(|e| log::warn!("couldn't register global hotkey '{}': {e}", config.hotkey))
            .ok()?;
        log::info!(
            "global hotkey '{}' registered (toggles visibility)",
            config.hotkey
        );
        Some(manager)
    });

    let options = eframe::NativeOptions {
        viewport: app::viewport(&config),
        wgpu_options: wgpu_options(),
        ..Default::default()
    };

    eframe::run_native(
        "GhostPerf Overlay",
        options,
        Box::new(move |cc| {
            let window = native_window::NativeWindow::new(cc);
            let commands = commands::listen(cc.egui_ctx.clone());
            Ok(Box::new(app::OverlayApp::new(
                config,
                config_path,
                manager,
                hotkey.map(|hk| hk.id()),
                window,
                commands,
            )))
        }),
    )
}

/// DX12 instead of wgpu's Vulkan-first default on Windows, for the same
/// driver-crash reason as `crates/ui/src/main.rs::wgpu_options` (read that
/// first). This version has one more requirement: DX12's default swapchain
/// is made straight from the HWND, and that kind has no transparent
/// `CompositeAlphaMode`. The overlay would render opaque, which is why a
/// Phase 6 note in TODO.md ruled DX12 out. A DirectComposition-visual
/// swapchain does support transparency.
fn wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup};
    use eframe::wgpu;

    let mut config = WgpuConfiguration::default();
    if cfg!(windows) {
        if let WgpuSetup::CreateNew(create_new) = &mut config.wgpu_setup {
            let desc = &mut create_new.instance_descriptor;
            if wgpu::Backends::from_env().is_none() {
                desc.backends = wgpu::Backends::DX12 | wgpu::Backends::GL;
            }
            desc.backend_options.dx12.presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual;
        }
    }
    config
}

/// `None` (hotkey disabled, not fatal) for an empty or unparsable string —
/// e.g. a hand-edited `overlay.toml`. The tray's menu item still toggles
/// the overlay either way.
fn hotkey_from_config(hotkey: &str) -> Option<HotKey> {
    if hotkey.trim().is_empty() {
        return None;
    }
    hotkey
        .parse()
        .inspect_err(|e| log::warn!("couldn't parse hotkey '{hotkey}': {e}"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_hotkey_string_actually_parses() {
        // agent_config's own test only checks the string's shape (it can't
        // depend on this crate); this is the real parser.
        let default = agent_config::OverlayConfig::default().hotkey;
        assert!(
            hotkey_from_config(&default).is_some(),
            "default hotkey {default:?} failed to parse"
        );
    }

    #[test]
    fn empty_hotkey_disables_it_without_an_error() {
        assert!(hotkey_from_config("").is_none());
        assert!(hotkey_from_config("   ").is_none());
    }

    #[test]
    fn garbage_hotkey_is_none_not_a_panic() {
        assert!(hotkey_from_config("not a hotkey").is_none());
    }

    #[test]
    fn two_different_hotkey_strings_get_different_ids() {
        // What `GlobalHotKeyEvent.id` is matched against in
        // `app::OverlayApp::poll_hotkey` — if two distinct accelerators
        // collided, the wrong one could toggle visibility.
        let a = hotkey_from_config("Ctrl+Alt+O").unwrap();
        let b = hotkey_from_config("Ctrl+Alt+P").unwrap();
        assert_ne!(a.id(), b.id());
    }
}
