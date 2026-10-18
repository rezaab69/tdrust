//! Bot-wide state shared by every bot: settings, feature flags, maintenance
//! mode, sudo users and global bans. It lives in PostgreSQL and is copied
//! into memory, refreshed on every scheduler tick (and at once after a local
//! change), so the hot path reads it without any I/O.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

/// Features the operators can switch off in every group at once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flag {
    /// Joining new groups; when off, the bot leaves groups it is added to.
    NewGroups,
    Fun,
    Captcha,
    Welcome,
    ForceAdd,
    ForceJoin,
    AutoAnswers,
    Filters,
    AntiFlood,
    AntiTabchi,
    Stats,
    AutoJobs,
    LogChannel,
    PvPanel,
}

pub const FLAGS: [Flag; 14] = [
    Flag::NewGroups,
    Flag::Fun,
    Flag::Captcha,
    Flag::Welcome,
    Flag::ForceAdd,
    Flag::ForceJoin,
    Flag::AutoAnswers,
    Flag::Filters,
    Flag::AntiFlood,
    Flag::AntiTabchi,
    Flag::Stats,
    Flag::AutoJobs,
    Flag::LogChannel,
    Flag::PvPanel,
];

impl Flag {
    /// Stable name, used in the stored settings, callback data and texts (`adm.flag_<code>`).
    pub fn code(self) -> &'static str {
        match self {
            Flag::NewGroups => "new_groups",
            Flag::Fun => "fun",
            Flag::Captcha => "captcha",
            Flag::Welcome => "welcome",
            Flag::ForceAdd => "force_add",
            Flag::ForceJoin => "force_join",
            Flag::AutoAnswers => "answers",
            Flag::Filters => "filters",
            Flag::AntiFlood => "flood",
            Flag::AntiTabchi => "tabchi",
            Flag::Stats => "stats",
            Flag::AutoJobs => "auto_jobs",
            Flag::LogChannel => "log_channel",
            Flag::PvPanel => "pv_panel",
        }
    }

    pub fn from_code(code: &str) -> Option<Flag> {
        FLAGS.into_iter().find(|f| f.code() == code)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum GbanMode {
    /// Ban globally banned users wherever they appear.
    Ban,
    /// Only delete their messages.
    Delete,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Maintenance {
    pub enabled: bool,
    /// Pauses everything, protection included; otherwise only commands and panels.
    pub full: bool,
    /// Custom notice (HTML); `None` uses the built-in text.
    pub message: Option<String>,
    pub since: i64,
    /// Ends by itself at this time.
    pub until: Option<i64>,
}

/// Bot-wide settings, stored as `global_settings.settings`. Missing fields
/// take their defaults, so the document can grow without migrations.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct GlobalSettings {
    pub maintenance: Maintenance,
    /// Feature flags that differ from their default (on).
    pub flags: BTreeMap<String, bool>,
    /// Language of new groups and of private chats; `None` uses `DEFAULT_LANG`.
    pub default_lang: Option<String>,
    pub gban_mode: GbanMode,
    /// Tell the operators when a bot is added to or removed from a group.
    pub notify_groups: bool,
    /// Leave new groups with fewer members than this (0: never).
    pub min_members: u32,
    /// Pause between two broadcast messages.
    pub broadcast_delay_ms: u32,
    /// Alert the operators about new errors.
    pub error_alerts: bool,
    /// Alert the operators when a bot goes down or comes back, and on error spikes.
    pub monitor_alerts: bool,
    /// Errors per 10 minutes that count as a spike.
    pub error_spike: u32,
    /// Where alerts go; `None`: the creators' private chats.
    pub alert_chat: Option<i64>,
    /// Hours between automatic backups (0: off).
    pub auto_backup_hours: u32,
    /// How many backup files to keep.
    pub backup_keep: u32,
    /// Days of statistics, audit log, errors and jobs to keep (0: forever).
    pub retention_days: u32,
    /// Log level override (`error`, `warn`, `info`, `debug`); `None`: `RUST_LOG`.
    pub log_level: Option<String>,
    /// When the scheduled backup and prune last ran (unix seconds).
    pub last_backup: i64,
    pub last_prune: i64,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        GlobalSettings {
            maintenance: Maintenance::default(),
            flags: BTreeMap::new(),
            default_lang: None,
            gban_mode: GbanMode::Ban,
            notify_groups: true,
            min_members: 0,
            broadcast_delay_ms: 100,
            error_alerts: true,
            monitor_alerts: true,
            error_spike: 50,
            alert_chat: None,
            auto_backup_hours: 0,
            backup_keep: 7,
            retention_days: 0,
            log_level: None,
            last_backup: 0,
            last_prune: 0,
        }
    }
}

impl GlobalSettings {
    pub fn flag(&self, flag: Flag) -> bool {
        self.flags.get(flag.code()).copied().unwrap_or(true)
    }

    pub fn set_flag(&mut self, flag: Flag, on: bool) {
        if on {
            self.flags.remove(flag.code());
        } else {
            self.flags.insert(flag.code().to_owned(), false);
        }
    }

    /// Whether maintenance is on right now (a finished timed one is off).
    pub fn maintenance_on(&self, now: i64) -> bool {
        let m = &self.maintenance;
        m.enabled && m.until.is_none_or(|until| until > now)
    }
}

/// The in-memory copy of the bot-wide state.
#[derive(Debug, Default)]
pub struct Global {
    pub settings: GlobalSettings,
    /// Sudo users added from the panel (the creators come from `BOT_SUDO_IDS`).
    pub sudoers: HashSet<i64>,
    pub gbans: HashSet<i64>,
}

impl Global {
    pub fn flag(&self, flag: Flag) -> bool {
        self.settings.flag(flag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_default_on_and_round_trip() {
        let mut s = GlobalSettings::default();
        assert!(FLAGS.iter().all(|f| s.flag(*f)));
        s.set_flag(Flag::Captcha, false);
        let back: GlobalSettings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert!(!back.flag(Flag::Captcha) && back.flag(Flag::Fun));
        s.set_flag(Flag::Captcha, true);
        assert!(s.flags.is_empty(), "flags at their default are not stored");
        let codes: HashSet<_> = FLAGS.iter().map(|f| f.code()).collect();
        assert_eq!(codes.len(), FLAGS.len());
        assert!(FLAGS.iter().all(|f| Flag::from_code(f.code()) == Some(*f)));
    }

    #[test]
    fn settings_survive_missing_fields() {
        let s: GlobalSettings = serde_json::from_str(r#"{"notify_groups":false}"#).unwrap();
        assert!(!s.notify_groups);
        assert_eq!(s.backup_keep, 7);
        assert_eq!(s.gban_mode, GbanMode::Ban);
    }

    #[test]
    fn timed_maintenance_ends() {
        let mut s = GlobalSettings::default();
        assert!(!s.maintenance_on(100));
        s.maintenance.enabled = true;
        assert!(s.maintenance_on(100));
        s.maintenance.until = Some(50);
        assert!(!s.maintenance_on(100));
    }
}
