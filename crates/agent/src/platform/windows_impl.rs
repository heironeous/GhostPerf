//! Windows counters via PDH (Performance Data Helper) perf counters and a
//! couple of narrowly-scoped Win32 calls. This is the shipping backend;
//! smoke-tested on Windows 11 (2026-11-16): context-switch/page-fault/GPU
//! rates and per-process handle/thread counts all land in SQLite.

use std::collections::HashMap;

use windows::core::PCWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhGetFormattedCounterValue, PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_COUNTERVALUE_ITEM_W,
    PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
};
use windows::Win32::System::Threading::{
    GetProcessHandleCount, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

use storage::{CpuPerf, SensorKind, TempReading};

use super::thermal;
use super::windows_gpu::GpuTemps;
use super::SystemCounters;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Adds a counter that may not exist on every machine; `None` if PDH
/// refuses it, rather than failing the whole query.
///
/// # Safety
/// `query` must be an open PDH query.
unsafe fn add_optional_counter(query: PDH_HQUERY, path: &str) -> Option<PDH_HCOUNTER> {
    let path = wide(path);
    let mut counter = PDH_HCOUNTER::default();
    (PdhAddEnglishCounterW(query, PCWSTR(path.as_ptr()), 0, &mut counter) == 0).then_some(counter)
}

pub struct PlatformCounters {
    query: PDH_HQUERY,
    ctxt_counter: PDH_HCOUNTER,
    pgfault_counter: PDH_HCOUNTER,
    /// `None` on machines where the GPU Engine counter set isn't present
    /// (older Windows, or a VM without a GPU scheduler) — GPU sampling is
    /// explicitly best-effort per the Phase 2 plan.
    gpu_counter: Option<PDH_HCOUNTER>,
    /// `None` if the PhysicalDisk counter set is missing or disabled
    /// (`diskperf -n`, or a damaged perf counter registry); best-effort
    /// like the GPU counter.
    disk_queue_counter: Option<PDH_HCOUNTER>,
    /// ACPI thermal zones (every instance), and the divisor that turns the
    /// counter into kelvins: 10 for `High Precision Temperature` (tenths of a
    /// kelvin), 1 for the older whole-kelvin `Temperature`.
    thermal_zone_counter: Option<(PDH_HCOUNTER, f64)>,
    /// `Processor Information(_Total)`: `Actual Frequency`, `% Processor
    /// Performance`, `% Performance Limit`, `Performance Limit Flags`.
    cpu_perf_counters: Option<[PDH_HCOUNTER; 4]>,
    gpu_temps: GpuTemps,
    /// Refreshed once per `sample_system` call via a single toolhelp
    /// snapshot, so per-process `thread_count` lookups are free instead of
    /// costing a syscall per process.
    thread_cache: HashMap<u32, u32>,
}

impl PlatformCounters {
    pub fn new() -> Self {
        unsafe {
            let mut query = PDH_HQUERY::default();
            let _ = PdhOpenQueryW(PCWSTR::null(), 0, &mut query);

            let mut ctxt_counter = PDH_HCOUNTER::default();
            let ctxt_path = wide(r"\System\Context Switches/sec");
            let _ = PdhAddEnglishCounterW(query, PCWSTR(ctxt_path.as_ptr()), 0, &mut ctxt_counter);

            let mut pgfault_counter = PDH_HCOUNTER::default();
            let pgfault_path = wide(r"\Memory\Page Faults/sec");
            let _ = PdhAddEnglishCounterW(
                query,
                PCWSTR(pgfault_path.as_ptr()),
                0,
                &mut pgfault_counter,
            );

            let gpu_counter = add_optional_counter(query, r"\GPU Engine(*)\Utilization Percentage");
            // "Avg." rather than "Current Disk Queue Length": the current
            // one is an instantaneous snapshot taken once a second, which
            // misses a queue that builds and drains between two samples.
            // The average covers the whole interval since the last
            // collection, which is what a 1 s sample should describe.
            // `_Total` sums the queues of all physical disks.
            let disk_queue_counter =
                add_optional_counter(query, r"\PhysicalDisk(_Total)\Avg. Disk Queue Length");

            // Only the whole-kelvin `Temperature` exists on older Windows.
            let thermal_zone_counter = add_optional_counter(
                query,
                r"\Thermal Zone Information(*)\High Precision Temperature",
            )
            .map(|c| (c, 10.0))
            .or_else(|| {
                add_optional_counter(query, r"\Thermal Zone Information(*)\Temperature")
                    .map(|c| (c, 1.0))
            });
            // All or nothing: the four come from one counter set, and
            // `CpuPerf` is only meaningful with all of them.
            let cpu_perf_counters = [
                r"\Processor Information(_Total)\Actual Frequency",
                r"\Processor Information(_Total)\% Processor Performance",
                r"\Processor Information(_Total)\% Performance Limit",
                r"\Processor Information(_Total)\Performance Limit Flags",
            ]
            .map(|path| add_optional_counter(query, path));
            let cpu_perf_counters = match cpu_perf_counters {
                [Some(a), Some(b), Some(c), Some(d)] => Some([a, b, c, d]),
                _ => None,
            };

            // Rate and average counters (`/sec`, GPU utilization, average
            // disk queue length) need two collections
            // before they can format a value. Prime the query now so the
            // first real sample reports a rate instead of 0 / None.
            let _ = PdhCollectQueryData(query);

            Self {
                query,
                ctxt_counter,
                pgfault_counter,
                gpu_counter,
                disk_queue_counter,
                thermal_zone_counter,
                cpu_perf_counters,
                gpu_temps: GpuTemps::new(),
                thread_cache: HashMap::new(),
            }
        }
    }

    pub fn sample_system(&mut self, _elapsed_secs: f64) -> SystemCounters {
        unsafe {
            let _ = PdhCollectQueryData(self.query);
        }

        let context_switches_per_sec = self.read_double(self.ctxt_counter).unwrap_or(0.0);
        let page_faults_per_sec = self.read_double(self.pgfault_counter).unwrap_or(0.0);
        let gpu_pct = self.gpu_counter.and_then(|c| self.read_gpu_array(c));
        let disk_queue_len = self
            .disk_queue_counter
            .and_then(|c| self.read_double(c))
            .map(|v| v as f32);

        let mut temps = self.gpu_temps.read();
        temps.extend(self.read_thermal_zones());
        let cpu_perf = self.read_cpu_perf();

        self.refresh_thread_cache();

        SystemCounters {
            context_switches_per_sec,
            page_faults_per_sec,
            gpu_pct,
            disk_queue_len,
            temps,
            cpu_perf,
        }
    }

    /// Every ACPI thermal zone Windows exposes, as a `Board` sensor. Often a
    /// fixed placeholder rather than a real sensor: on the dev machine (MSI
    /// Z690) the one zone reads exactly 301.0 K (27.85 °C) whatever the load.
    /// There's no telling from here, so it's recorded as reported and the UI
    /// points out a zone that never changes.
    fn read_thermal_zones(&self) -> Vec<TempReading> {
        let Some((counter, divisor)) = self.thermal_zone_counter else {
            return Vec::new();
        };
        self.read_array(counter)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(instance, value)| {
                Some(TempReading {
                    sensor: thermal::acpi_sensor_name(&instance),
                    kind: SensorKind::Board,
                    celsius: thermal::plausible(thermal::kelvin_to_celsius(value / divisor))?,
                })
            })
            .collect()
    }

    fn read_cpu_perf(&self) -> Option<CpuPerf> {
        let [freq, perf, limit, flags] = self.cpu_perf_counters?;
        Some(CpuPerf {
            freq_mhz: self.read_double(freq)? as f32,
            perf_pct: self.read_double(perf)? as f32,
            limit_pct: self.read_double(limit)? as f32,
            limit_flags: self.read_double(flags)? as u32,
        })
    }

    fn read_double(&self, counter: PDH_HCOUNTER) -> Option<f64> {
        unsafe {
            let mut value = PDH_FMT_COUNTERVALUE::default();
            let r = PdhGetFormattedCounterValue(counter, PDH_FMT_DOUBLE, None, &mut value);
            (r == 0).then(|| value.Anonymous.doubleValue)
        }
    }

    /// GPU Engine exposes one instance per engine (3D, Copy, Video Decode,
    /// ...), each already a 0-100% utilization. There's no single "GPU
    /// usage" counter, so this sums engines and clamps — an approximation,
    /// not an exact figure.
    fn read_gpu_array(&self, counter: PDH_HCOUNTER) -> Option<f32> {
        let sum: f64 = self.read_array(counter)?.iter().map(|(_, v)| v).sum();
        Some(sum.min(100.0) as f32)
    }

    /// `(instance name, value)` for every instance of a wildcard counter
    /// that has a valid value this collection.
    fn read_array(&self, counter: PDH_HCOUNTER) -> Option<Vec<(String, f64)>> {
        unsafe {
            let mut buffer_size: u32 = 0;
            let mut item_count: u32 = 0;
            let r = PdhGetFormattedCounterArrayW(
                counter,
                PDH_FMT_DOUBLE,
                &mut buffer_size,
                &mut item_count,
                None,
            );
            if r != PDH_MORE_DATA || buffer_size == 0 {
                return None;
            }

            let mut buf: Vec<u8> = vec![0u8; buffer_size as usize];
            let items_ptr = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            let r = PdhGetFormattedCounterArrayW(
                counter,
                PDH_FMT_DOUBLE,
                &mut buffer_size,
                &mut item_count,
                Some(items_ptr),
            );
            if r != 0 {
                return None;
            }

            // The names point into `buf`, so they're copied out before it's
            // dropped.
            let items = std::slice::from_raw_parts(items_ptr, item_count as usize);
            Some(
                items
                    .iter()
                    .filter(|it| it.FmtValue.CStatus == 0)
                    .map(|it| {
                        (
                            it.szName.to_string().unwrap_or_default(),
                            it.FmtValue.Anonymous.doubleValue,
                        )
                    })
                    .collect(),
            )
        }
    }

    fn refresh_thread_cache(&mut self) {
        self.thread_cache.clear();
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return;
            };
            let mut entry = PROCESSENTRY32W::default();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            if Process32FirstW(snap, &mut entry).is_ok() {
                loop {
                    self.thread_cache
                        .insert(entry.th32ProcessID, entry.cntThreads);
                    if Process32NextW(snap, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);
        }
    }

    pub fn thread_count(&self, pid: u32) -> Option<u32> {
        self.thread_cache.get(&pid).copied()
    }

    /// Requires `PROCESS_QUERY_LIMITED_INFORMATION`, which doesn't need
    /// elevation — unlike full `PROCESS_QUERY_INFORMATION`.
    pub fn handle_count(&self, pid: u32) -> Option<u32> {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut count: u32 = 0;
            let r = GetProcessHandleCount(handle, &mut count);
            let _ = CloseHandle(handle);
            r.ok().map(|_| count)
        }
    }
}

impl Drop for PlatformCounters {
    fn drop(&mut self) {
        unsafe {
            let _ = PdhCloseQuery(self.query);
        }
    }
}
