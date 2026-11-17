use std::path::PathBuf;

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
