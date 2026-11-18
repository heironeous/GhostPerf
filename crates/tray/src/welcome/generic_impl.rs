//! Non-Windows stand-in: `tray-icon` has no notification balloons on Linux,
//! and this build exists only for development (see `AGENTS.md`), so the
//! message just goes to the log.

use tray_icon::TrayIcon;

pub fn show_balloon(_tray: &TrayIcon, title: &str, text: &str) -> anyhow::Result<()> {
    log::info!("(first-run notification) {title}: {text}");
    Ok(())
}
