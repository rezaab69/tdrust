//! Runtime metrics. Events are counted in process-local atomics (no I/O on
//! the hot path) and flushed every scheduler tick into Redis hashes:
//!
//! | key | fields | kept |
//! | --- | --- | --- |
//! | `mon:m:<unix minute>` | `<bot>:<metric>` | 3 hours |
//! | `mon:h:<unix hour>` | `<bot>:<metric>` | 8 days |
//! | `mon:c:<unix hour>` | command name | 8 days |
//!
//! Every bot of the fleet writes into the same keys, so the monitoring
//! screen shows the whole fleet and each bot's share.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Mutex;

use crate::bot::{soft, Bot};
use crate::text::now;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Metric {
    /// Every update from TDLib.
    Updates,
    /// Incoming messages handled.
    Messages,
    Callbacks,
    Commands,
    /// Messages the bot deleted.
    Deleted,
    /// Warnings, mutes and bans applied.
    Punished,
    SendFailed,
    Warnings,
    Errors,
    /// Message handlers finished, and their total run time in milliseconds.
    Handled,
    HandlerMs,
}

pub const METRICS_ALL: [Metric; 11] = [
    Metric::Updates,
    Metric::Messages,
    Metric::Callbacks,
    Metric::Commands,
    Metric::Deleted,
    Metric::Punished,
    Metric::SendFailed,
    Metric::Warnings,
    Metric::Errors,
    Metric::Handled,
    Metric::HandlerMs,
];

impl Metric {
    pub fn code(self) -> &'static str {
        match self {
            Metric::Updates => "upd",
            Metric::Messages => "msg",
            Metric::Callbacks => "cb",
            Metric::Commands => "cmd",
            Metric::Deleted => "del",
            Metric::Punished => "pun",
            Metric::SendFailed => "sf",
            Metric::Warnings => "warn",
            Metric::Errors => "err",
            Metric::Handled => "hdl",
            Metric::HandlerMs => "hms",
        }
    }

    fn index(self) -> usize {
        METRICS_ALL.iter().position(|m| *m == self).expect("every metric is listed")
    }
}

struct Counters {
    /// Since the last flush.
    pending: [AtomicU64; METRICS_ALL.len()],
    /// Since this process started.
    total: [AtomicU64; METRICS_ALL.len()],
    commands: Mutex<BTreeMap<String, u64>>,
}

static COUNTERS: Counters = Counters {
    pending: [const { AtomicU64::new(0) }; METRICS_ALL.len()],
    total: [const { AtomicU64::new(0) }; METRICS_ALL.len()],
    commands: Mutex::new(BTreeMap::new()),
};

/// When the scheduler last ran its jobs here (unix seconds) and how long it took.
pub static TICK_AT: AtomicI64 = AtomicI64::new(0);
pub static TICK_MS: AtomicU64 = AtomicU64::new(0);

pub fn add(metric: Metric, n: u64) {
    let i = metric.index();
    COUNTERS.pending[i].fetch_add(n, Ordering::Relaxed);
    COUNTERS.total[i].fetch_add(n, Ordering::Relaxed);
}

pub fn inc(metric: Metric) {
    add(metric, 1);
}

pub fn command(name: &str) {
    inc(Metric::Commands);
    let mut commands = COUNTERS.commands.lock().unwrap_or_else(|p| p.into_inner());
    *commands.entry(name.to_owned()).or_default() += 1;
}

/// Counts since this process started.
pub fn since_start(metric: Metric) -> u64 {
    COUNTERS.total[metric.index()].load(Ordering::Relaxed)
}

pub fn minute_key(minute: i64) -> String {
    format!("mon:m:{minute}")
}

pub fn hour_key(hour: i64) -> String {
    format!("mon:h:{hour}")
}

pub fn command_key(hour: i64) -> String {
    format!("mon:c:{hour}")
}

/// Moves the pending counts into Redis. Counts are lost if Redis is down.
pub async fn flush(bot: &Bot) {
    let fields: Vec<(String, i64)> = METRICS_ALL
        .iter()
        .filter_map(|m| {
            let n = COUNTERS.pending[m.index()].swap(0, Ordering::Relaxed);
            (n > 0).then(|| (format!("{}:{}", bot.me, m.code()), n as i64))
        })
        .collect();
    let commands: Vec<(String, i64)> = std::mem::take(&mut *COUNTERS.commands.lock().unwrap_or_else(|p| p.into_inner()))
        .into_iter()
        .map(|(name, n)| (name, n as i64))
        .collect();
    let t = now();
    if !fields.is_empty() {
        soft(bot.cache.hincr_many(&minute_key(t / 60), &fields, 3 * 3600).await, "minute metrics");
        soft(bot.cache.hincr_many(&hour_key(t / 3600), &fields, 8 * 86_400).await, "hour metrics");
    }
    if !commands.is_empty() {
        soft(bot.cache.hincr_many(&command_key(t / 3600), &commands, 8 * 86_400).await, "command metrics");
    }
}

/// A time range of the monitoring screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Range {
    Hour,
    Day,
    Week,
}

impl Range {
    pub fn code(self) -> &'static str {
        match self {
            Range::Hour => "1h",
            Range::Day => "24h",
            Range::Week => "7d",
        }
    }

    pub fn from_code(code: &str) -> Range {
        match code {
            "24h" => Range::Day,
            "7d" => Range::Week,
            _ => Range::Hour,
        }
    }

    /// The Redis keys of the range, oldest first.
    pub fn keys(self, now: i64) -> Vec<String> {
        match self {
            Range::Hour => (0..60).rev().map(|i| minute_key(now / 60 - i)).collect(),
            Range::Day => (0..24).rev().map(|i| hour_key(now / 3600 - i)).collect(),
            Range::Week => (0..168).rev().map(|i| hour_key(now / 3600 - i)).collect(),
        }
    }

    pub fn command_keys(self, now: i64) -> Vec<String> {
        let hours = match self {
            Range::Hour => 1,
            Range::Day => 24,
            Range::Week => 168,
        };
        (0..hours).map(|i| command_key(now / 3600 - i)).collect()
    }
}

/// Metrics of a range: per bucket (oldest first) and per bot.
#[derive(Debug, Default)]
pub struct Series {
    /// metric code -> value per bucket, summed over bots.
    pub buckets: HashMap<&'static str, Vec<i64>>,
    /// (bot, metric code) -> total over the range.
    pub per_bot: BTreeMap<(i64, &'static str), i64>,
}

impl Series {
    pub fn from_buckets(hashes: &[HashMap<String, i64>]) -> Series {
        let mut series = Series::default();
        for m in METRICS_ALL {
            series.buckets.insert(m.code(), vec![0; hashes.len()]);
        }
        for (i, hash) in hashes.iter().enumerate() {
            for (field, value) in hash {
                let Some((bot, code)) = field.split_once(':') else { continue };
                let Some(metric) = METRICS_ALL.iter().find(|m| m.code() == code) else { continue };
                let Ok(bot) = bot.parse::<i64>() else { continue };
                series.buckets.get_mut(metric.code()).expect("inserted above")[i] += value;
                *series.per_bot.entry((bot, metric.code())).or_default() += value;
            }
        }
        series
    }

    pub fn total(&self, metric: Metric) -> i64 {
        self.buckets.get(metric.code()).map_or(0, |v| v.iter().sum())
    }

    pub fn values(&self, metric: Metric) -> &[i64] {
        self.buckets.get(metric.code()).map_or(&[], Vec::as_slice)
    }
}

/// A one-line chart of `values`, squeezed into at most `width` columns.
pub fn sparkline(values: &[i64], width: usize) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if values.is_empty() || width == 0 {
        return String::new();
    }
    let per = values.len().div_ceil(width);
    let columns: Vec<i64> = values.chunks(per).map(|c| c.iter().sum()).collect();
    let max = columns.iter().copied().max().unwrap_or(0);
    columns
        .iter()
        .map(|&v| if max <= 0 || v <= 0 { BARS[0] } else { BARS[((v * 7 + max - 1) / max).clamp(0, 7) as usize] })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparkline_scales_and_squeezes() {
        assert_eq!(sparkline(&[0, 1, 2, 4], 4), "▁▃▅█");
        assert_eq!(sparkline(&[0, 0], 2), "▁▁");
        assert_eq!(sparkline(&[1; 60], 30).chars().count(), 30);
        assert_eq!(sparkline(&[], 10), "");
    }

    #[test]
    fn series_sums_bots_and_buckets() {
        let bucket = |pairs: &[(&str, i64)]| pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect::<HashMap<_, _>>();
        let series = Series::from_buckets(&[bucket(&[("1:msg", 2), ("2:msg", 3)]), bucket(&[("1:msg", 1), ("bad", 9), ("1:zzz", 9)])]);
        assert_eq!(series.values(Metric::Messages), &[5, 1]);
        assert_eq!(series.total(Metric::Messages), 6);
        assert_eq!(series.per_bot.get(&(1, "msg")), Some(&3));
    }

    #[test]
    fn ranges_have_their_buckets() {
        assert_eq!(Range::Hour.keys(3600 * 100).len(), 60);
        assert_eq!(Range::Week.keys(3600 * 100).len(), 168);
        assert_eq!(Range::Day.keys(3600 * 100).last(), Some(&hour_key(100)));
        assert_eq!(Range::from_code("7d"), Range::Week);
    }
}
