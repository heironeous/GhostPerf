//! XDG autostart (`~/.config/autostart/*.desktop`) stand-in, used only so
//! this crate builds and the toggle is testable in this non-Windows dev
//! container — not the shipping mechanism (Windows uses the registry, see
//! `windows_impl`).

use std::fs;
use std::path::{Path, PathBuf};

const DESKTOP_FILE_NAME: &str = "ghostperf-tray.desktop";

fn autostart_dir_in(config_home: &Path) -> PathBuf {
    config_home.join("autostart")
}

fn desktop_file_contents(exe_path: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=GhostPerf\nExec=\"{}\"\nX-GNOME-Autostart-enabled=true\nNoDisplay=true\n",
        exe_path.display()
    )
}

fn resolve_config_home() -> anyhow::Result<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or_else(|| anyhow::anyhow!("neither XDG_CONFIG_HOME nor HOME is set"))
}

fn desktop_file_path() -> anyhow::Result<PathBuf> {
    Ok(autostart_dir_in(&resolve_config_home()?).join(DESKTOP_FILE_NAME))
}

pub fn enable() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let path = desktop_file_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, desktop_file_contents(&exe))?;
    Ok(())
}

pub fn disable() -> anyhow::Result<()> {
    let path = desktop_file_path()?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub fn is_enabled() -> anyhow::Result<bool> {
    Ok(desktop_file_path()?.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_file_contents_embeds_the_exe_path() {
        let contents = desktop_file_contents(Path::new("/opt/ghostperf/ghostperf-tray"));
        assert!(contents.contains("Exec=\"/opt/ghostperf/ghostperf-tray\""));
        assert!(contents.contains("[Desktop Entry]"));
    }

    #[test]
    fn autostart_dir_in_appends_autostart() {
        assert_eq!(
            autostart_dir_in(Path::new("/home/x/.config")),
            PathBuf::from("/home/x/.config/autostart")
        );
    }

    /// Exercises the actual file write/read/delete against an explicit
    /// directory, bypassing the env-var lookup (`resolve_config_home`) so
    /// this doesn't race with other tests mutating process-global env vars.
    #[test]
    fn enable_disable_roundtrip_against_an_explicit_directory() {
        let config_home = std::env::temp_dir().join(format!(
            "ghostperf-autostart-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = autostart_dir_in(&config_home).join(DESKTOP_FILE_NAME);
        fs::create_dir_all(file.parent().unwrap()).unwrap();

        assert!(!file.exists());
        fs::write(
            &file,
            desktop_file_contents(Path::new("/opt/ghostperf/ghostperf-tray")),
        )
        .unwrap();
        assert!(file.exists());
        let contents = fs::read_to_string(&file).unwrap();
        assert!(contents.contains("ghostperf-tray"));

        fs::remove_file(&file).unwrap();
        assert!(!file.exists());

        fs::remove_dir_all(&config_home).unwrap();
    }
}
