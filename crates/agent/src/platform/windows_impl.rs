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

use super::SystemCounters;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub struct PlatformCounters {
    query: PDH_HQUERY,
    ctxt_counter: PDH_HCOUNTER,
    pgfault_counter: PDH_HCOUNTER,
    /// `None` on machines where the GPU Engine counter set isn't present
    /// (older Windows, or a VM without a GPU scheduler) — GPU sampling is
    /// explicitly best-effort per the Phase 2 plan.
    gpu_counter: Option<PDH_HCOUNTER>,
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

            let mut gpu_counter_raw = PDH_HCOUNTER::default();
            let gpu_path = wide(r"\GPU Engine(*)\Utilization Percentage");
            let gpu_counter =
                if PdhAddEnglishCounterW(query, PCWSTR(gpu_path.as_ptr()), 0, &mut gpu_counter_raw)
                    == 0
                {
                    Some(gpu_counter_raw)
                } else {
                    None
                };

            // Rate counters (`/sec`, GPU utilization) need two collections
            // before they can format a value. Prime the query now so the
            // first real sample reports a rate instead of 0 / None.
            let _ = PdhCollectQueryData(query);

            Self {
                query,
                ctxt_counter,
                pgfault_counter,
                gpu_counter,
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

        self.refresh_thread_cache();

        SystemCounters {
            context_switches_per_sec,
            page_faults_per_sec,
            gpu_pct,
        }
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

            let items = std::slice::from_raw_parts(items_ptr, item_count as usize);
            let sum: f64 = items
                .iter()
                .filter(|it| it.FmtValue.CStatus == 0)
                .map(|it| it.FmtValue.Anonymous.doubleValue)
                .sum();
            Some(sum.min(100.0) as f32)
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
