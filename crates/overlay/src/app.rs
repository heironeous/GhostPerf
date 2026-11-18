use std::path::PathBuf;
use std::time::{Duration, Instant};

use agent_config::{OverlayConfig, OverlayCorner};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::live_reply::{self, LiveReading};

/// Fixed logical size (DPI-independent egui points — see `OverlayConfig`'s
/// doc on `margin_px`) for the small CPU/RAM/GPU readout. Not resizable:
/// there's nothing to resize it with (no decorations, no mouse — see
/// [`viewport`]), and a fixed size keeps corner placement simple.
pub const WINDOW_SIZE: (f32, f32) = (170.0, 78.0);

/// The window TODO.md Phase 6 asks for: click-through (so it never steals
/// input from the game), borderless, always-on-top, no taskbar entry.
/// `mouse_passthrough` alone covers "never steals input" for clicks;
/// [`crate::noactivate`] additionally stops it from ever taking focus
/// (activation), which nothing here covers — see that module.
pub fn viewport(config: &OverlayConfig) -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_inner_size(WINDOW_SIZE)
        .with_resizable(false)
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_taskbar(false)
        .with_mouse_passthrough(true)
        .with_active(false)
        .with_drag_and_drop(false)
        // Best-effort initial spot; `OverlayApp::update_placement` moves it
        // to the configured monitor/corner once real monitor info is
        // available (see that method's doc comment).
        .with_position(egui::pos2(config.margin_px, config.margin_px))
}

enum Reading {
    NotRunning,
    Waiting,
    Live(LiveReading),
}

enum Placement {
    /// Still trying to land on the configured monitor/corner. Retried
    /// every frame until it succeeds — the monitor info a freshly created
    /// window reports can be `None` for the first frame or two, and a
    /// non-zero `monitor_index` additionally needs the best-effort
    /// jump-via-fullscreen below to have taken effect first.
    Pending {
        jump_sent: bool,
    },
    Done,
}

pub struct OverlayApp {
    config: OverlayConfig,
    port_file: PathBuf,
    next_poll: Instant,
    reading: Reading,
    placement: Placement,
    visible: bool,
    /// Held only to keep the registration alive (its `Drop` unregisters
    /// the hotkey) — `main.rs` already logged the outcome of creating and
    /// registering it. `None` if either step failed or the config's
    /// hotkey string didn't parse; the hotkey is just unavailable then,
    /// not fatal (the tray's menu item still works).
    _hotkey_manager: Option<GlobalHotKeyManager>,
    hotkey_id: Option<u32>,
}

impl OverlayApp {
    pub fn new(
        config: OverlayConfig,
        hotkey_manager: Option<GlobalHotKeyManager>,
        hotkey_id: Option<u32>,
    ) -> Self {
        let placement = Placement::Pending {
            jump_sent: config.monitor_index == 0, // primary: no jump needed, so "already sent"
        };
        Self {
            placement,
            port_file: agent_config::port_file_path(),
            next_poll: Instant::now(),
            reading: Reading::Waiting,
            visible: true,
            _hotkey_manager: hotkey_manager,
            hotkey_id,
            config,
        }
    }

    /// Checked whether or not the window is currently visible: unlike
    /// `ui`, `eframe` still calls this while hidden (see its doc comment on
    /// [`eframe::App::logic`]), which is the only way a hidden window can
    /// ever ask to be shown again.
    fn poll_hotkey(&mut self, ctx: &egui::Context) {
        let Some(id) = self.hotkey_id else { return };
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.id == id && event.state == HotKeyState::Pressed {
                self.visible = !self.visible;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(self.visible));
                log::info!(
                    "hotkey toggled overlay {}",
                    if self.visible { "visible" } else { "hidden" }
                );
            }
        }
    }
}

impl eframe::App for OverlayApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_hotkey(ctx);
        // Still cheap while hidden: just the hotkey channel above, no
        // polling the agent or painting. See TODO.md Phase 6's footprint
        // requirement.
        ctx.request_repaint_after(self.refresh_interval());
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_hotkey(&ctx);
        self.update_placement(&ctx);
        self.maybe_poll();
        // Only repaint on the configured cadence, not continuously — this
        // is most of what keeps an always-running overlay's footprint
        // negligible (TODO.md Phase 6), the same technique already used
        // for the UI and tray's idle cost.
        ctx.request_repaint_after(self.refresh_interval());

        let alpha = (self.config.opacity.clamp(0.0, 1.0) * 255.0) as u8;
        egui::Frame::NONE
            .fill(egui::Color32::from_black_alpha(
                (alpha as u16 * 180 / 255) as u8,
            ))
            .inner_margin(8.0)
            .show(ui, |ui| {
                let text = egui::Color32::from_white_alpha(alpha);
                match &self.reading {
                    Reading::NotRunning => {
                        ui.colored_label(text, "GhostPerf agent not running");
                    }
                    Reading::Waiting => {
                        ui.colored_label(text, "GhostPerf — waiting for data…");
                    }
                    Reading::Live(r) => {
                        ui.colored_label(text, format!("CPU  {:>5.1}%", r.cpu_pct));
                        ui.colored_label(
                            text,
                            r.mem_used_pct.map_or_else(
                                || "RAM    —".to_string(),
                                |v| format!("RAM  {v:>5.1}%"),
                            ),
                        );
                        ui.colored_label(
                            text,
                            r.gpu_pct.map_or_else(
                                || "GPU    —".to_string(),
                                |v| format!("GPU  {v:>5.1}%"),
                            ),
                        );
                    }
                }
            });
    }
}

impl OverlayApp {
    fn refresh_interval(&self) -> Duration {
        Duration::from_millis(self.config.refresh_interval_ms.max(50))
    }

    fn maybe_poll(&mut self) {
        if Instant::now() < self.next_poll {
            return;
        }
        self.next_poll = Instant::now() + self.refresh_interval();
        self.reading = match agent_config::read_port_file(&self.port_file) {
            None => Reading::NotRunning,
            Some(port) => match agent_config::send_command(port, "live") {
                Ok(reply) => live_reply::parse(&reply).map_or(Reading::Waiting, Reading::Live),
                Err(_) => Reading::NotRunning,
            },
        };
    }

    /// Moves the window toward its configured monitor/corner, retrying
    /// until it succeeds (see [`Placement`]).
    ///
    /// Targeting a specific monitor by index has no direct, non-fullscreen
    /// API in egui's `ViewportBuilder`/`ViewportCommand` (checked against
    /// the egui 0.37 source — only `SetMonitor`, which is fullscreen-only
    /// by its own doc comment). Best-effort workaround: jump there
    /// fullscreen, then immediately drop fullscreen, hoping the window
    /// stays on that monitor at whatever size/position it ends up with
    /// before `place_in_corner` moves and sizes it properly. Unverified
    /// cross-platform — see TODO.md Phase 6.
    fn update_placement(&mut self, ctx: &egui::Context) {
        let Placement::Pending { jump_sent } = &mut self.placement else {
            return;
        };
        if !*jump_sent {
            ctx.send_viewport_cmd(egui::ViewportCommand::SetMonitor(self.config.monitor_index));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            *jump_sent = true;
            return; // give the jump a frame to land before trusting monitor_size
        }
        if self.place_in_corner(ctx) {
            self.placement = Placement::Done;
        }
    }

    /// `false` if the monitor size isn't known yet this frame (retry next
    /// frame); `true` once the move command has been sent.
    fn place_in_corner(&self, ctx: &egui::Context) -> bool {
        let Some(monitor_size) = ctx.input(|i| i.viewport().monitor_size) else {
            return false;
        };
        let pos = corner_position(
            self.config.corner,
            self.config.margin_px,
            WINDOW_SIZE,
            monitor_size,
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        true
    }
}

/// Pure position math for [`OverlayApp::place_in_corner`], split out so
/// it's testable without an `egui::Context`.
fn corner_position(
    corner: OverlayCorner,
    margin: f32,
    window_size: (f32, f32),
    monitor_size: egui::Vec2,
) -> egui::Pos2 {
    let (w, h) = window_size;
    match corner {
        OverlayCorner::TopLeft => egui::pos2(margin, margin),
        OverlayCorner::TopRight => egui::pos2(monitor_size.x - w - margin, margin),
        OverlayCorner::BottomLeft => egui::pos2(margin, monitor_size.y - h - margin),
        OverlayCorner::BottomRight => {
            egui::pos2(monitor_size.x - w - margin, monitor_size.y - h - margin)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITOR: egui::Vec2 = egui::vec2(1920.0, 1080.0);

    #[test]
    fn each_corner_hugs_its_own_edges_with_the_margin() {
        let (w, h) = WINDOW_SIZE;
        assert_eq!(
            corner_position(OverlayCorner::TopLeft, 10.0, WINDOW_SIZE, MONITOR),
            egui::pos2(10.0, 10.0)
        );
        assert_eq!(
            corner_position(OverlayCorner::TopRight, 10.0, WINDOW_SIZE, MONITOR),
            egui::pos2(1920.0 - w - 10.0, 10.0)
        );
        assert_eq!(
            corner_position(OverlayCorner::BottomLeft, 10.0, WINDOW_SIZE, MONITOR),
            egui::pos2(10.0, 1080.0 - h - 10.0)
        );
        assert_eq!(
            corner_position(OverlayCorner::BottomRight, 10.0, WINDOW_SIZE, MONITOR),
            egui::pos2(1920.0 - w - 10.0, 1080.0 - h - 10.0)
        );
    }

    #[test]
    fn zero_margin_touches_the_edge_exactly() {
        assert_eq!(
            corner_position(OverlayCorner::TopLeft, 0.0, WINDOW_SIZE, MONITOR),
            egui::pos2(0.0, 0.0)
        );
    }
}
