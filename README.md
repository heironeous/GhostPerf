# GhostPerf

A lightweight Windows performance monitor built to catch *intermittent* problems — stalls, spikes, leaks — that only show up occasionally and are gone by the time you go looking for them. A background agent records CPU/mem/disk/net/GPU, temperatures and CPU throttling, per-process stats, stall and threshold events, and correlated Event Log entries into a local SQLite file; a tray icon controls it; a session viewer lets you scrub back through a recording afterwards; an optional in-game overlay shows live CPU/RAM/GPU use and temperatures while you play.

The shipping target is Windows. It's developed partly in a Linux container (no Windows machine on hand during initial development), so Windows-only code has both a real implementation and a Linux stand-in used purely to keep the build and test suite running during development — see `AGENTS.md` for how that split works and how to verify Windows-only code without a Windows machine.

**Status:** Phases 1–6 (agent core, intermittent-issue detection, tray, session-viewer UI, packaging & polish, gaming overlay) and Phase 8 (temperatures and throttling, without the CPU's own temperature) are implemented and at least smoke-tested. Full roadmap, what's verified vs. not, and per-phase implementation notes live in [`TODO.md`](TODO.md) — read it before picking up work here.

## Layout

Four binaries, one shared storage format, talking over a local loopback TCP control channel:

| Crate | Binary | What it is |
|---|---|---|
| `crates/agent` | `ghostperf-agent` | Background collector: samples metrics, detects stalls/spikes, writes to SQLite. Runs the control server. |
| `crates/tray` | `ghostperf-tray` | System tray icon: start/stop recording, shows idle/recording state, launches the agent if it isn't running and relaunches it if it crashes, autostart toggle, "Open UI", "Show Overlay"/"Move Overlay". |
| `crates/ui` | `ghostperf-ui` | Session viewer: list recorded sessions, chart CPU/mem/disk/net with event markers, drill into per-process stats, rename/delete/export, and a settings screen — see "Using the UI" below. |
| `crates/overlay` | `ghostperf-overlay` | Click-through, always-on-top in-game readout of live CPU/RAM/GPU — see "Using the overlay" below. |
| `crates/storage` | (lib) | SQLite schema + queries, shared by `agent` and `ui`. |
| `crates/logging` | (lib) | File logging for the (windowless) binaries: the `log` facade backed by `%APPDATA%\GhostPerf\logs\<binary>.log`, plus panic logging. |
| `crates/agent_config` | (lib) | `AgentConfig`/`OverlayConfig` and their `agent.toml`/`overlay.toml` load/default logic, plus the `agent.port` file the agent advertises its control port in — shared by all four binaries so they agree on these without duplicating the format. |

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
- `GhostPerf-portable-<version>.zip` — the four exes plus an empty `data` folder. Unzip anywhere and run `ghostperf-tray.exe`; that folder's presence (not a flag) is what makes config/database/logs live next to the exe instead of `%APPDATA%\GhostPerf` — no installer, no registry entries, no autostart (the tray doesn't offer "Start with Windows" in a portable copy). Delete the `data` folder to switch that copy back to installed-style behavior. The folder can be moved or copied elsewhere (another drive, a USB stick) and keeps working with its recordings.
- `GhostPerf-Setup-<version>.exe` — an NSIS installer (`installer/ghostperf.nsi`), only if [NSIS](https://nsis.sourceforge.io/) is installed (`makensis` on `PATH`, or in NSIS's default install folder, where `winget install NSIS.NSIS` puts it without adding it to `PATH`); skipped with a warning otherwise. Installs per-user under `%LocalAppData%\Programs\GhostPerf` (no admin prompt), adds Start Menu shortcuts, registers autostart (the same `HKCU\...\Run` entry the tray's own "Start with Windows" checkbox manages, so either one can toggle it off), and ships an uninstaller that removes all of that but leaves `%APPDATA%\GhostPerf` (your recordings) alone.

## Releases (CI)

`.github/workflows/release.yml` runs on GitHub's Windows runners:

- **Push to `main`**: checks formatting, then runs `./build.ps1 -Test -Package`. If that passes, it tags the commit `vX.Y.Z` and publishes a GitHub release with both `GhostPerf-Setup-X.Y.Z.exe` and `GhostPerf-portable-X.Y.Z.zip` attached. A push that changes only docs (`*.md` anywhere, `docs/`, `LICENSE*`) doesn't run it.
- **Pull request to `main`**: the same build, test and package, with no tag or release. The packages are kept as a workflow artifact.
- **Manual run** (Actions → Build & Release → Run workflow): releases the branch it's run on (only `main` releases), with a choice of which version part to bump.

Versions follow semantic versioning. The commit messages since the last tag decide the bump (`.github/scripts/next-version.ps1`):

| Bump | Write this in any commit message since the last release |
|---|---|
| major (`1.4.2` → `2.0.0`) | `#major`, `BREAKING CHANGE:`, or a Conventional Commits `!` (`feat!: …`, `fix(ui)!: …`) |
| minor (`1.4.2` → `1.5.0`) | `#minor`, or a Conventional Commits `feat: …` / `feat(scope): …` |
| patch (`1.4.2` → `1.4.3`) | anything else (the default) |

Git tags are the record of released versions. CI writes the new version into its own copy of `Cargo.toml` before building, so the exes' version info, the installer and the file names all match the tag, but it doesn't commit that back. The `version` in `Cargo.toml` is therefore a floor: the first release uses it as is, and setting it above the latest tag (say to `1.0.0`) makes the next release exactly that version. Local builds keep reporting the `Cargo.toml` version.

## Running

1. Start `ghostperf-tray` (the agent doesn't need to be started separately — the tray launches it if it isn't already running, and relaunches it if it crashes).
2. Use the tray menu to start/stop a recording session.
3. **Quit** in the tray menu also shuts the agent down: any recording in progress is stopped and saved, then both processes exit.

Neither binary opens a window or console on Windows (the tray is just the tray icon). So that a first start isn't silent, the tray shows a one-time "GhostPerf is running" notification from its icon (once per data folder; delete `tray-welcome-shown` there to see it again). It waits for the next start if Windows notifications are turned off. Their output goes to `%APPDATA%\GhostPerf\logs\agent.log` and `tray.log`: startup, the control port, session start/stop, relaunches, and any errors or panics. Each file is rotated to `.log.1` once it passes 5 MiB. Check these logs first when something doesn't behave.

Config lives at `%APPDATA%\GhostPerf\agent.toml` (TOML, auto-created with defaults on first run — see `AgentConfig` in `crates/agent_config/src/lib.rs` for every field and what it does), unless a `data` folder exists next to the running exe, in which case everything below lives there instead (portable mode — see Packaging). The SQLite database defaults to `ghostperf.db` in that same folder (`db_path = "ghostperf.db"`; a relative `db_path` is relative to that folder, an absolute one is used as is). On non-Windows dev builds, both (and the `logs` directory) default under the OS temp dir instead, since `%APPDATA%` doesn't exist. Completed sessions older than `retention_days` (and, if `retention_max_db_mb` is set, the oldest ones beyond that size too) are auto-pruned after every recording and when the agent starts; a session still recording is never pruned.

The agent also exposes a plain-text control protocol on a loopback TCP port: one newline-terminated command per connection (`start [label]`, `stop`, `status`, `reload` (re-read `agent.toml` and apply it, even mid-recording), `live` (CPU/RAM/GPU for the overlay; also what keeps the sampler running with no session active), `shutdown`), one line of reply. The port isn't configurable: the OS assigns a free one each time the agent starts, and the agent writes it to `agent.port` next to `agent.toml` (deleted again on clean shutdown). This is what the tray uses, and you can drive it directly for debugging, e.g. from a shell:

```sh
PORT=$(cat "$APPDATA/GhostPerf/agent.port")   # on Linux dev builds: $TMPDIR/GhostPerf, usually /tmp/GhostPerf
echo "status" | timeout 2 bash -c "exec 3<>/dev/tcp/127.0.0.1/$PORT; cat >&3; cat <&3"
```

Only one agent runs at a time: each one takes an exclusive lock on `agent.lock` (same directory) at startup, and a second one prints which port the running agent is on and exits.

## Using the UI

Launch `ghostperf-ui` directly, or click "Open UI" in the tray menu, which brings the window to the front if it's already open. It reads the same SQLite database the agent writes to (read-only except for the rename/delete actions below) — it doesn't talk to the agent over the control channel, so it works equally well while a session is recording or with the agent stopped entirely.

- **Sessions** lists every recorded session with its date, duration, label, and quick stats (avg/peak CPU, peak memory, peak temperature of each kind of sensor, event count). Rename in place or delete (two-step confirm) from here, or click **Open** for the detail view.
- **Session detail** charts CPU, CPU speed, memory, disk I/O, disk queue length, network and temperature over the session, each titled on its left (hover a title for what it shows), with stall/spike/throttle/Event-Log markers overlaid. The disk queue chart only appears for sessions that recorded it; CPU speed and temperature say "Not available" instead when there was nothing to read (see "Temperatures" below). Left-drag pans a chart, right-drag box-zooms it (all of them stay in sync); drag on the thin "Select" strip below them to select a time range, which filters the per-process table underneath. Per-process CPU is normalized to whole-machine scale there (the raw per-core value `sysinfo` reports can exceed 100% per process — see TODO.md's Phase 1 note on this). Export the session to CSV or JSON from the bottom of this view.
- **Settings** edits `agent.toml` (sample interval, retention days, an optional max database size, CPU/memory spike and GPU/board temperature thresholds, Event Log correlation) and shows the DB location. Saving applies the changes to a running agent straight away (it sends the agent `reload`), including to a recording in progress; otherwise they apply when the agent starts.

### Temperatures

GhostPerf records what Windows exposes without admin rights:
- **GPU temperature**, for each graphics card, the same reading Task Manager shows. NVIDIA and AMD cards report it; integrated Intel graphics usually don't.
- **ACPI thermal zones** ("Board"): a motherboard or chassis sensor, never the CPU itself. Many desktop motherboards report a fixed placeholder here instead of a real temperature (27.85 °C is a common one). The detail view points out a sensor that never changed during a recording.
- **CPU speed and throttling**: the effective clock and how much the CPU is currently allowed to run (100% when nothing holds it back). A CPU slowing itself down under load is how a heat or power problem shows up, even without a temperature. Windows doesn't say whether heat or power is the cause.

The **CPU's own temperature isn't recorded**: Windows has no way to read it without a kernel driver (see TODO.md Phase 8).

Two kinds of event are marked on the charts. 	emp_spike marks a GPU or board sensor reaching its threshold in Settings (85 °C and 80 °C by default), once per crossing. cpu_throttle marks the CPU starting to be limited.

## Using the overlay

Tick "Show Overlay" in the tray menu (ticked while it's running; unticking closes it — a real start/stop toggle, not launch-or-focus like "Open UI"). It shows live CPU/RAM/GPU use, the hottest GPU temperature and the board temperature (`—` where the machine has none; see "Temperatures" above) in a small, click-through, always-on-top window with no border, taskbar button or Alt-Tab entry, so it never steals input or focus from whatever's running underneath it — including a game. It stays on top of borderless/windowed-fullscreen games, including ones whose own window is always-on-top; it can't draw over a game in *exclusive* fullscreen, so switch the game to borderless fullscreen to see it. It doesn't need a recording session running: it polls the agent's `live` command directly (see `TODO.md` Phase 6), which makes the sampler tick on its own while anything is actually asking for live numbers, and goes back to idle a few seconds after you stop.

Once it's running, the configured global hotkey (`Ctrl+Alt+O` by default) shows/hides it instantly without relaunching — handy for toggling it off for a screenshot or a menu, then back on. Position (which corner, which monitor, margin), opacity, and refresh rate live in `overlay.toml` next to `agent.toml` (`OverlayConfig` in `crates/agent_config/src/overlay_config.rs`; no settings-screen UI for it yet, hand-edit the file and restart the overlay to pick up changes).

To put it somewhere else, tick "Move Overlay" in the tray menu (it starts the overlay if needed). It gets an orange outline and can be dragged with the mouse; untick "Move Overlay" to lock it there, click-through again. The spot is saved to `overlay.toml` as `position = [x, y]` and used on every start instead of the corner; delete that line to go back to the corner.

Targeting a monitor other than the first one, and drawing over genuine exclusive-fullscreen games (rather than the borderless/windowed-fullscreen most games default to today) are both best-effort — see the caveats in `TODO.md` Phase 6. There's no live FPS counter yet; that needs its own follow-up (also detailed there).

## Testing

```sh
cargo test --workspace
```

Every phase so far has end-to-end-smoke-tested the agent (and, from Phase 3, the tray supervising it) against a real control-channel session, not just unit tests — see the "Smoke-tested..." notes under each phase in `TODO.md` for exactly what was exercised and on which platform.
