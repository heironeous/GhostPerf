//! Agent process supervision: launch it if it isn't running (cold start or
//! after a crash), detected by scanning for its process name rather than
//! tracking a `Child` handle — the tray may start after the agent already
//! did (e.g. the agent autostarted independently), so there's often no
//! `Child` to track in the first place.

use std::process::Command;

use sysinfo::{ProcessStatus, System};

use crate::paths::sibling_exe;

const AGENT_PROCESS_NAME: &str = "ghostperf-agent";

pub fn is_agent_running(sys: &mut System) -> bool {
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All);
    sys.processes().values().any(|p| {
        // A zombie is a dead process whose exit status its parent hasn't
        // collected yet — on Linux dev machines that's common enough (a
        // shell backgrounding a child it never `wait()`s on) to otherwise
        // make this permanently think a crashed agent is still up.
        p.status() != ProcessStatus::Zombie
            && p.name()
                .to_string_lossy()
                .eq_ignore_ascii_case(AGENT_PROCESS_NAME)
    })
}

fn spawn_agent() -> anyhow::Result<()> {
    let path = sibling_exe(AGENT_PROCESS_NAME)?;
    Command::new(path).spawn()?;
    Ok(())
}

/// Returns `true` if this call just launched the agent (it wasn't already
/// running), `false` if it found one already up.
pub fn ensure_agent_running(sys: &mut System) -> anyhow::Result<bool> {
    if is_agent_running(sys) {
        return Ok(false);
    }
    spawn_agent()?;
    Ok(true)
}
