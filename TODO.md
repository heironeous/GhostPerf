# Windows Performance Monitor — TODO

**Status at a glance:** Phases 1–3 (agent core, intermittent-issue detection, tray) are implemented — see each phase below for exactly what's verified vs. only compile-checked. Phase 4 (UI) is next and hasn't been started. Phases 5–6 are planned, not started. New to this repo? Read `AGENTS.md` first for how development actually works here (environment setup, how Windows-only code gets verified without a Windows machine, repo conventions) — this file covers *what* to build and *what's already true*, `AGENTS.md` covers *how* to work here.

## Architecture

Three separate binaries, one shared storage format, talking over a local IPC channel.

| Component | Language / Stack | Why |
|---|---|---|
| **Agent** (background collector) | Rust (`windows` crate for perf counters/ETW, `rusqlite` for storage) | Compiles to a small native binary, no runtime/GC, minimal idle CPU & memory footprint — critical since this runs continuously in the background on the machine it's measuring. |
| **Tray** (control icon) | Rust (`tray-icon` + `tao`) | Keeps the always-running footprint entirely in one lightweight language; talks to the Agent over the same loopback TCP control channel as any other client (see IPC below) and can spawn the UI process. |
| **UI** (session viewer) | Rust (`egui`, rendered natively via `wgpu`/OpenGL), charts via `egui_plot` | Maximizes performance: no webview, no GC, no runtime startup cost — and keeps the whole stack in one language so SQLite access/types can be shared directly with the agent/tray. |
| **Storage** | SQLite (single file per machine, one `sessions` table + one `metrics` table, WAL mode) | Local, zero-dependency, fast sequential writes in WAL mode, easy ad-hoc querying for the UI, trivial to back up/inspect. |

### IPC

- Tray ↔ Agent: loopback-only TCP socket (`127.0.0.1:38631`, configurable via `control_port`), one newline-terminated text command per connection (`start [label]`, `stop`, `status`, `shutdown`). Chosen over a named pipe for implementation simplicity — identical code on Windows/Linux, no extra crate, still same-machine-only.
- Tray → UI: spawn UI process directly (UI reads SQLite DB directly, read-only — no need to proxy through the agent).

### Data model (SQLite)

- `sessions(id, started_at, ended_at, label, host_info)`
- `metrics(session_id, ts, cpu_pct, cpu_per_core_json, mem_used_mb, mem_available_mb, disk_read_bps, disk_write_bps, net_rx_bps, net_tx_bps, top_processes_json)`
- Index on `(session_id, ts)` for fast per-session range queries.
- WAL mode + batched inserts (buffer N samples, flush periodically) to keep write overhead low.

---

## Features

### Phase 1 — Agent core
- [x] Rust project scaffold for agent (`agent/`) — part of a Cargo workspace with `crates/storage`, `crates/tray`, `crates/ui`
- [x] SQLite schema + migrations (`sessions`, `metrics` tables) — `crates/storage`
- [x] Batched/buffered writer so disk I/O doesn't happen on every sample — size- and time-based flush (`crates/agent/src/core.rs`); time-based flush fixed, see below
- [x] Sampling loop: CPU % (total + per-core)
- [x] Sampling loop: memory (used, available)
- [x] Sampling loop: disk I/O (read/write bytes/sec, summed across processes) — note: queue length not available via `sysinfo`; skipped for now
- [x] Sampling loop: network I/O (rx/tx bytes/sec, summed across adapters)
- [x] Per-process top-N snapshot (CPU/mem hogs) each sample
- [x] Configurable sample interval (default 1s, via config file)
- [x] Session lifecycle: `start_session()` / `stop_session()` writes session row with timestamps
- [x] Graceful shutdown on stop/shutdown command (flush buffer, close session) — also recovers orphaned sessions left open by an unclean shutdown on next startup
- [x] Control server for `start`/`stop`/`status`/`shutdown` commands — TCP loopback, see IPC note above (not a named pipe)
- [x] Config file (sample interval, flush thresholds, DB path, control port, retention policy) — TOML, auto-created with defaults on first run

Smoke-tested end-to-end on Linux (dev environment): started a session over the control channel, confirmed real CPU/mem/disk/net/per-process samples were written to SQLite, stopped the session, verified clean shutdown.

#### Fixes from first Windows run (2026-11-16)
Smoke-tested on Windows 11: release build, agent started, session started over the control channel, real samples confirmed in SQLite. That run found these issues, now fixed:

- [x] **`build.ps1` passed `--release` to cargo one character at a time.** The `if` expression returned a one-element array, which PowerShell unrolled to a plain string, and splatting a string passes it character by character (`error: unexpected argument '-' found`). Fixed by wrapping the result in `@()`.
- [x] **Default control port was unusable on some Windows machines.** Loopback ports 44620–48715 (a 4,096-port block) were held by a hidden system component, not listed in `netsh int ipv4 show excludedportrange`, and the old default `47631` fell inside it. Default moved to `38631` (at the time, `crates/agent/src/config.rs`; that module moved to `crates/agent_config/src/lib.rs` in Phase 3). Note: an existing `%APPDATA%\GhostPerf\agent.toml` overrides the default and must be edited by hand.
- [x] **Agent kept running silently when the control port couldn't be bound.** The bind happened inside the IPC thread, which only logged to stderr and exited, leaving an agent that could never be told to start recording. The socket is now bound on the main thread before startup, and failure exits with code 1 and a message naming the port and config file (`crates/agent/src/main.rs`, `crates/agent/src/ipc.rs`).
- [x] **Time-based flush never fired while recording.** The flush relied on `recv_timeout(flush_interval)`, which only times out after a full interval with no messages, but samples arrive every second. Data reached disk only in 30-sample batches, so a crash could lose up to ~29 s. The flush is now deadline-based and checked after every message (`crates/agent/src/core.rs`). Regression test: `core::tests::time_based_flush_fires_while_samples_keep_arriving`.

### Phase 2 — Intermittent-issue detection
- [x] `events` table (`crates/storage`): `(id, session_id, ts, kind, value, detail)`, indexed on `(session_id, ts)` — annotations live alongside `metrics`, not mixed into it
- [x] High-resolution stall/hang detector — dedicated thread (`crates/agent/src/stall.rs`) sleeps for `stall_check_interval_ms` (default 15ms) and records a `stall` event when it wakes up more than `stall_threshold_ms` (default 50ms) late
- [x] Threshold-based spike flagging — `cpu_spike_threshold_pct` / `mem_spike_threshold_pct` (default 90%) checked against every sample in `core::run`, written as `cpu_spike`/`mem_spike` events
- [x] GPU usage sampling via PDH (`\GPU Engine(*)\Utilization Percentage`, summed across engine instances and clamped to 100%) — optional: `None` if the counter set isn't present (older Windows, GPU-less VM)
- [x] Page fault / context-switch counters — system-wide rates (`MetricSample::page_faults_per_sec`/`context_switches_per_sec`) via PDH (`\Memory\Page Faults/sec`, `\System\Context Switches/sec`)
- [x] Handle count / thread count per process — `ProcessSample::handle_count`/`thread_count`, filled in only for the top-N processes (after truncation) to avoid a syscall per process per tick; thread counts come from one `CreateToolhelp32Snapshot` per tick, handle counts from `GetProcessHandleCount`
- [x] Event log correlation — on session stop, queries the System + Application channels via the WinEvt API for Level 1-3 (Critical/Error/Warning) entries within `[started_at, ended_at]` and stores them as `eventlog_critical`/`eventlog_error`/`eventlog_warning` events (`crates/agent/src/eventlog/`)

All of the above work on Linux too, via a second backend (`crates/agent/src/platform/linux_impl.rs`, `crates/agent/src/eventlog/generic_impl.rs`) that exists purely so the agent builds and is smoke-testable in this (non-Windows) dev container — not a shipping target. On Linux, GPU is always `None` and handle_count is approximated with the open-fd count (a different metric, same plumbing); event log correlation is a no-op (nothing to correlate against).

Tested so far: full unit-test suite (incl. new `events` CRUD, schema migration of a pre-Phase-2 DB, stall detector, threshold flagging, and pure-function tests for the Windows Event Log XML/XPath-timestamp parsing) passes on Linux; a real recording session was smoke-tested end-to-end on Linux confirming non-zero context-switch/page-fault rates and populated handle/thread counts land in SQLite. The Windows-only code (`platform::windows_impl`, `eventlog::windows_impl`) is unverified by an actual Windows run — only compile-checked (`cargo check --target x86_64-pc-windows-gnu`, since no MSVC `lib.exe` is available in this container to check the `msvc` triple). Needs the same first-real-run shakeout Phase 1 got.

#### Fixes from first Windows run of Phase 2 (2026-11-16)
Built (MSVC, debug + release via `build.ps1 -Test`) and smoke-tested on Windows 11: recorded sessions over the control channel in an isolated `%APPDATA%`, confirmed PDH context-switch/page-fault/GPU rates, per-process handle/thread counts, and stall events all land in SQLite; clean `shutdown` with exit code 0. That run found these issues, now fixed:

- [x] **Agent refused to start with an existing Phase 1 `agent.toml`.** The new Phase 2 fields had no serde defaults, so a config written by the older agent failed with `missing field 'stall_check_interval_ms'`. `AgentConfig` now has `#[serde(default)]`, so missing fields fall back to defaults (at the time, `crates/agent/src/config.rs`; now `crates/agent_config/src/lib.rs`, see Phase 3). Regression test: `config_from_older_version_missing_new_fields_still_loads` (now in `agent_config`'s own test module).
- [x] **Event log correlation silently returned nothing.** `EvtRender` emits single-quoted XML attributes (`<TimeCreated SystemTime='…'/>`), but `extract_attr` only matched double quotes (as did the Linux-written test fixtures), so every event was dropped at the timestamp parse. Now accepts either quote style, and requires a leading space so `Name=` can't match inside `EventSourceName=` (`crates/agent/src/eventlog/format.rs`). Regression tests use verbatim Windows output. Verified against `Get-WinEvent` for the same window (7/7 events, matching levels/providers/IDs) via the ignored machine-dependent test `eventlog::windows_impl::tests::correlate_reads_real_event_log` (run with `-- --ignored`, window via `GP_EVT_START_MS`/`GP_EVT_END_MS`/`GP_EVT_EXPECTED`).
- [x] **First sample after agent start had `context_switches_per_sec = 0`, `page_faults_per_sec = 0`, `gpu_pct = None`.** PDH rate counters need two collections; the query is now primed once in `PlatformCounters::new()` (`crates/agent/src/platform/windows_impl.rs`).
- [ ] **First sample of a session after an idle gap reports PDH rates averaged over the whole gap**, since the query is only collected while recording. Minor; fix by collecting on session start (or each tick) if it matters.

Observed: `thread::sleep` wakeup jitter on Windows 11 maxed at ~0.7 ms over 220 wakeups, so the 50 ms default `stall_threshold_ms` has plenty of headroom.

### Phase 3 — Tray
- [x] Rust project scaffold for tray (`tray/`) — `tray-icon` + `tao`
- [x] Tray icon with Start/Stop metric gathering menu items, plus Open UI / Start with Windows (checkbox) / Quit
- [x] Tray icon reflects current state (idle / recording) — polls `status` every second and swaps a runtime-generated grey/red circle icon (`crates/tray/src/icons.rs`, no image assets)
- [x] "Open UI" menu item — launches `ghostperf-ui` if it isn't already running; see note below on focusing an existing window
- [x] Control-channel client to send `start`/`stop`/`status` to the agent — TCP, not a named pipe (see IPC note at the top of this doc; that bullet's wording predates the Phase 1 IPC decision) (`crates/tray/src/ipc_client.rs`)
- [x] Autostart with Windows (optional toggle) — `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` (`crates/tray/src/autostart/`)
- [x] Agent process supervision (launch agent if not running; detect crashes) — scans for a `ghostperf-agent` process by name once a second and relaunches if it's gone (`crates/tray/src/supervisor.rs`)

Pulled `AgentConfig` out of `agent` into a new `agent_config` crate (`crates/agent_config`) so the tray can read `control_port` from the same `agent.toml` without duplicating the config format or its defaulting rules.

Architecture note: `main.rs` pushes both the 1-second poll and `muda`'s menu-click events into the `tao` event loop through an `EventLoopProxy` (a `TickEvent::{Poll, Menu}` user event), rather than having the loop itself poll on `ControlFlow::WaitUntil`/`Poll`. This is both the idiomatic way tray-icon examples integrate a background timer with a `tao` loop, and a fix for something found during smoke-testing below.

"Open UI" doesn't focus an existing window yet — the UI (Phase 4) doesn't have a real window to focus. It currently just avoids spawning a second copy by checking for a running `ghostperf-ui` process. Once Phase 4 exists, replace that check with an actual focus (`FindWindowW` + `SetForegroundWindow` on Windows).

Smoke-tested end-to-end on Linux (dev environment, headless: `Xvfb` + `dbus-launch`, no window manager or status-notifier host): tray launched the agent, a real session was started/recorded/stopped over the control channel, and killing the agent process out from under the tray triggered a relaunch within one poll tick. All of this required two fixes made during that smoke test:

- **`is_agent_running`/`open_ui`'s process scan matched zombie processes.** A killed child whose parent hasn't `wait()`-ed on it is still enumerated by `sysinfo` (status `Zombie`), so the supervisor saw the dead agent's zombie entry and concluded it was still running, and never relaunched it. Both scans now exclude `ProcessStatus::Zombie` (`crates/tray/src/supervisor.rs`, `crates/tray/src/open_ui.rs`).
- **`ControlFlow::WaitUntil` (and `Poll`) never reliably woke the loop** in this headless container: a tray icon with no window and no window manager/compositor driving the GTK main loop meant `tao`'s timer-based wakeups fired once (at startup) and then stopped. Switching to the `EventLoopProxy` push-based design above fixed it — confirmed back-to-back with `WaitUntil` (ticked once in 4+ seconds) vs. the proxy approach (ticked on schedule every second). Whether `WaitUntil` would have been fine on a real Windows desktop or a full Linux desktop session is untested; the proxy design sidesteps the question either way and is the more idiomatic choice regardless.

Not yet run on real Windows (needs the same first-real-run shakeout the agent got in Phase 1/2): the registry autostart (`crates/tray/src/autostart/windows_impl.rs`) is only compile-checked, via `cargo check --target x86_64-pc-windows-gnu` (this container has no MSVC `lib.exe` to check the `msvc` triple — see the Phase 2 note on this). Tray-icon rendering and click handling on real Windows is also unverified beyond compilation; this container's headless Linux smoke test exercises the supervision/IPC/autostart-file logic but not an actual visible tray icon or a real mouse click on a menu item.

### Phase 4 — UI
- [ ] Rust project scaffold for UI (`ui/`), `egui`/`eframe` app
- [ ] Read-only SQLite access layer (list sessions, fetch metrics for a session)
- [ ] Session list view (date, duration, label, quick summary stats)
- [ ] Session detail view: time-series charts (CPU, mem, disk, net) over the session window
- [ ] Zoom/pan on charts; brush-select a time range for detail
- [ ] Highlight flagged spikes/stalls on the timeline
- [ ] Per-process breakdown view within a session
- [ ] Rename/delete/export session
- [ ] Export session data (CSV/JSON) for external analysis
- [ ] Basic settings screen (sample interval, retention/cleanup policy, DB location)

### Phase 5 — Packaging & polish
- [ ] Single installer (MSI/NSIS) that installs agent + tray + UI and registers autostart
- [ ] Portable version: a zip of the three binaries that runs with no installer, no registry entries, and no autostart — config/DB default to a path next to the executable instead of `%APPDATA%` (likely needs a `--portable` flag or auto-detection so the installed and portable builds can share the same binaries)
- [ ] Data retention/cleanup (auto-prune old sessions past N days or size cap)
- [ ] Crash resilience: agent restarts cleanly after unexpected shutdown, recovers/closes orphaned session
- [ ] Resource-usage self-check: verify agent's own CPU/mem footprint stays negligible under load
- [ ] Documentation: README with build/run instructions for each component

### Phase 6 — Gaming overlay
- [ ] Rust project scaffold for overlay (`overlay/`) — new binary, launched/toggled from the tray like the UI
- [ ] Click-through, borderless, always-on-top window so it never steals input or focus from the game (Windows: `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE`, topmost, no taskbar entry)
- [ ] Renders on top of exclusive-fullscreen and borderless-window games alike; handle multi-monitor placement and DPI scaling
- [ ] Live CPU %, RAM, and GPU usage readout — reuse the agent's existing sampler/PDH counters (Phase 1/2) rather than sampling a second time
- [ ] FPS counter — the hard part: there's no OS-level "current game FPS" counter, so this needs either a present-call hook into the game's graphics API (DXGI/D3D11/D3D12 swap chain `Present`, eventually D3D9/Vulkan/OpenGL) or reusing an existing capture mechanism (e.g. the approach behind PresentMon). Needs its own research/spike before committing to an implementation — this is a different engineering problem from the rest of the app (injection/hooking vs. passive counters) and may deserve its own design doc
- [ ] Overlay on/off toggle via tray menu and a global hotkey
- [ ] Configurable overlay position, opacity, and refresh rate; keep its own CPU/GPU footprint negligible since it runs continuously *during* the gaming session it's trying not to disturb
