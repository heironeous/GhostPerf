//! Client side of the agent's loopback TCP control channel (see
//! `agent::ipc` for the server/protocol): one newline-terminated command per
//! connection, one line of reply, then the connection closes.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const IO_TIMEOUT: Duration = Duration::from_secs(2);

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

/// Whether the agent is currently recording, parsed from a `status` reply
/// like `OK recording=true session_id=3 started_at=... buffered=2`.
/// Returns `None` if the agent didn't reply at all (not running / wrong
/// port) rather than guessing a state.
pub fn is_recording(port: u16) -> Option<bool> {
    let reply = send_command(port, "status").ok()?;
    reply
        .split_whitespace()
        .find_map(|field| field.strip_prefix("recording="))
        .and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A minimal stand-in for the agent's control server: replies with a
    /// fixed line to whatever it's sent, once.
    ///
    /// Reads the *whole* command line before replying, as the real agent
    /// does. It used to do a single `read()`, which could return just
    /// `status` with the `\n` (sent as a separate write by `writeln!`) still
    /// in flight. Closing a socket with unread data makes Windows send an
    /// RST instead of a FIN, so the client's read of the reply failed with
    /// a reset and these tests failed ~1 run in 4 on Windows.
    fn serve_one_reply(reply: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut line = String::new();
                let _ = BufReader::new(&mut stream).read_line(&mut line);
                let _ = writeln!(stream, "{reply}");
            }
        });
        port
    }

    #[test]
    fn send_command_returns_the_trimmed_reply_line() {
        let port = serve_one_reply("OK session_id=1");
        assert_eq!(send_command(port, "start").unwrap(), "OK session_id=1");
    }

    #[test]
    fn is_recording_parses_true_from_a_status_reply() {
        let port = serve_one_reply("OK recording=true session_id=1 started_at=100 buffered=0");
        assert_eq!(is_recording(port), Some(true));
    }

    #[test]
    fn is_recording_parses_false_from_a_status_reply() {
        let port = serve_one_reply("OK recording=false session_id=- started_at=- buffered=0");
        assert_eq!(is_recording(port), Some(false));
    }

    #[test]
    fn is_recording_is_none_when_nothing_is_listening() {
        // Nothing bound on this port.
        assert_eq!(is_recording(1), None);
    }
}
