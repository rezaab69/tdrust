//! Queries for bot-wide administration (see `src/admin/`): global state,
//! statistics, fleet status, users and groups across the fleet, jobs,
//! errors, retention, database status, and backup/restore.

use std::collections::{BTreeMap, HashSet};

use anyhow::{bail, Context, Result};
use futures_util::TryStreamExt;
use serde_json::Value;

use super::{Repo, BOT_TIMEOUT_SECS};
use crate::admin::global::{Global, GlobalSettings};

/// Tables in a backup, parents first. The flag marks bulky history that only
/// full backups contain. Runtime state (bots, group_bots, jobs, errors) is
/// never backed up.
pub const BACKUP_TABLES: &[(&str, bool)] = &[
    ("groups", false),
    ("users", false),
    ("ranks", false),
    ("sanctions", false),
    ("warns", false),
    ("filters", false),
    ("answers", false),
    ("answer_responses", false),
    ("members", false),
    ("nicknames", false),
    ("profiles", false),
    ("custom_texts", false),
    ("timed_pins", false),
    ("sudo_users", false),
    ("global_bans", false),
    ("global_settings", false),
    ("pv_users", false),
    ("message_stats", true),
    ("audit_log", true),
];

#[derive(Debug, Clone)]
pub struct BotStatus {
    pub id: i64,
    pub username: String,
    pub alive: bool,
    /// Seconds since the last heartbeat.
    pub silent_for: i64,
    pub booted_at: i64,
    pub version: String,
    pub host: String,
    pub groups: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub struct Overview {
    pub groups: i64,
    pub groups_active: i64,
    pub groups_blocked: i64,
    pub groups_1d: i64,
    pub groups_7d: i64,
    pub members: i64,
    pub users: i64,
    pub users_1d: i64,
    pub users_7d: i64,
    pub pv: i64,
    pub pv_blocked: i64,
    pub active_1d: i64,
    pub active_7d: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub struct Moderation {
    pub msg_1d: i64,
    pub msg_7d: i64,
    pub msg_total: i64,
    pub bans: i64,
    pub mutes: i64,
    pub warns: i64,
    pub gbans: i64,
    pub sudo: i64,
    pub filters: i64,
    pub answers: i64,
    pub ranks: i64,
}

/// A user in a list: id, display name, username and a list-specific number.
#[derive(Debug, Clone)]
pub struct UserRef {
    pub id: i64,
    pub name: String,
    pub username: Option<String>,
    pub value: i64,
    pub note: String,
}

#[derive(Debug, Clone)]
pub struct UserCard {
    pub name: String,
    pub username: Option<String>,
    pub is_bot: bool,
    pub first_seen: i64,
    pub updated_at: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub struct UserTotals {
    pub warns: i64,
    pub sanctions: i64,
    pub messages: i64,
    pub messages_7d: i64,
    pub last_day: Option<i32>,
    pub groups: i64,
}

/// A group the user is seen in: (chat, title, messages, stored rank).
pub type UserGroup = (i64, String, i64, Option<i16>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupFilter {
    Active,
    Inactive,
    Blocked,
}

impl GroupFilter {
    pub fn code(self) -> &'static str {
        match self {
            GroupFilter::Active => "a",
            GroupFilter::Inactive => "i",
            GroupFilter::Blocked => "b",
        }
    }

    pub fn from_code(code: &str) -> GroupFilter {
        match code {
            "i" => GroupFilter::Inactive,
            "b" => GroupFilter::Blocked,
            _ => GroupFilter::Active,
        }
    }

    fn sql(self) -> &'static str {
        match self {
            GroupFilter::Active => "active AND NOT blocked",
            GroupFilter::Inactive => "NOT active AND NOT blocked",
            GroupFilter::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct GroupRow {
    pub chat_id: i64,
    pub title: String,
    pub members: i32,
    pub bot_id: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct GroupCard {
    pub title: String,
    pub settings: Value,
    pub active: bool,
    pub blocked: bool,
    pub bot_id: Option<i64>,
    pub members: i32,
    pub created_at: i64,
    pub ranks: i64,
    pub bans: i64,
    pub mutes: i64,
    pub warns: i64,
    pub filters: i64,
    pub answers: i64,
    pub messages_7d: i64,
    pub last_day: Option<i32>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct JobRow {
    pub id: i64,
    pub kind: String,
    pub payload: Value,
    pub bot_id: Option<i64>,
    pub origin_bot: i64,
    pub status: String,
    pub cancel: bool,
    pub progress: i32,
    pub total: i32,
    pub result: String,
    pub created_by: i64,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

const JOB_COLUMNS: &str = "id, kind, payload, bot_id, origin_bot, status, cancel, progress, total, result, created_by,
    extract(epoch FROM created_at)::bigint AS created_at, extract(epoch FROM finished_at)::bigint AS finished_at";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ErrorRow {
    pub id: i64,
    pub level: String,
    pub target: String,
    pub message: String,
    pub bot_id: i64,
    pub count: i64,
    pub first_seen: i64,
    pub last_seen: i64,
    pub resolved: bool,
}

const ERROR_COLUMNS: &str = "id, level, target, message, bot_id, count,
    extract(epoch FROM first_seen)::bigint AS first_seen, extract(epoch FROM last_seen)::bigint AS last_seen, resolved";

#[derive(Debug, sqlx::FromRow)]
pub struct ErrorCounts {
    pub open: i64,
    pub open_events: i64,
    pub last_hour: i64,
}

/// A migration recorded in the database.
#[derive(Debug, sqlx::FromRow)]
pub struct AppliedMigration {
    pub version: i64,
    pub description: String,
    pub installed_on: i64,
    pub success: bool,
    pub checksum: Vec<u8>,
    pub execution_ms: i64,
}

#[derive(Debug)]
pub struct DbInfo {
    pub server: String,
    pub size: i64,
    pub connections: i64,
    pub max_connections: String,
    /// (table, estimated rows, bytes incl. indexes), largest first.
    pub tables: Vec<(String, i64, i64)>,
    pub migrations: Vec<AppliedMigration>,
}

/// Rows removed by the retention job: (message stats, audit log, errors, jobs).
pub type Pruned = (u64, u64, u64, u64);

fn user_name(first: &str, last: &str) -> String {
    format!("{first} {last}").trim().to_owned()
}

fn user_refs(rows: Vec<(i64, String, String, Option<String>, i64, String)>) -> Vec<UserRef> {
    rows.into_iter()
        .map(|(id, first, last, username, value, note)| UserRef { id, name: user_name(&first, &last), username, value, note })
        .collect()
}

impl Repo {
    // --------------------------------------------------------- global state

    pub async fn load_global(&self) -> Result<Global> {
        let row: Option<(Value,)> =
            sqlx::query_as("SELECT settings FROM global_settings WHERE id = 1").fetch_optional(&self.pool).await?;
        let settings = row
            .map(|(json,)| {
                serde_json::from_value(json).unwrap_or_else(|err| {
                    log::error!("global settings are unreadable ({err}); using defaults");
                    GlobalSettings::default()
                })
            })
            .unwrap_or_default();
        let sudoers: Vec<(i64,)> = sqlx::query_as("SELECT user_id FROM sudo_users").fetch_all(&self.pool).await?;
        let gbans: Vec<(i64,)> = sqlx::query_as("SELECT user_id FROM global_bans").fetch_all(&self.pool).await?;
        Ok(Global {
            settings,
            sudoers: sudoers.into_iter().map(|(id,)| id).collect::<HashSet<_>>(),
            gbans: gbans.into_iter().map(|(id,)| id).collect::<HashSet<_>>(),
        })
    }

    /// Changes the global settings in one transaction (row locked), like `update_settings`.
    pub async fn update_global<R: Send>(&self, change: impl FnOnce(&mut GlobalSettings) -> R + Send) -> Result<R> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO global_settings (id) VALUES (1) ON CONFLICT (id) DO NOTHING").execute(&mut *tx).await?;
        let (json,): (Value,) =
            sqlx::query_as("SELECT settings FROM global_settings WHERE id = 1 FOR UPDATE").fetch_one(&mut *tx).await?;
        let mut settings: GlobalSettings = serde_json::from_value(json).unwrap_or_default();
        let result = change(&mut settings);
        sqlx::query("UPDATE global_settings SET settings = $1, updated_at = now() WHERE id = 1")
            .bind(serde_json::to_value(&settings)?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result)
    }

    // ------------------------------------------------------------ sudo users

    /// Returns false if the user was already sudo.
    pub async fn add_sudo(&self, user_id: i64, by: i64) -> Result<bool> {
        let done = sqlx::query("INSERT INTO sudo_users (user_id, added_by) VALUES ($1, $2) ON CONFLICT (user_id) DO NOTHING")
            .bind(user_id)
            .bind(by)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn remove_sudo(&self, user_id: i64) -> Result<bool> {
        let done = sqlx::query("DELETE FROM sudo_users WHERE user_id = $1").bind(user_id).execute(&self.pool).await?;
        Ok(done.rows_affected() > 0)
    }

    /// Sudo users; `value` is who added them.
    pub async fn sudo_list(&self) -> Result<Vec<UserRef>> {
        let rows = sqlx::query_as(
            "SELECT s.user_id, COALESCE(u.first_name, ''), COALESCE(u.last_name, ''), u.username, s.added_by, ''
             FROM sudo_users s LEFT JOIN users u ON u.user_id = s.user_id ORDER BY s.created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(user_refs(rows))
    }

    // ----------------------------------------------------------- global bans

    /// Returns false if the user was already banned (the reason is updated).
    pub async fn add_gban(&self, user_id: i64, reason: &str, actor: i64) -> Result<bool> {
        let (inserted,): (bool,) = sqlx::query_as(
            "INSERT INTO global_bans (user_id, reason, actor) VALUES ($1, $2, $3)
             ON CONFLICT (user_id) DO UPDATE SET reason = $2, actor = $3
             RETURNING (xmax = 0)",
        )
        .bind(user_id)
        .bind(reason)
        .bind(actor)
        .fetch_one(&self.pool)
        .await?;
        Ok(inserted)
    }

    pub async fn remove_gban(&self, user_id: i64) -> Result<bool> {
        let done = sqlx::query("DELETE FROM global_bans WHERE user_id = $1").bind(user_id).execute(&self.pool).await?;
        Ok(done.rows_affected() > 0)
    }

    /// (reason, actor, banned at) of a global ban.
    pub async fn gban(&self, user_id: i64) -> Result<Option<(String, i64, i64)>> {
        Ok(sqlx::query_as(
            "SELECT reason, actor, extract(epoch FROM created_at)::bigint FROM global_bans WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Global bans, newest first; `note` is the reason.
    pub async fn gban_list(&self, limit: i64, offset: i64) -> Result<(Vec<UserRef>, i64)> {
        let rows = sqlx::query_as(
            "SELECT b.user_id, COALESCE(u.first_name, ''), COALESCE(u.last_name, ''), u.username, b.actor, b.reason
             FROM global_bans b LEFT JOIN users u ON u.user_id = b.user_id ORDER BY b.created_at DESC LIMIT $1 OFFSET $2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let (total,): (i64,) = sqlx::query_as("SELECT count(*) FROM global_bans").fetch_one(&self.pool).await?;
        Ok((user_refs(rows), total))
    }

    /// Groups served by `bot_id` where a global ban of `user_id` applies:
    /// where the user was seen (to ban), or was banned by a global ban (to unban).
    pub async fn gban_sweep_chats(&self, bot_id: i64, user_id: i64, ban: bool) -> Result<Vec<i64>> {
        let sql = if ban {
            "SELECT g.chat_id FROM groups g WHERE g.bot_id = $1 AND g.active AND NOT g.blocked AND (
                 EXISTS (SELECT 1 FROM members m WHERE m.chat_id = g.chat_id AND m.user_id = $2)
                 OR EXISTS (SELECT 1 FROM message_stats s WHERE s.chat_id = g.chat_id AND s.user_id = $2))"
        } else {
            "SELECT s.chat_id FROM sanctions s JOIN groups g ON g.chat_id = s.chat_id
             WHERE g.bot_id = $1 AND s.user_id = $2 AND s.kind = 'ban' AND s.reason LIKE '[gban]%'"
        };
        let rows: Vec<(i64,)> = sqlx::query_as(sql).bind(bot_id).bind(user_id).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    // ------------------------------------------------------------------ bots

    /// Records what this bot process is, once at start.
    pub async fn boot(&self, bot_id: i64, version: &str, host: &str) -> Result<()> {
        sqlx::query("UPDATE bots SET booted_at = now(), version = $2, host = $3 WHERE bot_id = $1")
            .bind(bot_id)
            .bind(version)
            .bind(host)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn fleet_status(&self) -> Result<Vec<BotStatus>> {
        /// (id, username, alive, silent for, booted at, version, host, groups)
        type Row = (i64, String, bool, i64, i64, String, String, i64);
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "SELECT b.bot_id, b.username, b.heartbeat_at > now() - interval '{BOT_TIMEOUT_SECS} seconds',
                    extract(epoch FROM now() - b.heartbeat_at)::bigint, extract(epoch FROM b.booted_at)::bigint,
                    b.version, b.host, (SELECT count(*) FROM groups g WHERE g.bot_id = b.bot_id AND g.active)
             FROM bots b ORDER BY b.started_at"
        ))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, username, alive, silent_for, booted_at, version, host, groups)| BotStatus {
                id,
                username,
                alive,
                silent_for,
                booted_at,
                version,
                host,
                groups,
            })
            .collect())
    }

    // ------------------------------------------------------ private chat users

    pub async fn record_pv_user(&self, user_id: i64, bot_id: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO pv_users (user_id, bot_id) VALUES ($1, $2)
             ON CONFLICT (user_id, bot_id) DO UPDATE SET seen_at = now(), blocked = FALSE",
        )
        .bind(user_id)
        .bind(bot_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_pv_blocked(&self, user_id: i64, bot_id: i64) -> Result<()> {
        sqlx::query("UPDATE pv_users SET blocked = TRUE WHERE user_id = $1 AND bot_id = $2")
            .bind(user_id)
            .bind(bot_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Users this bot sends a broadcast to: those who started it, unless a
    /// running bot with a lower id can reach them too, so nobody gets it twice.
    pub async fn broadcast_users(&self, bot_id: i64) -> Result<Vec<i64>> {
        let rows: Vec<(i64,)> = sqlx::query_as(&format!(
            "SELECT p.user_id FROM pv_users p WHERE p.bot_id = $1 AND NOT p.blocked AND NOT EXISTS (
                 SELECT 1 FROM pv_users q JOIN bots b ON b.bot_id = q.bot_id
                 WHERE q.user_id = p.user_id AND q.bot_id < p.bot_id AND NOT q.blocked
                   AND b.heartbeat_at > now() - interval '{BOT_TIMEOUT_SECS} seconds')
             ORDER BY p.started_at"
        ))
        .bind(bot_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    pub async fn has_pv(&self, user_id: i64, bot_id: i64) -> Result<bool> {
        let row: Option<(bool,)> = sqlx::query_as("SELECT NOT blocked FROM pv_users WHERE user_id = $1 AND bot_id = $2")
            .bind(user_id)
            .bind(bot_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some_and(|(open,)| open))
    }

    /// Broadcast audience over the whole fleet: (groups, private chats).
    pub async fn audience(&self) -> Result<(i64, i64)> {
        Ok(sqlx::query_as(
            "SELECT (SELECT count(*) FROM groups WHERE active AND NOT blocked AND bot_id IS NOT NULL),
                    (SELECT count(DISTINCT user_id) FROM pv_users WHERE NOT blocked)",
        )
        .fetch_one(&self.pool)
        .await?)
    }

    // ------------------------------------------------------------ statistics

    /// `today` is the current day number (UTC); message days are local, so
    /// "today" is approximate by up to a day.
    pub async fn overview(&self, today: i32) -> Result<(Overview, Moderation)> {
        let overview = sqlx::query_as::<_, Overview>(
            "SELECT
                (SELECT count(*) FROM groups) AS groups,
                (SELECT count(*) FROM groups WHERE active AND NOT blocked) AS groups_active,
                (SELECT count(*) FROM groups WHERE blocked) AS groups_blocked,
                (SELECT count(*) FROM groups WHERE created_at > now() - interval '1 day') AS groups_1d,
                (SELECT count(*) FROM groups WHERE created_at > now() - interval '7 days') AS groups_7d,
                (SELECT COALESCE(sum(members), 0)::bigint FROM groups WHERE active AND NOT blocked) AS members,
                (SELECT count(*) FROM users WHERE NOT is_bot) AS users,
                (SELECT count(*) FROM users WHERE NOT is_bot AND created_at > now() - interval '1 day') AS users_1d,
                (SELECT count(*) FROM users WHERE NOT is_bot AND created_at > now() - interval '7 days') AS users_7d,
                (SELECT count(DISTINCT user_id) FROM pv_users) AS pv,
                (SELECT count(*) FROM (SELECT user_id FROM pv_users GROUP BY user_id HAVING bool_and(blocked)) b) AS pv_blocked,
                (SELECT count(DISTINCT user_id) FROM message_stats WHERE day >= $1) AS active_1d,
                (SELECT count(DISTINCT user_id) FROM message_stats WHERE day >= $1 - 6) AS active_7d",
        )
        .bind(today)
        .fetch_one(&self.pool)
        .await?;
        let moderation = sqlx::query_as::<_, Moderation>(
            "SELECT
                (SELECT COALESCE(sum(messages), 0)::bigint FROM message_stats WHERE day >= $1) AS msg_1d,
                (SELECT COALESCE(sum(messages), 0)::bigint FROM message_stats WHERE day >= $1 - 6) AS msg_7d,
                (SELECT COALESCE(sum(messages), 0)::bigint FROM message_stats) AS msg_total,
                (SELECT count(*) FROM sanctions WHERE kind = 'ban') AS bans,
                (SELECT count(*) FROM sanctions WHERE kind = 'mute') AS mutes,
                (SELECT COALESCE(sum(count), 0)::bigint FROM warns) AS warns,
                (SELECT count(*) FROM global_bans) AS gbans,
                (SELECT count(*) FROM sudo_users) AS sudo,
                (SELECT count(*) FROM filters) AS filters,
                (SELECT count(*) FROM answers) AS answers,
                (SELECT count(*) FROM ranks) AS ranks",
        )
        .bind(today)
        .fetch_one(&self.pool)
        .await?;
        Ok((overview, moderation))
    }

    /// Most active groups since `from_day`: (chat, title, messages).
    pub async fn top_groups(&self, from_day: i32, limit: i64) -> Result<Vec<(i64, String, i64)>> {
        Ok(sqlx::query_as(
            "SELECT s.chat_id, COALESCE(g.title, ''), sum(s.messages)::bigint AS total FROM message_stats s
             LEFT JOIN groups g ON g.chat_id = s.chat_id WHERE s.day >= $1
             GROUP BY s.chat_id, g.title ORDER BY total DESC LIMIT $2",
        )
        .bind(from_day)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// The most frequent audit actions of the last `hours`.
    pub async fn audit_actions(&self, hours: i32) -> Result<Vec<(String, i64)>> {
        Ok(sqlx::query_as(
            "SELECT action, count(*) AS n FROM audit_log WHERE created_at > now() - make_interval(hours => $1)
             GROUP BY action ORDER BY n DESC LIMIT 8",
        )
        .bind(hours)
        .fetch_all(&self.pool)
        .await?)
    }

    /// The latest audit entries of the operators (chat 0): (actor, target, action, detail, at).
    pub async fn operator_audit(&self, limit: i64) -> Result<Vec<(i64, Option<i64>, String, String, i64)>> {
        Ok(sqlx::query_as(
            "SELECT actor, target, action, detail, extract(epoch FROM created_at)::bigint FROM audit_log
             WHERE chat_id = 0 ORDER BY created_at DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    // ----------------------------------------------------------------- users

    pub async fn user_card(&self, user_id: i64) -> Result<Option<UserCard>> {
        let row: Option<(String, String, Option<String>, bool, i64, i64)> = sqlx::query_as(
            "SELECT first_name, last_name, username, is_bot, extract(epoch FROM created_at)::bigint,
                    extract(epoch FROM updated_at)::bigint FROM users WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(first, last, username, is_bot, first_seen, updated_at)| UserCard {
            name: user_name(&first, &last),
            username,
            is_bot,
            first_seen,
            updated_at,
        }))
    }

    pub async fn user_totals(&self, user_id: i64, today: i32) -> Result<UserTotals> {
        Ok(sqlx::query_as::<_, UserTotals>(
            "SELECT
                (SELECT COALESCE(sum(count), 0)::bigint FROM warns WHERE user_id = $1) AS warns,
                (SELECT count(*) FROM sanctions WHERE user_id = $1) AS sanctions,
                (SELECT COALESCE(sum(messages), 0)::bigint FROM message_stats WHERE user_id = $1) AS messages,
                (SELECT COALESCE(sum(messages), 0)::bigint FROM message_stats WHERE user_id = $1 AND day >= $2 - 6) AS messages_7d,
                (SELECT max(day) FROM message_stats WHERE user_id = $1) AS last_day,
                (SELECT count(*) FROM (SELECT chat_id FROM members WHERE user_id = $1
                                        UNION SELECT chat_id FROM message_stats WHERE user_id = $1
                                        UNION SELECT chat_id FROM ranks WHERE user_id = $1) c) AS groups",
        )
        .bind(user_id)
        .bind(today)
        .fetch_one(&self.pool)
        .await?)
    }

    /// The groups a user is seen in, most active first.
    pub async fn user_groups(&self, user_id: i64, limit: i64) -> Result<Vec<UserGroup>> {
        Ok(sqlx::query_as(
            "SELECT g.chat_id, g.title, COALESCE(s.messages, 0)::bigint AS messages, r.rank FROM groups g
             LEFT JOIN (SELECT chat_id, sum(messages)::bigint AS messages FROM message_stats WHERE user_id = $1 GROUP BY chat_id) s
                    ON s.chat_id = g.chat_id
             LEFT JOIN ranks r ON r.chat_id = g.chat_id AND r.user_id = $1
             WHERE s.chat_id IS NOT NULL OR r.user_id IS NOT NULL
                OR EXISTS (SELECT 1 FROM members m WHERE m.chat_id = g.chat_id AND m.user_id = $1)
             ORDER BY r.rank DESC NULLS LAST, messages DESC LIMIT $2",
        )
        .bind(user_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Bots the user opened a private chat with: (username, blocked).
    pub async fn user_bots(&self, user_id: i64) -> Result<Vec<(String, bool)>> {
        Ok(sqlx::query_as(
            "SELECT b.username, p.blocked FROM pv_users p JOIN bots b ON b.bot_id = p.bot_id WHERE p.user_id = $1 ORDER BY p.started_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn clear_warns_everywhere(&self, user_id: i64) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM warns WHERE user_id = $1").bind(user_id).execute(&self.pool).await?.rows_affected())
    }

    /// Removes every stored rank of a user and returns the groups.
    pub async fn remove_ranks_everywhere(&self, user_id: i64) -> Result<Vec<i64>> {
        let rows: Vec<(i64,)> =
            sqlx::query_as("DELETE FROM ranks WHERE user_id = $1 RETURNING chat_id").bind(user_id).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Users by id, @username or part of the name.
    pub async fn search_users(&self, query: &str, limit: i64) -> Result<Vec<UserRef>> {
        let query = query.trim().trim_start_matches('@');
        let rows = sqlx::query_as(
            "SELECT user_id, first_name, last_name, username, 0::bigint, '' FROM users
             WHERE user_id::text = $1 OR lower(username) = lower($1)
                OR strpos(lower(first_name || ' ' || last_name), lower($1)) > 0
             ORDER BY (lower(username) = lower($1)) DESC NULLS LAST, updated_at DESC LIMIT $2",
        )
        .bind(query)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(user_refs(rows))
    }

    /// Users first seen most recently; `value` is when (unix seconds).
    pub async fn recent_users(&self, limit: i64, offset: i64) -> Result<(Vec<UserRef>, i64)> {
        let rows = sqlx::query_as(
            "SELECT user_id, first_name, last_name, username, extract(epoch FROM created_at)::bigint, '' FROM users
             WHERE NOT is_bot ORDER BY created_at DESC, user_id LIMIT $1 OFFSET $2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let (total,): (i64,) = sqlx::query_as("SELECT count(*) FROM users WHERE NOT is_bot").fetch_one(&self.pool).await?;
        Ok((user_refs(rows), total))
    }

    /// Most active users since `from_day`; `value` is their messages.
    pub async fn top_users_global(&self, from_day: i32, limit: i64, offset: i64) -> Result<(Vec<UserRef>, i64)> {
        let rows = sqlx::query_as(
            "SELECT s.user_id, COALESCE(u.first_name, ''), COALESCE(u.last_name, ''), u.username, sum(s.messages)::bigint AS total, ''
             FROM message_stats s LEFT JOIN users u ON u.user_id = s.user_id WHERE s.day >= $1
             GROUP BY s.user_id, u.first_name, u.last_name, u.username ORDER BY total DESC LIMIT $2 OFFSET $3",
        )
        .bind(from_day)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let (total,): (i64,) = sqlx::query_as("SELECT count(DISTINCT user_id) FROM message_stats WHERE day >= $1")
            .bind(from_day)
            .fetch_one(&self.pool)
            .await?;
        Ok((user_refs(rows), total))
    }

    // ---------------------------------------------------------------- groups

    pub async fn groups_page(&self, filter: GroupFilter, limit: i64, offset: i64) -> Result<(Vec<GroupRow>, i64)> {
        let rows = sqlx::query_as::<_, GroupRow>(&format!(
            "SELECT chat_id, title, members, bot_id FROM groups WHERE {}
             ORDER BY members DESC, title, chat_id LIMIT $1 OFFSET $2",
            filter.sql()
        ))
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let (total,): (i64,) =
            sqlx::query_as(&format!("SELECT count(*) FROM groups WHERE {}", filter.sql())).fetch_one(&self.pool).await?;
        Ok((rows, total))
    }

    pub async fn group_card(&self, chat_id: i64, today: i32) -> Result<Option<GroupCard>> {
        Ok(sqlx::query_as::<_, GroupCard>(
            "SELECT g.title, g.settings, g.active, g.blocked, g.bot_id, g.members,
                    extract(epoch FROM g.created_at)::bigint AS created_at,
                    (SELECT count(*) FROM ranks WHERE chat_id = $1) AS ranks,
                    (SELECT count(*) FROM sanctions WHERE chat_id = $1 AND kind = 'ban') AS bans,
                    (SELECT count(*) FROM sanctions WHERE chat_id = $1 AND kind = 'mute') AS mutes,
                    (SELECT count(*) FROM warns WHERE chat_id = $1 AND count > 0) AS warns,
                    (SELECT count(*) FROM filters WHERE chat_id = $1) AS filters,
                    (SELECT count(*) FROM answers WHERE chat_id = $1) AS answers,
                    (SELECT COALESCE(sum(messages), 0)::bigint FROM message_stats WHERE chat_id = $1 AND day >= $2 - 6) AS messages_7d,
                    (SELECT max(day) FROM message_stats WHERE chat_id = $1) AS last_day
             FROM groups g WHERE g.chat_id = $1",
        )
        .bind(chat_id)
        .bind(today)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Bots that are members of a group.
    pub async fn group_bots(&self, chat_id: i64) -> Result<Vec<i64>> {
        let rows: Vec<(i64,)> =
            sqlx::query_as("SELECT bot_id FROM group_bots WHERE chat_id = $1").bind(chat_id).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    pub async fn set_blocked(&self, chat_id: i64, blocked: bool) -> Result<bool> {
        let done = sqlx::query("UPDATE groups SET blocked = $2, active = active AND NOT $2, updated_at = now() WHERE chat_id = $1")
            .bind(chat_id)
            .bind(blocked)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn is_blocked(&self, chat_id: i64) -> Result<bool> {
        let row: Option<(bool,)> =
            sqlx::query_as("SELECT blocked FROM groups WHERE chat_id = $1").bind(chat_id).fetch_optional(&self.pool).await?;
        Ok(row.is_some_and(|(blocked,)| blocked))
    }

    pub async fn set_group_members(&self, chat_id: i64, members: i32) -> Result<()> {
        sqlx::query("UPDATE groups SET members = $2 WHERE chat_id = $1").bind(chat_id).bind(members).execute(&self.pool).await?;
        Ok(())
    }

    /// After a bot left: a group no bot serves any more is inactive.
    pub async fn deactivate_if_unserved(&self, chat_id: i64) -> Result<()> {
        sqlx::query("UPDATE groups SET active = FALSE, updated_at = now() WHERE chat_id = $1 AND bot_id IS NULL")
            .bind(chat_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ------------------------------------------------------------------ jobs

    pub async fn insert_job(&self, kind: &str, payload: &Value, bot_id: Option<i64>, origin_bot: i64, by: i64) -> Result<i64> {
        let (id,): (i64,) = sqlx::query_as(
            "INSERT INTO jobs (kind, payload, bot_id, origin_bot, created_by) VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(kind)
        .bind(payload)
        .bind(bot_id)
        .bind(origin_bot)
        .bind(by)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    /// Takes the oldest pending job this bot may run. Safe with any number of bots.
    pub async fn claim_job(&self, bot_id: i64) -> Result<Option<JobRow>> {
        Ok(sqlx::query_as::<_, JobRow>(&format!(
            "UPDATE jobs SET status = 'running', started_at = now(), bot_id = $1
             WHERE id = (SELECT id FROM jobs WHERE status = 'pending' AND (bot_id IS NULL OR bot_id = $1)
                         ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED)
             RETURNING {JOB_COLUMNS}"
        ))
        .bind(bot_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Reports progress; returns whether the job was asked to stop.
    pub async fn job_progress(&self, id: i64, progress: i32, total: i32) -> Result<bool> {
        let row: Option<(bool,)> = sqlx::query_as("UPDATE jobs SET progress = $2, total = $3 WHERE id = $1 RETURNING cancel")
            .bind(id)
            .bind(progress)
            .bind(total)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_none_or(|(cancel,)| cancel))
    }

    pub async fn finish_job(&self, id: i64, status: &str, result: &str) -> Result<()> {
        sqlx::query("UPDATE jobs SET status = $2, result = $3, finished_at = now() WHERE id = $1")
            .bind(id)
            .bind(status)
            .bind(result)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn job(&self, id: i64) -> Result<Option<JobRow>> {
        Ok(sqlx::query_as::<_, JobRow>(&format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn recent_jobs(&self, limit: i64) -> Result<Vec<JobRow>> {
        Ok(sqlx::query_as::<_, JobRow>(&format!("SELECT {JOB_COLUMNS} FROM jobs ORDER BY id DESC LIMIT $1"))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?)
    }

    /// Jobs per status.
    pub async fn job_counts(&self) -> Result<BTreeMap<String, i64>> {
        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT status, count(*) FROM jobs GROUP BY status").fetch_all(&self.pool).await?;
        Ok(rows.into_iter().collect())
    }

    /// Cancels a pending job at once, or asks a running one to stop.
    pub async fn cancel_job(&self, id: i64) -> Result<bool> {
        let pending = sqlx::query("UPDATE jobs SET status = 'cancelled', finished_at = now() WHERE id = $1 AND status = 'pending'")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if pending.rows_affected() > 0 {
            return Ok(true);
        }
        let running = sqlx::query("UPDATE jobs SET cancel = TRUE WHERE id = $1 AND status = 'running'").bind(id).execute(&self.pool).await?;
        Ok(running.rows_affected() > 0)
    }

    /// Jobs this bot was running when it stopped.
    pub async fn fail_interrupted_jobs(&self, bot_id: i64) -> Result<u64> {
        Ok(sqlx::query(
            "UPDATE jobs SET status = 'failed', result = 'interrupted: the bot restarted', finished_at = now()
             WHERE status = 'running' AND bot_id = $1",
        )
        .bind(bot_id)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// Jobs whose bot stopped sending heartbeats while running them.
    pub async fn fail_orphaned_jobs(&self) -> Result<u64> {
        Ok(sqlx::query(&format!(
            "UPDATE jobs SET status = 'failed', result = 'interrupted: the bot went down', finished_at = now()
             WHERE status = 'running' AND bot_id IN (
                 SELECT bot_id FROM bots WHERE heartbeat_at < now() - interval '{BOT_TIMEOUT_SECS} seconds')"
        ))
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    // ---------------------------------------------------------------- errors

    /// Adds `count` occurrences. Returns the error's id and whether it was
    /// known before: `None` new, `Some(true)` resolved and now back, `Some(false)` open.
    pub async fn record_error(
        &self,
        fingerprint: &str,
        level: &str,
        target: &str,
        message: &str,
        bot_id: i64,
        count: i64,
    ) -> Result<(i64, Option<bool>)> {
        Ok(sqlx::query_as(
            "WITH old AS (SELECT resolved FROM error_events WHERE fingerprint = $1)
             INSERT INTO error_events (fingerprint, level, target, message, bot_id, count) VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (fingerprint) DO UPDATE SET message = $4, bot_id = $5, count = error_events.count + $6,
                 last_seen = now(), resolved = FALSE
             RETURNING id, (SELECT resolved FROM old)",
        )
        .bind(fingerprint)
        .bind(level)
        .bind(target)
        .bind(message)
        .bind(bot_id)
        .bind(count)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn open_errors(&self, limit: i64) -> Result<Vec<ErrorRow>> {
        Ok(sqlx::query_as::<_, ErrorRow>(&format!(
            "SELECT {ERROR_COLUMNS} FROM error_events WHERE NOT resolved ORDER BY last_seen DESC LIMIT $1"
        ))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn error(&self, id: i64) -> Result<Option<ErrorRow>> {
        Ok(sqlx::query_as::<_, ErrorRow>(&format!("SELECT {ERROR_COLUMNS} FROM error_events WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn resolve_error(&self, id: i64) -> Result<bool> {
        let done = sqlx::query("UPDATE error_events SET resolved = TRUE WHERE id = $1 AND NOT resolved").bind(id).execute(&self.pool).await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn resolve_all_errors(&self) -> Result<u64> {
        Ok(sqlx::query("UPDATE error_events SET resolved = TRUE WHERE NOT resolved").execute(&self.pool).await?.rows_affected())
    }

    pub async fn error_counts(&self) -> Result<ErrorCounts> {
        Ok(sqlx::query_as::<_, ErrorCounts>(
            "SELECT count(*) FILTER (WHERE NOT resolved) AS open,
                    COALESCE(sum(count) FILTER (WHERE NOT resolved), 0)::bigint AS open_events,
                    count(*) FILTER (WHERE last_seen > now() - interval '1 hour') AS last_hour
             FROM error_events",
        )
        .fetch_one(&self.pool)
        .await?)
    }

    // ------------------------------------------------------------- retention

    /// Deletes history older than `days`: message statistics, audit log,
    /// resolved errors and finished jobs.
    pub async fn prune(&self, days: u32, today: i32) -> Result<Pruned> {
        let days = i32::try_from(days.max(1)).unwrap_or(i32::MAX);
        let stats = sqlx::query("DELETE FROM message_stats WHERE day < $1").bind(today - days).execute(&self.pool).await?;
        let audit = sqlx::query("DELETE FROM audit_log WHERE created_at < now() - make_interval(days => $1)")
            .bind(days)
            .execute(&self.pool)
            .await?;
        let errors = sqlx::query("DELETE FROM error_events WHERE resolved AND last_seen < now() - make_interval(days => $1)")
            .bind(days)
            .execute(&self.pool)
            .await?;
        let jobs = sqlx::query(
            "DELETE FROM jobs WHERE status IN ('done', 'failed', 'cancelled') AND finished_at < now() - make_interval(days => $1)",
        )
        .bind(days)
        .execute(&self.pool)
        .await?;
        Ok((stats.rows_affected(), audit.rows_affected(), errors.rows_affected(), jobs.rows_affected()))
    }

    // ------------------------------------------------------- database status

    /// Round trip time of a trivial query, in milliseconds.
    pub async fn ping(&self) -> Result<u128> {
        let started = std::time::Instant::now();
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(started.elapsed().as_millis())
    }

    /// Connections of this process's pool: (open, idle, maximum).
    pub fn pool_stats(&self) -> (u32, usize, u32) {
        (self.pool.size(), self.pool.num_idle(), self.pool.options().get_max_connections())
    }

    pub async fn db_info(&self) -> Result<DbInfo> {
        let (server, size, connections): (String, i64, i64) = sqlx::query_as(
            "SELECT current_setting('server_version'), pg_database_size(current_database()),
                    (SELECT count(*) FROM pg_stat_activity WHERE datname = current_database())",
        )
        .fetch_one(&self.pool)
        .await?;
        let (max_connections,): (String,) = sqlx::query_as("SELECT current_setting('max_connections')").fetch_one(&self.pool).await?;
        let tables = sqlx::query_as(
            "SELECT relname::text, n_live_tup, pg_total_relation_size(relid) AS bytes FROM pg_stat_user_tables
             ORDER BY bytes DESC LIMIT 12",
        )
        .fetch_all(&self.pool)
        .await?;
        let migrations = sqlx::query_as::<_, AppliedMigration>(
            "SELECT version, description, extract(epoch FROM installed_on)::bigint AS installed_on, success, checksum,
                    (execution_time / 1000000)::bigint AS execution_ms
             FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(DbInfo { server, size, connections, max_connections, tables, migrations })
    }

    /// Refreshes the planner statistics of every table.
    pub async fn analyze(&self) -> Result<()> {
        sqlx::query("ANALYZE").execute(&self.pool).await?;
        Ok(())
    }

    /// The newest migration applied successfully.
    pub async fn schema_version(&self) -> Result<i64> {
        let (version,): (Option<i64>,) =
            sqlx::query_as("SELECT max(version) FROM _sqlx_migrations WHERE success").fetch_one(&self.pool).await?;
        Ok(version.unwrap_or(0))
    }

    // -------------------------------------------------------- backup/restore

    /// Streams every row of a backup table as JSON to `sink`. Returns the row count.
    pub async fn export_table(&self, table: &str, mut sink: impl FnMut(&str) -> std::io::Result<()>) -> Result<u64> {
        if !BACKUP_TABLES.iter().any(|(name, _)| *name == table) {
            bail!("`{table}` is not a backup table");
        }
        let sql = format!("SELECT row_to_json(t)::text FROM {table} t");
        let mut rows = sqlx::query_as::<_, (String,)>(&sql).fetch(&self.pool);
        let mut count = 0;
        while let Some((json,)) = rows.try_next().await? {
            sink(&json)?;
            count += 1;
        }
        Ok(count)
    }

    /// Replaces the content of the given backup tables with `rows`, in one
    /// transaction: rows missing from the backup are deleted, the others
    /// inserted or updated. Tables left out keep their content. Columns
    /// missing from older backups take their defaults.
    pub async fn restore(&self, tables: &[(String, Vec<Value>)]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for (name, _) in BACKUP_TABLES {
            // Table names in SQL come from this list only, never from the file.
            let Some((_, rows)) = tables.iter().find(|(t, _)| t == name) else { continue };
            let table = *name;
            let columns: Vec<String> = sqlx::query_scalar(
                "SELECT column_name::text FROM information_schema.columns
                 WHERE table_schema = current_schema() AND table_name = $1 ORDER BY ordinal_position",
            )
            .bind(table)
            .fetch_all(&mut *tx)
            .await?;
            let keys: Vec<String> = sqlx::query_scalar(
                "SELECT a.attname::text FROM pg_index i
                 JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey)
                 WHERE i.indrelid = $1::regclass AND i.indisprimary",
            )
            .bind(table)
            .fetch_all(&mut *tx)
            .await?;
            if columns.is_empty() || keys.is_empty() {
                bail!("cannot restore `{table}`: unknown table or no primary key");
            }
            let quote = |c: &String| format!("\"{c}\"");
            let present: Vec<String> =
                columns.iter().filter(|c| rows.iter().any(|r| r.get(c.as_str()).is_some())).map(quote).collect();
            sqlx::query(&format!("CREATE TEMP TABLE restore_rows (LIKE {table} INCLUDING DEFAULTS) ON COMMIT DROP"))
                .execute(&mut *tx)
                .await?;
            if !present.is_empty() {
                let list = present.join(", ");
                for chunk in rows.chunks(2000) {
                    sqlx::query(&format!(
                        "INSERT INTO restore_rows ({list}) SELECT {list} FROM jsonb_populate_recordset(NULL::{table}, $1)"
                    ))
                    .bind(Value::Array(chunk.to_vec()))
                    .execute(&mut *tx)
                    .await
                    .with_context(|| format!("cannot read the rows of `{table}`"))?;
                }
            }
            let same_key = keys.iter().map(|k| format!("r.\"{k}\" = t.\"{k}\"")).collect::<Vec<_>>().join(" AND ");
            sqlx::query(&format!("DELETE FROM {table} t WHERE NOT EXISTS (SELECT 1 FROM restore_rows r WHERE {same_key})"))
                .execute(&mut *tx)
                .await?;
            let key_list = keys.iter().map(quote).collect::<Vec<_>>().join(", ");
            let updates: Vec<String> =
                columns.iter().filter(|c| !keys.contains(c)).map(|c| format!("\"{c}\" = EXCLUDED.\"{c}\"")).collect();
            let on_conflict = if updates.is_empty() { "DO NOTHING".to_owned() } else { format!("DO UPDATE SET {}", updates.join(", ")) };
            sqlx::query(&format!("INSERT INTO {table} SELECT * FROM restore_rows ON CONFLICT ({key_list}) {on_conflict}"))
                .execute(&mut *tx)
                .await
                .with_context(|| format!("cannot restore `{table}`"))?;
            sqlx::query("DROP TABLE restore_rows").execute(&mut *tx).await?;
            if columns.iter().any(|c| c == "id") {
                sqlx::query(&format!(
                    "SELECT setval(pg_get_serial_sequence('{table}', 'id'), GREATEST((SELECT max(id) FROM {table}), 1))
                     WHERE pg_get_serial_sequence('{table}', 'id') IS NOT NULL"
                ))
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }
}

/// Needs PostgreSQL: `DATABASE_URL=postgres://… cargo test -p group-bot -- --ignored`.
/// Uses throwaway ids and cleans up, but point it at a scratch database.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::global::Flag;
    use crate::text::now;

    #[tokio::test]
    #[ignore = "needs PostgreSQL (DATABASE_URL)"]
    async fn admin_queries_run() {
        let url = std::env::var("DATABASE_URL").expect("set DATABASE_URL");
        let repo = Repo::connect(&url, 2).await.unwrap();
        let salt = crate::text::random(1_000_000) as i64;
        let chat = -1_000_000_000_000 - salt;
        let (bot, user, other) = (7_000_000_000 + salt, 8_000_000_000 + salt, 9_000_000_000 + salt);
        let today = (now() / 86_400) as i32;

        // Global state: settings, sudo users, global bans.
        let before = repo.load_global().await.unwrap();
        repo.update_global(|s| s.set_flag(Flag::Fun, false)).await.unwrap();
        assert!(!repo.load_global().await.unwrap().flag(Flag::Fun));
        repo.update_global(|s| s.set_flag(Flag::Fun, before.flag(Flag::Fun))).await.unwrap();
        assert!(repo.add_sudo(user, 1).await.unwrap());
        assert!(!repo.add_sudo(user, 1).await.unwrap());
        assert!(repo.sudo_list().await.unwrap().iter().any(|u| u.id == user));
        assert!(repo.load_global().await.unwrap().sudoers.contains(&user));
        assert!(repo.remove_sudo(user).await.unwrap());
        assert!(repo.add_gban(other, "spam", 1).await.unwrap());
        assert!(!repo.add_gban(other, "spam again", 1).await.unwrap());
        assert_eq!(repo.gban(other).await.unwrap().map(|g| g.0), Some("spam again".into()));
        assert!(repo.gban_list(50, 0).await.unwrap().0.iter().any(|u| u.id == other));

        // A group, a bot serving it, a member with messages.
        repo.heartbeat(bot, "admintestbot").await.unwrap();
        repo.boot(bot, "9.9.9", "host").await.unwrap();
        assert!(repo.claim(chat, bot, "en").await.unwrap());
        repo.set_group_info(chat, "Admin Test", true).await.unwrap();
        repo.set_group_members(chat, 42).await.unwrap();
        repo.record_membership(chat, bot).await.unwrap();
        repo.upsert_user(user, "Ada", "Admin", Some("ada_admin_test"), false).await.unwrap();
        repo.record_join(chat, other, None).await.unwrap();
        repo.add_messages(chat, user, today, 5).await.unwrap();
        repo.set_rank(chat, user, crate::model::Rank::Mod, None, 1).await.unwrap();
        let fleet = repo.fleet_status().await.unwrap();
        assert!(fleet.iter().any(|b| b.id == bot && b.alive && b.version == "9.9.9" && b.groups == 1));
        let (overview, moderation) = repo.overview(today).await.unwrap();
        assert!(overview.groups >= 1 && moderation.msg_1d >= 5 && moderation.gbans >= 1);
        assert!(repo.top_groups(today - 6, 50).await.unwrap().iter().any(|(c, _, n)| *c == chat && *n == 5));
        repo.audit(0, 1, Some(user), "admin_test", "x").await.unwrap();
        assert!(repo.audit_actions(1).await.unwrap().iter().any(|(a, _)| a == "admin_test"));
        assert!(!repo.operator_audit(5).await.unwrap().is_empty());

        // Users across groups.
        assert_eq!(repo.user_card(user).await.unwrap().map(|c| c.name), Some("Ada Admin".into()));
        let totals = repo.user_totals(user, today).await.unwrap();
        assert_eq!((totals.messages, totals.messages_7d, totals.groups, totals.last_day), (5, 5, 1, Some(today)));
        assert_eq!(repo.user_groups(user, 5).await.unwrap(), vec![(chat, "Admin Test".into(), 5, Some(3))]);
        assert_eq!(repo.search_users("ada_admin_test", 5).await.unwrap()[0].id, user);
        assert_eq!(repo.search_users(&user.to_string(), 5).await.unwrap()[0].id, user);
        assert!(repo.search_users("Ada Adm", 50).await.unwrap().iter().any(|u| u.id == user));
        assert!(repo.recent_users(1000, 0).await.unwrap().1 >= 1);
        assert!(repo.top_users_global(today - 6, 50, 0).await.unwrap().0.iter().any(|u| u.id == user && u.value == 5));
        assert_eq!(repo.gban_sweep_chats(bot, other, true).await.unwrap(), vec![chat], "seen as a member");
        repo.add_sanction(chat, other, "ban", None, 0, "[gban] spam").await.unwrap();
        assert_eq!(repo.gban_sweep_chats(bot, other, false).await.unwrap(), vec![chat]);
        assert_eq!(repo.clear_warns_everywhere(user).await.unwrap(), 0);
        assert_eq!(repo.remove_ranks_everywhere(user).await.unwrap(), vec![chat]);

        // Groups.
        let (rows, total) = repo.groups_page(GroupFilter::Active, 1000, 0).await.unwrap();
        assert!(total >= 1 && rows.iter().any(|g| g.chat_id == chat && g.members == 42));
        let card = repo.group_card(chat, today).await.unwrap().unwrap();
        assert_eq!((card.members, card.bans, card.messages_7d, card.bot_id), (42, 1, 5, Some(bot)));
        assert_eq!(repo.group_bots(chat).await.unwrap(), vec![bot]);
        assert!(repo.set_blocked(chat, true).await.unwrap());
        assert!(repo.is_blocked(chat).await.unwrap());
        assert!(!repo.owned_groups(bot).await.unwrap().contains(&chat), "blocked groups are not served");
        repo.set_blocked(chat, false).await.unwrap();

        // Private chat users.
        repo.record_pv_user(user, bot).await.unwrap();
        assert!(repo.has_pv(user, bot).await.unwrap());
        assert_eq!(repo.broadcast_users(bot).await.unwrap(), vec![user]);
        repo.heartbeat(bot - 1, "admintestbot0").await.unwrap();
        repo.record_pv_user(user, bot - 1).await.unwrap();
        assert!(repo.broadcast_users(bot).await.unwrap().is_empty(), "the lower bot sends it");
        assert_eq!(repo.broadcast_users(bot - 1).await.unwrap(), vec![user]);
        assert!(repo.audience().await.unwrap().1 >= 1);
        sqlx::query("DELETE FROM pv_users WHERE bot_id = $1").bind(bot - 1).execute(&repo.pool).await.unwrap();
        sqlx::query("DELETE FROM bots WHERE bot_id = $1").bind(bot - 1).execute(&repo.pool).await.unwrap();
        repo.set_pv_blocked(user, bot).await.unwrap();
        assert!(repo.broadcast_users(bot).await.unwrap().is_empty());
        assert_eq!(repo.user_bots(user).await.unwrap(), vec![("admintestbot".into(), true)]);

        // Jobs: claim, progress, cancel, finish.
        let payload = serde_json::json!({"type": "sync_groups"});
        let id = repo.insert_job("sync", &payload, Some(bot), bot, 1).await.unwrap();
        let job = repo.claim_job(bot).await.unwrap().expect("a pending job");
        assert_eq!((job.id, job.status.as_str()), (id, "running"));
        assert!(!repo.job_progress(id, 1, 2).await.unwrap());
        assert!(repo.cancel_job(id).await.unwrap());
        assert!(repo.job_progress(id, 2, 2).await.unwrap(), "asked to stop");
        repo.finish_job(id, "cancelled", "stopped").await.unwrap();
        assert!(repo.claim_job(bot).await.unwrap().is_none());
        assert_eq!(repo.job(id).await.unwrap().unwrap().status, "cancelled");
        assert!(repo.recent_jobs(10).await.unwrap().iter().any(|j| j.id == id));
        assert!(repo.job_counts().await.unwrap().contains_key("cancelled"));
        repo.fail_interrupted_jobs(bot).await.unwrap();
        repo.fail_orphaned_jobs().await.unwrap();

        // Errors: new, repeated, resolved and back.
        let fingerprint = format!("admin-test|{salt}");
        let (error_id, previous) = repo.record_error(&fingerprint, "error", "t", "boom", bot, 2).await.unwrap();
        assert_eq!(previous, None);
        assert_eq!(repo.record_error(&fingerprint, "error", "t", "boom", bot, 1).await.unwrap(), (error_id, Some(false)));
        assert_eq!(repo.error(error_id).await.unwrap().unwrap().count, 3);
        assert!(repo.open_errors(100).await.unwrap().iter().any(|e| e.id == error_id));
        assert!(repo.resolve_error(error_id).await.unwrap());
        assert_eq!(repo.record_error(&fingerprint, "error", "t", "boom", bot, 1).await.unwrap().1, Some(true));
        assert!(repo.error_counts().await.unwrap().open >= 1);

        // Status, retention and a restore round trip of one table.
        assert!(repo.ping().await.unwrap() < 10_000);
        let info = repo.db_info().await.unwrap();
        assert!(info.migrations.len() >= 3 && info.size > 0);
        assert!(repo.schema_version().await.unwrap() >= 3);
        repo.prune(36_500, today).await.unwrap();
        let mut rows = Vec::new();
        repo.export_table("global_bans", |row| {
            rows.push(serde_json::from_str::<Value>(row).unwrap());
            Ok(())
        })
        .await
        .unwrap();
        assert!(rows.iter().any(|r| r["user_id"] == other));
        repo.add_gban(user, "temporary", 1).await.unwrap();
        repo.restore(&[("global_bans".to_owned(), rows)]).await.unwrap();
        assert!(repo.gban(user).await.unwrap().is_none(), "rows missing from the backup are removed");
        assert!(repo.gban(other).await.unwrap().is_some(), "rows in the backup stay");
        assert!(repo.export_table("pg_authid", |_| Ok(())).await.is_err(), "only backup tables");

        // Clean up.
        sqlx::query("DELETE FROM error_events WHERE fingerprint = $1").bind(&fingerprint).execute(&repo.pool).await.unwrap();
        sqlx::query("DELETE FROM jobs WHERE id = $1").bind(id).execute(&repo.pool).await.unwrap();
        sqlx::query("DELETE FROM pv_users WHERE bot_id = $1").bind(bot).execute(&repo.pool).await.unwrap();
        sqlx::query("DELETE FROM message_stats WHERE chat_id = $1").bind(chat).execute(&repo.pool).await.unwrap();
        sqlx::query("DELETE FROM audit_log WHERE action = 'admin_test'").execute(&repo.pool).await.unwrap();
        sqlx::query("DELETE FROM users WHERE user_id = $1").bind(user).execute(&repo.pool).await.unwrap();
        repo.remove_gban(other).await.unwrap();
        repo.delete_group(chat).await.unwrap();
        sqlx::query("DELETE FROM bots WHERE bot_id = $1").bind(bot).execute(&repo.pool).await.unwrap();
        repo.close().await;
    }
}
