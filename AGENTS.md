# AGENTS.md

Instructions for an AI coding agent (or any contributor) picking up work in this repo.

## Read this first

1. **`TODO.md` is the single source of truth for project status.** It has the architecture, the full feature roadmap by phase, which phases are done, and — critically — per-phase notes on what was actually verified (tests run, smoke tests performed) vs. what's still unverified (usually: real-Windows-only code that could only be compile-checked in a Linux dev container). Read the whole file, not just the checkboxes, before starting anything. The prose under each phase documents gotchas that will bite you again if you don't know about them.
2. This repo is normally developed in a **Debian Linux container with no real Windows machine available**, even though Windows is the only shipping target. That constraint shaped a lot of the architecture and workflow below — most importantly, every Windows-only module has a second, real (not stubbed-out) implementation that runs on Linux purely so the code builds and its surrounding logic is testable in this environment. Check the environment you're actually in — if you do have a real Windows machine this time, prefer testing there directly and skip the cross-compile workarounds below.

## Environment setup (fresh Linux container)

None of this is needed if you're on an actual Windows machine — just use `./build.ps1`.

**Rust targets**, so Windows-only code can be compile-checked without a Windows machine:

```sh
rustup target add x86_64-pc-windows-msvc   # for completeness; see note below
rustup target add x86_64-pc-windows-gnu    # the one that actually works here
```

**mingw-w64, to cross-compile the `gnu` target** (needed because `storage` depends on `rusqlite` with the `bundled` feature, which compiles a C file — `cargo check` alone isn't enough, you need a real cross C compiler):

```sh
apt-get update && apt-get install -y gcc-mingw-w64-x86-64
```

**GTK/X11/D-Bus dev packages, to build `crates/tray` at all on Linux** (`tray-icon` + `tao` pull in a GTK3 backend on Linux):

```sh
apt-get install -y pkg-config libdbus-1-dev libgtk-3-dev libxdo-dev libayatana-appindicator3-dev
```

**Xvfb + dbus-x11, only if you need to actually run (not just build) the tray binary** for a smoke test:

```sh
apt-get install -y xvfb dbus-x11
```

## Verifying Windows-only code without a Windows machine

This is the single most important workflow in this repo. Don't skip it for anything touching `#[cfg(target_os = "windows")]` code, a `windows_impl.rs`, or a `build.rs` that embeds resources.

**Use the `gnu` target, not `msvc`.** `cargo check --target x86_64-pc-windows-msvc` fails in this container — not because of your code, but because `rusqlite`'s bundled SQLite needs a C compiler for that target and there's no MSVC `lib.exe`/linker available. `x86_64-pc-windows-gnu` works because mingw provides a real, complete cross C toolchain:

```sh
CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc cargo check --workspace --target x86_64-pc-windows-gnu --tests
```

Run this after touching any Windows-only code. It has repeatedly caught real bugs before ever reaching a Windows machine — wrong struct field paths, wrong argument types (`Option<u32>` vs. a bare integer, a `WIN32_ERROR` return instead of a `Result`), etc. A thing that "looks right" by eye in `windows`-crate code is not the same as a thing that compiles; always check.

**When writing new code against the `windows` crate, don't guess the API from memory or docs alone.** Spin up a disposable scratch crate, add just the `windows` features you need, and iterate there with `cargo check --target x86_64-pc-windows-gnu` until it compiles — *then* port the verified code into the real crate:

```sh
mkdir -p /tmp/scratch && cd /tmp/scratch && cargo init -q
cargo add windows --features "Win32_System_Whatever,Win32_Foundation"
# write a minimal snippet exercising the exact calls you need, then:
CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc cargo check --target x86_64-pc-windows-gnu
```

This is how the PDH counters (Phase 2), the registry autostart (Phase 3), and the Event Log correlation (Phase 2) were all written and verified — each one got this scratch-crate treatment before being ported into `crates/agent`/`crates/tray`.

**Even after this, "compiles" is not "works."** Every phase's TODO.md entry says explicitly which Windows-only code is still unverified by an actual run, and every phase so far has needed a handful of real fixes on first contact with real Windows (see the "Fixes from first Windows run" sections in `TODO.md`). Expect the same for whatever you add. If you get access to a real Windows machine, build with `./build.ps1 -Test`, actually exercise the feature, and record what broke and what you fixed in TODO.md the same way previous phases did (see "Documentation conventions" below) — that record is what lets the next session (possibly back in a Linux-only container) know what's actually trustworthy.

## Smoke-testing the tray on headless Linux

The tray has no visible window on its own — just a tray icon via `tray-icon`/`tao`'s GTK backend, which needs an X display and a D-Bus session bus to initialize at all, even though a real tray icon won't actually render without a status-notifier host (no desktop environment here). Set up a disposable display + bus and run it:

```sh
Xvfb :98 -screen 0 1280x1024x24 &
eval "$(DISPLAY=:98 dbus-launch --sh-syntax)"
DISPLAY=:98 DBUS_SESSION_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS" APPDATA=/tmp/scratch-appdata \
    ./target/debug/ghostperf-tray
```

This is enough to exercise supervision, the control-channel client, and the autostart file logic for real (confirm via `ps aux` for the spawned agent process, and by talking to the control port directly as in the README). It does *not* exercise actual icon rendering or real mouse clicks on the menu — that still needs a real desktop or a real Windows machine.

Two gotchas hit while doing this, now fixed but worth knowing if you're debugging something similar:
- **`pkill -f <pattern>` can match its own argv** and kill something unintended (including, confusingly, itself) if `<pattern>` appears in the `pkill` invocation's own command line. Use `pkill -x <exact-process-name>` instead when cleaning up test processes.
- **`sysinfo`'s process scan includes zombies** (dead, unreaped processes) on Linux. Any "is process X still running" check needs to filter `ProcessStatus::Zombie` out, or a crashed-and-not-yet-reaped child looks alive forever. See `crates/tray/src/supervisor.rs` for the fix and why it mattered (it broke crash-relaunch entirely until found).
- **`tao`'s `ControlFlow::WaitUntil`/`Poll` didn't reliably wake the event loop** in this headless, no-window-manager setup — only `Event::UserEvent` pushed in from a background thread via `EventLoopProxy` did. See `crates/tray/src/main.rs`. Don't revert to a `WaitUntil`-based poll loop without testing it on a real desktop/Windows first.

## Code conventions

These are patterns established consistently across Phases 1–3 — follow them for consistency rather than inventing a new approach each time.

- **Platform split**: a `mod.rs` re-exports either a `windows_impl` (under `#[cfg(target_os = "windows")]`) or a Linux/generic stand-in (`linux_impl.rs`/`generic_impl.rs`), both exposing the same function signatures. See `crates/agent/src/platform/`, `crates/agent/src/eventlog/`, `crates/tray/src/autostart/` for three examples of this shape.
- **Pure logic gets pulled out for testability**: anything parseable/formattable that a Windows-only module needs (date math, XML field extraction, registry-value string formatting) lives in its own always-compiled `format.rs`, imported by `windows_impl.rs`. This is what lets code that will only ever *run* on Windows still get real unit test coverage in this Linux container. When the pure half isn't used by anything on the current platform, it gets `#![allow(dead_code)]` with a one-line comment explaining why, rather than being `#[cfg]`'d away (which would also hide it from tests).
- **New Cargo dependencies**: use `cargo add`, not hand-edited `Cargo.toml` entries, so version/feature resolution is correct. Windows-only dependencies go under `[target.'cfg(windows)'.dependencies]` (see `crates/agent/Cargo.toml`, `crates/tray/Cargo.toml`).
- **Comments explain WHY, not WHAT** — a hidden constraint, a prior incident, a platform quirk, a reason an alternative was rejected. No comment restates what a well-named function already says.
- **`cargo fmt`** before calling anything done. No custom `rustfmt.toml`; defaults.
- **Don't duplicate config/state across crates.** When a second binary needed to read the agent's config (the tray needing `control_port`), the fix was extracting a shared `agent_config` lib crate (Phase 3), not re-parsing `agent.toml` independently.

## Documentation conventions

- `TODO.md` is updated as part of landing a phase, not as an afterthought: check off each bullet with a one-line pointer to the file/test that implements it, then a short paragraph of what was and wasn't verified and how (unit tests run, smoke test performed and on what platform, what's still only compile-checked).
- After a real run on hardware that wasn't available during initial implementation (so far: a real Windows machine), add a dated "Fixes from first Windows run" (or similarly named) subsection under that phase, one bullet per bug: **what broke**, the root cause, the fix, and the file(s)/regression test. This is the mechanism that lets a future session — possibly back in a Linux-only container — know which parts of a "done" phase are actually proven vs. still just plausible-looking code.
- Keep `README.md` user/contributor-facing (what this is, how to build/run/test) and this file (`AGENTS.md`) agent/workflow-facing (how to actually get work done in this specific environment). `TODO.md` owns status and roadmap. Don't let the same information drift out of sync across all three — update the one it actually belongs in and cross-reference.
