// Windowless on Windows: no console window to close by accident (which
// used to kill the agent with no clean shutdown). All output goes to
// `agent.log` through the `logging` crate instead.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod core;
mod eventlog;
mod instance;
mod ipc;
mod live;
mod platform;
mod sampler;
mod sampling_loop;
mod stall;
mod telemetry;
mod thermal_watch;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use agent_config::AgentConfig;
use anyhow::Context;
use storage::Store;

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn main() {
    let log_file = logging::init(&agent_config::log_dir(), "agent");
    log::info!(
        "starting (pid {}, log {})",
        std::process::id(),
        log_file.map_or_else(|| "unavailable".into(), |p| p.display().to_string())
    );
    // Returning the error from `main` would print it to a stderr that
    // doesn't exist in a windowless process, so log it before exiting.
    if let Err(e) = run() {
        log::error!("fatal: {e:#}");
        std::process::exit(1);
    }
    log::info!("exited cleanly");
}

fn run() -> anyhow::Result<()> {
    let config_path = AgentConfig::default_config_path();
    let config = AgentConfig::load_or_default(&config_path)?;
    let db_path = config.resolved_db_path();

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
        log::info!(
            "another agent is already running (control port {port}, per {}); exiting",
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
    log::info!(
        "db={} control_port={} (written to {}) sample_interval_ms={}",
        db_path.display(),
        control_port,
        port_file.display(),
        config.sample_interval_ms
    );

    let store = Store::open(&db_path)?;
    let closed = store.close_orphaned_sessions()?;
    if closed > 0 {
        log::warn!("closed {closed} orphaned session(s) left open by a prior unclean exit");
    }
    core::prune_by_retention(&store, &config);

    let (core_tx, core_rx) = mpsc::channel();
    let recording_flag = Arc::new(AtomicBool::new(false));
    // Shared with the sampler and stall threads so `reload` reaches them.
    let live = live::LiveSettings::new(&config);
    // Shared between the sampler thread (publishes) and the `live` control
    // command (reads) — see telemetry.rs. Phase 6's overlay.
    let telemetry = Arc::new(telemetry::Telemetry::new());

    let core_handle = {
        let config = config.clone();
        let recording_flag = recording_flag.clone();
        let live = live.clone();
        std::thread::spawn(move || core::run(store, core_rx, config, recording_flag, live))
    };

    {
        let recording_flag = recording_flag.clone();
        let core_tx = core_tx.clone();
        let live = live.clone();
        let telemetry = telemetry.clone();
        std::thread::spawn(move || {
            let mut sampler = sampler::Sampler::new(live.top_n_processes());
            let interval = || live.sample_interval();
            // Active for either reason a sample is needed: a session is
            // recording, or the overlay polled `live` recently enough that
            // its lease hasn't expired (see telemetry.rs). Neither reason
            // knows about the other; the sampler thread doesn't need to.
            // While neither holds, the overlay's cached reading is dropped
            // (see `Telemetry::sampler_idle`): this closure is what the loop
            // checks every few ms while idle, so it's where idleness shows.
            let active = || {
                let active =
                    recording_flag.load(Ordering::SeqCst) || telemetry.wants_samples(now_millis());
                if !active {
                    telemetry.sampler_idle();
                }
                active
            };
            sampling_loop::run(interval, active, |step| match step {
                sampling_loop::Step::Prime => {
                    sampler.prime();
                    true
                }
                sampling_loop::Step::Sample => {
                    sampler.set_top_n(live.top_n_processes());
                    let sample = sampler.sample(now_millis());
                    telemetry.publish(&sample);
                    // Only persisted if a session is actually recording
                    // (core::run's CoreMsg::Sample handler checks that) —
                    // publishing above happens either way, so the overlay
                    // gets live numbers with no session active at all.
                    // Err: the core thread has shut down, so stop sampling.
                    core_tx.send(core::CoreMsg::Sample(sample)).is_ok()
                }
            });
        });
    }

    {
        let core_tx = core_tx.clone();
        let telemetry = telemetry.clone();
        std::thread::spawn(move || ipc::serve(listener, core_tx, telemetry));
    }

    {
        let core_tx = core_tx.clone();
        let recording_flag = recording_flag.clone();
        std::thread::spawn(move || stall::run(core_tx, recording_flag, || live.stall_timing()));
    }

    // The control thread owns the shutdown path: once `core::run` returns
    // (after a CoreMsg::Shutdown), there's nothing left to keep the process
    // alive for, so exit instead of leaking the sampler/ipc threads.
    let _ = core_handle.join();
    // Lets clients see "not running" immediately instead of timing out
    // against a dead port. Safe to delete unconditionally: we still hold the
    // instance lock, so no other agent can have written it since.
    if let Err(e) = agent_config::remove_port_file(&port_file) {
        log::warn!("failed to remove {}: {e}", port_file.display());
    }
    Ok(())
}
