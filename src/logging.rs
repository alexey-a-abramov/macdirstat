//! Lightweight file logging plus a global panic handler.
//!
//! Scan activity, warnings and crashes are written to
//! `~/Library/Logs/MacDirStat/macdirstat.log` (the macOS-conventional
//! location, also visible in Console.app) so behaviour can be analysed after
//! the fact. We use the `log` facade, so warnings emitted by eframe/winit/wgpu
//! are captured too.

use std::backtrace::Backtrace;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use log::{Level, LevelFilter, Metadata, Record};

/// Rotate the log once it grows past this size, to bound disk usage.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

struct FileLogger {
    file: Mutex<Option<File>>,
}

static LOGGER: FileLogger = FileLogger {
    file: Mutex::new(None),
};

/// `~/Library/Logs/MacDirStat/macdirstat.log`
fn log_file_path() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(|home| {
        PathBuf::from(home)
            .join("Library")
            .join("Logs")
            .join("MacDirStat")
            .join("macdirstat.log")
    })
}

/// Current local time formatted as `YYYY-MM-DD HH:MM:SS`.
/// Uses libc (already a dependency) so we avoid pulling in a date crate.
fn timestamp() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return "????-??-?? ??:??:??".to_string();
        }
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        // Our own logs at Info and above; noisy frameworks only at Warn+.
        if metadata.target().starts_with("macdirstat") {
            metadata.level() <= Level::Info
        } else {
            metadata.level() <= Level::Warn
        }
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} [{:>5}] {} - {}\n",
            timestamp(),
            record.level(),
            record.target(),
            record.args()
        );
        if let Ok(mut guard) = self.file.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = f.write_all(line.as_bytes());
                let _ = f.flush();
            }
        }
        // Mirror errors to stderr so they're also visible when run from a terminal.
        if record.level() <= Level::Error {
            eprint!("{line}");
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = f.flush();
            }
        }
    }
}

/// Initialise file logging. Safe to call once at startup; all errors are
/// swallowed so logging can never stop the app from running.
pub fn init() {
    if let Some(path) = log_file_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Single-file rotation to keep the log bounded.
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > MAX_LOG_BYTES {
                let _ = std::fs::rename(&path, path.with_extension("log.old"));
            }
        }
        if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
            if let Ok(mut guard) = LOGGER.file.lock() {
                *guard = Some(f);
            }
        }
    }
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(LevelFilter::Info);
}

/// Install a global panic hook ("global catch") that records the panic, its
/// location and a backtrace to the log before delegating to the default
/// handler.
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".to_string());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        let backtrace = Backtrace::force_capture();
        log::error!("PANIC at {location}: {msg}\n{backtrace}");
        log::logger().flush();
        default_hook(info);
    }));
}
