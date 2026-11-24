//! GPU temperatures through D3DKMT, the kernel graphics interface Task
//! Manager reads its own GPU temperature from. Vendor-neutral (one code
//! path for NVIDIA, AMD and Intel, as far as each driver reports it), and
//! needs no admin rights.
//!
//! Probed on the dev machine (RTX 3060 Ti, non-elevated, 2026-11-23): 32.1
//! °C, matching NVIDIA's own NVML (32 °C), at a few µs per query. That made
//! NVML, the original plan in TODO.md, unnecessary: it only covers NVIDIA,
//! needs `nvml.dll` loaded dynamically, and took 13 ms just to initialize.

use std::ffi::c_void;

use storage::{SensorKind, TempReading};
use windows::Wdk::Graphics::Direct3D::{
    D3DKMTCloseAdapter, D3DKMTOpenAdapterFromLuid, D3DKMTQueryAdapterInfo, D3DKMT_ADAPTERADDRESS,
    D3DKMT_ADAPTER_PERFDATA, D3DKMT_CLOSEADAPTER, D3DKMT_OPENADAPTERFROMLUID,
    D3DKMT_QUERYADAPTERINFO, KMTQAITYPE_ADAPTERADDRESS, KMTQAITYPE_ADAPTERPERFDATA,
    KMTQUERYADAPTERINFOTYPE,
};
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
};

use super::thermal;

struct Gpu {
    luid: LUID,
    sensor: String,
}

pub struct GpuTemps {
    gpus: Vec<Gpu>,
}

impl GpuTemps {
    /// Finds the GPUs once, at agent start. A GPU added later (an eGPU
    /// plugged in) isn't picked up until the agent restarts.
    pub fn new() -> Self {
        let found = enumerate().unwrap_or_else(|e| {
            log::warn!("GPU temperatures unavailable: couldn't list adapters ({e})");
            Vec::new()
        });
        let names = thermal::unique_names(found.iter().map(|(_, name)| name.clone()).collect());
        let gpus: Vec<Gpu> = found
            .into_iter()
            .zip(names)
            .map(|((luid, _), sensor)| Gpu { luid, sensor })
            .collect();
        log::info!(
            "GPU temperature sensors: {}",
            if gpus.is_empty() {
                "none".to_string()
            } else {
                gpus.iter()
                    .map(|g| g.sensor.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
        Self { gpus }
    }

    pub fn read(&self) -> Vec<TempReading> {
        self.gpus
            .iter()
            .filter_map(|gpu| {
                let perf: D3DKMT_ADAPTER_PERFDATA =
                    with_adapter(gpu.luid, |h| query(h, KMTQAITYPE_ADAPTERPERFDATA))??;
                // 0 is the driver saying it doesn't report a temperature
                // (Intel iGPUs, older drivers), not 0 °C.
                if perf.Temperature == 0 {
                    return None;
                }
                Some(TempReading {
                    sensor: gpu.sensor.clone(),
                    kind: SensorKind::Gpu,
                    celsius: thermal::plausible(thermal::deci_celsius(perf.Temperature))?,
                })
            })
            .collect()
    }
}

/// Real GPUs as `(LUID, adapter name)`, one per physical device.
///
/// DXGI lists the same GPU more than once when a virtual display driver
/// renders through it: on the dev machine, Parsec's virtual display made
/// the RTX 3060 Ti appear twice, under two LUIDs, with identical names and
/// readings. Only the real one has a PCI address (the other's bus is
/// `0xFFFFFFFF`), so adapters without one are skipped and the rest are
/// deduplicated by address. Two identical GPUs have different addresses, so
/// they stay two sensors.
fn enumerate() -> windows::core::Result<Vec<(LUID, String)>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut seen_addresses = Vec::new();
    let mut out = Vec::new();
    let mut index = 0;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(index) } {
        index += 1;
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let Some(address) = with_adapter(desc.AdapterLuid, |h| {
            query::<D3DKMT_ADAPTERADDRESS>(h, KMTQAITYPE_ADAPTERADDRESS)
        })
        .flatten() else {
            continue;
        };
        let key = (
            address.BusNumber,
            address.DeviceNumber,
            address.FunctionNumber,
        );
        if address.BusNumber == u32::MAX || seen_addresses.contains(&key) {
            continue;
        }
        seen_addresses.push(key);
        let name = String::from_utf16_lossy(&desc.Description)
            .trim_end_matches('\0')
            .trim()
            .to_string();
        out.push((desc.AdapterLuid, name));
    }
    Ok(out)
}

/// Opens the adapter, runs `f` on its handle, and closes it again. Opened
/// per call rather than held: a handle goes stale when the driver restarts
/// (an update, or a GPU reset after a hang), and opening costs a few µs.
fn with_adapter<T>(luid: LUID, f: impl FnOnce(u32) -> T) -> Option<T> {
    let mut open = D3DKMT_OPENADAPTERFROMLUID {
        AdapterLuid: luid,
        hAdapter: 0,
    };
    if unsafe { D3DKMTOpenAdapterFromLuid(&mut open) }.is_err() {
        return None;
    }
    let result = f(open.hAdapter);
    let _ = unsafe {
        D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER {
            hAdapter: open.hAdapter,
        })
    };
    Some(result)
}

/// `T` must be the struct D3DKMT documents for `kind`.
fn query<T: Default>(adapter: u32, kind: KMTQUERYADAPTERINFOTYPE) -> Option<T> {
    let mut out = T::default();
    let mut info = D3DKMT_QUERYADAPTERINFO {
        hAdapter: adapter,
        Type: kind,
        pPrivateDriverData: &mut out as *mut T as *mut c_void,
        PrivateDriverDataSize: std::mem::size_of::<T>() as u32,
    };
    unsafe { D3DKMTQueryAdapterInfo(&mut info) }
        .is_ok()
        .then_some(out)
}
