//! The shared bot context: storage, texts, sending, ranks and moderation actions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::Result;
use tdrust::prelude::*;
use tdrust::types::{
    ChatMemberStatusLeft, ChatMemberStatusMember, ChatMemberStatusRestricted, ChatPermissions, ChatType,
    InputMessageReplyToMessage, MessageSenderUser, ReplyMarkup,
};
use tokio::sync::oneshot;

use crate::admin::global::{Flag, Global};
use crate::admin::metrics::{self, Metric};
use crate::cache::Cache;
use crate::config::Config;
use crate::content;
use crate::db::Repo;
use crate::i18n::Texts;
use crate::model::{Punish, Rank, Settings};
use crate::text::{format_duration, h, mention_html, now};

const SETTINGS_TTL: u64 = 3600;

pub struct Bot {
    pub client: Client,
    pub repo: Repo,
    pub cache: Cache,
    pub config: Config,
    pub me: i64,
    pub username: String,
    pub sent: SentMessages,
    /// Whether this bot serves a group, cached briefly: chat -> (owner, checked at).
    pub owners: Mutex<HashMap<i64, (bool, std::time::Instant)>>,
    /// Every bot sharing the database, cached briefly.
    pub fleet: Mutex<Option<(Arc<Fleet>, std::time::Instant)>>,
    /// Bot-wide settings, flags, sudo users and global bans (see `admin::global`).
    pub global: RwLock<Arc<Global>>,
    /// When this process started (unix seconds).
    pub booted: i64,
}

/// Bots sharing the database: id -> username.
pub type Fleet = HashMap<i64, String>;

/// How long a handled update stays claimed. Telegram keeps a bot's pending
/// updates for a day, so no bot can receive an update after its claim expires.
pub const UPDATE_CLAIM_SECS: u64 = 25 * 3600;

/// Supergroup message ids are the same for every bot; basic group message ids
/// are per account.
pub fn is_supergroup(chat_id: i64) -> bool {
    chat_id < -1_000_000_000_000
}

/// A snapshot of one group's settings and texts, taken per update.
pub struct Group {
    pub id: i64,
    pub s: Settings,
    pub tx: Texts,
}

impl Group {
    pub fn t(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.tx.t(key, args)
    }

    pub fn s(&self, key: &str) -> String {
        self.tx.s(key)
    }
}

pub fn soft<T>(result: Result<T>, what: &str) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(err) => {
            log::warn!("{what}: {err:#}");
            None
        }
    }
}

impl Bot {
    // ------------------------------------------------------------- ownership

    /// Whether this bot serves `chat_id`, i.e. runs its scheduled jobs. Every
    /// bot in a group handles its updates (see `first_to_handle`), but one bot
    /// owns it for the jobs; the others take over if the owner is down.
    pub async fn owns(&self, chat_id: i64) -> bool {
        const FRESH: Duration = Duration::from_secs(30);
        if let Some((owned, at)) = self.owners.lock().unwrap_or_else(|p| p.into_inner()).get(&chat_id) {
            if at.elapsed() < FRESH {
                return *owned;
            }
        }
        let owned = match self.repo.claim(chat_id, self.me, &self.default_lang()).await {
            Ok(owned) => owned,
            Err(err) => {
                log::error!("cannot check who serves {chat_id}: {err:#}");
                return false;
            }
        };
        self.owners.lock().unwrap_or_else(|p| p.into_inner()).insert(chat_id, (owned, std::time::Instant::now()));
        owned
    }

    pub fn forget_owner(&self, chat_id: i64) {
        self.owners.lock().unwrap_or_else(|p| p.into_inner()).remove(&chat_id);
    }

    /// Several bots in one group all receive its updates; the first to claim
    /// one (by `key`) handles it, so nothing is done twice. Bots that are not
    /// administrators there leave the work to those that are, or to the
    /// owner. Without Redis, only the owner acts.
    pub async fn first_to_handle(&self, chat_id: i64, key: &str, ttl_secs: u64) -> bool {
        let owned = self.owns(chat_id).await;
        if !owned && !self.tg_admins(chat_id).await.contains_key(&self.me) {
            return false;
        }
        match self.cache.once(&format!("upd:{key}"), ttl_secs).await {
            Ok(first) => first,
            Err(err) => {
                log::warn!("update claim: {err:#}");
                owned
            }
        }
    }

    /// Every bot sharing the database, this one included, by id.
    pub async fn fleet(&self) -> Arc<Fleet> {
        const FRESH: Duration = Duration::from_secs(60);
        let cached = self.fleet.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some((fleet, at)) = &cached {
            if at.elapsed() < FRESH {
                return fleet.clone();
            }
        }
        let fleet = match self.repo.fleet().await {
            Ok(rows) => Arc::new(rows.into_iter().collect::<Fleet>()),
            Err(err) => {
                log::warn!("cannot list bots: {err:#}");
                // Retry soon, keeping what we knew.
                return cached.map(|(fleet, _)| fleet).unwrap_or_default();
            }
        };
        *self.fleet.lock().unwrap_or_else(|p| p.into_inner()) = Some((fleet.clone(), std::time::Instant::now()));
        fleet
    }

    /// Whether `user_id` is this bot or another bot sharing the database.
    pub async fn is_fleet(&self, user_id: i64) -> bool {
        user_id == self.me || self.fleet().await.contains_key(&user_id)
    }

    /// Whether `username` belongs to another bot sharing the database.
    pub async fn is_other_fleet_bot(&self, username: &str) -> bool {
        !username.eq_ignore_ascii_case(&self.username)
            && self.fleet().await.values().any(|name| !name.is_empty() && name.eq_ignore_ascii_case(username))
    }

    /// Records (at most hourly) that this bot is in a group, for fail-over.
    pub async fn note_membership(&self, chat_id: i64) {
        if soft(self.cache.once(&format!("gm:{chat_id}:{}", self.me), 3600).await, "membership throttle") != Some(false) {
            soft(self.repo.record_membership(chat_id, self.me).await, "record membership");
        }
    }

    // ------------------------------------------------------ bot-wide state

    pub fn global(&self) -> Arc<Global> {
        self.global.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Reloads the bot-wide state from PostgreSQL (every tick, and after changes).
    pub async fn refresh_global(&self) {
        match self.repo.load_global().await {
            Ok(global) => {
                crate::admin::logs::set_level(global.settings.log_level.as_deref());
                *self.global.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(global);
            }
            Err(err) => log::warn!("cannot load the global settings: {err:#}"),
        }
    }

    /// Whether a feature is enabled bot-wide.
    pub fn flag(&self, flag: Flag) -> bool {
        self.global().flag(flag)
    }

    /// The language of new groups and private chats.
    pub fn default_lang(&self) -> String {
        self.global()
            .settings
            .default_lang
            .clone()
            .filter(|lang| crate::i18n::is_language(lang))
            .unwrap_or_else(|| self.config.default_lang.clone())
    }

    /// A creator of the bot (`BOT_SUDO_IDS`).
    pub fn is_creator(&self, user_id: i64) -> bool {
        self.config.sudo_ids.contains(&user_id)
    }

    /// A creator or a sudo user added from the panel.
    pub fn is_sudo(&self, user_id: i64) -> bool {
        self.is_creator(user_id) || self.global().sudoers.contains(&user_id)
    }

    // -------------------------------------------------------------- settings

    pub async fn settings(&self, chat_id: i64) -> Settings {
        let key = format!("s:{chat_id}");
        if let Some(Some(settings)) = soft(self.cache.get_json::<Settings>(&key).await, "settings cache") {
            return settings;
        }
        match self.repo.settings(chat_id, &self.default_lang()).await {
            Ok(settings) => {
                soft(self.cache.set_json(&key, &settings, SETTINGS_TTL).await, "settings cache");
                settings
            }
            Err(err) => {
                log::error!("cannot load settings of {chat_id}: {err:#}");
                Settings { lang: self.default_lang(), ..Settings::default() }
            }
        }
    }

    /// Changes settings atomically in PostgreSQL and refreshes the cache.
    pub async fn update<R: Send>(&self, chat_id: i64, change: impl FnOnce(&mut Settings) -> R + Send) -> Result<R> {
        let (settings, result) = self.repo.update_settings(chat_id, &self.default_lang(), change).await?;
        soft(self.cache.set_json(&format!("s:{chat_id}"), &settings, SETTINGS_TTL).await, "settings cache");
        Ok(result)
    }

    pub async fn texts(&self, chat_id: i64, lang: &str) -> Texts {
        let key = format!("txt:{chat_id}:{lang}");
        let overrides = match soft(self.cache.get_json::<HashMap<String, String>>(&key).await, "texts cache").flatten() {
            Some(overrides) => overrides,
            None => {
                let overrides = soft(self.repo.texts(chat_id, lang).await, "custom texts").unwrap_or_default();
                soft(self.cache.set_json(&key, &overrides, SETTINGS_TTL).await, "texts cache");
                overrides
            }
        };
        Texts::new(lang, Arc::new(overrides))
    }

    pub async fn invalidate_texts(&self, chat_id: i64, lang: &str) {
        soft(self.cache.del(&format!("txt:{chat_id}:{lang}")).await, "texts cache");
    }

    pub async fn group(&self, chat_id: i64) -> Group {
        let s = self.settings(chat_id).await;
        let tx = self.texts(chat_id, &s.lang).await;
        Group { id: chat_id, s, tx }
    }

    // --------------------------------------------------------------- sending

    pub async fn send_input(
        &self,
        chat_id: i64,
        reply_to: Option<i64>,
        markup: Option<ReplyMarkup>,
        input: impl Into<tdrust::types::InputMessageContent>,
    ) -> Option<Message> {
        let reply = reply_to.map(|message_id| InputMessageReplyToMessage { message_id, ..Default::default() }.into());
        match self.client.send_message(chat_id, None, reply, None, markup, input).await {
            Ok(message) => Some(message),
            Err(err) => {
                metrics::inc(Metric::SendFailed);
                log::warn!("cannot send to {chat_id}: {err}");
                None
            }
        }
    }

    pub async fn send(&self, chat_id: i64, html: &str) -> Option<Message> {
        self.send_input(chat_id, None, None, InputMessageContent::text(content::html(html))).await
    }

    pub async fn send_markup(&self, chat_id: i64, html: &str, markup: ReplyMarkup, reply_to: Option<i64>) -> Option<Message> {
        self.send_input(chat_id, reply_to, Some(markup), InputMessageContent::text(content::html(html))).await
    }

    pub async fn reply(&self, to: &Message, html: &str) -> Option<Message> {
        self.send_input(to.chat_id, Some(to.id), None, InputMessageContent::text(content::html(html))).await
    }

    /// Sends a message that deletes itself after `seconds`.
    pub async fn notice(&self, chat_id: i64, html: &str, seconds: u64) {
        if let Some(message) = self.send(chat_id, html).await {
            self.delete_later(message, seconds);
        }
    }

    /// Deletes a message the bot sent once it is confirmed and `seconds` have passed.
    pub fn delete_later(&self, message: Message, seconds: u64) {
        let client = self.client.clone();
        let sent = self.sent.clone();
        tokio::spawn(async move {
            let message = if message.sending_state.is_some() { sent.confirmed(&message).await } else { Some(message) };
            let Some(message) = message else { return };
            tokio::time::sleep(Duration::from_secs(seconds)).await;
            if let Err(err) = client.delete_messages(message.chat_id, vec![message.id], true).await {
                log::debug!("cannot delete {} in {}: {err}", message.id, message.chat_id);
            }
        });
    }

    /// Deletes someone else's message after `seconds` (e.g. temporary media).
    pub fn delete_message_later(&self, chat_id: i64, message_id: i64, seconds: u64) {
        let client = self.client.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(seconds)).await;
            let _ = client.delete_messages(chat_id, vec![message_id], true).await;
        });
    }

    pub async fn delete(&self, chat_id: i64, ids: Vec<i64>) -> usize {
        let mut deleted = 0;
        for chunk in ids.chunks(100) {
            match self.client.delete_messages(chat_id, chunk.to_vec(), true).await {
                Ok(_) => {
                    deleted += chunk.len();
                    metrics::add(Metric::Deleted, chunk.len() as u64);
                }
                Err(err) => log::debug!("cannot delete messages in {chat_id}: {err}"),
            }
        }
        deleted
    }

    // ----------------------------------------------------------------- users

    pub async fn remember_user(&self, user: &User) {
        if soft(self.cache.once(&format!("seen:{}", user.id), 3600).await, "user cache") == Some(false) {
            return;
        }
        let username = user.usernames.as_ref().map(|u| u.editable_username.as_str()).filter(|u| !u.is_empty());
        let is_bot = matches!(user.r#type, tdrust::types::UserType::Bot(_));
        soft(self.repo.upsert_user(user.id, &user.first_name, &user.last_name, username, is_bot).await, "store user");
    }

    pub async fn display_name(&self, user_id: i64) -> String {
        if let Ok(user) = self.client.get_user(user_id).await {
            let name = format!("{} {}", user.first_name, user.last_name).trim().to_owned();
            if !name.is_empty() {
                return name;
            }
        }
        if let Some(Some(user)) = soft(self.repo.user(user_id).await, "user") {
            return format!("{} {}", user.first_name, user.last_name).trim().to_owned();
        }
        user_id.to_string()
    }

    pub async fn nicknames(&self, chat_id: i64) -> HashMap<i64, String> {
        let key = format!("nick:{chat_id}");
        if let Some(Some(map)) = soft(self.cache.get_json::<HashMap<i64, String>>(&key).await, "nick cache") {
            return map;
        }
        let map = soft(self.repo.nicknames(chat_id).await, "nicknames").unwrap_or_default();
        soft(self.cache.set_json(&key, &map, SETTINGS_TTL).await, "nick cache");
        map
    }

    /// A clickable mention, using the member's nickname in this group if set.
    pub async fn mention(&self, chat_id: i64, user_id: i64) -> String {
        if user_id == 0 {
            return "—".into();
        }
        let name = match self.nicknames(chat_id).await.remove(&user_id) {
            Some(nickname) => nickname,
            None => self.display_name(user_id).await,
        };
        mention_html(user_id, &name)
    }

    // ----------------------------------------------------------------- ranks

    /// Telegram administrators of a chat: user -> is creator.
    pub async fn tg_admins(&self, chat_id: i64) -> HashMap<i64, bool> {
        let key = format!("tga:{chat_id}");
        if let Some(Some(map)) = soft(self.cache.get_json::<HashMap<i64, bool>>(&key).await, "admin cache") {
            return map;
        }
        let (map, ttl) = match self.client.get_chat_administrators(chat_id).await {
            Ok(admins) => (admins.administrators.iter().map(|a| (a.user_id, a.is_owner)).collect(), 300),
            Err(err) => {
                log::debug!("cannot list admins of {chat_id}: {err}");
                (HashMap::new(), 60)
            }
        };
        soft(self.cache.set_json(&key, &map, ttl).await, "admin cache");
        map
    }

    pub async fn stored_ranks(&self, chat_id: i64) -> HashMap<i64, i16> {
        let key = format!("rk:{chat_id}");
        if let Some(Some(map)) = soft(self.cache.get_json::<HashMap<i64, i16>>(&key).await, "rank cache") {
            return map;
        }
        let map: HashMap<i64, i16> = soft(self.repo.ranks(chat_id).await, "ranks")
            .unwrap_or_default()
            .into_iter()
            .map(|(user, rank, _)| (user, rank.as_db()))
            .collect();
        soft(self.cache.set_json(&key, &map, 600).await, "rank cache");
        map
    }

    pub async fn rank(&self, chat_id: i64, user_id: i64) -> Rank {
        // Bots sharing the database are never targeted by each other.
        if self.is_sudo(user_id) || self.is_fleet(user_id).await {
            return Rank::Sudo;
        }
        let admins = self.tg_admins(chat_id).await;
        let telegram = match admins.get(&user_id) {
            Some(true) => return Rank::Creator,
            Some(false) => Rank::Mod,
            None => Rank::Member,
        };
        let stored = self.stored_ranks(chat_id).await.get(&user_id).map_or(Rank::Member, |r| Rank::from_db(*r));
        telegram.max(stored)
    }

    pub async fn invalidate_ranks(&self, chat_id: i64) {
        soft(self.cache.del(&format!("rk:{chat_id}")).await, "rank cache");
    }

    pub async fn invalidate_admins(&self, chat_id: i64) {
        soft(self.cache.del(&format!("tga:{chat_id}")).await, "admin cache");
    }

    // ------------------------------------------------------- telegram actions

    /// Mutes a member until a unix time (0 = forever).
    pub async fn restrict(&self, chat_id: i64, user_id: i64, until: i64) -> tdrust::Result<()> {
        let status = ChatMemberStatusRestricted {
            is_member: true,
            restricted_until_date: i32::try_from(until).unwrap_or(0),
            permissions: ChatPermissions::default(),
        };
        self.client.set_chat_member_status(chat_id, MessageSenderUser { user_id }, status).await.map(drop)
    }

    pub async fn unrestrict(&self, chat_id: i64, user_id: i64) -> tdrust::Result<()> {
        self.client
            .set_chat_member_status(chat_id, MessageSenderUser { user_id }, ChatMemberStatusMember { member_until_date: 0 })
            .await
            .map(drop)
    }

    pub async fn ban(&self, chat_id: i64, user_id: i64, until: i64) -> tdrust::Result<()> {
        self.client.ban_chat_member(chat_id, MessageSenderUser { user_id }, i32::try_from(until).unwrap_or(0), false).await.map(drop)
    }

    pub async fn unban(&self, chat_id: i64, user_id: i64) -> tdrust::Result<()> {
        self.client
            .set_chat_member_status(chat_id, MessageSenderUser { user_id }, ChatMemberStatusLeft {})
            .await
            .map(drop)
    }

    /// Removes a member without banning them.
    pub async fn kick(&self, chat_id: i64, user_id: i64) -> tdrust::Result<()> {
        self.ban(chat_id, user_id, 0).await?;
        self.unban(chat_id, user_id).await
    }

    // ---------------------------------------------------------- punishments

    /// Applies a punishment and returns a localized report (empty for plain deletion).
    pub async fn apply(&self, g: &Group, user_id: i64, punish: Punish, actor: i64, reason: &str) -> String {
        match punish {
            Punish::Warn => self.warn(g, user_id, actor, reason).await,
            other => self.apply_hard(g, user_id, other, actor, reason).await,
        }
    }

    /// Mute, temporary mute or ban.
    pub async fn apply_hard(&self, g: &Group, user_id: i64, punish: Punish, actor: i64, reason: &str) -> String {
        let user = self.mention(g.id, user_id).await;
        let reason_arg = if reason.is_empty() { g.s("common.no_reason") } else { reason.to_owned() };
        let (result, key, kind, until) = match punish {
            Punish::Delete | Punish::Warn => return String::new(),
            Punish::Mute => (self.restrict(g.id, user_id, 0).await, "punish.done_mute", "mute", None),
            Punish::TempMute(seconds) => {
                let until = now() + seconds.max(60);
                (self.restrict(g.id, user_id, until).await, "punish.done_tempmute", "mute", Some(until))
            }
            Punish::Ban => (self.ban(g.id, user_id, 0).await, "punish.done_ban", "ban", None),
        };
        if let Err(err) = result {
            return g.t("err.action_failed", &[("error", &h(&err.to_string()))]);
        }
        soft(self.repo.add_sanction(g.id, user_id, kind, until, actor, reason).await, "store sanction");
        metrics::inc(Metric::Punished);
        let duration = if let Punish::TempMute(seconds) = punish { format_duration(seconds) } else { String::new() };
        let report = g.t(key, &[("user", &user), ("reason", &reason_arg), ("duration", &duration)]);
        self.log_event(g, actor, Some(user_id), kind, &report).await;
        self.record_moderation(g, actor).await;
        report
    }

    /// Adds a warning; at the limit, applies the group's warning punishment.
    pub async fn warn(&self, g: &Group, user_id: i64, actor: i64, reason: &str) -> String {
        let user = self.mention(g.id, user_id).await;
        let count = match self.repo.change_warns(g.id, user_id, 1).await {
            Ok(count) => count,
            Err(err) => {
                log::error!("cannot store warning: {err:#}");
                return g.s("err.internal");
            }
        };
        metrics::inc(Metric::Punished);
        let max = g.s.warns.max.max(1) as i32;
        let reason_arg = if reason.is_empty() { g.s("common.no_reason") } else { reason.to_owned() };
        if count < max {
            let report = g.t(
                "warn.added",
                &[("user", &user), ("count", &count.to_string()), ("max", &max.to_string()), ("reason", &reason_arg)],
            );
            self.log_event(g, actor, Some(user_id), "warn", &report).await;
            return report;
        }
        soft(self.repo.reset_warns(g.id, user_id).await, "reset warns");
        let punish = match g.s.warns.punish {
            Punish::Warn | Punish::Delete => Punish::Mute,
            other => other,
        };
        let result = self.apply_hard(g, user_id, punish, actor, &g.s("warn.limit_reason")).await;
        g.t("warn.limit", &[("user", &user), ("max", &max.to_string()), ("result", &result)])
    }

    // ------------------------------------------------------------ audit, log

    pub async fn log_event(&self, g: &Group, actor: i64, target: Option<i64>, action: &str, report: &str) {
        soft(self.repo.audit(g.id, actor, target, action, report).await, "audit");
        let Some(log_chat) = g.s.security.log_chat.filter(|_| self.flag(Flag::LogChannel)) else { return };
        let title = self.client.get_chat(g.id).await.map(|c| c.title).unwrap_or_default();
        let actor = if actor == 0 { g.s("common.system") } else { self.mention(g.id, actor).await };
        let entry = g.t("log.entry", &[("group", &h(&title)), ("actor", &actor), ("action", report)]);
        self.send(log_chat, &entry).await;
    }

    /// Anti-betrayal: counts bans and mutes by an admin and demotes them when
    /// they exceed the hourly limit.
    pub async fn record_moderation(&self, g: &Group, actor: i64) {
        if !g.s.security.anti_betrayal || actor == 0 || actor == self.me {
            return;
        }
        if self.rank(g.id, actor).await >= Rank::Creator {
            return;
        }
        let Some(count) = soft(self.cache.hit(&format!("betray:{}:{actor}", g.id), 3600).await, "betrayal counter") else {
            return;
        };
        if count != i64::from(g.s.security.betrayal_limit) + 1 {
            return;
        }
        for rank in [Rank::Mod, Rank::Owner] {
            soft(self.repo.remove_rank(g.id, actor, rank).await, "demote");
        }
        self.invalidate_ranks(g.id).await;
        let demoted_in_telegram = self
            .client
            .set_chat_member_status(g.id, MessageSenderUser { user_id: actor }, ChatMemberStatusMember { member_until_date: 0 })
            .await
            .is_ok();
        self.invalidate_admins(g.id).await;
        let user = self.mention(g.id, actor).await;
        let key = if demoted_in_telegram { "sec.betrayal_demoted" } else { "sec.betrayal_demoted_bot_only" };
        let report = g.t(key, &[("user", &user), ("limit", &g.s.security.betrayal_limit.to_string())]);
        self.send(g.id, &report).await;
        self.log_event(g, 0, Some(actor), "betrayal", &report).await;
    }

    // ----------------------------------------------------------- group state

    /// Closes or reopens the whole group through Telegram permissions,
    /// remembering the permissions it had before closing.
    pub async fn set_chat_closed(&self, chat_id: i64, closed: bool) -> Result<()> {
        if closed {
            let current = self.client.get_chat(chat_id).await?.permissions;
            self.update(chat_id, move |s| {
                if s.group_lock.saved_permissions.is_none() {
                    s.group_lock.saved_permissions = Some(current);
                }
            })
            .await?;
            self.client.set_chat_permissions(chat_id, ChatPermissions::default()).await?;
        } else {
            let saved = self.update(chat_id, |s| s.group_lock.saved_permissions.take()).await?;
            let permissions = saved.unwrap_or_else(default_permissions);
            self.client.set_chat_permissions(chat_id, permissions).await?;
        }
        Ok(())
    }

    /// The supergroup id of a chat, if it is a supergroup.
    pub async fn supergroup_id(&self, chat_id: i64) -> Option<i64> {
        match self.client.get_chat(chat_id).await.ok()?.r#type {
            ChatType::Supergroup(s) if !s.is_channel => Some(s.supergroup_id),
            _ => None,
        }
    }
}

/// Permissions for a reopened group when nothing was saved.
pub fn default_permissions() -> ChatPermissions {
    ChatPermissions {
        can_send_basic_messages: true,
        can_send_audios: true,
        can_send_documents: true,
        can_send_photos: true,
        can_send_videos: true,
        can_send_video_notes: true,
        can_send_voice_notes: true,
        can_send_polls: true,
        can_send_other_messages: true,
        can_add_link_previews: true,
        can_react_to_messages: true,
        can_invite_users: true,
        ..ChatPermissions::default()
    }
}

// ------------------------------------------------------------ sent messages

enum Entry {
    Waiting(oneshot::Sender<Message>),
    Done(Message),
}

/// Maps the temporary ids of messages the bot sent to their final versions,
/// fed from `Update::MessageSendSucceeded`/`MessageSendFailed`.
#[derive(Clone, Default)]
pub struct SentMessages(Arc<Mutex<HashMap<i64, Entry>>>);

impl SentMessages {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Entry>> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn succeeded(&self, old_message_id: i64, message: Message) {
        let mut map = self.lock();
        match map.remove(&old_message_id) {
            Some(Entry::Waiting(tx)) => {
                let _ = tx.send(message);
            }
            _ => {
                if map.len() > 2000 {
                    map.retain(|_, entry| matches!(entry, Entry::Waiting(_)));
                }
                map.insert(old_message_id, Entry::Done(message));
            }
        }
    }

    pub fn failed(&self, old_message_id: i64) {
        self.lock().remove(&old_message_id);
    }

    /// Waits (up to a minute) until a sent message is confirmed.
    pub async fn confirmed(&self, temporary: &Message) -> Option<Message> {
        let rx = {
            let mut map = self.lock();
            if let Some(Entry::Done(message)) = map.remove(&temporary.id) {
                return Some(message);
            }
            let (tx, rx) = oneshot::channel();
            map.insert(temporary.id, Entry::Waiting(tx));
            rx
        };
        tokio::time::timeout(Duration::from_secs(60), rx).await.ok()?.ok()
    }
}
