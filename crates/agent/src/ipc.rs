use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;

use crate::core::CoreMsg;
use crate::now_millis;
use crate::telemetry::Telemetry;

/// Binds the loopback-only control socket on an OS-assigned free port (see
/// `agent_config::control_port` for why the port isn't fixed). Kept
/// separate from `serve` so the caller can bind on the main thread and treat
/// failure as fatal — an agent with no control channel can never be told to
/// start recording.
pub fn bind() -> std::io::Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", 0))
}

/// Loopback-only control server. Accepts one newline-terminated command per
/// connection (`start [label]`, `stop`, `status`, `reload`, `live`,
/// `shutdown`) and replies with a single line before closing. Binding to
/// 127.0.0.1 keeps it off the network entirely — this is a same-machine
/// control channel, not a remote API.
pub fn serve(listener: TcpListener, core_tx: Sender<CoreMsg>, telemetry: Arc<Telemetry>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let core_tx = core_tx.clone();
                let telemetry = telemetry.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle_connection(stream, core_tx, telemetry) {
                        log::warn!("connection error: {e}");
                    }
                });
            }
            Err(e) => log::warn!("accept error: {e}"),
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    core_tx: Sender<CoreMsg>,
    telemetry: Arc<Telemetry>,
) -> anyhow::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let line = line.trim();

    let response = dispatch(line, &core_tx, &telemetry);
    writeln!(stream, "{response}")?;
    Ok(())
}

fn dispatch(line: &str, core_tx: &Sender<CoreMsg>, telemetry: &Telemetry) -> String {
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
        // Re-reads agent.toml and applies it to the running agent. A file
        // that doesn't parse is refused, and the agent keeps its current
        // settings rather than falling back to defaults.
        "reload" => {
            let path = agent_config::AgentConfig::default_config_path();
            let config = match agent_config::AgentConfig::load_or_default(&path) {
                Ok(config) => config,
                Err(e) => {
                    log::warn!("reload refused, {} unreadable: {e:#}", path.display());
                    // TOML errors run over several lines (with a caret
                    // under the bad spot), but a reply is exactly one line.
                    let e = format!("{e:#}");
                    let first_line = e.lines().next().unwrap_or_default();
                    return format!("ERR {}: {first_line}", path.display());
                }
            };
            let (reply, rx) = channel();
            let msg = CoreMsg::Reload {
                config: Box::new(config),
                reply,
            };
            if core_tx.send(msg).is_err() {
                return "ERR agent core unavailable".to_string();
            }
            match rx.recv() {
                Ok(changes) => format!("OK reloaded changes={}", changes.len()),
                Err(_) => "ERR no reply from agent core".to_string(),
            }
        }
        // The overlay's live CPU/RAM/GPU readout (Phase 6). Doesn't touch
        // `core_tx`/the session store at all — see telemetry.rs for why.
        // Polling this is also what keeps the sampler thread ticking while
        // nothing is recording, so a response with no data yet (the
        // sampler hasn't ticked since this lease started) is expected on
        // the very first poll or two, not an error.
        // The temperature fields (Phase 8) came later; an older overlay
        // skips keys it doesn't know, so adding fields doesn't break it.
        "live" => match telemetry.poll(now_millis()) {
            Some(s) => {
                let opt = |v: Option<f32>| v.map_or_else(|| "-".to_string(), |v| format!("{v:.1}"));
                format!(
                    "OK cpu_pct={:.1} mem_used_pct={} gpu_pct={} cpu_temp_c={} gpu_temp_c={} \
                     board_temp_c={} age_ms={}",
                    s.cpu_pct,
                    opt(s.mem_used_pct),
                    opt(s.gpu_pct),
                    opt(s.cpu_temp_c),
                    opt(s.gpu_temp_c),
                    opt(s.board_temp_c),
                    (now_millis() - s.ts).max(0)
                )
            }
            None => "OK no_data_yet".to_string(),
        },
        "shutdown" => {
            let _ = core_tx.send(CoreMsg::Shutdown);
            "OK shutting_down".to_string()
        }
        other => format!("ERR unknown command '{other}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use storage::MetricSample;

    fn sample(ts: i64, cpu_pct: f32) -> MetricSample {
        MetricSample {
            ts,
            cpu_pct,
            cpu_per_core_pct: vec![cpu_pct],
            mem_used_mb: 1.0,
            mem_available_mb: 1.0,
            disk_read_bps: 0.0,
            disk_write_bps: 0.0,
            net_rx_bps: 0.0,
            net_tx_bps: 0.0,
            context_switches_per_sec: 0.0,
            page_faults_per_sec: 0.0,
            gpu_pct: Some(5.0),
            disk_queue_len: None,
            temps: Vec::new(),
            cpu_perf: None,
            top_processes: Vec::new(),
        }
    }

    #[test]
    fn live_with_no_data_published_yet() {
        let (core_tx, _core_rx) = channel();
        let telemetry = Telemetry::new();
        assert_eq!(dispatch("live", &core_tx, &telemetry), "OK no_data_yet");
    }

    #[test]
    fn live_reports_the_latest_published_sample() {
        let (core_tx, _core_rx) = channel();
        let telemetry = Telemetry::new();
        telemetry.publish(&sample(now_millis(), 42.0));
        let reply = dispatch("live", &core_tx, &telemetry);
        assert!(
            reply.starts_with("OK cpu_pct=42.0 mem_used_pct=") && reply.contains("gpu_pct=5.0"),
            "{reply}"
        );
    }

    #[test]
    fn live_reports_the_hottest_sensor_of_each_kind() {
        use storage::{SensorKind, TempReading};
        let (core_tx, _core_rx) = channel();
        let telemetry = Telemetry::new();
        let mut s = sample(now_millis(), 10.0);
        let t = |sensor: &str, kind, celsius| TempReading {
            sensor: sensor.into(),
            kind,
            celsius,
        };
        s.temps = vec![
            t("GPU 0", SensorKind::Gpu, 55.0),
            t("GPU 1", SensorKind::Gpu, 71.5),
            t(r"ACPI \_TZ.TZ00", SensorKind::Board, 27.85),
        ];
        telemetry.publish(&s);
        let reply = dispatch("live", &core_tx, &telemetry);
        assert!(
            reply.contains(" cpu_temp_c=- gpu_temp_c=71.5 board_temp_c=27.9 age_ms="),
            "{reply}"
        );
    }

    #[test]
    fn unknown_command_is_an_error() {
        let (core_tx, _core_rx) = channel();
        let telemetry = Telemetry::new();
        assert_eq!(
            dispatch("nonsense", &core_tx, &telemetry),
            "ERR unknown command 'nonsense'"
        );
    }
}
