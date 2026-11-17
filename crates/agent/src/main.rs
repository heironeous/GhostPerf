mod core;
mod eventlog;
mod instance;
mod ipc;
mod platform;
mod sampler;
mod stall;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent_config::AgentConfig;
use anyhow::Context;
use storage::Store;

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn main() -> anyhow::Result<()> {
    let config_path = AgentConfig::default_config_path();
    let config = AgentConfig::load_or_default(&config_path)?;

    // Must come before opening the store: a second agent would otherwise
    // close the first one's in-progress session as "orphaned" below.
    let lock_path = agent_config::data_dir().join("agent.lock");
    let Some(_instance_lock) = instance::acquire(&lock_path)
        .with_context(|| format!("failed to open instance lock {}", lock_path.display()))?
    else {
        let port_file = agent_config::port_file_path();
        let port = agent_config::read_port_file(&port_file)
            .map(|p| p.to_string())
            .unwrap_or_else(|| "unknown".into());
        println!(
            "ghostperf-agent: another agent is already running (control port {port}, per {}); exiting",
            port_file.display()
        );
        return Ok(());
    };

    // Bind the control socket before anything else and fail hard if it's
    // unavailable: without it the agent can't be started or stopped, and
    // previously it would keep running silently in that useless state.
    let listener = ipc::bind().context("failed to bind a loopback control port")?;
    let control_port = listener.local_addr()?.port();
    let port_file = agent_config::port_file_path();
    agent_config::write_port_file(&port_file, control_port).with_context(|| {
        format!(
            "failed to write control port to {} — clients couldn't find this agent",
            port_file.display()
        )
    })?;
    println!(
        "ghostperf-agent: db={} control_port={} (written to {}) sample_interval_ms={}",
        config.db_path.display(),
        control_port,
        port_file.display(),
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
        let core_tx = core_tx.clone();
        std::thread::spawn(move || ipc::serve(listener, core_tx));
    }

    {
        let core_tx = core_tx.clone();
        let recording_flag = recording_flag.clone();
        let check_interval = Duration::from_millis(config.stall_check_interval_ms);
        let threshold = Duration::from_millis(config.stall_threshold_ms);
        std::thread::spawn(move || stall::run(core_tx, recording_flag, check_interval, threshold));
    }

    // The control thread owns the shutdown path: once `core::run` returns
    // (after a CoreMsg::Shutdown), there's nothing left to keep the process
    // alive for, so exit instead of leaking the sampler/ipc threads.
    let _ = core_handle.join();
    // Lets clients see "not running" immediately instead of timing out
    // against a dead port. Safe to delete unconditionally: we still hold the
    // instance lock, so no other agent can have written it since.
    if let Err(e) = agent_config::remove_port_file(&port_file) {
        eprintln!(
            "ghostperf-agent: failed to remove {}: {e}",
            port_file.display()
        );
    }
    Ok(())
}
