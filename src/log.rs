//! Tiny append-only file logger with size-based rotation (one backup file).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use windows::Win32::System::SystemInformation::GetLocalTime;

const MAX_LOG_BYTES: u64 = 1024 * 1024;

struct Logger {
    path: Option<PathBuf>,
    console: bool,
}

static LOGGER: Mutex<Logger> = Mutex::new(Logger {
    path: None,
    console: true,
});

/// Directory for the log file: %LOCALAPPDATA%\KeepVMResolution
pub fn log_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("KeepVMResolution"))
}

pub fn log_path() -> Option<PathBuf> {
    log_dir().map(|d| d.join("keep-vm-resolution.log"))
}

/// Enable file logging (and optionally mirror to stdout).
pub fn init(to_file: bool, console: bool) {
    let mut lg = LOGGER.lock().unwrap_or_else(|e| e.into_inner());
    lg.console = console;
    if to_file && let Some(dir) = log_dir() {
        let _ = fs::create_dir_all(&dir);
        lg.path = Some(dir.join("keep-vm-resolution.log"));
    }
}

fn timestamp() -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

pub fn write(level: &str, msg: &str) {
    let line = format!("{} [{}] {}\r\n", timestamp(), level, msg);
    let lg = LOGGER.lock().unwrap_or_else(|e| e.into_inner());
    if lg.console {
        print!("{line}");
        let _ = std::io::stdout().flush();
    }
    if let Some(path) = &lg.path {
        if fs::metadata(path)
            .map(|m| m.len() > MAX_LOG_BYTES)
            .unwrap_or(false)
        {
            let _ = fs::rename(path, path.with_extension("log.1"));
        }
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

#[macro_export]
macro_rules! info { ($($a:tt)*) => { $crate::log::write("INFO", &format!($($a)*)) } }
#[macro_export]
macro_rules! warn { ($($a:tt)*) => { $crate::log::write("WARN", &format!($($a)*)) } }
#[macro_export]
macro_rules! error { ($($a:tt)*) => { $crate::log::write("ERROR", &format!($($a)*)) } }
