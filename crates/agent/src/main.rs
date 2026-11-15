mod config;
mod core;
mod ipc;
mod sampler;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use config::AgentConfig;
use storage::Store;

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn main() -> anyhow::Result<()> {
    let config = AgentConfig::load_or_default(AgentConfig::default_config_path())?;
    println!(
        "ghostperf-agent: db={} control_port={} sample_interval_ms={}",
        config.db_path.display(),
        config.control_port,
        config.sample_interval_ms
    );

    let store = Store::open(&config.db_path)?;
    let closed = store.close_orphaned_sessions(now_millis())?;
    if closed > 0 {
        println!("ghostperf-agent: closed {closed} orphaned session(s) from a prior run");
    }

    let (core_tx, core_rx) = mpsc::channel();
    let recording_flag = Arc::new(AtomicBool::new(false));

    let core_handle = {
        let config = config.clone();
        let recording_flag = recording_flag.clone();
        std::thread::spawn(move || core::run(store, core_rx, config, recording_flag))
    };

    {
        let sample_interval = Duration::from_millis(config.sample_interval_ms);
        let top_n = config.top_n_processes;
        let recording_flag = recording_flag.clone();
        let core_tx = core_tx.clone();
        std::thread::spawn(move || {
            let mut sampler = sampler::Sampler::new(top_n);
            loop {
                std::thread::sleep(sample_interval);
                if !recording_flag.load(Ordering::SeqCst) {
                    continue;
                }
                let sample = sampler.sample(now_millis());
                if core_tx.send(core::CoreMsg::Sample(sample)).is_err() {
                    break; // core thread shut down
                }
            }
        });
    }

    {
        let port = config.control_port;
        let core_tx = core_tx.clone();
        std::thread::spawn(move || {
            if let Err(e) = ipc::serve(port, core_tx) {
                eprintln!("ghostperf-agent: ipc server stopped: {e}");
            }
        });
    }

    // The control thread owns the shutdown path: once `core::run` returns
    // (after a CoreMsg::Shutdown), there's nothing left to keep the process
    // alive for, so exit instead of leaking the sampler/ipc threads.
    let _ = core_handle.join();
    Ok(())
}
