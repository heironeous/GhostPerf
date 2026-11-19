//! "Show Overlay" and "Move Overlay" menu actions. "Show Overlay" launches
//! `ghostperf-overlay` if it isn't running, or kills it if it is.
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
//!
//! The overlay's stdin is kept as a command pipe: "Move Overlay" sends edit
//! mode on/off down it (`crates/overlay/src/commands.rs`). Since the tray
//! holds the `Child` anyway, that needs no port or file.

use std::io::Write;
use std::process::{Child, Stdio};

use agent_config::{OVERLAY_EDIT_OFF, OVERLAY_EDIT_ON};
use anyhow::Context;

use crate::paths::sibling_exe;

const OVERLAY_PROCESS_NAME: &str = "ghostperf-overlay";

#[derive(Default)]
pub struct OverlayHandle {
    child: Option<Child>,
    /// Edit mode as last sent to the running overlay. Reset whenever the
    /// overlay goes away, since a new one always starts locked.
    editing: bool,
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
                    self.forget();
                    false
                }
                Err(e) => {
                    log::warn!("couldn't check overlay status ({e}); assuming it's gone");
                    self.forget();
                    false
                }
            },
            None => false,
        }
    }

    fn forget(&mut self) {
        self.child = None;
        self.editing = false;
    }

    /// Returns whether the overlay is running *after* this call.
    pub fn toggle(&mut self) -> anyhow::Result<bool> {
        if self.is_running() {
            if let Some(mut child) = self.child.take() {
                child.kill()?;
                let _ = child.wait(); // avoid leaving a zombie behind
            }
            self.forget();
            return Ok(false);
        }
        let path = sibling_exe(OVERLAY_PROCESS_NAME)?;
        self.child = Some(
            std::process::Command::new(path)
                .stdin(Stdio::piped())
                .spawn()?,
        );
        Ok(true)
    }

    /// Whether the overlay is running and in edit mode.
    pub fn is_editing(&mut self) -> bool {
        self.is_running() && self.editing
    }

    /// Switches edit mode, starting the overlay first if it's needed and
    /// not running: there's nothing to move otherwise.
    pub fn set_editing(&mut self, on: bool) -> anyhow::Result<()> {
        if on && !self.is_running() {
            self.toggle()?;
        }
        let Some(child) = self.child.as_mut() else {
            return Ok(()); // switching off with no overlay: nothing to do
        };
        let pipe = child
            .stdin
            .as_mut()
            .context("the overlay has no command pipe")?;
        writeln!(
            pipe,
            "{}",
            if on {
                OVERLAY_EDIT_ON
            } else {
                OVERLAY_EDIT_OFF
            }
        )
        .and_then(|()| pipe.flush())
        .context("couldn't send the command to the overlay")?;
        self.editing = on;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl OverlayHandle {
        fn from_child(child: Child) -> Self {
            Self {
                child: Some(child),
                editing: false,
            }
        }
    }

    // A plain sleeper rather than `sibling_exe` + a real `ghostperf-overlay`:
    // this only needs *some* child process whose lifetime it controls, and
    // a test binary has no built overlay exe sitting next to it. On Windows
    // `ping` stands in for `sleep` (one echo per second, the first
    // immediate); it's spawned directly rather than through `cmd`, since
    // killing a `cmd /C` wrapper would leave the sleeper itself running.
    #[cfg(not(windows))]
    fn spawn_sleep(secs: u32) -> Child {
        std::process::Command::new("sh")
            .args(["-c", &format!("sleep {secs}")])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap()
    }

    #[cfg(windows)]
    fn spawn_sleep(secs: u32) -> Child {
        std::process::Command::new("ping")
            .args(["-n", &(secs + 1).to_string(), "127.0.0.1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[test]
    fn is_running_is_true_while_the_child_is_alive() {
        let mut handle = OverlayHandle::from_child(spawn_sleep(5));
        assert!(handle.is_running());
        handle.toggle().unwrap(); // kills it; cleans up the test process
    }

    #[test]
    fn is_running_notices_the_child_exiting_on_its_own() {
        let mut handle = OverlayHandle::from_child(spawn_sleep(0));
        // Exits almost immediately, but polled up to a deadline rather than
        // after one fixed sleep, so a slow process start on a loaded
        // machine doesn't fail the test.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while handle.is_running() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!handle.is_running());
        // Reaped, not just reported dead: a second check doesn't try to
        // `wait()` on an already-reaped child and panic/error.
        assert!(!handle.is_running());
    }

    #[test]
    fn toggle_kills_a_running_overlay() {
        let mut handle = OverlayHandle::from_child(spawn_sleep(5));
        assert!(handle.is_running());
        assert!(!handle.toggle().unwrap());
        assert!(!handle.is_running());
    }

    #[test]
    fn edit_mode_is_tracked_and_ends_with_the_overlay() {
        let mut handle = OverlayHandle::from_child(spawn_sleep(5));
        assert!(!handle.is_editing());
        handle.set_editing(true).unwrap();
        assert!(handle.is_editing());
        handle.set_editing(false).unwrap();
        assert!(!handle.is_editing());

        handle.set_editing(true).unwrap();
        assert!(!handle.toggle().unwrap()); // kills it
        assert!(!handle.is_editing(), "a new overlay starts locked");
    }

    #[test]
    fn edit_mode_ends_when_the_overlay_exits_on_its_own() {
        let mut handle = OverlayHandle::from_child(spawn_sleep(0));
        // The pipe can already be closed by the time this writes; only the
        // state afterwards matters here.
        let _ = handle.set_editing(true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while handle.is_running() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!handle.is_editing());
    }

    #[test]
    fn switching_edit_mode_off_with_no_overlay_does_nothing() {
        let mut handle = OverlayHandle::new();
        handle.set_editing(false).unwrap();
        assert!(!handle.is_running());
        assert!(!handle.is_editing());
    }

    #[test]
    fn no_overlay_running_has_is_running_false() {
        let mut handle = OverlayHandle::new();
        assert!(!handle.is_running());
    }
}
