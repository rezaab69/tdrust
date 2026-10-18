//! Logging: wraps env_logger to keep the latest lines in memory (for the
//! logs screen), count warnings and errors (for monitoring) and group
//! errors by fingerprint, which the scheduler stores in `error_events` and
//! turns into alerts.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use log::{Level, LevelFilter, Log, Metadata, Record};

use super::metrics::{self, Metric};
use crate::text::now;

/// Lines kept in memory.
const KEEP: usize = 1000;
/// Longest line kept, in characters.
const LINE_CHARS: usize = 1000;

#[derive(Clone, Debug)]
pub struct LogLine {
    pub at: i64,
    pub level: Level,
    pub target: String,
    pub text: String,
}

/// Errors with the same fingerprint since the last flush.
#[derive(Clone, Debug)]
pub struct ErrorGroup {
    pub target: String,
    /// The latest text.
    pub message: String,
    pub count: i64,
}

struct Logger {
    inner: env_logger::Logger,
    lines: Mutex<VecDeque<LogLine>>,
    errors: Mutex<HashMap<String, ErrorGroup>>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();
/// The level `RUST_LOG` allows, restored when the override is removed.
static ENV_LEVEL: OnceLock<LevelFilter> = OnceLock::new();

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if !self.inner.matches(record) {
            return;
        }
        self.inner.log(record);
        let text: String = record.args().to_string().chars().take(LINE_CHARS).collect();
        match record.level() {
            Level::Error => {
                metrics::inc(Metric::Errors);
                let mut errors = self.errors.lock().unwrap_or_else(|p| p.into_inner());
                let group = errors.entry(fingerprint(record.target(), &text)).or_insert_with(|| ErrorGroup {
                    target: record.target().to_owned(),
                    message: String::new(),
                    count: 0,
                });
                group.message.clone_from(&text);
                group.count += 1;
            }
            Level::Warn => metrics::inc(Metric::Warnings),
            _ => {}
        }
        let mut lines = self.lines.lock().unwrap_or_else(|p| p.into_inner());
        if lines.len() >= KEEP {
            lines.pop_front();
        }
        lines.push_back(LogLine { at: now(), level: record.level(), target: record.target().to_owned(), text });
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

/// Installs the logger (instead of `env_logger::init`) and a panic hook that
/// logs panics as errors, so they reach error monitoring.
pub fn init() {
    let inner = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).format_timestamp_secs().build();
    let level = inner.filter();
    let _ = ENV_LEVEL.set(level);
    let logger = LOGGER.get_or_init(|| Logger { inner, lines: Mutex::default(), errors: Mutex::default() });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level);
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!(target: "panic", "{info}");
        default_hook(info);
    }));
}

/// Applies a level override (`None`: back to `RUST_LOG`). Modules that
/// `RUST_LOG` filters out stay filtered.
pub fn set_level(level: Option<&str>) {
    let env = ENV_LEVEL.get().copied().unwrap_or(LevelFilter::Info);
    let wanted = level.and_then(|l| l.parse::<LevelFilter>().ok()).unwrap_or(env);
    if log::max_level() != wanted {
        log::set_max_level(wanted);
    }
}

/// The latest lines at or above `min`, newest first, skipping `skip` of them.
pub fn recent(min: Level, skip: usize, limit: usize) -> (Vec<LogLine>, usize) {
    let Some(logger) = LOGGER.get() else { return (Vec::new(), 0) };
    let lines = logger.lines.lock().unwrap_or_else(|p| p.into_inner());
    let matching: Vec<&LogLine> = lines.iter().rev().filter(|l| l.level <= min).collect();
    let total = matching.len();
    (matching.into_iter().skip(skip).take(limit).cloned().collect(), total)
}

/// Every kept line at or above `min`, oldest first, for a downloadable file.
pub fn dump(min: Level) -> String {
    let Some(logger) = LOGGER.get() else { return String::new() };
    let lines = logger.lines.lock().unwrap_or_else(|p| p.into_inner());
    lines
        .iter()
        .filter(|l| l.level <= min)
        .map(|l| format!("{} {:<5} {}: {}\n", crate::text::iso_time(l.at), l.level, l.target, l.text))
        .collect()
}

/// Takes the errors logged since the last call, by fingerprint.
pub fn take_errors() -> HashMap<String, ErrorGroup> {
    LOGGER.get().map(|l| std::mem::take(&mut *l.errors.lock().unwrap_or_else(|p| p.into_inner()))).unwrap_or_default()
}

/// Groups errors that differ only in numbers (ids, counts, durations).
pub fn fingerprint(target: &str, text: &str) -> String {
    let mut out = String::with_capacity(target.len() + 1 + text.len().min(200));
    out.push_str(target);
    out.push('|');
    let mut in_number = false;
    for c in text.chars().take(200) {
        if c.is_ascii_digit() {
            if !in_number {
                out.push('#');
            }
            in_number = true;
        } else {
            in_number = false;
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_ignore_numbers() {
        let a = fingerprint("group_bot", "message 123 in -100456: timeout after 30s");
        let b = fingerprint("group_bot", "message 9 in -100777: timeout after 5s");
        assert_eq!(a, b);
        assert_eq!(a, "group_bot|message # in -#: timeout after #s");
        assert_ne!(a, fingerprint("other", "message 9 in -100777: timeout after 5s"));
    }
}
