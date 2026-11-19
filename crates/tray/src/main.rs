// Windowless on Windows: as a console program the tray owned a console
// window, and closing it killed the tray and the agent (which inherited
// it) with no clean shutdown. Output goes to `tray.log` instead.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod autostart;
mod icons;
mod ipc_client;
mod open_ui;
mod overlay;
mod paths;
mod supervisor;
mod welcome;

use std::time::Duration;

use sysinfo::System;
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::TrayIconBuilder;

/// How often to re-check the agent's recording state and whether it's
/// still alive, independent of menu clicks — catches state changes made by
/// another control client and crashes/relaunches.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Pushed into the `tao` loop from off-thread sources (a timer thread, and
/// `muda`'s own menu-click channel) via an `EventLoopProxy`, rather than
/// having the loop poll those sources on a timer itself. `ControlFlow::Poll`
/// (busy-looping to check both) or `ControlFlow::WaitUntil` (relying on
/// `tao`'s internal timer) both turned out to be unreliable when there's no
/// window manager driving the GTK main loop — only a bare tray icon, as is
/// the case in a headless dev-container smoke test. Pushing events in
/// directly sidesteps that; it's also just the idiomatic way tray-icon
/// examples integrate a background poll with a `tao` loop.
enum TickEvent {
    Poll,
    Menu(MenuEvent),
}

/// Menu-click commands are fire-and-forget (the next poll reflects the
/// result in the icon), but a failure is logged rather than silently
/// dropped so a click that did nothing can be diagnosed.
fn send_to_agent(port: Option<u16>, command: &str) {
    let Some(port) = port else {
        log::warn!("'{command}' not sent — agent hasn't published a control port yet");
        return;
    };
    match ipc_client::send_command(port, command) {
        Ok(reply) => log::info!("'{command}' -> {reply}"),
        Err(e) => log::warn!("'{command}' to agent on port {port} failed: {e}"),
    }
}

/// Sets a checkmark to `on` if it isn't already. The menu flips a
/// checkmark itself when it's clicked, before the click is handled, so each
/// handler sets it back to the real state afterwards.
fn sync_check(item: &CheckMenuItem, on: bool) {
    if item.is_checked() != on {
        item.set_checked(on);
    }
}

/// Sets the "Start with Windows" checkmark to what's actually registered.
/// Left alone if the registry can't be read, rather than guessing.
fn sync_autostart_check(item: &CheckMenuItem) {
    if let Ok(enabled) = autostart::is_enabled() {
        if item.is_checked() != enabled {
            log::info!(
                "'Start with Windows' is now {}",
                if enabled { "on" } else { "off" }
            );
            item.set_checked(enabled);
        }
    }
}

fn main() {
    let log_file = logging::init(&agent_config::log_dir(), "tray");
    log::info!(
        "starting (pid {}, log {})",
        std::process::id(),
        log_file.map_or_else(|| "unavailable".into(), |p| p.display().to_string())
    );
    // See the agent's `main`: a returned error would go to a stderr that
    // doesn't exist in a windowless process.
    if let Err(e) = run() {
        log::error!("fatal: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    // Re-read on every use, never cached: the agent picks a new port each
    // time it starts, including every crash-relaunch below.
    let port_file = agent_config::port_file_path();
    let agent_port = move || agent_config::read_port_file(&port_file);

    let mut sys = System::new();
    if supervisor::ensure_agent_running(&mut sys)? {
        log::info!("agent wasn't running, launched it");
    } else {
        log::info!("agent already running");
    }

    // Must exist before any menu/tray-icon construction: on Linux this is
    // what calls `gtk::init()` under the hood, and building a `Menu` first
    // panics with "GTK has not been initialized."
    let event_loop = EventLoopBuilder::<TickEvent>::with_user_event().build();

    let menu_proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let _ = menu_proxy.send_event(TickEvent::Menu(event));
    }));

    let poll_proxy = event_loop.create_proxy();
    std::thread::spawn(move || loop {
        std::thread::sleep(POLL_INTERVAL);
        if poll_proxy.send_event(TickEvent::Poll).is_err() {
            break; // event loop (and so the whole app) is gone
        }
    });

    let start_item = MenuItem::new("Start recording", true, None);
    let stop_item = MenuItem::new("Stop recording", false, None);
    let open_ui_item = MenuItem::new("Open UI", true, None);
    // Checkmarks rather than a "Show"/"Hide" label that changes: ticked
    // while the overlay is running, and while it's in edit mode (draggable
    // rather than click-through).
    let overlay_item = CheckMenuItem::new("Show Overlay", true, false, None);
    let move_overlay_item = CheckMenuItem::new("Move Overlay", true, false, None);
    let mut overlay = overlay::OverlayHandle::new();
    let autostart_on = autostart::is_enabled().unwrap_or(false);
    let autostart_item = CheckMenuItem::new("Start with Windows", true, autostart_on, None);
    // A portable copy is meant to leave nothing behind on the machine, and
    // a Run entry pointing into a folder on a USB stick breaks as soon as
    // the stick is gone. So the option is hidden there, unless this copy
    // was already registered (by an earlier version, which offered it), so
    // that it can still be switched off.
    let offer_autostart = !agent_config::is_portable() || autostart_on;
    let quit_item = MenuItem::new("Quit", true, None);

    let menu = Menu::new();
    menu.append(&start_item)?;
    menu.append(&stop_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&open_ui_item)?;
    menu.append(&overlay_item)?;
    menu.append(&move_overlay_item)?;
    if offer_autostart {
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&autostart_item)?;
    }
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit_item)?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("GhostPerf")
        .with_icon(icons::idle_icon())
        .build()?;
    welcome::show_on_first_run(&tray);

    let mut recording = false;
    // Set once Quit starts stopping the agent. Poll ticks queued while Quit
    // blocks on the agent's exit are still delivered afterwards, and without
    // this the supervisor would see the agent gone and relaunch it.
    let mut quitting = false;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        if quitting {
            return;
        }

        let menu_event = match event {
            Event::UserEvent(TickEvent::Menu(event)) => Some(event),
            Event::UserEvent(TickEvent::Poll) => {
                let status = agent_port().and_then(ipc_client::is_recording);
                // A `status` reply proves the agent is alive, so the process
                // scan (enumerating every process on the system, the bulk of
                // the tray's idle CPU when it ran every second) only runs when
                // the agent doesn't answer: not started yet, still binding its
                // port, crashed, or hung (found running, so not relaunched).
                if status.is_none() {
                    match supervisor::ensure_agent_running(&mut sys) {
                        Ok(true) => log::warn!("agent wasn't running, relaunched it"),
                        Ok(false) => {}
                        Err(e) => log::error!("failed to relaunch agent: {e:#}"),
                    }
                }

                // The Run value can change behind the tray's back: the
                // installer and uninstaller write it, and so does a second
                // copy of GhostPerf. Reading it once at startup left the
                // checkmark wrong until the next start. One registry read a
                // second is negligible next to the status request above.
                if offer_autostart {
                    sync_autostart_check(&autostart_item);
                }

                if let Some(now_recording) = status {
                    if now_recording != recording {
                        recording = now_recording;
                        start_item.set_enabled(!recording);
                        stop_item.set_enabled(recording);
                        let icon = if recording {
                            icons::recording_icon()
                        } else {
                            icons::idle_icon()
                        };
                        let _ = tray.set_icon(Some(icon));
                    }
                }

                // Catches the overlay exiting on its own (e.g. a crash),
                // not just toggles through this menu. `is_running` is a
                // non-blocking `Child::try_wait`, not a process scan — see
                // overlay.rs's module doc for why that's fine to do every
                // poll tick, unlike the agent/UI checks above.
                sync_check(&overlay_item, overlay.is_running());
                sync_check(&move_overlay_item, overlay.is_editing());
                None
            }
            _ => None,
        };

        let Some(menu_event) = menu_event else { return };

        if menu_event.id() == quit_item.id() {
            // Quit stops the agent too: the tray launches and supervises it,
            // so a tray-less agent would be left recording with nothing
            // watching it and no visible way to stop it. Same for the
            // overlay, if it's running — nothing else would ever stop it,
            // since it's click-through and has no menu or window of its
            // own.
            quitting = true;
            let _ = tray.set_tooltip(Some("GhostPerf — shutting down…"));
            log::info!("quit requested, shutting down agent");
            if overlay.is_running() {
                if let Err(e) = overlay.toggle() {
                    log::warn!("failed to close the overlay on quit: {e:#}");
                }
            }
            match supervisor::shutdown_agent(&mut sys, agent_port()) {
                Ok(()) => log::info!("agent shut down cleanly; exiting"),
                Err(e) => log::error!("exiting, but agent shutdown failed: {e:#}"),
            }
            *control_flow = ControlFlow::Exit;
        } else if menu_event.id() == start_item.id() {
            send_to_agent(agent_port(), "start");
        } else if menu_event.id() == stop_item.id() {
            send_to_agent(agent_port(), "stop");
        } else if menu_event.id() == open_ui_item.id() {
            if let Err(e) = open_ui::open_ui(&mut sys) {
                log::error!("failed to open UI: {e:#}");
            }
        } else if menu_event.id() == overlay_item.id() {
            match overlay.toggle() {
                Ok(running) => {
                    log::info!("overlay {}", if running { "started" } else { "stopped" })
                }
                Err(e) => log::error!("failed to toggle the overlay: {e:#}"),
            }
            sync_check(&overlay_item, overlay.is_running());
            sync_check(&move_overlay_item, overlay.is_editing());
        } else if menu_event.id() == move_overlay_item.id() {
            // From the overlay's actual edit state, not the checkmark, for
            // the same reason as "Start with Windows" below.
            let on = !overlay.is_editing();
            match overlay.set_editing(on) {
                Ok(()) => log::info!("overlay edit mode {}", if on { "on" } else { "off" }),
                Err(e) => log::error!("failed to switch overlay edit mode: {e:#}"),
            }
            sync_check(&overlay_item, overlay.is_running());
            sync_check(&move_overlay_item, overlay.is_editing());
        } else if menu_event.id() == autostart_item.id() {
            // Flips what's registered, not what the checkmark now shows. The
            // menu toggles the checkmark itself on click, but a poll handled
            // between that click and this event may already have reset it
            // from the registry, and acting on it would then undo the click.
            let result = match autostart::is_enabled() {
                Ok(true) => autostart::disable(),
                Ok(false) => autostart::enable(),
                Err(e) => Err(e),
            };
            if let Err(e) = result {
                log::error!("failed to update autostart: {e:#}");
            }
            sync_autostart_check(&autostart_item);
        }
    });
}
