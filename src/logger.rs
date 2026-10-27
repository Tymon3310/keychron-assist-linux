//! Logger writing the native client's format to `~/.Keychron_Screen/log.json`
//! (`[INFO][<unix secs>][Common]: message`) and to stderr.

use log::{Level, LevelFilter, Log, Metadata, Record};
use std::{fs::OpenOptions, io::Write, path::PathBuf, sync::Mutex};

struct Logger {
    file: Mutex<Option<std::fs::File>>,
}

impl Log for Logger {
    fn enabled(&self, m: &Metadata) -> bool {
        m.level() <= Level::Debug && (m.level() <= Level::Info || std::env::var_os("KEYCHRON_DEBUG").is_some())
    }
    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) || !r.target().starts_with("keychron") {
            return;
        }
        let level = match r.level() {
            Level::Error => "ERROR",
            Level::Warn => "WARNING",
            Level::Info => "INFO",
            _ => "DEBUG",
        };
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let module = if r.target().contains("rdp") { "Common" } else { "QuickStart" };
        let line = format!("[{level}][{ts}][{module}]: {}", r.args());
        eprintln!("{line}");
        if let Some(f) = self.file.lock().unwrap().as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }
    fn flush(&self) {}
}

pub fn init(root: &std::path::Path) {
    let path: PathBuf = root.join("log.json");
    // Keep the file from growing forever.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 8 << 20) {
        let _ = std::fs::rename(&path, root.join("log.old.json"));
    }
    let file = OpenOptions::new().create(true).append(true).open(path).ok();
    let logger = Box::leak(Box::new(Logger { file: Mutex::new(file) }));
    let _ = log::set_logger(logger);
    log::set_max_level(LevelFilter::Debug);
}

/// Log to stderr only (diagnostic commands).
pub fn init_stderr() {
    let logger = Box::leak(Box::new(Logger { file: Mutex::new(None) }));
    let _ = log::set_logger(logger);
    log::set_max_level(LevelFilter::Debug);
}
