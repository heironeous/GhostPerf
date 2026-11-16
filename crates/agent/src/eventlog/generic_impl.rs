use storage::NewEvent;

/// No Windows Event Log on this platform; nothing to correlate.
pub fn correlate(_started_at_ms: i64, _ended_at_ms: i64) -> Vec<NewEvent> {
    Vec::new()
}
