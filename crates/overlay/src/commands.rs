//! Commands from the tray, one per line on the overlay's stdin. The tray
//! starts the overlay and keeps the pipe (`crates/tray/src/overlay.rs`), so
//! this needs no port or file of its own. Read on a thread, since a read
//! blocks, and handed to the app through a channel.

use std::sync::mpsc::{channel, Receiver};

use agent_config::{OVERLAY_EDIT_OFF, OVERLAY_EDIT_ON};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Edit mode: the window takes the mouse and can be dragged, instead
    /// of being click-through ("Move Overlay" in the tray).
    Edit(bool),
}

pub fn parse(line: &str) -> Option<Command> {
    match line.trim() {
        OVERLAY_EDIT_ON => Some(Command::Edit(true)),
        OVERLAY_EDIT_OFF => Some(Command::Edit(false)),
        _ => None,
    }
}

/// Starts the reader thread. `ctx` is woken for each command, since the
/// app otherwise only wakes up once per refresh interval.
pub fn listen(ctx: egui::Context) -> Receiver<Command> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            match parse(&line) {
                Some(command) => {
                    if tx.send(command).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
                None => log::warn!("unknown command from the tray: {line:?}"),
            }
        }
        // End of input: the tray has exited (or this overlay was started by
        // hand, with no pipe). Nothing could switch edit mode off anymore,
        // so it's switched off here rather than left click-blocking.
        let _ = tx.send(Command::Edit(false));
        ctx.request_repaint();
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_tray_commands() {
        assert_eq!(parse(OVERLAY_EDIT_ON), Some(Command::Edit(true)));
        assert_eq!(parse(OVERLAY_EDIT_OFF), Some(Command::Edit(false)));
        // `lines()` strips `\n` but leaves a `\r` from a CRLF writer.
        assert_eq!(parse("edit on\r"), Some(Command::Edit(true)));
        assert_eq!(parse("edit sideways"), None);
        assert_eq!(parse(""), None);
    }
}
