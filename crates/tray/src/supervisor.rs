//! Agent process supervision: launch it if it isn't running (cold start or
//! after a crash), detected by scanning for its process name rather than
//! tracking a `Child` handle — the tray may start after the agent already
//! did (e.g. the agent autostarted independently), so there's often no
//! `Child` to track in the first place.

use std::process::Command;
use std::time::{Duration, Instant};

use sysinfo::{ProcessStatus, System};

use crate::ipc_client;
use crate::paths::{is_process_named, sibling_exe};

/// How long Quit waits for the agent to finish shutting down. Shutdown
/// flushes buffered samples and, if a session is recording, runs the Event
/// Log query for its whole time window, so this is not instant, but it
/// should be seconds at most.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_POLL: Duration = Duration::from_millis(100);

const AGENT_PROCESS_NAME: &str = "ghostperf-agent";

pub fn is_agent_running(sys: &mut System) -> bool {
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All);
    sys.processes().values().any(|p| {
        // A zombie is a dead process whose exit status its parent hasn't
        // collected yet — on Linux dev machines that's common enough (a
        // shell backgrounding a child it never `wait()`s on) to otherwise
        // make this permanently think a crashed agent is still up.
        p.status() != ProcessStatus::Zombie && is_process_named(p.name(), AGENT_PROCESS_NAME)
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

/// Asks the agent to shut down (which ends any recording session, flushes
/// to the DB, and closes it) and waits for the process to actually exit.
///
/// Waits for the process rather than trusting the `OK shutting_down` reply:
/// the agent replies before it has finished closing the session, and
/// waiting is what lets the tray exit knowing the session was closed.
///
/// Never kills the agent on timeout. A forced kill mid-shutdown would leave
/// the session open (recovered as "orphaned" on the next start); an agent
/// left running can still be shut down later.
pub fn shutdown_agent(sys: &mut System, port: Option<u16>) -> anyhow::Result<()> {
    if !is_agent_running(sys) {
        return Ok(());
    }
    let port = port.ok_or_else(|| {
        anyhow::anyhow!("agent is running but hasn't published a control port yet")
    })?;
    let reply = ipc_client::send_command(port, "shutdown")?;
    if !reply.starts_with("OK") {
        anyhow::bail!("agent refused shutdown: {reply}");
    }

    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    while is_agent_running(sys) {
        if Instant::now() >= deadline {
            anyhow::bail!(
                "agent still running {}s after shutdown was requested",
                SHUTDOWN_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(SHUTDOWN_POLL);
    }
    Ok(())
}
