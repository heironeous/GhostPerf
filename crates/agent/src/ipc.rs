use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{channel, Sender};

use crate::core::CoreMsg;

/// Binds the loopback-only control socket. Kept separate from `serve` so the
/// caller can bind on the main thread and treat failure as fatal — an agent
/// with no control channel can never be told to start recording.
pub fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port))
}

/// Loopback-only control server. Accepts one newline-terminated command per
/// connection (`start [label]`, `stop`, `status`, `shutdown`) and replies
/// with a single line before closing. Binding to 127.0.0.1 keeps it off the
/// network entirely — this is a same-machine control channel, not a remote API.
pub fn serve(listener: TcpListener, core_tx: Sender<CoreMsg>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let core_tx = core_tx.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle_connection(stream, core_tx) {
                        eprintln!("ghostperf-agent: ipc connection error: {e}");
                    }
                });
            }
            Err(e) => eprintln!("ghostperf-agent: ipc accept error: {e}"),
        }
    }
}

fn handle_connection(mut stream: TcpStream, core_tx: Sender<CoreMsg>) -> anyhow::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let line = line.trim();

    let response = dispatch(line, &core_tx);
    writeln!(stream, "{response}")?;
    Ok(())
}

fn dispatch(line: &str, core_tx: &Sender<CoreMsg>) -> String {
    let mut parts = line.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("").to_ascii_lowercase();
    let arg = parts.next().map(str::to_string);

    match cmd.as_str() {
        "start" => {
            let (reply, rx) = channel();
            if core_tx.send(CoreMsg::Start { label: arg, reply }).is_err() {
                return "ERR agent core unavailable".to_string();
            }
            match rx.recv() {
                Ok(Ok(id)) => format!("OK session_id={id}"),
                Ok(Err(e)) => format!("ERR {e}"),
                Err(_) => "ERR no reply from agent core".to_string(),
            }
        }
        "stop" => {
            let (reply, rx) = channel();
            if core_tx.send(CoreMsg::Stop { reply }).is_err() {
                return "ERR agent core unavailable".to_string();
            }
            match rx.recv() {
                Ok(Ok(Some(id))) => format!("OK session_id={id}"),
                Ok(Ok(None)) => "OK no_active_session".to_string(),
                Ok(Err(e)) => format!("ERR {e}"),
                Err(_) => "ERR no reply from agent core".to_string(),
            }
        }
        "status" => {
            let (reply, rx) = channel();
            if core_tx.send(CoreMsg::Status { reply }).is_err() {
                return "ERR agent core unavailable".to_string();
            }
            match rx.recv() {
                Ok(s) => format!(
                    "OK recording={} session_id={} started_at={} buffered={}",
                    s.recording,
                    s.session_id
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".into()),
                    s.started_at
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".into()),
                    s.samples_buffered
                ),
                Err(_) => "ERR no reply from agent core".to_string(),
            }
        }
        "shutdown" => {
            let _ = core_tx.send(CoreMsg::Shutdown);
            "OK shutting_down".to_string()
        }
        other => format!("ERR unknown command '{other}'"),
    }
}
