//! "Open UI" menu action: starts the session viewer, or, if it's already
//! running, brings its window to the front. It used to only avoid starting
//! a second copy, so a click while the viewer was open behind other windows
//! or minimized seemed to do nothing at all.

#[cfg(not(target_os = "windows"))]
mod generic_impl;
#[cfg(target_os = "windows")]
mod windows_impl;

#[cfg(not(target_os = "windows"))]
use generic_impl::bring_to_front;
#[cfg(target_os = "windows")]
use windows_impl::bring_to_front;

use std::process::Command;

use sysinfo::{ProcessStatus, System};

use crate::paths::{is_process_named, sibling_exe};

const UI_PROCESS_NAME: &str = "ghostperf-ui";

pub fn open_ui(sys: &mut System) -> anyhow::Result<()> {
    crate::supervisor::refresh_process_names(sys);
    // See `supervisor::is_agent_running` for why zombies are excluded.
    let running: Vec<u32> = sys
        .processes()
        .iter()
        .filter(|(_, p)| {
            p.status() != ProcessStatus::Zombie && is_process_named(p.name(), UI_PROCESS_NAME)
        })
        .map(|(pid, _)| pid.as_u32())
        .collect();
    if running.is_empty() {
        let path = sibling_exe(UI_PROCESS_NAME)?;
        Command::new(path).spawn()?;
        return Ok(());
    }
    // Normally one; several only if started by hand outside the tray.
    if !running.iter().any(|&pid| bring_to_front(pid)) {
        // Still starting up (no window yet), so there's nothing to focus,
        // and starting another copy would only make two.
        log::info!("the UI is running but has no window to bring forward yet");
    }
    Ok(())
}
