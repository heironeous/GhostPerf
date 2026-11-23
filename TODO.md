# Windows Performance Monitor — TODO

**Status at a glance:** Phases 1–6 (agent core, intermittent-issue detection, tray, UI, packaging & polish, gaming overlay) are implemented. Their full history — what's verified vs. only compile-checked, the "Windows run" fix-up sections, every item checked off — has moved to [`planning/DONE-TODO.md`](planning/DONE-TODO.md). Three items from those phases are still open and tracked below instead: code signing and the CI workflow's first run on GitHub (Phase 5), and the FPS counter (Phase 6). Phases 7–10 are planned, not started. Phase 7 (live session view) and Phase 8 (temperatures and thermal throttling) were added 2026-11-17 and are numbered after the overlay only to keep existing phase references stable, not as a priority order. Phase 9 (full settings coverage) was added 2026-11-19, and Phase 10 (overlay history graphs) on 2026-11-23. New to this repo? Read `AGENTS.md` first for how development actually works here (environment setup, how Windows-only code gets verified without a Windows machine, repo conventions) — this file covers *what's left to build*, [`planning/DONE-TODO.md`](planning/DONE-TODO.md) covers what's already done, `AGENTS.md` covers *how* to work here.

## Architecture

Four separate binaries, one shared storage format, talking over a local IPC channel.

| Component | Language / Stack | Why |
|---|---|---|
| **Agent** (background collector) | Rust (`windows` crate for perf counters/ETW, `rusqlite` for storage) | Compiles to a small native binary, no runtime/GC, minimal idle CPU & memory footprint — critical since this runs continuously in the background on the machine it's measuring. |
| **Tray** (control icon) | Rust (`tray-icon` + `tao`) | Keeps the always-running footprint entirely in one lightweight language; talks to the Agent over the same loopback TCP control channel as any other client (see IPC below) and can spawn the UI/overlay processes. |
| **UI** (session viewer) | Rust (`egui`, rendered natively via `wgpu`/OpenGL), charts via `egui_plot` | Maximizes performance: no webview, no GC, no runtime startup cost — and keeps the whole stack in one language so SQLite access/types can be shared directly with the agent/tray. |
| **Overlay** (in-game readout, Phase 6) | Rust (`egui`/`eframe`, same stack as the UI) | A click-through, always-on-top, borderless window; one rendering stack for the whole app rather than a second one (e.g. hand-rolled GDI/Direct2D) just for this. |
| **Storage** | SQLite (single file per machine, one `sessions` table + one `metrics` table, WAL mode) | Local, zero-dependency, fast sequential writes in WAL mode, easy ad-hoc querying for the UI, trivial to back up/inspect. |

### IPC

- Tray ↔ Agent: loopback-only TCP socket on an OS-assigned port (agent binds `127.0.0.1:0` at startup and publishes the port in `%APPDATA%\GhostPerf\agent.port`; clients re-read it on every connect, since it changes on each agent start — see `crates/agent_config/src/control_port.rs`), one newline-terminated text command per connection (`start [label]`, `stop`, `status`, `reload`, `live`, `shutdown`). Chosen over a named pipe for implementation simplicity — identical code on Windows/Linux, no extra crate, still same-machine-only. The client is `agent_config::send_command`, shared by the tray, the UI, and the overlay.
- Tray → UI: spawn UI process directly (UI reads SQLite DB directly, read-only — no need to proxy through the agent).
- Tray → Overlay: spawn/kill the process directly (Phase 6) — see that phase for why this is a true toggle rather than launch-or-focus like the UI.
- UI → Agent: only `reload`, sent after the settings screen saves `agent.toml`.
- Overlay → Agent: only `live`, polled on `OverlayConfig::refresh_interval_ms` — see Phase 6. Doesn't touch session state or `AgentConfig` at all, so it's answered directly without round-tripping through the agent's core thread (`crates/agent/src/telemetry.rs`).

### Data model (SQLite)

- `sessions(id, started_at, ended_at, label, host_info)`
- `metrics(session_id, ts, cpu_pct, cpu_per_core_json, mem_used_mb, mem_available_mb, disk_read_bps, disk_write_bps, net_rx_bps, net_tx_bps, top_processes_json)`, plus columns added later in place by `Store::migrate`: `context_switches_per_sec`, `page_faults_per_sec`, `gpu_pct` (Phase 2), `disk_queue_len` (2026-11-18)
- `events(id, session_id, ts, kind, value, detail)` (Phase 2)
- Index on `(session_id, ts)` for fast per-session range queries.
- WAL mode + batched inserts (buffer N samples, flush periodically) to keep write overhead low.

---

## Features

### Phases 1–6 — done
See [`planning/DONE-TODO.md`](planning/DONE-TODO.md) for the full record of these phases (what was built, how it was tested, and the dated "Fixes from first Windows run" write-ups). Three items from these phases are still open and tracked here:

#### Open items from Phase 5 — Packaging & polish
- [ ] **Code signing**, left open by choice: the installer and exes are unsigned, so a downloaded copy gets a SmartScreen warning. Needs a code-signing certificate, then `signtool` in `build.ps1 -Package` for each exe and for the installer.
- [ ] **First run of the CI build/release workflow on GitHub** (`.github/workflows/release.yml`, added 2026-11-23; what it does and what was verified locally is in [`planning/DONE-TODO.md`](planning/DONE-TODO.md), Phase 5, "CI builds and GitHub releases"). Confirm that a push to `main` builds, tags and publishes a release with both packages, that a docs-only push is skipped, and that a pull request builds without releasing. It has never run on GitHub, so none of these is confirmed yet: the runner image's toolchain/NSIS, the tag push, `gh release create` (including `--notes-file` combined with `--generate-notes`), or the `paths-ignore` skip.

#### Open item from Phase 6 — Gaming overlay
- [ ] FPS counter — not implemented; see the research notes just below. Left open by choice, the same way Phase 5 left code signing open: the task itself flagged this as needing its own spike, and that held up.

**FPS counter — research notes, not an implementation.** There's no OS-level "current FPS" counter for an arbitrary game, so this needs to observe present calls somehow. Two real approaches, both researched here against what's already in this codebase rather than from scratch:

- **Present-call hooking** (what RTSS/MSI Afterburner/Discord's overlay do): detour the game's own graphics calls — `IDXGISwapChain::Present`/`Present1` for D3D11/12, `IDirect3DDevice9::Present`/`EndScene` for D3D9, `wglSwapBuffers`/`glXSwapBuffers` for OpenGL, `vkQueuePresent` for Vulkan — which means injecting a DLL into the game process (`CreateRemoteThread`+`LoadLibrary`, or a hook via `SetWindowsHookEx`) and maintaining a separate detour implementation per graphics API, each with its own calling convention and version quirks. This is a fundamentally different engineering problem from everything else in this app so far: every other Windows-specific thing here (PDH counters, Event Log correlation, the registry autostart, the window-icon `HWND` tweaks) reads or writes through a documented API on a process GhostPerf owns. This would be the first thing that reaches *into* another process. It's also the category of technique anti-cheat systems (EAC, BattlEye) are built to flag, including false positives on tools with no malicious intent at all — a real risk for anything meant to run while playing an online game.
- **ETW-based passive observation** (what PresentMon — and everything built on it — actually uses instead, specifically to avoid the above): Windows' own graphics stack emits ETW (Event Tracing for Windows) events on every present, system-wide, from kernel-mode components (`Microsoft-Windows-DxgKrnl`, `Microsoft-Windows-DXGI`) that run regardless of which process is presenting. Consuming these needs no injection into the game at all — a separate process starts a real-time trace session and watches events from everyone, the same shape as this app's *existing* Windows integrations: Phase 2's Event Log correlation already uses the adjacent WinEvt API (also via the `windows` crate) for exactly this kind of passive, no-injection observation, just as a batched post-hoc query rather than a live stream. ETW is the natural next step in the same direction, not a different one.

  What a spike would still need to pin down, concretely: the exact provider GUIDs and event schemas to subscribe to (PresentMon's own source, MIT-licensed, is the obvious reference); whether starting a real-time ETW *session* (as opposed to just reading the Event Log) needs admin or membership in "Performance Log Users" — if it needs admin, that's the same tension Phase 8 already flagged for CPU temperature, and would mean the agent either prompts for elevation just for this feature or it stays tray-overlay-only without agent involvement; how to filter the system-wide event stream down to the one foregrounded/fullscreen game (by PID, matching the foreground window the same way `open_ui::windows_impl::bring_to_front` already finds windows by owning process); and the real-time consumer's own overhead, since this is a continuously-running trace for the entire length of a gaming session — the exact thing this phase's resource-footprint requirement is about.

  **Recommendation:** ETW over hooking, for all three reasons above (no anti-cheat risk at all since the game process is never touched, closer to this codebase's existing Windows-API experience than four graphics-API-specific detours, and it's what the most credible tools in this exact space use today for the anti-cheat reason specifically). The privilege question is the one thing that could change the shape of the design (agent-side vs. a separate elevated helper), so it's the first thing a real spike should answer.

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

### Phase 9 — Full settings coverage
The Settings screen (`crates/ui/src/views/settings.rs`, Phase 4) only covers the `AgentConfig` fields someone thought to add a row for at the time, and two user-facing toggles — autostart and the overlay's "Move Overlay"/opacity — live only in the tray menu or a hand-edited `overlay.toml`, with no UI path to them at all. This phase closes both gaps: every `AgentConfig` field that's actually meaningful to change becomes editable, and the two overlay/tray-only controls get a route into the UI too.

- [ ] **Expose the rest of `AgentConfig`:** `flush_batch_size`, `flush_interval_ms`, `stall_check_interval_ms`, `stall_threshold_ms` are real settings today — defaulted, saved, reloaded by a running agent (`AgentConfig::changed_settings`) — just not shown. Add them to the existing `settings_grid` with the same `egui::DragValue` pattern already used for `sample_interval_ms`/`top_n_processes`/the spike thresholds. No agent or `agent_config` changes needed; this is purely wiring up fields that already round-trip through `reload`.
- [ ] **"Start with Windows" in the Settings screen, not only the tray menu.** The autostart logic (`crates/tray/src/autostart/`: the `HKCU\...\Run` registry read/write, platform-split the same way as `eventlog`/`platform`) is private to the `tray` crate, so the UI can't call it as is. Move it into `agent_config` alongside `AgentConfig`/`OverlayConfig` — the same fix Phase 3 already used once for `AgentConfig` itself ("the fix was extracting a shared crate, not re-parsing independently") — so both `tray` and `ui` depend on one `is_enabled`/`enable`/`disable`, not two copies of the registry logic. Keep the portable-mode guard (`agent_config::is_portable()`) that already hides the checkbox in the tray menu for a portable copy; the Settings screen should hide or disable it for the same reason, with the same already-registered-by-an-older-version escape hatch the tray menu has.
- [ ] **A "Move Overlay" button and an opacity slider in the Settings screen**, acting on the *running* overlay, not just editing `overlay.toml` for its next launch. The hard part: the UI is a separate process from the tray, and today only the tray can reach a running overlay, over the `Child`'s stdin pipe it alone holds (`crates/tray/src/overlay.rs`) — there's no port, file, or anything else to connect to from outside. Options:
  - (a) Give the overlay its own control port, the same shape `agent.port`/`agent_config::send_command` already give the agent: bind `127.0.0.1:0` at startup, write `overlay.port` next to `agent.port` in `data_dir()`, and accept the same `edit on`/`edit off` lines (plus a new `reload`) over it instead of over stdin. `agent_config::send_command` already takes an arbitrary port and command string, so most of the client side exists; the overlay gains a small control-server thread, basically `agent::ipc` cut down to the two commands it needs.
  - (b) Leave the stdin pipe as the overlay's only control surface, and have the UI ask the *tray* to forward the command (a new tray-side control port). Adds a second hop and a second process that has to be running, for no benefit over (a).

  Leaning (a): one control-channel shape for the whole app instead of two, and both the tray's existing "Move Overlay" menu item and a new UI button end up calling the same thing — the tray would switch from writing to its own held stdin pipe to sending itself a command over loopback, which also means edit mode keeps working correctly if the UI and tray ever touch it at the same time, instead of the tray's in-memory `editing` flag silently going stale.
  - Opacity: the slider writes `opacity` to `overlay.toml` (`OverlayConfig::save`) and then sends the new `reload` command, parallel to how the agent's settings already apply instantly — "Saved and applied to the running overlay" vs. "it'll use this from its next start" if nothing's listening on `overlay.port`.
  - The Settings screen needs to know whether an overlay is even running, to show "Start & move" vs. "Move" or to grey out the opacity slider: `read_port_file` succeeding against `overlay.port` is a reasonable proxy, the same way `agent.port`'s presence already stands in for "is the agent running" in `tell_agent_to_reload`.
- [ ] **Settings reflect live state, not only what was true when the screen opened.** Autostart's registration and whether the overlay is running can both change from outside this screen (the tray menu, the installer/uninstaller, another copy) while it's open. Re-check them on a repaint timer, the way `session_list` already polls `PRAGMA data_version` instead of only loading once (Phase 5's "Before Phase 6") — `SettingsView::load()` alone isn't enough once this screen can be left open across other changes.
- [ ] **Deliberately left out of this phase:** the overlay's corner/monitor/margin/position and its hotkey string stay hand-edited in `overlay.toml`. Dragging in edit mode already covers "put it somewhere else," and a monitor picker or a hotkey-capture widget is a separate, lower-value chunk of UI work that doesn't block the settings this phase is actually after.

### Phase 10 — Overlay history graphs
A short history graph next to each live reading in the overlay, CPU, RAM and GPU today, covering about the last minute. A spike that has just passed then stays visible after the number has dropped back. Built so that any metric added later (Phase 8's temperatures and throttling, the FPS counter) gets a graph without new drawing code.

Where things stand: the overlay polls the agent's `live` command every `refresh_interval_ms` and keeps only the latest reading (`Reading::Live` in `crates/overlay/src/app.rs`). The agent keeps no history either. `Telemetry` holds one `LiveSnapshot`, and the sampler only runs within the 3 s lease after a poll (`crates/agent/src/telemetry.rs`). So while the overlay is hidden, nothing is sampled at all.

- [ ] **History buffer in the overlay, not the agent:** one ring buffer of `(timestamp, value)` per metric, covering a new `OverlayConfig::history_seconds` (default 60).
  - Timestamp each point as poll time minus `age_ms`, and skip a reading with the same timestamp as the last one. The overlay can poll faster than the agent samples (`sample_interval_ms`), and repeats of one sample would show up as a flat run.
  - A missing value (`gpu_pct=-`) is a gap, not 0, the same rule as `live_reply`'s `dash_fields_are_none_not_zero`.
  - Why not a buffer in the agent: it could only show a full graph the moment the overlay appears if the agent kept sampling while nobody polls, which is the idle cost the lease exists to avoid (Phase 6's footprint requirement).
- [ ] **Gaps:** hiding the overlay with the hotkey stops polling, and with it the agent's sampler; so does the agent not running. Break the line where consecutive points are more than about twice the sample interval apart, rather than drawing a straight line across time nobody measured.
- [ ] **Drawing:** one small line graph per metric, painted directly with `ui.painter()`. Don't add `egui_plot` to the overlay: it needs no axes, labels or interaction (it's click-through). Percentages use a fixed 0–100 scale so the graphs compare at a glance. `WINDOW_SIZE` (170×78) is a constant sized for three text lines; derive it from the number of metric rows instead, since every later metric adds one. Re-check `corner_position` and its tests, and that a saved `position` near a screen edge still keeps the larger window on screen.
- [ ] **One table of metrics, not one code path per metric:** today each metric is a hand-written field in `LiveReading`, a branch in `live_reply::parse`, and its own `colored_label` line. Replace these with one table of metric definitions: key in the `live` reply, label, unit, and scale (fixed 0–100 for %, fixed or auto-scaled for °C and FPS). A later metric is then one new field in the agent's `live` reply plus one table entry, and it gets its text row and its graph automatically. `parse` already ignores unknown keys, so an older overlay keeps working against a newer agent.
- [ ] **Settings:** `show_history` (default on) and `history_seconds` in `overlay.toml`. Add them to the Settings screen along with Phase 9's overlay controls.
- [ ] **Footprint:** repainting stays at `refresh_interval_ms`, and 60 s at the default 1 s sampling is 60 points per metric. Re-measure the overlay's CPU use against Phase 6's numbers once it draws graphs.
