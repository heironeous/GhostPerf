//! Where the running agent advertises its control port.
//!
//! The port isn't configured: the agent binds `127.0.0.1:0` at startup and
//! lets the OS hand out a free one. A fixed port kept colliding with
//! something on real machines — first a hidden Windows reservation
//! (44620–48715), then, on every agent restart, the previous agent's socket
//! not yet being released (`os error 10048`). Asking the OS for a port is
//! also atomic, unlike scanning for a free one and then binding it, which
//! races against anything else grabbing that port in between.
//!
//! The chosen port is written to `agent.port` in [`crate::data_dir`] so
//! the tray and UI can find the agent. It changes on every agent start, so
//! clients must re-read it each time they connect rather than caching it.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn port_file_path() -> PathBuf {
    crate::data_dir().join("agent.port")
}

/// Plain overwrite rather than write-temp-then-rename: a rename onto a file
/// some other reader has open without `FILE_SHARE_DELETE` (PowerShell,
/// Notepad, …) fails on Windows, which would stop the agent from starting.
/// The payload is a few bytes written in one call, and readers already treat
/// an unparseable file as "not available yet" and retry on their next poll.
pub fn write_port_file(path: &Path, port: u16) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{port}\n"))
}

/// `None` if the file is missing (agent not started yet, or shut down
/// cleanly) or doesn't hold a port. A crashed agent leaves a stale file
/// behind; connecting to that port just fails, the same as "not running",
/// until the relaunched agent overwrites it.
pub fn read_port_file(path: &Path) -> Option<u16> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|&port| port != 0)
}

pub fn remove_port_file(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Client side of the agent's control channel (see `agent::ipc` for the
/// server and protocol): one newline-terminated command per connection, one
/// line of reply, then the connection closes. Here rather than in the tray
/// because the UI sends `reload` too.
pub fn send_command(port: u16, command: &str) -> anyhow::Result<String> {
    let addr = ("127.0.0.1", port);
    let addr = std::net::ToSocketAddrs::to_socket_addrs(&addr)?
        .next()
        .ok_or_else(|| anyhow::anyhow!("could not resolve 127.0.0.1:{port}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    writeln!(stream, "{command}")?;

    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    Ok(reply.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ghostperf-agent-config-test-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("agent.port")
    }

    #[test]
    fn written_port_reads_back() {
        let path = scratch_file("roundtrip");
        write_port_file(&path, 51234).unwrap();
        assert_eq!(read_port_file(&path), Some(51234));
        // Overwritten on the next agent start, not appended to.
        write_port_file(&path, 60001).unwrap();
        assert_eq!(read_port_file(&path), Some(60001));
    }

    #[test]
    fn missing_file_is_none() {
        let path = scratch_file("missing");
        remove_port_file(&path).unwrap();
        assert_eq!(read_port_file(&path), None);
        // Removing an already-missing file is fine (clean shutdown after a
        // crash-relaunch race, or a second shutdown).
        remove_port_file(&path).unwrap();
    }

    #[test]
    fn garbage_or_partial_file_is_none() {
        let path = scratch_file("garbage");
        for contents in ["", "\n", "not-a-port", "70000", "0"] {
            std::fs::write(&path, contents).unwrap();
            assert_eq!(read_port_file(&path), None, "contents {contents:?}");
        }
    }

    #[test]
    fn send_command_returns_the_trimmed_reply_line() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Read the whole command line before replying, as the real agent
            // does: closing with unread data makes Windows reset the
            // connection, failing the client's read (see the tray's
            // `ipc_client` tests, where this first bit).
            let mut line = String::new();
            BufReader::new(&mut stream).read_line(&mut line).unwrap();
            writeln!(stream, "OK echo {}", line.trim()).unwrap();
        });
        assert_eq!(send_command(port, "reload").unwrap(), "OK echo reload");
        server.join().unwrap();
    }

    #[test]
    fn send_command_fails_when_nothing_is_listening() {
        assert!(send_command(1, "status").is_err());
    }

    #[test]
    fn tolerates_crlf_and_surrounding_whitespace() {
        let path = scratch_file("whitespace");
        std::fs::write(&path, " 40123\r\n").unwrap();
        assert_eq!(read_port_file(&path), Some(40123));
    }
}
