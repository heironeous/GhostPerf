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

#[cfg(test)]
mod tests {
    use super::*;

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
