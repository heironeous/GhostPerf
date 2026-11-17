//! CSV/JSON export of a session's metrics + events, for external analysis
//! (TODO.md Phase 4). Pure functions over `impl Write` so they're testable
//! without touching the filesystem.

use std::io::Write;

use storage::{Event, MetricSample, Session};

#[derive(serde::Serialize)]
struct JsonExport<'a> {
    session: &'a Session,
    metrics: &'a [MetricSample],
    events: &'a [Event],
}

pub fn export_json(
    session: &Session,
    metrics: &[MetricSample],
    events: &[Event],
    writer: impl Write,
) -> serde_json::Result<()> {
    serde_json::to_writer_pretty(
        writer,
        &JsonExport {
            session,
            metrics,
            events,
        },
    )
}

/// One row per sample. `cpu_per_core_pct` and `top_processes` don't fit a
/// flat row, so they're serialized to JSON text in their own columns rather
/// than dropped — CSV is for spreadsheets/plotting on the scalar columns,
/// not meant to be the lossless format (that's `export_json`).
pub fn export_csv(metrics: &[MetricSample], writer: impl Write) -> anyhow::Result<()> {
    let mut w = csv::Writer::from_writer(writer);
    w.write_record([
        "ts",
        "cpu_pct",
        "cpu_per_core_pct",
        "mem_used_mb",
        "mem_available_mb",
        "disk_read_bps",
        "disk_write_bps",
        "net_rx_bps",
        "net_tx_bps",
        "context_switches_per_sec",
        "page_faults_per_sec",
        "gpu_pct",
        "top_processes",
    ])?;
    for m in metrics {
        w.write_record([
            m.ts.to_string(),
            m.cpu_pct.to_string(),
            serde_json::to_string(&m.cpu_per_core_pct)?,
            m.mem_used_mb.to_string(),
            m.mem_available_mb.to_string(),
            m.disk_read_bps.to_string(),
            m.disk_write_bps.to_string(),
            m.net_rx_bps.to_string(),
            m.net_tx_bps.to_string(),
            m.context_switches_per_sec.to_string(),
            m.page_faults_per_sec.to_string(),
            m.gpu_pct.map_or_else(String::new, |v| v.to_string()),
            serde_json::to_string(&m.top_processes)?,
        ])?;
    }
    w.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use storage::ProcessSample;

    fn sample() -> MetricSample {
        MetricSample {
            ts: 1000,
            cpu_pct: 12.5,
            cpu_per_core_pct: vec![10.0, 15.0],
            mem_used_mb: 2048.0,
            mem_available_mb: 4096.0,
            disk_read_bps: 1.0,
            disk_write_bps: 2.0,
            net_rx_bps: 3.0,
            net_tx_bps: 4.0,
            context_switches_per_sec: 500.0,
            page_faults_per_sec: 10.0,
            gpu_pct: Some(33.0),
            top_processes: vec![ProcessSample {
                pid: 1,
                name: "a.exe".into(),
                cpu_pct: 9.0,
                mem_mb: 1.0,
                handle_count: None,
                thread_count: None,
            }],
        }
    }

    #[test]
    fn csv_has_header_and_one_row_per_sample() {
        let mut buf = Vec::new();
        export_csv(&[sample(), sample()], &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[0].starts_with("ts,cpu_pct,"));
        assert!(lines[1].contains("1000,12.5,"));
    }

    #[test]
    fn csv_missing_gpu_is_an_empty_field_not_a_literal_none() {
        let mut m = sample();
        m.gpu_pct = None;
        let mut buf = Vec::new();
        export_csv(&[m], &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(!text.contains("None"), "{text}");
    }

    #[test]
    fn json_round_trips_session_metrics_and_events() {
        let session = Session {
            id: 1,
            started_at: 1000,
            ended_at: Some(2000),
            label: Some("test".into()),
            host_info: None,
        };
        let events = vec![Event {
            id: 1,
            session_id: 1,
            ts: 1500,
            kind: "stall".into(),
            value: 60.0,
            detail: None,
        }];
        let mut buf = Vec::new();
        export_json(&session, &[sample()], &events, &mut buf).unwrap();

        let parsed: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(parsed["session"]["label"], "test");
        assert_eq!(parsed["metrics"][0]["ts"], 1000);
        assert_eq!(parsed["events"][0]["kind"], "stall");
    }
}
