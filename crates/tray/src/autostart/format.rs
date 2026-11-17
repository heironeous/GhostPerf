// Only consumed by `windows_impl` (not compiled on this platform), but kept
// as pure, platform-independent string logic so it's unit tested here
// regardless of which OS this builds on — same reasoning as
// `agent::eventlog::format`.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use std::path::Path;

pub(super) const VALUE_NAME: &str = "GhostPerf";

/// What to write into the `Run` value: the exe path, quoted so a path
/// containing spaces (e.g. under "Program Files") still runs as a single
/// argument rather than being split on the first space.
pub(super) fn run_value_for(exe_path: &Path) -> String {
    format!("\"{}\"", exe_path.display())
}

/// Whether an existing `Run` value already points at this install's exe,
/// tolerating the with/without-quotes variants — a value written by an
/// older build (or hand-edited) might not match byte-for-byte.
pub(super) fn matches_current(existing_value: &str, exe_path: &Path) -> bool {
    let existing = existing_value.trim().trim_matches('"');
    existing.eq_ignore_ascii_case(&exe_path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn run_value_wraps_the_path_in_quotes() {
        let path = PathBuf::from(r"C:\Program Files\GhostPerf\ghostperf-tray.exe");
        assert_eq!(
            run_value_for(&path),
            r#""C:\Program Files\GhostPerf\ghostperf-tray.exe""#
        );
    }

    #[test]
    fn matches_current_accepts_quoted_and_unquoted_values() {
        let path = PathBuf::from(r"C:\GhostPerf\ghostperf-tray.exe");
        assert!(matches_current(
            r#""C:\GhostPerf\ghostperf-tray.exe""#,
            &path
        ));
        assert!(matches_current(r"C:\GhostPerf\ghostperf-tray.exe", &path));
    }

    #[test]
    fn matches_current_is_case_insensitive() {
        let path = PathBuf::from(r"C:\GhostPerf\ghostperf-tray.exe");
        assert!(matches_current(
            r#""c:\ghostperf\GHOSTPERF-TRAY.EXE""#,
            &path
        ));
    }

    #[test]
    fn matches_current_rejects_a_different_path() {
        let path = PathBuf::from(r"C:\GhostPerf\ghostperf-tray.exe");
        assert!(!matches_current(r#""C:\Other\thing.exe""#, &path));
    }
}
