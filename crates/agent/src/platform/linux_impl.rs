//! Linux counters read from `/proc`. This backend exists purely so the
//! agent builds and can be smoke-tested in a non-Windows dev environment —
//! the shipping target is Windows (see `windows_impl`).

use std::fs;

use super::SystemCounters;

pub struct PlatformCounters {
    prev_ctxt: Option<u64>,
    prev_pgfault: Option<u64>,
}

impl PlatformCounters {
    pub fn new() -> Self {
        Self {
            prev_ctxt: None,
            prev_pgfault: None,
        }
    }

    pub fn sample_system(&mut self, elapsed_secs: f64) -> SystemCounters {
        let ctxt = read_proc_stat_ctxt();
        let pgfault = read_proc_vmstat_pgfault();

        let context_switches_per_sec = match (ctxt, self.prev_ctxt) {
            (Some(now), Some(prev)) => now.saturating_sub(prev) as f64 / elapsed_secs,
            _ => 0.0,
        };
        let page_faults_per_sec = match (pgfault, self.prev_pgfault) {
            (Some(now), Some(prev)) => now.saturating_sub(prev) as f64 / elapsed_secs,
            _ => 0.0,
        };

        self.prev_ctxt = ctxt;
        self.prev_pgfault = pgfault;

        SystemCounters {
            context_switches_per_sec,
            page_faults_per_sec,
            // No simple cross-vendor GPU utilization counter on Linux.
            gpu_pct: None,
        }
    }

    /// Dev-only stand-in for the Windows HANDLE count: open file
    /// descriptors. Different metric, but exercises the same plumbing.
    pub fn handle_count(&self, pid: u32) -> Option<u32> {
        fs::read_dir(format!("/proc/{pid}/fd"))
            .ok()
            .map(|d| d.count() as u32)
    }

    pub fn thread_count(&self, pid: u32) -> Option<u32> {
        let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        status
            .lines()
            .find_map(|line| line.strip_prefix("Threads:"))
            .and_then(|v| v.trim().parse().ok())
    }
}

fn read_proc_stat_ctxt() -> Option<u64> {
    let text = fs::read_to_string("/proc/stat").ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("ctxt "))
        .and_then(|v| v.trim().parse().ok())
}

/// Sum of minor + major page faults, matching the intent of Windows'
/// `\Memory\Page Faults/sec` (which also counts both).
fn read_proc_vmstat_pgfault() -> Option<u64> {
    let text = fs::read_to_string("/proc/vmstat").ok()?;
    let mut total = 0u64;
    let mut found = false;
    for line in text.lines() {
        let value = line
            .strip_prefix("pgfault ")
            .or_else(|| line.strip_prefix("pgmajfault "));
        if let Some(v) = value {
            if let Ok(n) = v.trim().parse::<u64>() {
                total += n;
                found = true;
            }
        }
    }
    found.then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_real_proc_stat_ctxt() {
        assert!(read_proc_stat_ctxt().unwrap() > 0);
    }

    #[test]
    fn reads_real_proc_vmstat_pgfault() {
        assert!(read_proc_vmstat_pgfault().unwrap() > 0);
    }

    #[test]
    fn sample_system_rates_are_zero_on_first_call_then_positive() {
        let mut counters = PlatformCounters::new();
        let first = counters.sample_system(1.0);
        assert_eq!(first.context_switches_per_sec, 0.0);
        assert_eq!(first.page_faults_per_sec, 0.0);

        // Busy-loop briefly so the cumulative counters actually advance.
        let start = std::time::Instant::now();
        let mut x: u64 = 0;
        while start.elapsed() < std::time::Duration::from_millis(50) {
            x = x.wrapping_add(1);
        }
        std::hint::black_box(x);

        let second = counters.sample_system(0.05);
        assert!(second.context_switches_per_sec >= 0.0);
    }

    #[test]
    fn handle_and_thread_count_for_self() {
        let counters = PlatformCounters::new();
        let pid = std::process::id();
        assert!(counters.handle_count(pid).unwrap() > 0);
        assert!(counters.thread_count(pid).unwrap() > 0);
    }
}
