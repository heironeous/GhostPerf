use std::time::Instant;

use storage::{MetricSample, ProcessSample};
use sysinfo::{Networks, ProcessRefreshKind, System};

/// Owns the sysinfo handles and the previous-tick state needed to turn
/// cumulative counters into per-second rates.
pub struct Sampler {
    sys: System,
    networks: Networks,
    top_n: usize,
    last_tick: Instant,
}

impl Sampler {
    pub fn new(top_n: usize) -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();
        let networks = Networks::new_with_refreshed_list();
        Self {
            sys,
            networks,
            top_n,
            last_tick: Instant::now(),
        }
    }

    /// Take one sample. Call this on a fixed interval; the very first call
    /// after construction may report 0 for rate-based fields since there is
    /// no prior tick to diff against.
    pub fn sample(&mut self, ts_millis: i64) -> MetricSample {
        let elapsed_secs = self.last_tick.elapsed().as_secs_f64().max(0.001);
        self.last_tick = Instant::now();

        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.sys
            .refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, ProcessRefreshKind::everything());
        self.networks.refresh();

        let cpu_pct = self.sys.global_cpu_usage();
        let cpu_per_core_pct: Vec<f32> = self.sys.cpus().iter().map(|c| c.cpu_usage()).collect();
        let mem_used_mb = self.sys.used_memory() as f32 / (1024.0 * 1024.0);
        let mem_available_mb = self.sys.available_memory() as f32 / (1024.0 * 1024.0);

        let (mut disk_read, mut disk_write): (u64, u64) = (0, 0);
        let mut process_samples: Vec<ProcessSample> = Vec::with_capacity(self.sys.processes().len());
        for (pid, proc_) in self.sys.processes() {
            let disk_usage = proc_.disk_usage();
            disk_read += disk_usage.read_bytes;
            disk_write += disk_usage.written_bytes;
            process_samples.push(ProcessSample {
                pid: pid.as_u32(),
                name: proc_.name().to_string_lossy().into_owned(),
                cpu_pct: proc_.cpu_usage(),
                mem_mb: proc_.memory() as f32 / (1024.0 * 1024.0),
            });
        }
        process_samples.sort_by(|a, b| b.cpu_pct.partial_cmp(&a.cpu_pct).unwrap_or(std::cmp::Ordering::Equal));
        process_samples.truncate(self.top_n);

        let (mut net_rx, mut net_tx) = (0u64, 0u64);
        for (_iface, data) in self.networks.iter() {
            net_rx += data.received();
            net_tx += data.transmitted();
        }

        MetricSample {
            ts: ts_millis,
            cpu_pct,
            cpu_per_core_pct,
            mem_used_mb,
            mem_available_mb,
            disk_read_bps: disk_read as f64 / elapsed_secs,
            disk_write_bps: disk_write as f64 / elapsed_secs,
            net_rx_bps: net_rx as f64 / elapsed_secs,
            net_tx_bps: net_tx as f64 / elapsed_secs,
            top_processes: process_samples,
        }
    }
}
