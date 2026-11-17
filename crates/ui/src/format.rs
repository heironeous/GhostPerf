//! Pure display formatting for timestamps/durations (`ts`/`started_at`/
//! `ended_at` throughout `storage` are epoch milliseconds — UTC, no
//! timezone stored), kept separate from the egui views so it's testable
//! without a GUI.

/// `2026-11-17 14:32:05` in the local timezone.
pub fn format_datetime(ms: i64) -> String {
    match chrono::DateTime::from_timestamp_millis(ms) {
        Some(utc) => chrono::DateTime::<chrono::Local>::from(utc)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        None => "invalid timestamp".to_string(),
    }
}

/// `started_at`/`ended_at` (`None` while a session is still recording) to a
/// human duration like `12m 34s` or `1h 02m 03s`.
pub fn format_duration(started_at: i64, ended_at: Option<i64>) -> String {
    let Some(ended_at) = ended_at else {
        return "recording…".to_string();
    };
    let total_secs = ended_at.saturating_sub(started_at).max(0) / 1000;
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// A byte rate as a compact chart label: `0`, `512 B/s`, `300 KB/s`,
/// `2.5 MB/s`.
///
/// Decimal units (1 KB = 1000 B), not binary: `egui_plot` places y ticks at
/// round raw values (10 000 000, 20 000 000, …), and decimal units turn those
/// into round labels (`10 MB/s`) where binary ones would give `9.5 MB/s`.
/// Also kept short on purpose: every chart shares one fixed y-axis width
/// (see `session_detail::Y_AXIS_WIDTH`), and a label wider than that would
/// widen its own chart and knock the charts out of horizontal alignment.
pub fn format_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", format_si_bytes(bytes_per_sec))
}

fn format_si_bytes(bytes: f64) -> String {
    if bytes == 0.0 {
        return "0 B".to_string();
    }
    let sign = if bytes < 0.0 { "-" } else { "" };
    let mut v = bytes.abs();
    let mut unit = 0;
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    // 999.5, not 1000: anything that would *print* as "1000 KB" moves up.
    while v >= 999.5 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    // One decimal only where it carries information (below 10), so labels
    // stay short and round ticks stay round ("20 MB", not "20.0 MB").
    if v < 10.0 && unit > 0 && (v * 10.0).round() % 10.0 != 0.0 {
        format!("{sign}{v:.1} {}", UNITS[unit])
    } else {
        format!("{sign}{v:.0} {}", UNITS[unit])
    }
}

/// Memory in MB (as `sysinfo` reports it) as a chart label: `22800 MB`.
/// Stays in MB rather than switching to GB because the memory chart's ticks
/// are typically 100 MB apart, which GB labels would round together.
pub fn format_mb(mb: f64) -> String {
    format!("{mb:.0} MB")
}

pub fn format_pct(pct: f64) -> String {
    format!("{pct:.0}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_use_round_decimal_units() {
        assert_eq!(format_rate(0.0), "0 B/s");
        assert_eq!(format_rate(512.0), "512 B/s");
        assert_eq!(format_rate(300_000.0), "300 KB/s");
        assert_eq!(format_rate(10_000_000.0), "10 MB/s");
        assert_eq!(format_rate(20_000_000.0), "20 MB/s");
        assert_eq!(format_rate(2_500_000.0), "2.5 MB/s");
        assert_eq!(format_rate(227_544_248.6), "228 MB/s");
        assert_eq!(format_rate(1_500_000_000.0), "1.5 GB/s");
        // Values that would round up to "1000 <unit>" move to the next unit.
        assert_eq!(format_rate(999_999.0), "1 MB/s");
        assert_eq!(format_rate(9_960_000.0), "10 MB/s");
    }

    #[test]
    fn rates_below_zero_keep_their_sign() {
        // Auto-bounds pad the y range below 0, so negative ticks do occur.
        assert_eq!(format_rate(-5_000_000.0), "-5 MB/s");
    }

    #[test]
    fn axis_labels_stay_short_enough_for_the_shared_axis_width() {
        // The widest realistic labels; see `format_rate`'s doc for why
        // length matters.
        for label in [
            format_rate(-999_000_000.0),
            format_rate(9_900_000.0),
            format_mb(131_072.0),
            format_pct(100.0),
        ] {
            assert!(label.chars().count() <= 10, "{label:?} is too long");
        }
    }

    #[test]
    fn duration_formats_scale_to_magnitude() {
        assert_eq!(format_duration(0, Some(45_000)), "45s");
        assert_eq!(format_duration(0, Some(754_000)), "12m 34s");
        assert_eq!(format_duration(0, Some(3_723_000)), "1h 02m 03s");
    }

    #[test]
    fn duration_of_still_recording_session_says_so() {
        assert_eq!(format_duration(0, None), "recording…");
    }

    #[test]
    fn duration_never_goes_negative_on_clock_skew() {
        // ended_at before started_at shouldn't happen, but a clock step
        // during recording could produce it; don't underflow/panic.
        assert_eq!(format_duration(10_000, Some(5_000)), "0s");
    }

    #[test]
    fn datetime_formats_a_known_instant() {
        // 2026-11-17T12:00:00Z
        let ms = 1794916800_000;
        let utc = chrono::DateTime::from_timestamp_millis(ms).unwrap();
        let expected = chrono::DateTime::<chrono::Local>::from(utc)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        assert_eq!(format_datetime(ms), expected);
    }
}
