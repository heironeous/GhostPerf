//! Real Windows Event Log correlation via the WinEvt API. Verified on
//! Windows 11 (2026-11-16) against `Get-WinEvent` for the same window — see
//! the ignored `correlate_reads_real_event_log` test below.

use windows::core::PCWSTR;
use windows::Win32::System::EventLog::{
    EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EvtQueryReverseDirection, EvtRender,
    EvtRenderEventXml, EVT_HANDLE,
};

use storage::NewEvent;

use super::format::{extract_attr, extract_text, iso8601_utc, parse_iso8601_to_ms};

const CHANNELS: [&str; 2] = ["System", "Application"];
/// Caps how much a single session's correlation can pull in — a long
/// session on a noisy machine shouldn't turn session-stop into a stall of
/// its own.
const MAX_EVENTS_PER_CHANNEL: usize = 200;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn correlate(started_at_ms: i64, ended_at_ms: i64) -> Vec<NewEvent> {
    let start = iso8601_utc(started_at_ms);
    let end = iso8601_utc(ended_at_ms);
    // Level 1-3 = Critical, Error, Warning (Level 0 "LogAlways" and 4-5
    // Information/Verbose are too noisy to be useful here).
    let query_str = wide(&format!(
        "*[System[(Level=1 or Level=2 or Level=3) and \
          TimeCreated[@SystemTime>='{start}' and @SystemTime<='{end}']]]"
    ));

    let mut out = Vec::new();
    for channel in CHANNELS {
        out.extend(correlate_channel(channel, &query_str));
    }
    out
}

fn correlate_channel(channel: &str, query_str: &[u16]) -> Vec<NewEvent> {
    let mut out = Vec::new();
    unsafe {
        let channel_wide = wide(channel);
        let handle = match EvtQuery(
            None,
            PCWSTR(channel_wide.as_ptr()),
            PCWSTR(query_str.as_ptr()),
            (EvtQueryChannelPath.0 | EvtQueryReverseDirection.0) as u32,
        ) {
            Ok(h) => h,
            // Channel may not exist on this machine (e.g. no "Application"
            // log in some stripped-down installs) — skip it, not fatal.
            Err(e) => {
                log::warn!("skipping Event Log channel {channel}: {e}");
                return out;
            }
        };

        'pages: loop {
            let mut raw_events: [isize; 16] = [0; 16];
            let mut returned: u32 = 0;
            if EvtNext(handle, &mut raw_events, 0, 0, &mut returned).is_err() || returned == 0 {
                break;
            }
            for raw in &raw_events[..returned as usize] {
                let ev = EVT_HANDLE(*raw);
                if let Some(event) = render_event(ev, channel) {
                    out.push(event);
                }
                let _ = EvtClose(ev);
                if out.len() >= MAX_EVENTS_PER_CHANNEL {
                    break 'pages;
                }
            }
        }
        let _ = EvtClose(handle);
    }
    out
}

fn render_event(ev: EVT_HANDLE, channel: &str) -> Option<NewEvent> {
    unsafe {
        let mut buffer_used = 0u32;
        let mut property_count = 0u32;
        let _ = EvtRender(
            None,
            ev,
            EvtRenderEventXml.0,
            0,
            None,
            &mut buffer_used,
            &mut property_count,
        );
        if buffer_used == 0 {
            return None;
        }

        let mut buf: Vec<u16> = vec![0u16; buffer_used as usize / 2 + 1];
        let rendered = EvtRender(
            None,
            ev,
            EvtRenderEventXml.0,
            (buf.len() * 2) as u32,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            &mut buffer_used,
            &mut property_count,
        );
        if rendered.is_err() {
            return None;
        }

        let xml = String::from_utf16_lossy(&buf);
        parse_event_xml(&xml, channel)
    }
}

fn parse_event_xml(xml: &str, channel: &str) -> Option<NewEvent> {
    let level: i64 = extract_text(xml, "Level")?.parse().ok()?;
    let event_id = extract_text(xml, "EventID").unwrap_or_else(|| "?".to_string());
    let provider = extract_attr(xml, "Provider", "Name").unwrap_or_else(|| "unknown".to_string());
    let time_created = extract_attr(xml, "TimeCreated", "SystemTime")?;
    let ts = parse_iso8601_to_ms(&time_created)?;

    let kind = match level {
        1 => "eventlog_critical",
        2 => "eventlog_error",
        _ => "eventlog_warning",
    };

    Some(NewEvent {
        ts,
        kind: kind.to_string(),
        value: level as f64,
        detail: Some(format!(
            "channel={channel} provider={provider} event_id={event_id}"
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the real WinEvt query against this machine's logs. Machine-dependent,
    /// so ignored by default. Pass a window known (via Event Viewer /
    /// `Get-WinEvent`) to contain Warning+ events:
    ///   $env:GP_EVT_START_MS=...; $env:GP_EVT_END_MS=...; $env:GP_EVT_EXPECTED=N
    ///   cargo test -p agent eventlog::windows_impl -- --ignored --nocapture
    #[test]
    #[ignore]
    fn correlate_reads_real_event_log() {
        let var = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<i64>().ok());
        let now = crate::now_millis();
        let start = var("GP_EVT_START_MS").unwrap_or(now - 24 * 3600 * 1000);
        let end = var("GP_EVT_END_MS").unwrap_or(now);
        let events = correlate(start, end);
        for e in &events {
            println!("{} {} {:?}", e.ts, e.kind, e.detail);
            assert!(
                e.ts >= start - 1000 && e.ts <= end + 1000,
                "event outside window"
            );
        }
        println!("{} events", events.len());
        if let Some(expected) = var("GP_EVT_EXPECTED") {
            assert_eq!(events.len() as i64, expected);
        }
    }
}
