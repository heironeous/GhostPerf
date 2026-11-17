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
        eprintln!(
            "ghostperf-tray: '{command}' not sent — agent hasn't published a control port yet"
        );
        return;
    };
    if let Err(e) = ipc_client::send_command(port, command) {
        eprintln!("ghostperf-tray: '{command}' to agent on port {port} failed: {e}");
    }
}

fn main() -> anyhow::Result<()> {
    // Re-read on every use, never cached: the agent picks a new port each
    // time it starts, including every crash-relaunch below.
    let port_file = agent_config::port_file_path();
    let agent_port = move || agent_config::read_port_file(&port_file);

    let mut sys = System::new();
    supervisor::ensure_agent_running(&mut sys)?;

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

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;

        let menu_event = match event {
            Event::UserEvent(TickEvent::Menu(event)) => Some(event),
            Event::UserEvent(TickEvent::Poll) => {
                if let Ok(true) = supervisor::ensure_agent_running(&mut sys) {
                    println!("ghostperf-tray: agent wasn't running, relaunched it");
                }

                if let Some(now_recording) = agent_port().and_then(ipc_client::is_recording) {
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
            *control_flow = ControlFlow::Exit;
        } else if menu_event.id() == start_item.id() {
            send_to_agent(agent_port(), "start");
        } else if menu_event.id() == stop_item.id() {
            send_to_agent(agent_port(), "stop");
        } else if menu_event.id() == open_ui_item.id() {
            let _ = open_ui::open_ui(&mut sys);
        } else if menu_event.id() == autostart_item.id() {
            let result = if autostart_item.is_checked() {
                autostart::enable()
            } else {
                autostart::disable()
            };
            if let Err(e) = result {
                eprintln!("ghostperf-tray: failed to update autostart: {e}");
                // Reflect reality rather than the toggle the user just made.
                autostart_item.set_checked(!autostart_item.is_checked());
            }
        }
    });
}
