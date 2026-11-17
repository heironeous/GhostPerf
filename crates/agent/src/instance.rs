//! Single-instance guard: an exclusive OS file lock on `agent.lock`.
//!
//! Binding a fixed control port used to double as this guard, implicitly.
//! Now that each agent gets its own OS-assigned port, nothing else would
//! stop a second agent from starting — and a second agent is actively
//! harmful, not just wasteful: on startup it closes "orphaned" sessions,
//! which would include the first agent's still-recording one.
//!
//! An OS lock rather than a PID file because the OS drops it the moment the
//! holder exits, however it exits (including a crash or `taskkill /F`), so
//! there's no stale state to detect or clean up.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

/// The lock is held for as long as the returned `File` is alive, so the
/// caller keeps it for the life of the process. `None` means another agent
/// already holds it.
pub fn acquire(path: &Path) -> std::io::Result<Option<File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_fails_until_first_is_released() {
        let dir =
            std::env::temp_dir().join(format!("ghostperf-instance-test-{}", std::process::id()));
        let path = dir.join("agent.lock");

        let first = acquire(&path).unwrap();
        assert!(first.is_some(), "first agent should get the lock");
        // Separate handle, as a second agent process would have — locks are
        // per-handle on Windows and per-open-file-description on Linux, so
        // this exercises the real contention path within one test process.
        assert!(
            acquire(&path).unwrap().is_none(),
            "second agent must be refused"
        );

        drop(first);
        assert!(
            acquire(&path).unwrap().is_some(),
            "lock should be free again once the first agent exits"
        );
    }
}
