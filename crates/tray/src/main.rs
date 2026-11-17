// Windowless on Windows: as a console program the tray owned a console
// window, and closing it killed the tray and the agent (which inherited
// it) with no clean shutdown. Output goes to `tray.log` instead.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod autostart;
mod icons;
mod ipc_client;
mod open_ui;
mod paths;
mod supervisor;

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
    let autostart_item = CheckMenuItem::new(
        "Start with Windows",
        true,
        autostart::is_enabled().unwrap_or(false),
        None,
    );
    let quit_item = MenuItem::new("Quit", true, None);

    let menu = Menu::new();
    menu.append(&start_item)?;
    menu.append(&stop_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&open_ui_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&autostart_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit_item)?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("GhostPerf")
        .with_icon(icons::idle_icon())
        .build()?;

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
                None
            }
            _ => None,
        };

        let Some(menu_event) = menu_event else { return };

        if menu_event.id() == quit_item.id() {
            // Quit stops the agent too: the tray launches and supervises it,
            // so a tray-less agent would be left recording with nothing
            // watching it and no visible way to stop it.
            quitting = true;
            let _ = tray.set_tooltip(Some("GhostPerf — shutting down…"));
            log::info!("quit requested, shutting down agent");
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
        } else if menu_event.id() == autostart_item.id() {
            let result = if autostart_item.is_checked() {
                autostart::enable()
            } else {
                autostart::disable()
            };
            if let Err(e) = result {
                log::error!("failed to update autostart: {e}");
                // Reflect reality rather than the toggle the user just made.
                autostart_item.set_checked(!autostart_item.is_checked());
            }
        }
    });
}
