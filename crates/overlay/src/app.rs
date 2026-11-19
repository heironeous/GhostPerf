use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use agent_config::{OverlayConfig, OverlayCorner};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::commands::Command;
use crate::live_reply::{self, LiveReading};
use crate::native_window::NativeWindow;

/// Fixed logical size (DPI-independent egui points — see `OverlayConfig`'s
/// doc on `margin_px`) for the small CPU/RAM/GPU readout. Not resizable:
/// there's nothing to resize it with (no decorations, no mouse — see
/// [`viewport`]), and a fixed size keeps corner placement simple.
pub const WINDOW_SIZE: (f32, f32) = (170.0, 78.0);

/// The window TODO.md Phase 6 asks for: click-through (so it never steals
/// input from the game), borderless, always-on-top, no taskbar entry.
/// `mouse_passthrough` alone covers "never steals input" for clicks;
/// [`crate::native_window`] additionally stops it from ever taking focus
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
        // The remembered position if there is one. Otherwise a best-effort
        // initial spot; `OverlayApp::update_placement` moves it to the
        // configured monitor/corner once real monitor info is available
        // (see that method's doc comment).
        .with_position(
            saved_position(config).unwrap_or(egui::pos2(config.margin_px, config.margin_px)),
        )
}

fn saved_position(config: &OverlayConfig) -> Option<egui::Pos2> {
    config.position.map(|[x, y]| egui::pos2(x as f32, y as f32))
}

/// How long the window has to stay put after a drag before its position is
/// written to `overlay.toml`. Waiting for the mouse button to come up
/// instead isn't reliable: the OS's window-move loop that `StartDrag` hands
/// the drag to swallows the button release, so egui may never see it.
const SETTLE: Duration = Duration::from_millis(500);

/// Edit mode's outline and heading, so it's obvious the window can be
/// grabbed and isn't click-through right now.
const EDIT_ACCENT: egui::Color32 = egui::Color32::from_rgb(255, 176, 32);

enum Reading {
    NotRunning,
    Waiting,
    Live(LiveReading),
}

#[derive(Clone, Copy)]
enum Placement {
    /// Move to the remembered position (`OverlayConfig::position`). The
    /// window was created there already, but this goes through the same
    /// points-to-pixels conversion that saved it.
    Saved,
    /// Moved to the remembered position last frame: check it's on a monitor,
    /// and fall back to the corner if not.
    CheckSaved,
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
    window: NativeWindow,
    config_path: PathBuf,
    commands: Receiver<Command>,
    /// Edit mode: takes the mouse and can be dragged ("Move Overlay").
    editing: bool,
    /// Whether the window has been dragged in this edit mode. Without a
    /// drag nothing is saved, so just switching edit mode on and off
    /// doesn't pin the corner placement into `overlay.toml`.
    dragged: bool,
    /// In edit mode: the position seen last frame, and since when.
    settle: Option<(egui::Pos2, Instant)>,
}

impl OverlayApp {
    pub fn new(
        config: OverlayConfig,
        config_path: PathBuf,
        hotkey_manager: Option<GlobalHotKeyManager>,
        hotkey_id: Option<u32>,
        window: NativeWindow,
        commands: Receiver<Command>,
    ) -> Self {
        let placement = if config.position.is_some() {
            Placement::Saved
        } else {
            Placement::Pending {
                jump_sent: config.monitor_index == 0, // primary: no jump needed, so "already sent"
            }
        };
        Self {
            placement,
            port_file: agent_config::port_file_path(),
            next_poll: Instant::now(),
            reading: Reading::Waiting,
            visible: true,
            _hotkey_manager: hotkey_manager,
            hotkey_id,
            window,
            config_path,
            commands,
            editing: false,
            dragged: false,
            settle: None,
            config,
        }
    }

    /// In `logic`, like the hotkey, so a command reaches a hidden window too.
    fn poll_commands(&mut self, ctx: &egui::Context) {
        while let Ok(command) = self.commands.try_recv() {
            match command {
                Command::Edit(on) => self.set_editing(ctx, on),
            }
        }
    }

    fn set_editing(&mut self, ctx: &egui::Context, on: bool) {
        if on == self.editing {
            return;
        }
        self.editing = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(!on));
        if on && !self.visible {
            // Hidden by the hotkey: there'd be nothing to drag.
            self.visible = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }
        if !on {
            if self.dragged {
                if let Some(pos) = current_position(ctx) {
                    self.save_position(pos);
                }
            }
            self.dragged = false;
            self.settle = None;
        }
        log::info!("edit mode {}", if on { "on" } else { "off" });
    }

    /// Writes `pos` to `overlay.toml` as the remembered position. Re-reads
    /// the file first and changes only that line, so edits made by hand
    /// since this overlay started are kept, and a file that no longer parses
    /// is left alone rather than overwritten with defaults.
    fn save_position(&mut self, pos: egui::Pos2) {
        let position = [f64::from(pos.x.round()), f64::from(pos.y.round())];
        if self.config.position == Some(position) {
            return;
        }
        let path = &self.config_path;
        let mut on_disk = match OverlayConfig::load_or_default(path) {
            Ok(on_disk) => on_disk,
            Err(e) => {
                log::warn!(
                    "position not saved: {} doesn't load ({e:#})",
                    path.display()
                );
                return;
            }
        };
        on_disk.position = Some(position);
        match on_disk.save(path) {
            Ok(()) => {
                log::info!("position saved: {position:?}");
                self.config.position = Some(position);
            }
            Err(e) => log::warn!("position not saved to {}: {e:#}", path.display()),
        }
    }

    /// Edit mode, every frame: starts a drag on a click anywhere in the
    /// window, and saves the position once a drag has come to rest.
    fn handle_editing(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.pointer.primary_pressed()) {
            // The OS's own window move, which keeps following the mouse
            // even when it outruns the window.
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            self.dragged = true;
            // The window hasn't moved yet when the press is seen: a still
            // position timed from before it would save the old spot at once.
            self.settle = None;
        }
        let Some(pos) = current_position(ctx) else {
            return;
        };
        match self.settle {
            Some((last, since)) if last == pos => {
                if self.dragged && since.elapsed() >= SETTLE {
                    self.save_position(pos);
                }
            }
            _ => self.settle = Some((pos, Instant::now())),
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
        self.poll_commands(ctx);
        // After the hotkey and commands: showing the window again, and
        // switching mouse passthrough, are changes after which winit resets
        // the extended style.
        self.window.ensure(self.editing);
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
        // Here rather than in `logic`: only while visible, at the refresh
        // rate, so a newly activated topmost window covers it for at most
        // one refresh interval.
        self.window.raise();
        if self.editing {
            self.handle_editing(&ctx);
        }
        // Only repaint on the configured cadence, not continuously — this
        // is most of what keeps an always-running overlay's footprint
        // negligible (TODO.md Phase 6), the same technique already used
        // for the UI and tray's idle cost.
        ctx.request_repaint_after(self.refresh_interval());

        // egui labels are selectable by default, which shows a text cursor
        // over them in edit mode, as if a press would select text, when a
        // press anywhere drags the window. Nothing here is worth copying.
        ui.style_mut().interaction.selectable_labels = false;

        let alpha = (self.config.opacity.clamp(0.0, 1.0) * 255.0) as u8;
        let whole_window = ui.max_rect();
        let editing = self.editing;
        egui::Frame::NONE
            .fill(egui::Color32::from_black_alpha(
                (alpha as u16 * 180 / 255) as u8,
            ))
            .inner_margin(8.0)
            .show(ui, |ui| {
                // The background covers the whole window, not just the text:
                // sized to its content, it stopped short of the window's
                // right and bottom edges, leaving a ragged box in the corner.
                ui.set_min_size(ui.available_size());
                let text = egui::Color32::from_white_alpha(alpha);
                if editing {
                    // In place of the readout, so the window keeps its size
                    // and the position saved is the one it'll have locked.
                    ui.colored_label(EDIT_ACCENT, egui::RichText::new("Drag to move").strong());
                    ui.colored_label(text, "Untick \"Move Overlay\"");
                    ui.colored_label(text, "in the tray to lock");
                    return;
                }
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
        if editing {
            ui.painter().rect_stroke(
                whole_window,
                0.0,
                egui::Stroke::new(2.0, EDIT_ACCENT),
                egui::StrokeKind::Inside,
            );
        }
    }
}

fn current_position(ctx: &egui::Context) -> Option<egui::Pos2> {
    ctx.input(|i| i.viewport().outer_rect).map(|rect| rect.min)
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
        match self.placement {
            Placement::Done => {}
            Placement::Saved => {
                if let Some(pos) = saved_position(&self.config) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
                }
                self.placement = Placement::CheckSaved;
            }
            Placement::CheckSaved => {
                self.placement = if self.window.on_screen() == Some(false) {
                    log::warn!(
                        "saved position {:?} isn't on any monitor; using the {:?} corner",
                        self.config.position,
                        self.config.corner
                    );
                    Placement::Pending {
                        jump_sent: self.config.monitor_index == 0,
                    }
                } else {
                    Placement::Done
                };
            }
            Placement::Pending { jump_sent: false } => {
                ctx.send_viewport_cmd(egui::ViewportCommand::SetMonitor(self.config.monitor_index));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                // Give the jump a frame to land before trusting monitor_size.
                self.placement = Placement::Pending { jump_sent: true };
            }
            Placement::Pending { jump_sent: true } => {
                if self.place_in_corner(ctx) {
                    self.placement = Placement::Done;
                }
            }
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
