//! Domain types: group settings (stored as JSONB), ranks, punishments and
//! stored message content.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tdrust::types::ChatPermissions;

use crate::locks::Lock;

/// Privilege levels, lowest first. Stored ranks use `as_db`/`from_db`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Rank {
    Member,
    Exempt,
    Vip,
    Mod,
    Owner,
    /// The group's creator in Telegram.
    Creator,
    /// Bot operators from `BOT_SUDO_IDS`.
    Sudo,
}

impl Rank {
    pub fn as_db(self) -> i16 {
        match self {
            Rank::Exempt => 1,
            Rank::Vip => 2,
            Rank::Mod => 3,
            Rank::Owner => 4,
            _ => 0,
        }
    }

    pub fn from_db(value: i16) -> Rank {
        match value {
            1 => Rank::Exempt,
            2 => Rank::Vip,
            3 => Rank::Mod,
            4 => Rank::Owner,
            _ => Rank::Member,
        }
    }

    /// i18n key of the rank's name.
    pub fn key(self) -> &'static str {
        match self {
            Rank::Member => "rank.member",
            Rank::Exempt => "rank.exempt",
            Rank::Vip => "rank.vip",
            Rank::Mod => "rank.mod",
            Rank::Owner => "rank.owner",
            Rank::Creator => "rank.creator",
            Rank::Sudo => "rank.sudo",
        }
    }
}

/// How a rule violation is punished.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Punish {
    Delete,
    Warn,
    Mute,
    /// Mute for a number of seconds.
    TempMute(i64),
    Ban,
}

impl Punish {
    pub fn key(self) -> &'static str {
        match self {
            Punish::Delete => "punish.delete",
            Punish::Warn => "punish.warn",
            Punish::Mute => "punish.mute",
            Punish::TempMute(_) => "punish.tempmute",
            Punish::Ban => "punish.ban",
        }
    }

    /// Filter actions as stored in the `filters` table.
    pub fn as_filter(self) -> &'static str {
        match self {
            Punish::Warn => "warn",
            Punish::Mute | Punish::TempMute(_) => "mute",
            Punish::Ban => "ban",
            Punish::Delete => "delete",
        }
    }

    pub fn from_filter(value: &str) -> Punish {
        match value {
            "warn" => Punish::Warn,
            "mute" => Punish::Mute,
            "ban" => Punish::Ban,
            _ => Punish::Delete,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Audience {
    All,
    Ranked,
    Owners,
}

impl Audience {
    pub fn as_db(self) -> &'static str {
        match self {
            Audience::All => "all",
            Audience::Ranked => "ranked",
            Audience::Owners => "owners",
        }
    }

    pub fn from_db(value: &str) -> Audience {
        match value {
            "ranked" => Audience::Ranked,
            "owners" => Audience::Owners,
            _ => Audience::All,
        }
    }

    pub fn allows(self, rank: Rank) -> bool {
        match self {
            Audience::All => true,
            Audience::Ranked => rank >= Rank::Vip,
            Audience::Owners => rank >= Rank::Owner,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum MediaKind {
    Photo,
    Video,
    Animation,
    Audio,
    Voice,
    VideoNote,
    Sticker,
    Document,
}

/// A message the bot can store and send again: HTML text, or media by its
/// remote file id with an HTML caption.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub enum Content {
    Text(String),
    Media { kind: MediaKind, file_id: String, caption: String },
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct GroupLock {
    pub locked: bool,
    pub until: Option<i64>,
    pub saved_permissions: Option<ChatPermissions>,
    /// Daily window in local minutes: (lock at, unlock at).
    pub auto: Option<(u16, u16)>,
    pub auto_active: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Rate {
    pub enabled: bool,
    pub count: u32,
    pub seconds: u32,
    pub punish: Punish,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Strict {
    pub enabled: bool,
    pub punish: Punish,
    pub types: BTreeSet<Lock>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabchiTiming {
    Join,
    Message,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Tabchi {
    pub punish: Punish,
    pub timing: TabchiTiming,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Warns {
    pub max: u32,
    pub punish: Punish,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct ForceAdd {
    pub enabled: bool,
    pub count: u32,
    pub expire_days: Option<u32>,
    pub text: Option<String>,
    /// Only members who joined after `since` must add.
    pub new_only: bool,
    pub since: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct ForceJoin {
    pub enabled: bool,
    pub channel_id: Option<i64>,
    pub channel_title: String,
    pub channel_link: String,
    pub text: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Welcome {
    pub enabled: bool,
    pub text: Option<String>,
    pub media: Option<Content>,
    /// Seconds before the welcome message is deleted; 0 keeps it.
    pub delete_after: u32,
    pub goodbye_enabled: bool,
    pub goodbye_text: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReportMode {
    Group,
    Private,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Security {
    pub captcha: bool,
    pub captcha_minutes: u32,
    pub anti_betrayal: bool,
    /// Bans and mutes an admin may do per hour before being demoted.
    pub betrayal_limit: u32,
    pub duplicate_hours: u32,
    pub duplicate_min_len: u32,
    pub report_mode: ReportMode,
    pub delete_reason: bool,
    pub log_chat: Option<i64>,
}

/// Everything configurable about a group, stored as `groups.settings`.
/// Unknown or missing fields fall back to the defaults, so the document can
/// evolve without migrations.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    pub lang: String,
    /// UTC offset in minutes.
    pub tz: i32,
    pub locks: BTreeSet<Lock>,
    pub media_lock_until: Option<i64>,
    /// Media is deleted this many minutes after being sent.
    pub temp_media_minutes: Option<u32>,
    pub group_lock: GroupLock,
    pub flood: Rate,
    pub limit: Rate,
    pub max_chars: u32,
    pub min_chars: u32,
    pub strict: Strict,
    pub tabchi: Tabchi,
    pub warns: Warns,
    pub force_add: ForceAdd,
    pub force_join: ForceJoin,
    pub welcome: Welcome,
    pub answer_cooldown: u32,
    pub rules: Option<String>,
    pub link: Option<String>,
    /// Local minutes of the day at which history is cleaned / stats are posted.
    pub auto_clean: Vec<u16>,
    pub auto_stats: Vec<u16>,
    pub security: Security,
    /// The last local minute (day * 1440 + minute) a scheduled job ran, per job.
    pub last_auto_clean: i64,
    pub last_auto_stats: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            lang: "fa".into(),
            tz: 210,
            locks: [Lock::Link, Lock::Bot, Lock::Forward].into_iter().collect(),
            media_lock_until: None,
            temp_media_minutes: None,
            group_lock: GroupLock::default(),
            flood: Rate::default(),
            limit: Rate { enabled: false, count: 10, seconds: 60, punish: Punish::Delete },
            max_chars: 0,
            min_chars: 0,
            strict: Strict::default(),
            tabchi: Tabchi::default(),
            warns: Warns::default(),
            force_add: ForceAdd::default(),
            force_join: ForceJoin::default(),
            welcome: Welcome::default(),
            answer_cooldown: 30,
            rules: None,
            link: None,
            auto_clean: Vec::new(),
            auto_stats: Vec::new(),
            security: Security::default(),
            last_auto_clean: 0,
            last_auto_stats: 0,
        }
    }
}

impl Default for Rate {
    fn default() -> Self {
        Rate { enabled: true, count: 5, seconds: 3, punish: Punish::TempMute(30 * 60) }
    }
}

impl Default for Strict {
    fn default() -> Self {
        Strict { enabled: false, punish: Punish::Warn, types: BTreeSet::new() }
    }
}

impl Default for Tabchi {
    fn default() -> Self {
        Tabchi { punish: Punish::Ban, timing: TabchiTiming::Join }
    }
}

impl Default for Warns {
    fn default() -> Self {
        Warns { max: 3, punish: Punish::Mute }
    }
}

impl Default for ForceAdd {
    fn default() -> Self {
        ForceAdd { enabled: false, count: 3, expire_days: None, text: None, new_only: false, since: 0 }
    }
}

impl Default for Welcome {
    fn default() -> Self {
        Welcome {
            enabled: true,
            text: None,
            media: None,
            delete_after: 0,
            goodbye_enabled: false,
            goodbye_text: None,
        }
    }
}

impl Default for Security {
    fn default() -> Self {
        Security {
            captcha: false,
            captcha_minutes: 3,
            anti_betrayal: false,
            betrayal_limit: 10,
            duplicate_hours: 12,
            duplicate_min_len: 30,
            report_mode: ReportMode::Group,
            delete_reason: true,
            log_chat: None,
        }
    }
}

impl Settings {
    pub fn locked(&self, lock: Lock) -> bool {
        self.locks.contains(&lock)
    }

    /// Whether media is locked right now (permanently or until a time).
    pub fn media_locked(&self, now: i64) -> bool {
        self.locked(Lock::Media) || self.media_lock_until.is_some_and(|until| until > now)
    }

    pub fn group_locked(&self, now: i64) -> bool {
        let lock = &self.group_lock;
        lock.auto_active || (lock.locked && lock.until.is_none_or(|until| until > now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_missing_fields() {
        let settings: Settings = serde_json::from_str(r#"{"lang":"en","locks":["Link"]}"#).unwrap();
        assert_eq!(settings.lang, "en");
        assert!(settings.locked(Lock::Link));
        assert_eq!(settings.warns.max, 3);
        assert_eq!(settings.flood.count, 5);
    }

    #[test]
    fn settings_round_trip() {
        let mut settings = Settings::default();
        settings.group_lock.auto = Some((0, 480));
        settings.strict.types.insert(Lock::Forward);
        let json = serde_json::to_value(&settings).unwrap();
        let back: Settings = serde_json::from_value(json).unwrap();
        assert_eq!(back.group_lock.auto, Some((0, 480)));
        assert!(back.strict.types.contains(&Lock::Forward));
    }

    #[test]
    fn ranks_are_ordered() {
        assert!(Rank::Sudo > Rank::Creator && Rank::Creator > Rank::Owner && Rank::Mod > Rank::Vip);
        assert_eq!(Rank::from_db(Rank::Mod.as_db()), Rank::Mod);
    }
}
