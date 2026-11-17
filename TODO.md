# Windows Performance Monitor — TODO

**Status at a glance:** Phases 1–4 (agent core, intermittent-issue detection, tray, UI) are implemented — see each phase below for exactly what's verified vs. only compile-checked. Phases 1–4 have each had a real-Windows run (see the "Windows run" subsections), though not every path in each is proven yet. Phases 5–8 are planned, not started. Phase 7 (live session view) and Phase 8 (temperatures and thermal throttling) were added 2026-11-17 and are numbered after the overlay only to keep existing phase references stable, not as a priority order. New to this repo? Read `AGENTS.md` first for how development actually works here (environment setup, how Windows-only code gets verified without a Windows machine, repo conventions) — this file covers *what* to build and *what's already true*, `AGENTS.md` covers *how* to work here.

## Architecture

Three separate binaries, one shared storage format, talking over a local IPC channel.

| Component | Language / Stack | Why |
|---|---|---|
| **Agent** (background collector) | Rust (`windows` crate for perf counters/ETW, `rusqlite` for storage) | Compiles to a small native binary, no runtime/GC, minimal idle CPU & memory footprint — critical since this runs continuously in the background on the machine it's measuring. |
| **Tray** (control icon) | Rust (`tray-icon` + `tao`) | Keeps the always-running footprint entirely in one lightweight language; talks to the Agent over the same loopback TCP control channel as any other client (see IPC below) and can spawn the UI process. |
| **UI** (session viewer) | Rust (`egui`, rendered natively via `wgpu`/OpenGL), charts via `egui_plot` | Maximizes performance: no webview, no GC, no runtime startup cost — and keeps the whole stack in one language so SQLite access/types can be shared directly with the agent/tray. |
| **Storage** | SQLite (single file per machine, one `sessions` table + one `metrics` table, WAL mode) | Local, zero-dependency, fast sequential writes in WAL mode, easy ad-hoc querying for the UI, trivial to back up/inspect. |

### IPC

- Tray ↔ Agent: loopback-only TCP socket on an OS-assigned port (agent binds `127.0.0.1:0` at startup and publishes the port in `%APPDATA%\GhostPerf\agent.port`; clients re-read it on every connect, since it changes on each agent start — see `crates/agent_config/src/control_port.rs`), one newline-terminated text command per connection (`start [label]`, `stop`, `status`, `shutdown`). Chosen over a named pipe for implementation simplicity — identical code on Windows/Linux, no extra crate, still same-machine-only.
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

#### Found reviewing a real Windows recording (2026-11-17)
A session recorded from the tray on Windows 11 (20 logical cores) was checked sample by sample: no gaps, no NULLs, every value in range, mem used + available constant. The data is correct, but the review turned up two open issues:

- [x] **Per-process `cpu_pct` uses a different scale from the system-wide `cpu_pct`.** `ProcessSample::cpu_pct` stores `sysinfo`'s `Process::cpu_usage()`, where 100% means one full core, so it can exceed 100%. `MetricSample::cpu_pct` stores `global_cpu_usage()`, where 100% means the whole machine (`crates/agent/src/sampler.rs`). Seen: `Code.exe` at 111.2% while the system total was 24.1%; on 20 cores that's about 5.6% of the machine. Resolved the second way this note suggested: storage keeps the raw per-core value (no schema change, no reinterpretation of already-recorded rows), and Phase 4's UI normalizes to whole-machine scale at display time, dividing by `cpu_per_core_pct.len()` from the *same* sample rather than a session-wide constant (`crates/ui/src/process_agg.rs::normalize_cpu_pct`). CSV/JSON export still emits the raw stored value, since that's for external analysis against the original data, not the UI's display convention.
- [ ] **The sampling loop drifts about 4.5% slow.** The sampler thread does `sleep(sample_interval)` and then `sample()` (`crates/agent/src/main.rs`), so each tick takes the interval plus however long sampling takes (the process scan, PDH collection, and the per-process handle/thread lookups). Seen: median gap 1.045 s (range 1.021–1.362 s), so a 41.0 s session produced 39 samples instead of 41. The drift grows with the number of processes running and with `top_n_processes`. Fix by sleeping until a fixed deadline (`next += interval`, sleeping `next - now`), skipping missed ticks rather than bursting to catch up after a stall. Regression test: assert the sample count over N intervals with an artificially slow `sample()`.

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

Pulled `AgentConfig` out of `agent` into a new `agent_config` crate (`crates/agent_config`) so the tray can read `control_port` from the same `agent.toml` without duplicating the config format or its defaulting rules. (`control_port` has since been removed — see the first-Windows-run fixes below — and the crate now owns the `agent.port` file location/format instead.)

Architecture note: `main.rs` pushes both the 1-second poll and `muda`'s menu-click events into the `tao` event loop through an `EventLoopProxy` (a `TickEvent::{Poll, Menu}` user event), rather than having the loop itself poll on `ControlFlow::WaitUntil`/`Poll`. This is both the idiomatic way tray-icon examples integrate a background timer with a `tao` loop, and a fix for something found during smoke-testing below.

"Open UI" doesn't focus an existing window yet — the UI (Phase 4) doesn't have a real window to focus. It currently just avoids spawning a second copy by checking for a running `ghostperf-ui` process. Once Phase 4 exists, replace that check with an actual focus (`FindWindowW` + `SetForegroundWindow` on Windows).

Smoke-tested end-to-end on Linux (dev environment, headless: `Xvfb` + `dbus-launch`, no window manager or status-notifier host): tray launched the agent, a real session was started/recorded/stopped over the control channel, and killing the agent process out from under the tray triggered a relaunch within one poll tick. All of this required two fixes made during that smoke test:

- **`is_agent_running`/`open_ui`'s process scan matched zombie processes.** A killed child whose parent hasn't `wait()`-ed on it is still enumerated by `sysinfo` (status `Zombie`), so the supervisor saw the dead agent's zombie entry and concluded it was still running, and never relaunched it. Both scans now exclude `ProcessStatus::Zombie` (`crates/tray/src/supervisor.rs`, `crates/tray/src/open_ui.rs`).
- **`ControlFlow::WaitUntil` (and `Poll`) never reliably woke the loop** in this headless container: a tray icon with no window and no window manager/compositor driving the GTK main loop meant `tao`'s timer-based wakeups fired once (at startup) and then stopped. Switching to the `EventLoopProxy` push-based design above fixed it — confirmed back-to-back with `WaitUntil` (ticked once in 4+ seconds) vs. the proxy approach (ticked on schedule every second). Whether `WaitUntil` would have been fine on a real Windows desktop or a full Linux desktop session is untested; the proxy design sidesteps the question either way and is the more idiomatic choice regardless.

Not yet run on real Windows (needs the same first-real-run shakeout the agent got in Phase 1/2): the registry autostart (`crates/tray/src/autostart/windows_impl.rs`) is only compile-checked, via `cargo check --target x86_64-pc-windows-gnu` (this container has no MSVC `lib.exe` to check the `msvc` triple — see the Phase 2 note on this). Tray-icon rendering and click handling on real Windows is also unverified beyond compilation; this container's headless Linux smoke test exercises the supervision/IPC/autostart-file logic but not an actual visible tray icon or a real mouse click on a menu item.

#### Fixes from first Windows run of Phase 3 (2026-11-17)
Built with MSVC (`build.ps1 -Test`, release) on Windows 11 and ran the tray, with its stdout redirected to a log to observe supervision: the tray launched the agent, a session was recorded and stopped over the control channel, and a force-killed agent was relaunched exactly once, on a new port. That run found:

- [x] **The tray respawned the agent every second, forever.** On Windows `sysinfo` reports the process name with its `.exe` suffix (`ghostperf-agent.exe`), and the supervisor compared it against the bare `ghostperf-agent`, so it never saw the running agent. Each tick it spawned a duplicate, which died on `os error 10048` binding the fixed control port (the only thing keeping duplicates from piling up). Same bug in "Open UI"'s already-running check. The Linux smoke test couldn't catch it: Linux process names have no extension. Both now go through `paths::is_process_named`, which strips a case-insensitive `.exe` (`crates/tray/src/paths.rs`; tests in its `tests` module).
- [x] **Fixed control port replaced by an OS-assigned one.** This is the second fixed-port failure (after Phase 1's hidden 44620–48715 reservation), and a fixed port also races on restart with the previous agent's not-yet-released socket. `control_port` is gone from `AgentConfig`. The agent binds `127.0.0.1:0` and writes the port to `agent.port` in the data dir, deleting it on clean shutdown (`crates/agent/src/main.rs`, `crates/agent_config/src/control_port.rs`). The tray re-reads it on every poll/click (`crates/tray/src/main.rs`). Old `agent.toml` files still containing `control_port` load fine because unknown fields are ignored. Regression test: `config_with_removed_control_port_field_still_loads`.
- [x] **Single-instance guard, needed because of the above.** The fixed port's bind failure had been the only thing stopping a second agent, which is harmful rather than just wasteful: on startup it runs `close_orphaned_sessions`, which would close the first agent's in-progress session. The agent now takes an exclusive `File::try_lock` on `agent.lock` before opening the store. The OS releases the lock on any exit, crash included, and a second agent prints the running agent's port and exits 0 (`crates/agent/src/instance.rs`; test `second_acquire_fails_until_first_is_released`). Verified by launching a second agent by hand alongside a running one.

- [x] **Quit left the agent running, and still recording if a session was active.** The handler only set `ControlFlow::Exit`. The agent is spawned with a plain `Command::spawn`, so it outlived the tray with nothing supervising it. Quit now calls `supervisor::shutdown_agent`, which sends `shutdown` and waits up to 15 s for the process to exit (not just for the `OK shutting_down` reply, which comes before the session is closed). It never kills the agent on timeout. A `quitting` flag stops poll ticks queued during that wait from relaunching the agent (`crates/tray/src/main.rs`, `crates/tray/src/supervisor.rs`).
- [x] **`shutdown` ended a recording differently from `stop`.** It skipped Event Log correlation and silently discarded a `stop_session` error. Both paths now go through `core::finish_session` (`crates/agent/src/core.rs`). Regression test: `core::tests::shutdown_while_recording_flushes_and_closes_the_session`. Verified on the real binaries: `shutdown` mid-recording exits in about 0.2 s with code 0; the session is closed with all buffered samples written, `agent.port` is removed, `agent.lock` is released, and the WAL is checkpointed (0 bytes).
- [x] **Closing the tray's console window killed both processes without a clean shutdown.** Both binaries were console-subsystem and the agent inherited the tray's console, so closing that window terminated both on the spot: no flush, the session left open (recovered as orphaned on the next start), and a stale `agent.port`. This happened once during this run. Both are now `#![cfg_attr(windows, windows_subsystem = "windows")]` (PE subsystem 2, verified), so there's no console to close. With no stdout, all output goes through the `log` facade into a new shared `logging` crate (`crates/logging`), which writes `%APPDATA%\GhostPerf\logs\{agent,tray}.log`, one unbuffered write per line. The file is rotated to `.log.1` on startup past 5 MiB. Lines are also copied to stderr, which is discarded when there's no console but is useful on Linux or with redirected stderr. A panic hook logs panics with a backtrace, since a windowless process's default panic output vanishes. `main` now logs a fatal error before `exit(1)` instead of returning it. The agent also now logs session start/stop/shutdown, and the tray logs launches, relaunches, Quit, and each menu command's reply.
  - Verified on Windows: no conhost attached to either process when launched normally. Both logs record startup, session start/stop, a refused duplicate agent, and a crash-relaunch. A forced startup failure (isolated `%APPDATA%` with a directory where `agent.port` goes) logged `ERROR fatal: …` and exited 1. Tests: `logging::tests::*` (line format, append across reopen, rotation).
  - Not verified: the panic hook (needs a deliberate panic), and log output on Linux beyond compiling. On Linux the attribute is a no-op and logs land in `$TMPDIR/GhostPerf/logs`.

Still unverified on Windows: icon rendering/state swap and real menu clicks (left to manual testing), including that clicking Quit runs the shutdown path above (only the agent's half of it was exercised directly). The registry autostart toggle is also unverified.

### Phase 4 — UI
- [x] Rust project scaffold for UI (`ui/`), `egui`/`eframe` app — `crates/ui`, binary `ghostperf-ui`. Renderer is `wgpu` (eframe's default), not `glow`; on Linux this needs a Vulkan or GL adapter (a software one via `mesa-vulkan-drivers`/llvmpipe is enough for dev/smoke-testing, see `AGENTS.md`).
- [x] Read-only(-ish) SQLite access layer (list sessions, fetch metrics/events for a session) — `crates/ui/src/data.rs`, a thin wrapper over `storage::Store`. Not purely read-only: it also owns the rename/delete calls below, since those are real UI-driven writes, not metrics ingestion.
- [x] Session list view (date, duration, label, quick summary stats) — `crates/ui/src/views/session_list.rs`. Stats (avg/peak CPU, peak mem, event count) come from a new `Store::session_summary` SQL aggregate (`crates/storage/src/lib.rs`) rather than loading every sample per row.
- [x] Session detail view: time-series charts (CPU, mem, disk, net) over the session window — `crates/ui/src/views/session_detail.rs`, four `egui_plot` charts with a shared `link_axis`/`link_cursor` group so panning/zooming one moves the others together.
- [x] Zoom/pan on charts; brush-select a time range for detail — zoom/pan is `egui_plot`'s native left-drag-to-pan and right-drag-to-box-zoom on the 4 charts (scroll-wheel zoom is deliberately off on them — see below). Brush-select is a separate thin overview strip beneath the charts with all native interaction disabled, where drag start/end (via `PlotUi::pointer_coordinate` on `drag_started`/`dragged`) sets a selected time range, drawn as two `VLine`s and feeding the per-process table's filter.
- [x] Highlight flagged spikes/stalls on the timeline — each session's `events` are drawn as colored `VLine`s on all 4 charts (`event_color`: stall=orange, cpu_spike=red, mem_spike=purple, `eventlog_*`=gray), named so hovering shows the kind.
- [x] Per-process breakdown view within a session, respecting the brush-selected range — `crates/ui/src/views/session_detail.rs::process_table`, backed by `crates/ui/src/process_agg.rs::aggregate_processes`. Resolves the Phase 1 open item: CPU is normalized to whole-machine scale for display (see that item, above).
- [x] Rename/delete session — inline in the session list (rename-in-place, two-step delete confirm) and in the detail view's header. `Store::rename_session` is new (`crates/storage/src/lib.rs`).
- [x] Export session data (CSV/JSON) for external analysis — `crates/ui/src/export.rs`, to a user-editable path (default under `<data_dir>/exports/`). CSV is one row per sample with the scalar columns flat and `cpu_per_core_pct`/`top_processes` as JSON-text columns (lossless but not spreadsheet-flat); JSON is the session + full metrics + events, straight `serde_json`.
- [x] Basic settings screen (sample interval, retention/cleanup policy, DB location) — `crates/ui/src/views/settings.rs`, edits `AgentConfig` in place and writes it back via a new `AgentConfig::save` (`crates/agent_config/src/lib.rs`). DB location is shown, not editable (changing it wouldn't move the existing file). Takes effect on the agent's next start — the UI doesn't signal a running agent to reload.

Tested so far: full unit-test suite (new tests for `process_agg`'s normalization/aggregation, `format`'s date/duration formatting, `export`'s CSV/JSON output, plus the new `storage`/`agent_config` methods) passes on Linux. The whole workspace, including `ui`, compile-checks clean on `x86_64-pc-windows-gnu`. Smoke-tested end-to-end on headless Linux (Xvfb, with `mesa-vulkan-drivers` for a software Vulkan adapter and `libxkbcommon-x11-0` — `winit` needs both, see `AGENTS.md`): launched `ghostperf-ui` against a real database recorded by the agent/tray, and, via `xdotool` driving real clicks/drags/typing (screenshotted with `import` at each step to confirm), exercised every bullet above — opened a session from the list, saw all 4 charts render real data with a working legend, dragged a brush selection on the overview strip and watched the per-process table's peak CPU change accordingly, renamed a session in place, hit (and canceled) the two-step delete confirm, exported to a real CSV file and checked its contents, and edited + saved a setting, confirming `agent.toml` on disk changed.

One bug the smoke test caught and fixed before landing: scrolling the mouse wheel to reach content below the fold (the process table / export controls, if the window isn't tall enough) could land the cursor over one of the 4 charts, and `egui_plot`'s default scroll-to-pan took over, panning that chart into a nonsensical range instead of scrolling the page. Fixed by disabling scroll-wheel interaction on those 4 charts (`allow_scroll(false)`) — pan/zoom there is left-drag/right-drag only, which doesn't conflict with the page's `ScrollArea`.

Not yet verified: a real Windows run (icon/window rendering via a real GPU rather than llvmpipe, and real mouse/keyboard input) — needs the same first-real-run shakeout Phases 1–3 got. The headless Linux test above also couldn't exercise real keyboard focus for typing into a text field without `xdotool windowfocus` workarounds (no window manager in this container, same limitation noted for the tray in Phase 3) — unlikely to matter on a real desktop, which has normal window-manager-driven focus, but untested.

#### First Windows run of Phase 4 (2026-11-17)
Built with MSVC (`build.ps1 -Test`, release; all 64 tests pass) and run on Windows 11 at 3840×2160, 150% scaling, rendering through wgpu's Vulkan backend on an RTX 3060 Ti. Driven with real Win32 mouse and keyboard input (`SetCursorPos`/`SendInput` from a PowerShell helper, screenshots via `CopyFromScreen`: Windows' equivalent of the xdotool/`import` workflow in `AGENTS.md`), against a database recorded by the real agent, including a 38 s session with an 8 s all-core CPU burst. Every value checked was cross-checked against SQLite.

Verified working on Windows:
- **Session list:** values match the DB (avg/peak CPU, peak mem, event counts).
- **Detail charts:** a `cpu_spike` marker sits at the 100% sample on all 4 charts.
- **Zoom and pan:** right-drag box-zoom is linked across the charts, double-click resets them all, the linked hover cursor works, and the mouse wheel scrolls the page instead of panning charts.
- **Brush-select:** 15.0–20.0 s on the overview strip recomputes the process table to exactly the DB-derived values for that window.
- **Per-process table:** values are normalized to whole-machine scale. A process pegging one core shows 5.0% on 20 cores, matching the DB.
- **Rename:** in the detail header and inline in the list (Enter commits). Keyboard focus and typing work normally, which the headless Linux run couldn't confirm.
- **Delete:** two-step confirm, with Cancel placed where Delete was. A confirmed delete removes the session plus its metrics and events.
- **Export:** CSV and JSON both export complete and correct data (35/35 rows, all 13 columns; JSON has session, metrics and events).
- **Settings:** saving writes `agent.toml`.
- **Health:** no WARN, ERROR or panic in `ui.log`. Idle CPU is 0.3% of one core, since egui repaints only on input.

Found on that run. Fixed the same day and re-verified on Windows with the same input-driving method, except for the tray CPU item:
- [x] **Choosing JSON export overwrote the CSV export.** The CSV/JSON radio only flipped `export_as_json`, and the path kept its default `session-<id>.csv`, so a JSON export was written to `session-<id>.csv`, silently replacing an earlier CSV export. Switching format now swaps a `.csv`/`.json` extension; a user-typed name with any other extension is left alone (`export::path_for_format`, tests `switching_format_*`). Verified: CSV export, switch to JSON (path becomes `session-7.json`), JSON export → both files exist with correct contents.
- [x] **Export result was shown at the top of the page, far from the Export button.** It went in the page's top status line, which was off-screen at the bottom of a scrolled page, and inserting it pushed everything down ~30 px under the cursor. A first fix put it on a line under the export row, but on Windows that line landed below the visible area. It's now in the export row itself, right of the button: "✔ Saved session-7.csv", with the full path or error on hover, truncated to fit (`export_controls`, new `export_status` field). Verified: message visible on each export, no layout shift.
- [x] **The 4 charts' plot areas weren't horizontally aligned.** Each `egui_plot` sized its y-axis to its own labels ("100" vs "20000000"), so the same timestamp was drawn at a different x on each chart (one `cpu_spike` marker at x = 697/725/745/731 px zoomed). All charts and the overview now share `Y_AXIS_WIDTH` via `y_axis_min_width`, with tick labels kept short enough to fit (`format_*`, test `axis_labels_stay_short_enough_for_the_shared_axis_width`). Verified: ticks and the marker at identical x on all 5 plots, full-range and zoomed to 14–22 s.
- [x] **Smaller UI issues:**
  - Y-axis values were raw bytes/s (`20000000`). Now unit-formatted (`20 MB/s`, `300 KB/s`, `22800 MB`, `100%`; decimal units so round ticks stay round, see `format::format_rate`), hover labels too (`Disk read: 22 MB/s` / `16.0 s`; event markers show name and time only).
  - The CPU chart's y-axis labels disappeared after box-zoom. The cause wasn't axis width: `egui_plot`'s default powers-of-10 grid only labels marks > 20 pt apart, so at ~15–88% in 140 pt the 10s were too dense and the 100s out of view. Replaced with a 1-2-10-20-… grid (`session_detail::nice_grid_steps`, tests in that module), levels at most 5× apart, so some level is always labelled. Verified: 20/40/60/80% shown zoomed, and every 20% unzoomed (previously only 0/100).
  - The overview strip zoomed along with the charts (same link group). It's now unlinked, so it always shows the whole session for brush-selecting. Verified: brushed 15–20 s on it while the charts were zoomed to 14–22 s.
  - In the session list, entering delete-confirm widened the auto-sized Actions column and reflowed the whole table (this entry originally said the row shifted relative to its headers; the screenshot crop only showed one row). The column now starts at 300 pt, wide enough for the confirm state (`session_list.rs`). Verified: columns at identical x in both states.
- [x] **Flaky tray test, found while landing the above:** `ipc_client::tests::is_recording_parses_{true,false}_from_a_status_reply` failed 2 runs in 8 on Windows. The test stub server did a single `read()`, which could return before the client's `\n` (sent as a separate write by `writeln!`) arrived. It then closed with unread data, which on Windows sends an RST, so the client's read of the reply failed. The stub now reads the whole line, as the real agent does (`crates/tray/src/ipc_client.rs`). 30 of 30 runs pass after the fix. Test-only; the real client/agent exchange wasn't affected.
- [x] **Memory shown as a percentage of total.** The memory chart now plots % in use (same 0–100 scale as CPU), with absolute figures in its hover text (`Mem used: 35% (22848 MB of 65329 MB)`). The session list's Peak Mem shows both (`22848 MB (35%)`). Total = used + available per sample, now defined once as `MetricSample::mem_total_mb`/`mem_used_pct` (`crates/storage`) and used by the agent's `mem_spike` check too, so spike markers sit where the plotted curve crosses the threshold. `Store::session_summary` gained `peak_mem_used_pct`, its SQL mirroring that definition (test checks they agree, and that a zero total yields `None` rather than dividing by zero). Tests: `mem_used_pct_is_used_over_used_plus_available`, `memory_hover_shows_percent_and_absolute_megabytes`.
- [x] **Export bar always visible.** The export controls (path, CSV/JSON, Export, result) moved out of the scrolling page into a bottom `egui::Panel` drawn by `app.rs` for the detail view, like a status bar. Previously they sat below the process table and needed scrolling past every chart to reach.
  - Unit-tested, built and launched on Windows (UI responding, no log errors), but **not yet checked on screen**: during that run the whole desktop captured as black (not just GhostPerf; display asleep or session not visible), so the UI wasn't driven blind.
- [ ] **The tray costs ~1.4% of one core while idle** (measured over 10 s; ~0.07% of this 20-core machine), most likely from `supervisor::is_agent_running` refreshing every process on the system once a second. Not a Phase 4 issue; relevant to Phase 5's resource-usage self-check.

Not verified on Windows: "Open UI" from the tray menu (tray menu clicks are still manual-only, see Phase 3), and behavior at 100%/other DPI scalings or on a second monitor.

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

### Phase 7 — Live session view
Watch a session while it records, instead of only after it stops.

Where things stand: the UI loads a session once (`DetailView::load`), so a recording session (`ended_at` NULL, shown as "recording…" in the list) is a frozen snapshot. The DB also lags the live data. The agent buffers samples and stall/spike events in memory and writes them every `flush_batch_size` (30) samples or `flush_interval_ms` (10 s), whichever comes first, and Event Log entries are only correlated when the session stops.

- [ ] **Live data path from agent to UI.** Options:
  - (a) The UI polls the DB for rows newer than the last one it has. Simple, read-only, and fine under WAL, but up to ~10 s behind.
  - (b) A new control-channel command (e.g. `tail <since_ts>`) returning the agent's buffered, unflushed samples and events as one JSON line, which the UI merges with what's already on disk.
  - (c) Lower the flush interval while a session records. Simplest, but more disk writes, which the batching was there to avoid.

  Leaning (a) + (b): ~1 s latency with no extra disk I/O. (b) is also what Phase 6's overlay needs ("reuse the agent's existing sampler … rather than sampling a second time"), so design it for both.
- [ ] **Auto-refresh the detail view while `ended_at` is NULL**, about every `sample_interval_ms` (`ctx.request_repaint_after`), appending new points rather than reloading the whole session. Stop refreshing, and pick up the Event Log entries, once the session ends.
- [ ] **Follow mode:** the charts show a trailing window (e.g. the last 5 min) that scrolls with new data. Panning or zooming pauses following, and a "Jump to live" button resumes it. The overview strip keeps growing with the session, and brush-select keeps working on recorded data.
- [ ] **Live session list:** duration and summary stats of the recording session update in place, with a clear "● recording" marker.
- [ ] **Open straight into the live view from the tray:** "Open UI" while recording opens the current session (e.g. a `ghostperf-ui --session <id>` / `--live` argument). Optionally, Start/Stop from the UI too.
- [ ] **Keep it cheap for long sessions:** incremental appends instead of re-collecting every series each frame; throttle the per-process re-aggregation (`process_agg::aggregate_processes`) to about once a second.

### Phase 8 — Temperatures and thermal throttling
Record temperatures where the machine exposes them (CPU, GPU, …), to spot heat-related problems.

Probed on the Windows 11 dev machine (i7-12700KF, RTX 3060 Ti), non-elevated, 2026-11-17. The agent runs non-elevated (the tray spawns it), so admin-only sources don't count unless Phase 5 moves it into a service.

| Source | Reads | Admin? | On the dev machine |
|---|---|---|---|
| NVIDIA NVML (`nvml.dll`, ships with the driver) | GPU temp | No | Present; GPU at 30.8 °C via `nvidia-smi` |
| D3DKMT `KMTQAITYPE_ADAPTERPERFDATA` → `D3DKMT_ADAPTER_PERFDATA.Temperature` (deci-°C), vendor-neutral, what Task Manager shows, WDDM 2.9+ | GPU temp | No (believed) | Not yet probed; needs the scratch-crate treatment from `AGENTS.md` |
| PDH `\Thermal Zone Information(*)\Temperature` (Kelvin) | ACPI thermal zones | No | One zone, `\_tz.tz00`, at 27.9 °C: a board/chassis zone, **not** the CPU die |
| WMI `root\WMI` `MSAcpi_ThermalZoneTemperature` (the source `sysinfo`'s `Components` uses on Windows) | Same ACPI zones | Yes | Access denied; don't rely on `sysinfo` for temps here |
| CPU package/core temp (Intel `IA32_PACKAGE_THERM_STATUS` MSR / AMD SMN) | Real CPU die temp | Kernel driver | No user-mode API exists |
| LibreHardwareMonitor WMI (`root\LibreHardwareMonitor`) or HWiNFO shared memory | Everything those tools see | Only if the user runs them | Neither present |
| PDH `\Processor Information(_Total)\% Processor Performance`, `% of Maximum Frequency`, `Processor Frequency`, `Performance Limit Flags` | CPU throttling, not temperature | No | All present (idle: 105.8%, 59.0%, 3600 MHz, flags 0) |

- [ ] **GPU temperature:**
  - NVML (`nvmlDeviceGetTemperature`), loaded dynamically, since `nvml.dll` is absent on non-NVIDIA machines.
  - Try D3DKMT as a vendor-neutral path, which could cover AMD/Intel GPUs and iGPUs without vendor SDKs. If it doesn't pan out, those need ADLX (AMD) / IGCL (Intel).
  - Store `None` when unavailable, like `gpu_pct`.
- [ ] **CPU throttling as the practical CPU signal:**
  - Sample the four `Processor Information` counters above through the existing PDH query (`platform::windows_impl`).
  - Under load, a drop in `% Processor Performance` or non-zero `Performance Limit Flags` shows thermal or power limiting even where no CPU temperature is readable, and that's the symptom of a heat problem.
  - The meaning of the individual flag bits still needs confirming.
- [ ] **CPU die temperature, only through a safe source:**
  - Don't ship WinRing0, the driver older monitoring tools used for MSR access. It's on Microsoft's vulnerable-driver blocklist, and Defender flags it.
  - Options: optional pass-through from LibreHardwareMonitor or HWiNFO when the user runs one, or later a signed driver (e.g. PawnIO, which LibreHardwareMonitor moved to), weighed against Phase 5's installer.
- [ ] **ACPI thermal zones:** record them via PDH (no admin) but label them as board zones, not CPU temperature.
- [ ] **Storage:** sensor counts vary by machine (several GPUs, several zones), so store temperatures as a JSON column of `{sensor, celsius}` (like `cpu_per_core_json`), with throttling as plain columns. Add a schema migration like Phase 2's, and a Linux stand-in (`/sys/class/thermal`) per the platform-split convention in `AGENTS.md`.
- [ ] **Detection:**
  - `thermal_throttle` events (sustained performance drop under load, or limit flags set).
  - `temp_spike` events above configurable thresholds (e.g. `gpu_temp_threshold_c`).
  - Both drawn as event markers like stalls and spikes.
- [ ] **UI:**
  - A temperature chart (°C, one line per sensor) and a CPU-performance % chart, aligned with the others (`Y_AXIS_WIDTH`).
  - Peak temperatures in the session list and the CSV/JSON export.
  - Clear "not available on this machine" states instead of empty charts.
