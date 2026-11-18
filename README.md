# GhostPerf

A lightweight Windows performance monitor built to catch *intermittent* problems — stalls, spikes, leaks — that only show up occasionally and are gone by the time you go looking for them. A background agent records CPU/mem/disk/net/GPU, per-process stats, stall and threshold events, and correlated Event Log entries into a local SQLite file; a tray icon controls it; a session viewer lets you scrub back through a recording afterwards.

The shipping target is Windows. It's developed partly in a Linux container (no Windows machine on hand during initial development), so Windows-only code has both a real implementation and a Linux stand-in used purely to keep the build and test suite running during development — see `AGENTS.md` for how that split works and how to verify Windows-only code without a Windows machine.

**Status:** Phases 1–5 (agent core, intermittent-issue detection, tray, session-viewer UI, packaging & polish) are implemented and at least smoke-tested. Full roadmap, what's verified vs. not, and per-phase implementation notes live in [`TODO.md`](TODO.md) — read it before picking up work here.

## Layout

Three binaries, one shared storage format, talking over a local loopback TCP control channel:

| Crate | Binary | What it is |
|---|---|---|
| `crates/agent` | `ghostperf-agent` | Background collector: samples metrics, detects stalls/spikes, writes to SQLite. Runs the control server. |
| `crates/tray` | `ghostperf-tray` | System tray icon: start/stop recording, shows idle/recording state, launches the agent if it isn't running and relaunches it if it crashes, autostart toggle, "Open UI". |
| `crates/ui` | `ghostperf-ui` | Session viewer: list recorded sessions, chart CPU/mem/disk/net with event markers, drill into per-process stats, rename/delete/export, and a settings screen — see "Using the UI" below. |
| `crates/storage` | (lib) | SQLite schema + queries, shared by `agent` and `ui`. |
| `crates/logging` | (lib) | File logging for the (windowless) binaries: the `log` facade backed by `%APPDATA%\GhostPerf\logs\<binary>.log`, plus panic logging. |
| `crates/agent_config` | (lib) | `AgentConfig` and the `agent.toml` load/default logic, plus the `agent.port` file the agent advertises its control port in — shared by `agent`, `tray`, and `ui` so all agree on these without duplicating the format. |

See [`TODO.md`](TODO.md) for the full architecture write-up (data model, IPC protocol, why each language/library choice was made) and [`assets/README.md`](assets/README.md) for the icon/branding assets and how to regenerate them.

## Building

Requires a stable Rust toolchain ([rustup.rs](https://rustup.rs)).

**Windows** (the shipping target):

```powershell
./build.ps1              # release build
./build.ps1 -Debug        # debug build
./build.ps1 -Test         # also runs the test suite after building
```

Binaries land in `target\release\` (or `target\debug\` with `-Debug`).

**Any platform** (e.g. for development on Linux/macOS):

```sh
cargo build --workspace --release
cargo test --workspace
```

On Linux, building `crates/tray` (and, via the same underlying X11/Wayland/GTK packages, `crates/ui`) needs some system packages first — see `AGENTS.md` for the exact install command. `crates/agent`, `crates/agent_config`, and `crates/storage` have no such requirement. Actually *running* `crates/ui` additionally needs a working GPU adapter — on Linux without a real GPU, install `mesa-vulkan-drivers` for a software one (see `AGENTS.md`).

## Packaging

```powershell
./build.ps1 -Package
```

Produces, in `dist\`:
- `GhostPerf-portable-<version>.zip` — the three exes plus an empty `data` folder. Unzip anywhere and run `ghostperf-tray.exe`; that folder's presence (not a flag) is what makes config/database/logs live next to the exe instead of `%APPDATA%\GhostPerf` — no installer, no registry entries, no autostart. Delete the `data` folder to switch that copy back to installed-style behavior. The folder can be moved or copied elsewhere (another drive, a USB stick) and keeps working with its recordings.
- `GhostPerf-Setup-<version>.exe` — an NSIS installer (`installer/ghostperf.nsi`), only if [NSIS](https://nsis.sourceforge.io/) is installed (`makensis` on `PATH`, or in NSIS's default install folder, where `winget install NSIS.NSIS` puts it without adding it to `PATH`); skipped with a warning otherwise. Installs per-user under `%LocalAppData%\Programs\GhostPerf` (no admin prompt), adds Start Menu shortcuts, registers autostart (the same `HKCU\...\Run` entry the tray's own "Start with Windows" checkbox manages, so either one can toggle it off), and ships an uninstaller that removes all of that but leaves `%APPDATA%\GhostPerf` (your recordings) alone.

## Running

1. Start `ghostperf-tray` (the agent doesn't need to be started separately — the tray launches it if it isn't already running, and relaunches it if it crashes).
2. Use the tray menu to start/stop a recording session.
3. **Quit** in the tray menu also shuts the agent down: any recording in progress is stopped and saved, then both processes exit.

Neither binary opens a window or console on Windows (the tray is just the tray icon). Their output goes to `%APPDATA%\GhostPerf\logs\agent.log` and `tray.log`: startup, the control port, session start/stop, relaunches, and any errors or panics. Each file is rotated to `.log.1` once it passes 5 MiB. Check these logs first when something doesn't behave.

Config lives at `%APPDATA%\GhostPerf\agent.toml` (TOML, auto-created with defaults on first run — see `AgentConfig` in `crates/agent_config/src/lib.rs` for every field and what it does), unless a `data` folder exists next to the running exe, in which case everything below lives there instead (portable mode — see Packaging). The SQLite database defaults to `ghostperf.db` in that same folder (`db_path = "ghostperf.db"`; a relative `db_path` is relative to that folder, an absolute one is used as is). On non-Windows dev builds, both (and the `logs` directory) default under the OS temp dir instead, since `%APPDATA%` doesn't exist. Completed sessions older than `retention_days` (and, if `retention_max_db_mb` is set, the oldest ones beyond that size too) are auto-pruned after every recording and when the agent starts; a session still recording is never pruned.

The agent also exposes a plain-text control protocol on a loopback TCP port: one newline-terminated command per connection (`start [label]`, `stop`, `status`, `shutdown`), one line of reply. The port isn't configurable: the OS assigns a free one each time the agent starts, and the agent writes it to `agent.port` next to `agent.toml` (deleted again on clean shutdown). This is what the tray uses, and you can drive it directly for debugging, e.g. from a shell:

```sh
PORT=$(cat "$APPDATA/GhostPerf/agent.port")   # on Linux dev builds: $TMPDIR/GhostPerf, usually /tmp/GhostPerf
echo "status" | timeout 2 bash -c "exec 3<>/dev/tcp/127.0.0.1/$PORT; cat >&3; cat <&3"
```

Only one agent runs at a time: each one takes an exclusive lock on `agent.lock` (same directory) at startup, and a second one prints which port the running agent is on and exits.

## Using the UI

Launch `ghostperf-ui` directly, or click "Open UI" in the tray menu. It reads the same SQLite database the agent writes to (read-only except for the rename/delete actions below) — it doesn't talk to the agent over the control channel, so it works equally well while a session is recording or with the agent stopped entirely.

- **Sessions** lists every recorded session with its date, duration, label, and quick stats (avg/peak CPU, peak memory, event count). Rename in place or delete (two-step confirm) from here, or click **Open** for the detail view.
- **Session detail** charts CPU/mem/disk/net over the session, with stall/spike/Event-Log markers overlaid. Left-drag pans a chart, right-drag box-zooms it (all 4 stay in sync); drag on the thin strip below them to select a time range, which filters the per-process table underneath. Per-process CPU is normalized to whole-machine scale there (the raw per-core value `sysinfo` reports can exceed 100% per process — see TODO.md's Phase 1 note on this). Export the session to CSV or JSON from the bottom of this view.
- **Settings** edits `agent.toml` (sample interval, retention days, an optional max database size, spike thresholds, Event Log correlation) and shows the DB location. Takes effect the next time the agent starts.

## Testing

```sh
cargo test --workspace
```

Every phase so far has end-to-end-smoke-tested the agent (and, from Phase 3, the tray supervising it) against a real control-channel session, not just unit tests — see the "Smoke-tested..." notes under each phase in `TODO.md` for exactly what was exercised and on which platform.
