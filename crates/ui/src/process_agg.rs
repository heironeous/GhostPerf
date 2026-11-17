//! Per-process CPU normalization and aggregation.
//!
//! `ProcessSample::cpu_pct` (see `storage`) is per-core: `sysinfo`'s
//! `Process::cpu_usage()`, where 100% means one full core saturated, so a
//! single-threaded process pegging a core on a 20-core machine reads
//! ~100% while the session-wide `MetricSample::cpu_pct` for the same
//! instant reads ~5%. Charting or comparing the two as-is is misleading
//! (see TODO.md Phase 1's open item on this). Rather than changing what's
//! stored (that would retroactively change the meaning of already-recorded
//! rows), the UI normalizes to the same whole-machine scale at display
//! time, dividing by the logical core count recorded in the *same* sample
//! (`MetricSample::cpu_per_core_pct.len()`) rather than a session-wide
//! constant, since nothing stops that count from changing between samples
//! (CPU hot-add, or a VM resize) even though it never has in practice yet.

use storage::MetricSample;

/// `proc_cpu_pct` (per-core scale) converted to whole-machine scale. Returns
/// the input unchanged if `logical_cores` is `0` (e.g. an empty/corrupt
/// sample) rather than dividing by zero.
pub fn normalize_cpu_pct(proc_cpu_pct: f32, logical_cores: usize) -> f32 {
    if logical_cores == 0 {
        proc_cpu_pct
    } else {
        proc_cpu_pct / logical_cores as f32
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessAgg {
    pub pid: u32,
    pub name: String,
    pub sample_count: usize,
    /// Whole-machine-normalized average/peak CPU over the samples this
    /// process appeared in (not the whole window — a short-lived process
    /// isn't penalized for samples before it existed).
    pub avg_cpu_pct: f32,
    pub peak_cpu_pct: f32,
    pub peak_mem_mb: f32,
    pub peak_handle_count: Option<u32>,
    pub peak_thread_count: Option<u32>,
}

/// Aggregates the top-process snapshots across `samples` into one row per
/// `(pid, name)`, restricted to samples with `ts` in `[range.0, range.1]`
/// when `range` is `Some` (the session detail view's brush-selected window).
/// A pid can be reused by an unrelated process over a long session, so pid
/// alone isn't a stable identity — pairing it with the name is enough to
/// avoid merging two different processes without needing a real start-time
/// based identity.
pub fn aggregate_processes(samples: &[MetricSample], range: Option<(i64, i64)>) -> Vec<ProcessAgg> {
    let mut by_key: Vec<(u32, String, ProcessAgg)> = Vec::new();

    for sample in samples {
        if let Some((start, end)) = range {
            if sample.ts < start || sample.ts > end {
                continue;
            }
        }
        let cores = sample.cpu_per_core_pct.len();
        for p in &sample.top_processes {
            let normalized = normalize_cpu_pct(p.cpu_pct, cores);
            match by_key
                .iter_mut()
                .find(|(pid, name, _)| *pid == p.pid && name == &p.name)
            {
                Some((_, _, agg)) => {
                    agg.sample_count += 1;
                    agg.avg_cpu_pct += normalized;
                    agg.peak_cpu_pct = agg.peak_cpu_pct.max(normalized);
                    agg.peak_mem_mb = agg.peak_mem_mb.max(p.mem_mb);
                    agg.peak_handle_count = max_option(agg.peak_handle_count, p.handle_count);
                    agg.peak_thread_count = max_option(agg.peak_thread_count, p.thread_count);
                }
                None => {
                    by_key.push((
                        p.pid,
                        p.name.clone(),
                        ProcessAgg {
                            pid: p.pid,
                            name: p.name.clone(),
                            sample_count: 1,
                            avg_cpu_pct: normalized,
                            peak_cpu_pct: normalized,
                            peak_mem_mb: p.mem_mb,
                            peak_handle_count: p.handle_count,
                            peak_thread_count: p.thread_count,
                        },
                    ));
                }
            }
        }
    }

    by_key
        .into_iter()
        .map(|(_, _, mut agg)| {
            agg.avg_cpu_pct /= agg.sample_count as f32;
            agg
        })
        .collect()
}

fn max_option(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use storage::ProcessSample;

    fn proc(pid: u32, name: &str, cpu_pct: f32, mem_mb: f32) -> ProcessSample {
        ProcessSample {
            pid,
            name: name.to_string(),
            cpu_pct,
            mem_mb,
            handle_count: None,
            thread_count: None,
        }
    }

    fn sample(ts: i64, cores: usize, procs: Vec<ProcessSample>) -> MetricSample {
        MetricSample {
            ts,
            cpu_pct: 0.0,
            cpu_per_core_pct: vec![0.0; cores],
            mem_used_mb: 0.0,
            mem_available_mb: 0.0,
            disk_read_bps: 0.0,
            disk_write_bps: 0.0,
            net_rx_bps: 0.0,
            net_tx_bps: 0.0,
            context_switches_per_sec: 0.0,
            page_faults_per_sec: 0.0,
            gpu_pct: None,
            top_processes: procs,
        }
    }

    #[test]
    fn normalize_divides_by_core_count() {
        // 111.2% on one core of a 20-core machine is ~5.6% of the whole
        // machine (the real case from TODO.md's Phase 1 note: Code.exe at
        // 111.2% while the system total read 24.1%).
        assert!((normalize_cpu_pct(111.2, 20) - 5.56).abs() < 0.001);
        assert_eq!(normalize_cpu_pct(50.0, 1), 50.0);
    }

    #[test]
    fn normalize_with_zero_cores_passes_through() {
        assert_eq!(normalize_cpu_pct(42.0, 0), 42.0);
    }

    #[test]
    fn aggregates_avg_and_peak_across_samples() {
        let samples = vec![
            sample(1, 4, vec![proc(10, "code.exe", 40.0, 100.0)]),
            sample(2, 4, vec![proc(10, "code.exe", 80.0, 200.0)]),
        ];
        let agg = aggregate_processes(&samples, None);
        assert_eq!(agg.len(), 1);
        let p = &agg[0];
        assert_eq!(p.sample_count, 2);
        // normalized: 10.0 and 20.0 -> avg 15.0, peak 20.0
        assert_eq!(p.avg_cpu_pct, 15.0);
        assert_eq!(p.peak_cpu_pct, 20.0);
        assert_eq!(p.peak_mem_mb, 200.0);
    }

    #[test]
    fn same_pid_different_name_is_a_different_process() {
        let samples = vec![
            sample(1, 2, vec![proc(10, "old.exe", 10.0, 50.0)]),
            sample(2, 2, vec![proc(10, "new.exe", 20.0, 60.0)]),
        ];
        let agg = aggregate_processes(&samples, None);
        assert_eq!(agg.len(), 2);
    }

    #[test]
    fn range_filters_to_selected_window() {
        let samples = vec![
            sample(1, 1, vec![proc(10, "a.exe", 10.0, 1.0)]),
            sample(100, 1, vec![proc(10, "a.exe", 90.0, 1.0)]),
        ];
        let agg = aggregate_processes(&samples, Some((0, 50)));
        assert_eq!(agg.len(), 1);
        assert_eq!(agg[0].sample_count, 1);
        assert_eq!(agg[0].peak_cpu_pct, 10.0);
    }

    #[test]
    fn short_lived_process_is_not_penalized_for_samples_before_it_existed() {
        let samples = vec![
            sample(1, 1, vec![]),
            sample(2, 1, vec![proc(10, "late.exe", 50.0, 1.0)]),
        ];
        let agg = aggregate_processes(&samples, None);
        assert_eq!(agg.len(), 1);
        assert_eq!(agg[0].sample_count, 1);
        assert_eq!(agg[0].avg_cpu_pct, 50.0);
    }
}
