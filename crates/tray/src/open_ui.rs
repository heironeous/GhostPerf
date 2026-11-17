//! "Open UI" menu action. The UI (Phase 4) doesn't exist yet as a real
//! window, so there's nothing to focus — this only avoids spawning a
//! second copy. Once the UI has a real window, replace the
//! already-running check with an actual focus (e.g. `FindWindowW` +
//! `SetForegroundWindow` on Windows) so clicking "Open UI" twice brings the
//! existing window forward instead of silently doing nothing.

use std::process::Command;

use sysinfo::{ProcessStatus, System};

use crate::paths::{is_process_named, sibling_exe};

const UI_PROCESS_NAME: &str = "ghostperf-ui";

pub fn open_ui(sys: &mut System) -> anyhow::Result<()> {
    crate::supervisor::refresh_process_names(sys);
    // See `supervisor::is_agent_running` for why zombies are excluded.
    let already_running = sys.processes().values().any(|p| {
        p.status() != ProcessStatus::Zombie && is_process_named(p.name(), UI_PROCESS_NAME)
    });
    if already_running {
        return Ok(());
    }

    let path = sibling_exe(UI_PROCESS_NAME)?;
    Command::new(path).spawn()?;
    Ok(())
}
