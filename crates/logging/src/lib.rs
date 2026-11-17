//! File logging shared by the GhostPerf binaries.
//!
//! The agent and tray are built as Windows-subsystem (windowless) programs.
//! They used to be console programs, which meant a console window existed,
//! and closing it killed both processes on the spot with no clean shutdown.
//! With no console there's no stdout to read, so everything goes through the
//! `log` facade into `<log_dir>/<name>.log` instead.
//!
//! Each line is written straight to the file with no userspace buffering.
//! Release builds use `panic = "abort"` and fatal errors end in
//! `process::exit`, and neither runs destructors, so anything still sitting
//! in a buffer would be lost exactly when it matters most. Log volume is a
//! handful of lines per session, so the per-line syscall costs nothing.
//!
//! Lines are also copied to stderr. In a windowless process that handle is
//! null and Rust's std silently discards the writes. It still shows up
//! wherever stderr does exist: Linux dev runs, `cargo test`, or a Windows
//! launch with stderr redirected.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Rotated on startup once it's past this size, keeping one previous file
/// (`<name>.log.1`). A size cap rather than daily files: the agent normally
/// logs a few lines per session, so only something pathological (a crash
/// loop being relaunched every second) could grow it quickly.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

pub struct FileLogger {
    file: Mutex<Option<File>>,
}

impl FileLogger {
    /// Never fails: a logger that can't open its file still writes to
    /// stderr, and a monitoring tool refusing to start because its log
    /// directory is unwritable would be worse than one that logs less.
    pub fn open(dir: &Path, name: &str) -> (Self, Option<PathBuf>) {
        let path = dir.join(format!("{name}.log"));
        let file = std::fs::create_dir_all(dir)
            .and_then(|()| {
                rotate_if_large(&path, MAX_LOG_BYTES)?;
                OpenOptions::new().create(true).append(true).open(&path)
            })
            .map_err(|e| eprintln!("{name}: can't open log file {}: {e}", path.display()))
            .ok();
        let opened = file.as_ref().map(|_| path);
        (
            Self {
                file: Mutex::new(file),
            },
            opened,
        )
    }
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format_line(
            &chrono::Local::now()
                .format("%Y-%m-%d %H:%M:%S%.3f")
                .to_string(),
            record.level(),
            record.target(),
            &record.args().to_string(),
        );
        // A poisoned lock only means another thread panicked mid-write; the
        // file handle itself is still fine, and losing logging after a panic
        // is the worst possible time to lose it.
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(file) = file.as_mut() {
            let _ = file.write_all(line.as_bytes());
        }
        let _ = std::io::stderr().write_all(line.as_bytes());
    }

    fn flush(&self) {}
}

/// Installs the file logger as the global `log` backend, plus a panic hook
/// that records panics in the same file (a windowless process's default
/// panic message goes to a null stderr and vanishes). Call once, first
/// thing in `main`. Returns the log file path, if it could be opened.
pub fn init(dir: &Path, name: &str) -> Option<PathBuf> {
    let (logger, path) = FileLogger::open(dir, name);
    // Leaked: the global logger must be `'static` and lives as long as the
    // process anyway.
    if log::set_logger(Box::leak(Box::new(logger))).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!(
            "panic: {info}\n{}",
            std::backtrace::Backtrace::force_capture()
        );
        default_hook(info);
    }));
    path
}

/// The crate path in `target` is trimmed to its top-level module and the
/// binary prefix dropped (`agent::core` -> `core`): the file name already
/// says which binary wrote the line.
fn format_line(timestamp: &str, level: log::Level, target: &str, message: &str) -> String {
    let module = target.split_once("::").map_or(target, |(_, rest)| rest);
    let module = module.split("::").next().unwrap_or(module);
    if module.is_empty() || !target.contains("::") {
        format!("{timestamp} {level:<5} {message}\n")
    } else {
        format!("{timestamp} {level:<5} [{module}] {message}\n")
    }
}

fn rotate_if_large(path: &Path, max_bytes: u64) -> std::io::Result<()> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > max_bytes => {
            let mut rotated = path.as_os_str().to_owned();
            rotated.push(".1");
            std::fs::rename(path, rotated)
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Log;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ghostperf-logging-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn line_has_timestamp_level_module_and_message() {
        assert_eq!(
            format_line(
                "2026-11-17 15:16:52.138",
                log::Level::Warn,
                "agent::core",
                "x"
            ),
            "2026-11-17 15:16:52.138 WARN  [core] x\n"
        );
        // Nested modules collapse to the top-level one.
        assert_eq!(
            format_line("t", log::Level::Info, "agent::eventlog::windows_impl", "y"),
            "t INFO  [eventlog] y\n"
        );
        // `main.rs` logs under the bare crate name: no module tag.
        assert_eq!(
            format_line("t", log::Level::Error, "agent", "z"),
            "t ERROR z\n"
        );
    }

    #[test]
    fn records_are_appended_to_the_named_file_in_a_created_dir() {
        let dir = scratch_dir("append").join("logs");
        let (logger, path) = FileLogger::open(&dir, "agent");
        let path = path.expect("log file should open");
        assert_eq!(path, dir.join("agent.log"));
        log::set_max_level(log::LevelFilter::Info);

        for msg in ["first", "second"] {
            logger.log(
                &log::Record::builder()
                    .level(log::Level::Info)
                    .target("agent::ipc")
                    .args(format_args!("{msg}"))
                    .build(),
            );
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].ends_with("INFO  [ipc] first"), "{}", lines[0]);
        assert!(lines[1].ends_with("INFO  [ipc] second"), "{}", lines[1]);

        // Re-opening (next process start) appends rather than truncating.
        drop(logger);
        let (_logger, _) = FileLogger::open(&dir, "agent");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn oversized_log_is_rotated_to_dot_1_on_open() {
        let dir = scratch_dir("rotate");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent.log");
        std::fs::write(&path, "old contents").unwrap();

        rotate_if_large(&path, 1000).unwrap();
        assert!(path.exists(), "small file must not be rotated");

        rotate_if_large(&path, 5).unwrap();
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("agent.log.1")).unwrap(),
            "old contents"
        );

        // A second rotation replaces the previous `.1`, keeping just one.
        std::fs::write(&path, "newer contents").unwrap();
        rotate_if_large(&path, 5).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("agent.log.1")).unwrap(),
            "newer contents"
        );
    }
}
