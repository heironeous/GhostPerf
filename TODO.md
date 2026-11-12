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

- Tray ↔ Agent: local named pipe (`\\.\pipe\perfmon-agent`), simple text/JSON commands (`start`, `stop`, `status`).
- Tray → UI: spawn UI process directly (UI reads SQLite DB directly, read-only — no need to proxy through the agent).

### Data model (SQLite)

- `sessions(id, started_at, ended_at, label, host_info)`
- `metrics(session_id, ts, cpu_pct, mem_used_mb, mem_committed_mb, disk_read_bps, disk_write_bps, net_rx_bps, net_tx_bps, top_processes_json, ...)`
- Index on `(session_id, ts)` for fast per-session range queries.
- WAL mode + batched inserts (buffer N samples, flush periodically) to keep write overhead low.

---

## Features

### Phase 1 — Agent core
- [ ] Rust project scaffold for agent (`agent/`)
- [ ] SQLite schema + migrations (`sessions`, `metrics` tables)
- [ ] Batched/buffered writer so disk I/O doesn't happen on every sample
- [ ] Sampling loop: CPU % (total + per-core)
- [ ] Sampling loop: memory (working set, committed, available)
- [ ] Sampling loop: disk I/O (read/write bytes/sec, queue length)
- [ ] Sampling loop: network I/O (rx/tx bytes/sec per adapter)
- [ ] Per-process top-N snapshot (CPU/mem hogs) each sample
- [ ] Configurable sample interval (default e.g. 1s, adjustable)
- [ ] Session lifecycle: `start_session()` / `stop_session()` writes session row with timestamps
- [ ] Graceful shutdown on stop command (flush buffer, close session)
- [ ] Named-pipe server for control commands (`start`, `stop`, `status`)
- [ ] Config file (sample interval, DB path, retention policy)

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
