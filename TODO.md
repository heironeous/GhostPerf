# Windows Performance Monitor — TODO

## Architecture

Three separate binaries, one shared storage format, talking over a local IPC channel.

| Component | Language / Stack | Why |
|---|---|---|
| **Agent** (background collector) | Rust (`windows` crate for perf counters/ETW, `rusqlite` for storage) | Compiles to a small native binary, no runtime/GC, minimal idle CPU & memory footprint — critical since this runs continuously in the background on the machine it's measuring. |
| **Tray** (control icon) | Rust (`tray-icon` + `tao`) | Keeps the always-running footprint entirely in one lightweight language; talks to the Agent over a named pipe and can spawn the UI process. |
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
- [x] **Default control port was unusable on some Windows machines.** Loopback ports 44620–48715 (a 4,096-port block) were held by a hidden system component, not listed in `netsh int ipv4 show excludedportrange`, and the old default `47631` fell inside it. Default moved to `38631` (`crates/agent/src/config.rs`). Note: an existing `%APPDATA%\GhostPerf\agent.toml` overrides the default and must be edited by hand.
- [x] **Agent kept running silently when the control port couldn't be bound.** The bind happened inside the IPC thread, which only logged to stderr and exited, leaving an agent that could never be told to start recording. The socket is now bound on the main thread before startup, and failure exits with code 1 and a message naming the port and config file (`crates/agent/src/main.rs`, `crates/agent/src/ipc.rs`).
- [x] **Time-based flush never fired while recording.** The flush relied on `recv_timeout(flush_interval)`, which only times out after a full interval with no messages, but samples arrive every second. Data reached disk only in 30-sample batches, so a crash could lose up to ~29 s. The flush is now deadline-based and checked after every message (`crates/agent/src/core.rs`). Regression test: `core::tests::time_based_flush_fires_while_samples_keep_arriving`.

### Phase 2 — Intermittent-issue detection
- [ ] High-resolution stall/hang detector (measure scheduler latency via a dedicated timing thread)
- [ ] Threshold-based spike flagging (e.g. CPU > X%, mem > Y%) written as annotated events alongside metrics
- [ ] GPU usage sampling (via PDH/`D3DKMT` counters) — optional, if available
- [ ] Page fault / context-switch counters
- [ ] Handle count / thread count per process (leak detection)
- [ ] Event log correlation (surface relevant Windows Event Log entries during a session window)

### Phase 3 — Tray
- [ ] Rust project scaffold for tray (`tray/`)
- [ ] Tray icon with Start/Stop metric gathering menu items
- [ ] Tray icon reflects current state (idle / recording)
- [ ] "Open UI" menu item — launches UI process (or focuses existing window)
- [ ] Named-pipe client to send `start`/`stop`/`status` to agent
- [ ] Autostart with Windows (optional toggle)
- [ ] Agent process supervision (launch agent if not running; detect crashes)

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
- [ ] Data retention/cleanup (auto-prune old sessions past N days or size cap)
- [ ] Crash resilience: agent restarts cleanly after unexpected shutdown, recovers/closes orphaned session
- [ ] Resource-usage self-check: verify agent's own CPU/mem footprint stays negligible under load
- [ ] Documentation: README with build/run instructions for each component
