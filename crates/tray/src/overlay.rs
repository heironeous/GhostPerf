//! "Show/Hide Overlay" menu action: launches `ghostperf-overlay` if it
//! isn't running, or kills it if it is.
//!
//! Unlike "Open UI", this is a true process-lifecycle toggle, not
//! launch-or-focus: the overlay is click-through and never takes focus by
//! design (`crates/overlay/src/app.rs`), so there's no window to bring
//! forward, and no state to lose by being killed (no session, nothing
//! buffered — it's a pure display client of the agent's `live` command;
//! see `crates/agent/src/telemetry.rs`).
//!
//! Tracked via the `Child` this toggle's own `spawn` returns, not a
//! process-name scan like `supervisor`/`open_ui` use for the agent/UI.
//! Those exist because the agent or UI might already be running before the
//! tray starts (the agent can autostart independently; the UI can be
//! launched by hand). The overlay has no such path — the tray is the only
//! thing that ever starts it — so the tray already has the exact `Child`
//! in hand, and `Child::try_wait` is a non-blocking, effectively free way
//! to check it's still alive, with no `sysinfo` process-table scan at all
//! (the kind Phase 5 had to specifically avoid doing every poll tick for
//! the agent).

use std::process::Child;

use crate::paths::sibling_exe;

const OVERLAY_PROCESS_NAME: &str = "ghostperf-overlay";

#[derive(Default)]
pub struct OverlayHandle {
    child: Option<Child>,
}

impl OverlayHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` if the overlay is running right now. Reaps the `Child` if it
    /// exited on its own (a crash, or a future self-close) so the next
    /// toggle spawns a fresh one instead of silently doing nothing.
    pub fn is_running(&mut self) -> bool {
        match &mut self.child {
            Some(child) => match child.try_wait() {
                Ok(None) => true,
                Ok(Some(status)) => {
                    log::info!("overlay exited on its own ({status})");
                    self.child = None;
                    false
                }
                Err(e) => {
                    log::warn!("couldn't check overlay status ({e}); assuming it's gone");
                    self.child = None;
                    false
                }
            },
            None => false,
        }
    }

    /// Returns whether the overlay is running *after* this call.
    pub fn toggle(&mut self) -> anyhow::Result<bool> {
        if self.is_running() {
            if let Some(mut child) = self.child.take() {
                child.kill()?;
                let _ = child.wait(); // avoid leaving a zombie behind
            }
            return Ok(false);
        }
        let path = sibling_exe(OVERLAY_PROCESS_NAME)?;
        self.child = Some(std::process::Command::new(path).spawn()?);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl OverlayHandle {
        fn from_child(child: Child) -> Self {
            Self { child: Some(child) }
        }
    }

    // `sh`/`sleep` rather than `sibling_exe` + a real `ghostperf-overlay`:
    // this only needs *some* child process whose lifetime it controls, and
    // the dev container these tests run in doesn't have a built overlay
    // exe sitting next to the test binary.
    fn spawn_sleep(secs: &str) -> Child {
        std::process::Command::new("sh")
            .args(["-c", &format!("sleep {secs}")])
            .spawn()
            .unwrap()
    }

    #[test]
    fn is_running_is_true_while_the_child_is_alive() {
        let mut handle = OverlayHandle::from_child(spawn_sleep("5"));
        assert!(handle.is_running());
        handle.toggle().unwrap(); // kills it; cleans up the test process
    }

    #[test]
    fn is_running_notices_the_child_exiting_on_its_own() {
        let mut handle = OverlayHandle::from_child(spawn_sleep("0"));
        // `sleep 0` exits almost immediately; give it a moment.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!handle.is_running());
        // Reaped, not just reported dead: a second check doesn't try to
        // `wait()` on an already-reaped child and panic/error.
        assert!(!handle.is_running());
    }

    #[test]
    fn toggle_kills_a_running_overlay() {
        let mut handle = OverlayHandle::from_child(spawn_sleep("5"));
        assert!(handle.is_running());
        assert!(!handle.toggle().unwrap());
        assert!(!handle.is_running());
    }

    #[test]
    fn no_overlay_running_has_is_running_false() {
        let mut handle = OverlayHandle::new();
        assert!(!handle.is_running());
    }
}
