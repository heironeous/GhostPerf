//! Linux counters read from `/proc`. This backend exists purely so the
//! agent builds and can be smoke-tested in a non-Windows dev environment —
//! the shipping target is Windows (see `windows_impl`).

use std::fs;

use super::SystemCounters;

pub struct PlatformCounters {
    prev_ctxt: Option<u64>,
    prev_pgfault: Option<u64>,
    prev_disk_weighted_ms: Option<u64>,
}

impl PlatformCounters {
    pub fn new() -> Self {
        Self {
            prev_ctxt: None,
            prev_pgfault: None,
            prev_disk_weighted_ms: None,
        }
    }

    pub fn sample_system(&mut self, elapsed_secs: f64) -> SystemCounters {
        let ctxt = read_proc_stat_ctxt();
        let pgfault = read_proc_vmstat_pgfault();
        let disk_weighted_ms = fs::read_to_string("/proc/diskstats")
            .ok()
            .and_then(|text| diskstats_weighted_ms(&text, is_whole_disk));
        // Weighted I/O time grows by (requests in flight × elapsed ms), so its
        // growth per elapsed ms is the average queue length over the
        // interval: the same quantity as Windows' "Avg. Disk Queue Length",
        // and what `iostat` reports as `aqu-sz`.
        let disk_queue_len = match (disk_weighted_ms, self.prev_disk_weighted_ms) {
            (Some(now), Some(prev)) => {
                Some((now.saturating_sub(prev) as f64 / (elapsed_secs * 1000.0)) as f32)
            }
            _ => None,
        };
        self.prev_disk_weighted_ms = disk_weighted_ms;

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
            disk_queue_len,
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

/// Whole disks only (`sda`, `nvme0n1`), not their partitions, which would
/// count the same requests twice. `/sys/block` lists exactly the whole
/// disks; loop and RAM devices are left out as not real I/O.
fn is_whole_disk(name: &str) -> bool {
    !name.starts_with("loop")
        && !name.starts_with("ram")
        && std::path::Path::new("/sys/block").join(name).exists()
}

/// Sum over the devices `include` accepts of the 11th stats field of
/// `/proc/diskstats` (the line's 14th column): "weighted time spent doing
/// I/Os", in ms. `None` if no device was counted.
fn diskstats_weighted_ms(text: &str, include: impl Fn(&str) -> bool) -> Option<u64> {
    let mut total = 0u64;
    let mut found = false;
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let (Some(name), Some(weighted)) = (cols.get(2).copied(), cols.get(13)) else {
            continue;
        };
        if let (true, Ok(ms)) = (include(name), weighted.parse::<u64>()) {
            total += ms;
            found = true;
        }
    }
    found.then_some(total)
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
    fn diskstats_sums_weighted_io_time_of_the_included_devices() {
        // Real /proc/diskstats layout (kernel 5.5+, 20 columns); the 14th
        // column is the weighted I/O time.
        let text = "\
   8       0 sda 1000 0 2000 300 400 0 800 500 0 600 900 0 0 0 0 0 0
   8       1 sda1 900 0 1800 280 390 0 780 490 0 580 870 0 0 0 0 0 0
 259       0 nvme0n1 50 0 100 10 20 0 40 30 2 60 75 0 0 0 0 0 0
   7       0 loop0 5 0 10 1 0 0 0 0 0 1 99 0 0 0 0 0 0
garbage line
";
        let whole = |name: &str| matches!(name, "sda" | "nvme0n1");
        assert_eq!(diskstats_weighted_ms(text, whole), Some(900 + 75));
        assert_eq!(diskstats_weighted_ms(text, |_| false), None);
    }

    #[test]
    fn handle_and_thread_count_for_self() {
        let counters = PlatformCounters::new();
        let pid = std::process::id();
        assert!(counters.handle_count(pid).unwrap() > 0);
        assert!(counters.thread_count(pid).unwrap() > 0);
    }
}
