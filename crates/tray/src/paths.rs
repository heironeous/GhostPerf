use std::ffi::OsStr;
use std::path::PathBuf;

/// Whether a process name as reported by `sysinfo` belongs to the binary
/// `name` (given without extension). On Windows `sysinfo` reports the image
/// name *with* `.exe` (`ghostperf-agent.exe`), so a plain comparison never
/// matched there — the supervisor thought the agent was never running and
/// spawned a duplicate every second.
pub fn is_process_named(process_name: &OsStr, name: &str) -> bool {
    let process_name = process_name.to_string_lossy();
    let base = process_name
        .len()
        .checked_sub(".exe".len())
        .and_then(|i| Some((i, process_name.get(i..)?)))
        .filter(|(_, ext)| ext.eq_ignore_ascii_case(".exe"))
        .map_or(&*process_name, |(i, _)| &process_name[..i]);
    base.eq_ignore_ascii_case(name)
}

/// Path to another binary in this install, assumed to sit next to the tray
/// executable (same directory) — true for both the installed layout and a
/// future portable zip (see TODO.md Phase 5).
pub fn sibling_exe(name: &str) -> anyhow::Result<PathBuf> {
    let dir = std::env::current_exe()?
        .parent()
        .ok_or_else(|| anyhow::anyhow!("current exe has no parent directory"))?
        .to_path_buf();
    let file_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    Ok(dir.join(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_windows_image_name_with_exe_suffix() {
        assert!(is_process_named(
            OsStr::new("ghostperf-agent.exe"),
            "ghostperf-agent"
        ));
        assert!(is_process_named(
            OsStr::new("GhostPerf-Agent.EXE"),
            "ghostperf-agent"
        ));
    }

    #[test]
    fn matches_bare_unix_process_name() {
        assert!(is_process_named(
            OsStr::new("ghostperf-agent"),
            "ghostperf-agent"
        ));
    }

    #[test]
    fn rejects_other_processes() {
        for other in [
            "ghostperf-tray.exe",
            "ghostperf-agent-old.exe",
            "ghostperf-agent.exe.bak",
            "agent.exe",
            ".exe",
            "",
        ] {
            assert!(
                !is_process_named(OsStr::new(other), "ghostperf-agent"),
                "{other:?} should not match"
            );
        }
    }
}
