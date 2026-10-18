//! PostgreSQL persistence. Timestamps are passed in and out as unix seconds
//! and stored as TIMESTAMPTZ.

use std::collections::HashMap;

use anyhow::{Context, Result};
use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::model::{Audience, Content, Punish, Rank, Settings};

pub mod admin;

/// A bot whose last heartbeat is older than this is considered down, and
/// standby bots take over its groups.
pub const BOT_TIMEOUT_SECS: u32 = 90;

/// The schema migrations compiled into this build.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
pub struct Repo {
    pool: PgPool,
}

pub struct Answer {
    pub keyword: String,
    pub audience: Audience,
    pub responses: Vec<Content>,
}

pub struct MemberRow {
    pub joined_at: i64,
    pub added_count: i32,
    pub force_done_at: Option<i64>,
}

pub struct UserRow {
    pub first_name: String,
    pub last_name: String,
}

impl Repo {
    pub async fn connect(url: &str, max_connections: u32) -> Result<Repo> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect(url)
            .await
            .context("cannot connect to PostgreSQL")?;
        MIGRATOR.run(&pool).await.context("cannot run database migrations")?;
        Ok(Repo { pool })
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    // ---------------------------------------------------------------- groups

    /// Loads a group's settings, registering the group with defaults if new.
    pub async fn settings(&self, chat_id: i64, default_lang: &str) -> Result<Settings> {
        let row: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT settings FROM groups WHERE chat_id = $1").bind(chat_id).fetch_optional(&self.pool).await?;
        if let Some((json,)) = row {
            return Ok(serde_json::from_value(json).unwrap_or_else(|err| {
                log::error!("settings of {chat_id} are unreadable ({err}); using defaults");
                Settings::default()
            }));
        }
        let settings = Settings { lang: default_lang.to_owned(), ..Settings::default() };
        sqlx::query("INSERT INTO groups (chat_id, settings) VALUES ($1, $2) ON CONFLICT (chat_id) DO NOTHING")
            .bind(chat_id)
            .bind(serde_json::to_value(&settings)?)
            .execute(&self.pool)
            .await?;
        Ok(settings)
    }

    /// Loads, changes and saves settings in one transaction, locking the row
    /// so concurrent changes (from any bot instance) are never lost.
    pub async fn update_settings<R: Send>(
        &self,
        chat_id: i64,
        default_lang: &str,
        change: impl FnOnce(&mut Settings) -> R + Send,
    ) -> Result<(Settings, R)> {
        let defaults = Settings { lang: default_lang.to_owned(), ..Settings::default() };
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO groups (chat_id, settings) VALUES ($1, $2) ON CONFLICT (chat_id) DO NOTHING")
            .bind(chat_id)
            .bind(serde_json::to_value(&defaults)?)
            .execute(&mut *tx)
            .await?;
        let (json,): (serde_json::Value,) = sqlx::query_as("SELECT settings FROM groups WHERE chat_id = $1 FOR UPDATE")
            .bind(chat_id)
            .fetch_one(&mut *tx)
            .await?;
        let mut settings: Settings = serde_json::from_value(json).unwrap_or(defaults);
        let result = change(&mut settings);
        sqlx::query("UPDATE groups SET settings = $2, updated_at = now() WHERE chat_id = $1")
            .bind(chat_id)
            .bind(serde_json::to_value(&settings)?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok((settings, result))
    }

    pub async fn set_group_info(&self, chat_id: i64, title: &str, active: bool) -> Result<()> {
        sqlx::query("UPDATE groups SET title = $2, active = $3, updated_at = now() WHERE chat_id = $1")
            .bind(chat_id)
            .bind(title)
            .bind(active)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ------------------------------------------------------------------ bots

    /// Marks a bot as alive. Bots whose heartbeat is older than
    /// `BOT_TIMEOUT_SECS` lose their groups to standby bots.
    pub async fn heartbeat(&self, bot_id: i64, username: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO bots (bot_id, username) VALUES ($1, $2)
             ON CONFLICT (bot_id) DO UPDATE SET username = $2, heartbeat_at = now()",
        )
        .bind(bot_id)
        .bind(username)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Makes `bot_id` the group's owner if the group has no owner, is already
    /// its own, or its owner stopped sending heartbeats. Atomic, so two bots
    /// never both own a group. Returns whether `bot_id` owns it now.
    pub async fn claim(&self, chat_id: i64, bot_id: i64, default_lang: &str) -> Result<bool> {
        let defaults = Settings { lang: default_lang.to_owned(), ..Settings::default() };
        let row: Option<(i64,)> = sqlx::query_as(&format!(
            "INSERT INTO groups (chat_id, settings, bot_id) VALUES ($1, $3, $2)
             ON CONFLICT (chat_id) DO UPDATE SET bot_id = $2
             WHERE groups.bot_id IS NULL OR groups.bot_id = $2 OR NOT EXISTS (
                 SELECT 1 FROM bots b WHERE b.bot_id = groups.bot_id
                 AND b.heartbeat_at > now() - interval '{BOT_TIMEOUT_SECS} seconds')
             RETURNING chat_id"
        ))
        .bind(chat_id)
        .bind(bot_id)
        .bind(serde_json::to_value(&defaults)?)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// Gives up a group (the bot left it); a standby bot will claim it.
    pub async fn release(&self, chat_id: i64, bot_id: i64) -> Result<()> {
        sqlx::query("UPDATE groups SET bot_id = NULL WHERE chat_id = $1 AND bot_id = $2")
            .bind(chat_id)
            .bind(bot_id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM group_bots WHERE chat_id = $1 AND bot_id = $2").bind(chat_id).bind(bot_id).execute(&self.pool).await?;
        Ok(())
    }

    /// Records that a bot is a member of a group (so it can take over later).
    pub async fn record_membership(&self, chat_id: i64, bot_id: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO group_bots (chat_id, bot_id) SELECT $1, $2 WHERE EXISTS (SELECT 1 FROM groups WHERE chat_id = $1)
             ON CONFLICT (chat_id, bot_id) DO UPDATE SET seen_at = now()",
        )
        .bind(chat_id)
        .bind(bot_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Claims every group this bot is a member of whose owner is gone.
    pub async fn claim_orphans(&self, bot_id: i64) -> Result<Vec<i64>> {
        let rows: Vec<(i64,)> = sqlx::query_as(&format!(
            "UPDATE groups g SET bot_id = $1 FROM group_bots m
             WHERE m.chat_id = g.chat_id AND m.bot_id = $1 AND g.bot_id IS DISTINCT FROM $1 AND (
                 g.bot_id IS NULL OR NOT EXISTS (
                     SELECT 1 FROM bots b WHERE b.bot_id = g.bot_id
                     AND b.heartbeat_at > now() - interval '{BOT_TIMEOUT_SECS} seconds'))
             RETURNING g.chat_id"
        ))
        .bind(bot_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Groups served by a bot.
    pub async fn owned_groups(&self, bot_id: i64) -> Result<Vec<i64>> {
        let rows: Vec<(i64,)> = sqlx::query_as("SELECT chat_id FROM groups WHERE bot_id = $1 AND active AND NOT blocked")
            .bind(bot_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Every bot: (id, username, alive, groups served).
    pub async fn bots_overview(&self) -> Result<Vec<(i64, String, bool, i64)>> {
        Ok(sqlx::query_as(&format!(
            "SELECT b.bot_id, b.username, b.heartbeat_at > now() - interval '{BOT_TIMEOUT_SECS} seconds',
                    (SELECT count(*) FROM groups g WHERE g.bot_id = b.bot_id)
             FROM bots b ORDER BY b.started_at"
        ))
        .fetch_all(&self.pool)
        .await?)
    }

    /// Every bot sharing this database: (id, username).
    pub async fn fleet(&self) -> Result<Vec<(i64, String)>> {
        Ok(sqlx::query_as("SELECT bot_id, username FROM bots").fetch_all(&self.pool).await?)
    }

    /// The bot serving a group, if any.
    pub async fn owner(&self, chat_id: i64) -> Result<Option<i64>> {
        let row: Option<(Option<i64>,)> =
            sqlx::query_as("SELECT bot_id FROM groups WHERE chat_id = $1").bind(chat_id).fetch_optional(&self.pool).await?;
        Ok(row.and_then(|(bot,)| bot))
    }

    // ----------------------------------------------------------------- users

    pub async fn upsert_user(&self, user_id: i64, first: &str, last: &str, username: Option<&str>, is_bot: bool) -> Result<()> {
        sqlx::query(
            "INSERT INTO users (user_id, first_name, last_name, username, is_bot) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (user_id) DO UPDATE SET first_name = $2, last_name = $3, username = $4, is_bot = $5, updated_at = now()",
        )
        .bind(user_id)
        .bind(first)
        .bind(last)
        .bind(username)
        .bind(is_bot)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn user_by_username(&self, username: &str) -> Result<Option<i64>> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT user_id FROM users WHERE lower(username) = lower($1) LIMIT 1")
            .bind(username)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|(id,)| id))
    }

    pub async fn user(&self, user_id: i64) -> Result<Option<UserRow>> {
        let row: Option<(String, String)> = sqlx::query_as("SELECT first_name, last_name FROM users WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|(first_name, last_name)| UserRow { first_name, last_name }))
    }

    // ----------------------------------------------------------------- ranks

    pub async fn set_rank(&self, chat_id: i64, user_id: i64, rank: Rank, expires_at: Option<i64>, by: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO ranks (chat_id, user_id, rank, expires_at, granted_by) VALUES ($1, $2, $3, to_timestamp($4::bigint), $5)
             ON CONFLICT (chat_id, user_id) DO UPDATE SET rank = $3, expires_at = to_timestamp($4::bigint), granted_by = $5",
        )
        .bind(chat_id)
        .bind(user_id)
        .bind(rank.as_db())
        .bind(expires_at)
        .bind(by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Removes a user's stored rank if it is `rank`. Returns whether it was.
    pub async fn remove_rank(&self, chat_id: i64, user_id: i64, rank: Rank) -> Result<bool> {
        let done = sqlx::query("DELETE FROM ranks WHERE chat_id = $1 AND user_id = $2 AND rank = $3")
            .bind(chat_id)
            .bind(user_id)
            .bind(rank.as_db())
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Stored ranks that have not expired: (user, rank, expires_at).
    pub async fn ranks(&self, chat_id: i64) -> Result<Vec<(i64, Rank, Option<i64>)>> {
        let rows: Vec<(i64, i16, Option<i64>)> = sqlx::query_as(
            "SELECT user_id, rank, extract(epoch FROM expires_at)::bigint FROM ranks
             WHERE chat_id = $1 AND (expires_at IS NULL OR expires_at > now()) ORDER BY rank DESC, created_at",
        )
        .bind(chat_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(user, rank, expires)| (user, Rank::from_db(rank), expires)).collect())
    }

    pub async fn clear_rank(&self, chat_id: i64, rank: Rank) -> Result<u64> {
        let done = sqlx::query("DELETE FROM ranks WHERE chat_id = $1 AND rank = $2")
            .bind(chat_id)
            .bind(rank.as_db())
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }

    /// Deletes expired temporary ranks in the groups a bot serves and returns them.
    pub async fn take_expired_ranks(&self, bot_id: i64) -> Result<Vec<(i64, i64, Rank)>> {
        let rows: Vec<(i64, i64, i16)> = sqlx::query_as(
            "DELETE FROM ranks r USING groups g
             WHERE r.chat_id = g.chat_id AND g.bot_id = $1 AND r.expires_at <= now()
             RETURNING r.chat_id, r.user_id, r.rank",
        )
        .bind(bot_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(chat, user, rank)| (chat, user, Rank::from_db(rank))).collect())
    }

    // ------------------------------------------------------------- sanctions

    pub async fn add_sanction(&self, chat_id: i64, user_id: i64, kind: &str, until: Option<i64>, actor: i64, reason: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO sanctions (chat_id, user_id, kind, until, actor, reason) VALUES ($1, $2, $3, to_timestamp($4::bigint), $5, $6)
             ON CONFLICT (chat_id, user_id, kind) DO UPDATE SET until = to_timestamp($4::bigint), actor = $5, reason = $6, created_at = now()",
        )
        .bind(chat_id)
        .bind(user_id)
        .bind(kind)
        .bind(until)
        .bind(actor)
        .bind(reason)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_sanction(&self, chat_id: i64, user_id: i64, kind: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM sanctions WHERE chat_id = $1 AND user_id = $2 AND kind = $3")
            .bind(chat_id)
            .bind(user_id)
            .bind(kind)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Active sanctions of a kind: (user, until).
    pub async fn sanctions(&self, chat_id: i64, kind: &str) -> Result<Vec<(i64, Option<i64>)>> {
        let rows = sqlx::query_as(
            "SELECT user_id, extract(epoch FROM until)::bigint FROM sanctions
             WHERE chat_id = $1 AND kind = $2 AND (until IS NULL OR until > now()) ORDER BY created_at",
        )
        .bind(chat_id)
        .bind(kind)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Deletes all sanctions of a kind and returns the users.
    pub async fn clear_sanctions(&self, chat_id: i64, kind: &str) -> Result<Vec<i64>> {
        let rows: Vec<(i64,)> =
            sqlx::query_as("DELETE FROM sanctions WHERE chat_id = $1 AND kind = $2 RETURNING user_id")
                .bind(chat_id)
                .bind(kind)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    pub async fn prune_expired_sanctions(&self) -> Result<u64> {
        let done = sqlx::query("DELETE FROM sanctions WHERE until <= now()").execute(&self.pool).await?;
        Ok(done.rows_affected())
    }

    // ----------------------------------------------------------------- warns

    /// Adds `delta` warnings (may be negative) and returns the new count.
    pub async fn change_warns(&self, chat_id: i64, user_id: i64, delta: i32) -> Result<i32> {
        let (count,): (i32,) = sqlx::query_as(
            "INSERT INTO warns (chat_id, user_id, count) VALUES ($1, $2, GREATEST($3, 0))
             ON CONFLICT (chat_id, user_id) DO UPDATE SET count = GREATEST(warns.count + $3, 0), updated_at = now()
             RETURNING count",
        )
        .bind(chat_id)
        .bind(user_id)
        .bind(delta)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    pub async fn reset_warns(&self, chat_id: i64, user_id: i64) -> Result<()> {
        sqlx::query("DELETE FROM warns WHERE chat_id = $1 AND user_id = $2").bind(chat_id).bind(user_id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn warns(&self, chat_id: i64) -> Result<Vec<(i64, i32)>> {
        Ok(sqlx::query_as("SELECT user_id, count FROM warns WHERE chat_id = $1 AND count > 0 ORDER BY count DESC")
            .bind(chat_id)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn warn_count(&self, chat_id: i64, user_id: i64) -> Result<i32> {
        let row: Option<(i32,)> = sqlx::query_as("SELECT count FROM warns WHERE chat_id = $1 AND user_id = $2")
            .bind(chat_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map_or(0, |(count,)| count))
    }

    pub async fn clear_warns(&self, chat_id: i64) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM warns WHERE chat_id = $1").bind(chat_id).execute(&self.pool).await?.rows_affected())
    }

    // --------------------------------------------------------------- filters

    pub async fn add_filter(&self, chat_id: i64, word: &str, action: Punish) -> Result<()> {
        sqlx::query(
            "INSERT INTO filters (chat_id, word, action) VALUES ($1, $2, $3)
             ON CONFLICT (chat_id, word) DO UPDATE SET action = $3",
        )
        .bind(chat_id)
        .bind(word)
        .bind(action.as_filter())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_filter(&self, chat_id: i64, word: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM filters WHERE chat_id = $1 AND word = $2").bind(chat_id).bind(word).execute(&self.pool).await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn filters(&self, chat_id: i64) -> Result<Vec<(String, Punish)>> {
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT word, action FROM filters WHERE chat_id = $1 ORDER BY word")
            .bind(chat_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|(word, action)| (word, Punish::from_filter(&action))).collect())
    }

    pub async fn clear_filters(&self, chat_id: i64) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM filters WHERE chat_id = $1").bind(chat_id).execute(&self.pool).await?.rows_affected())
    }

    // --------------------------------------------------------------- answers

    /// Adds a response to a keyword. Returns the number of responses, or
    /// `None` if the keyword already has `max` responses.
    pub async fn add_answer(&self, chat_id: i64, keyword: &str, content: &Content, max: i64) -> Result<Option<i64>> {
        let mut tx = self.pool.begin().await?;
        let (answer_id,): (i64,) = sqlx::query_as(
            "INSERT INTO answers (chat_id, keyword) VALUES ($1, $2)
             ON CONFLICT (chat_id, keyword) DO UPDATE SET keyword = EXCLUDED.keyword RETURNING id",
        )
        .bind(chat_id)
        .bind(keyword)
        .fetch_one(&mut *tx)
        .await?;
        let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM answer_responses WHERE answer_id = $1")
            .bind(answer_id)
            .fetch_one(&mut *tx)
            .await?;
        if count >= max {
            return Ok(None);
        }
        sqlx::query("INSERT INTO answer_responses (answer_id, content) VALUES ($1, $2)")
            .bind(answer_id)
            .bind(serde_json::to_value(content)?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(count + 1))
    }

    pub async fn remove_answer(&self, chat_id: i64, keyword: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM answers WHERE chat_id = $1 AND keyword = $2").bind(chat_id).bind(keyword).execute(&self.pool).await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn set_answer_audience(&self, chat_id: i64, keyword: &str, audience: Audience) -> Result<bool> {
        let done = sqlx::query("UPDATE answers SET audience = $3 WHERE chat_id = $1 AND keyword = $2")
            .bind(chat_id)
            .bind(keyword)
            .bind(audience.as_db())
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn answers(&self, chat_id: i64) -> Result<Vec<Answer>> {
        let rows: Vec<(String, String, Option<serde_json::Value>)> = sqlx::query_as(
            "SELECT a.keyword, a.audience, r.content FROM answers a
             LEFT JOIN answer_responses r ON r.answer_id = a.id WHERE a.chat_id = $1 ORDER BY a.keyword, r.id",
        )
        .bind(chat_id)
        .fetch_all(&self.pool)
        .await?;
        let mut answers: Vec<Answer> = Vec::new();
        for (keyword, audience, content) in rows {
            if answers.last().is_none_or(|last| last.keyword != keyword) {
                answers.push(Answer { keyword: keyword.clone(), audience: Audience::from_db(&audience), responses: Vec::new() });
            }
            if let Some(content) = content.and_then(|c| serde_json::from_value(c).ok()) {
                answers.last_mut().expect("pushed above").responses.push(content);
            }
        }
        Ok(answers)
    }

    pub async fn clear_answers(&self, chat_id: i64) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM answers WHERE chat_id = $1").bind(chat_id).execute(&self.pool).await?.rows_affected())
    }

    // --------------------------------------------------------------- members

    pub async fn record_join(&self, chat_id: i64, user_id: i64, invited_by: Option<i64>) -> Result<()> {
        sqlx::query(
            "INSERT INTO members (chat_id, user_id, invited_by) VALUES ($1, $2, $3)
             ON CONFLICT (chat_id, user_id) DO UPDATE SET joined_at = now(), invited_by = COALESCE($3, members.invited_by)",
        )
        .bind(chat_id)
        .bind(user_id)
        .bind(invited_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Counts an invitation by `inviter` and returns their force-add count.
    pub async fn count_invite(&self, chat_id: i64, inviter: i64) -> Result<i32> {
        let (count,): (i32,) = sqlx::query_as(
            "INSERT INTO members (chat_id, user_id, joined_at, added_count, total_added) VALUES ($1, $2, to_timestamp(0), 1, 1)
             ON CONFLICT (chat_id, user_id) DO UPDATE SET added_count = members.added_count + 1, total_added = members.total_added + 1
             RETURNING added_count",
        )
        .bind(chat_id)
        .bind(inviter)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    pub async fn member(&self, chat_id: i64, user_id: i64) -> Result<Option<MemberRow>> {
        let row: Option<(i64, i32, Option<i64>)> = sqlx::query_as(
            "SELECT extract(epoch FROM joined_at)::bigint, added_count, extract(epoch FROM force_done_at)::bigint
             FROM members WHERE chat_id = $1 AND user_id = $2",
        )
        .bind(chat_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(joined_at, added_count, force_done_at)| MemberRow { joined_at, added_count, force_done_at }))
    }

    /// Marks force-add as completed (`Some(now)`) or restarts it (`None`, which also resets the count).
    pub async fn set_force_done(&self, chat_id: i64, user_id: i64, done_at: Option<i64>) -> Result<()> {
        sqlx::query(
            "UPDATE members SET force_done_at = to_timestamp($3::bigint),
             added_count = CASE WHEN $3::bigint IS NULL THEN 0 ELSE added_count END
             WHERE chat_id = $1 AND user_id = $2",
        )
        .bind(chat_id)
        .bind(user_id)
        .bind(done_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn reset_force_adds(&self, chat_id: i64) -> Result<()> {
        sqlx::query("UPDATE members SET added_count = 0, force_done_at = NULL WHERE chat_id = $1").bind(chat_id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn top_inviters(&self, chat_id: i64, limit: i64) -> Result<Vec<(i64, i32)>> {
        Ok(sqlx::query_as(
            "SELECT user_id, total_added FROM members WHERE chat_id = $1 AND total_added > 0 ORDER BY total_added DESC LIMIT $2",
        )
        .bind(chat_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    // ----------------------------------------------------------------- stats

    /// Adds buffered message counts (see `Cache::drain_stats`).
    pub async fn add_messages(&self, chat_id: i64, user_id: i64, day: i32, count: i32) -> Result<()> {
        sqlx::query(
            "INSERT INTO message_stats (chat_id, user_id, day, messages) VALUES ($1, $2, $3, $4)
             ON CONFLICT (chat_id, day, user_id) DO UPDATE SET messages = message_stats.messages + $4",
        )
        .bind(chat_id)
        .bind(user_id)
        .bind(day)
        .bind(count)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Most active users between two local days (inclusive).
    pub async fn top_users(&self, chat_id: i64, from_day: i32, to_day: i32, limit: i64) -> Result<Vec<(i64, i64)>> {
        Ok(sqlx::query_as(
            "SELECT user_id, sum(messages)::bigint AS total FROM message_stats
             WHERE chat_id = $1 AND day BETWEEN $2 AND $3 GROUP BY user_id ORDER BY total DESC LIMIT $4",
        )
        .bind(chat_id)
        .bind(from_day)
        .bind(to_day)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Messages per day between two local days, for one user or everyone.
    pub async fn daily(&self, chat_id: i64, user_id: Option<i64>, from_day: i32, to_day: i32) -> Result<HashMap<i32, i64>> {
        let rows: Vec<(i32, i64)> = sqlx::query_as(
            "SELECT day, sum(messages)::bigint FROM message_stats
             WHERE chat_id = $1 AND day BETWEEN $2 AND $3 AND ($4::bigint IS NULL OR user_id = $4) GROUP BY day",
        )
        .bind(chat_id)
        .bind(from_day)
        .bind(to_day)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }

    /// All-time message count, for one user or everyone.
    pub async fn total_messages(&self, chat_id: i64, user_id: Option<i64>) -> Result<i64> {
        let (total,): (Option<i64>,) = sqlx::query_as(
            "SELECT sum(messages)::bigint FROM message_stats WHERE chat_id = $1 AND ($2::bigint IS NULL OR user_id = $2)",
        )
        .bind(chat_id)
        .bind(user_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(total.unwrap_or(0))
    }

    // ------------------------------------------------------ nicknames, cards

    pub async fn set_nickname(&self, chat_id: i64, user_id: i64, nickname: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO nicknames (chat_id, user_id, nickname) VALUES ($1, $2, $3)
             ON CONFLICT (chat_id, user_id) DO UPDATE SET nickname = $3",
        )
        .bind(chat_id)
        .bind(user_id)
        .bind(nickname)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_nickname(&self, chat_id: i64, user_id: i64) -> Result<bool> {
        let done = sqlx::query("DELETE FROM nicknames WHERE chat_id = $1 AND user_id = $2").bind(chat_id).bind(user_id).execute(&self.pool).await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn nicknames(&self, chat_id: i64) -> Result<HashMap<i64, String>> {
        let rows: Vec<(i64, String)> = sqlx::query_as("SELECT user_id, nickname FROM nicknames WHERE chat_id = $1")
            .bind(chat_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().collect())
    }

    pub async fn clear_nicknames(&self, chat_id: i64) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM nicknames WHERE chat_id = $1").bind(chat_id).execute(&self.pool).await?.rows_affected())
    }

    /// Stores a profile card; `chat_id` 0 is the global card.
    pub async fn set_profile(&self, chat_id: i64, user_id: i64, content: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO profiles (chat_id, user_id, content) VALUES ($1, $2, $3)
             ON CONFLICT (chat_id, user_id) DO UPDATE SET content = $3",
        )
        .bind(chat_id)
        .bind(user_id)
        .bind(content)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The group card, falling back to the global one.
    pub async fn profile(&self, chat_id: i64, user_id: i64) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT content FROM profiles WHERE user_id = $2 AND chat_id IN ($1, 0) ORDER BY chat_id = 0 LIMIT 1",
        )
        .bind(chat_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(content,)| content))
    }

    pub async fn remove_profile(&self, chat_id: i64, user_id: i64) -> Result<bool> {
        let done = sqlx::query("DELETE FROM profiles WHERE chat_id IN ($1, 0) AND user_id = $2").bind(chat_id).bind(user_id).execute(&self.pool).await?;
        Ok(done.rows_affected() > 0)
    }

    // ---------------------------------------------------------- custom texts

    pub async fn set_text(&self, chat_id: i64, key: &str, lang: &str, content: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO custom_texts (chat_id, key, lang, content) VALUES ($1, $2, $3, $4)
             ON CONFLICT (chat_id, key, lang) DO UPDATE SET content = $4",
        )
        .bind(chat_id)
        .bind(key)
        .bind(lang)
        .bind(content)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_text(&self, chat_id: i64, key: &str, lang: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM custom_texts WHERE chat_id = $1 AND key = $2 AND lang = $3")
            .bind(chat_id)
            .bind(key)
            .bind(lang)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn texts(&self, chat_id: i64, lang: &str) -> Result<HashMap<String, String>> {
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, content FROM custom_texts WHERE chat_id = $1 AND lang = $2")
            .bind(chat_id)
            .bind(lang)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().collect())
    }

    // ------------------------------------------------------------ timed pins

    pub async fn add_timed_pin(&self, chat_id: i64, message_id: i64, unpin_at: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO timed_pins (chat_id, message_id, unpin_at) VALUES ($1, $2, to_timestamp($3::bigint))
             ON CONFLICT (chat_id, message_id) DO UPDATE SET unpin_at = to_timestamp($3::bigint)",
        )
        .bind(chat_id)
        .bind(message_id)
        .bind(unpin_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Timed pins that are due in the groups a bot serves.
    pub async fn take_due_pins(&self, bot_id: i64) -> Result<Vec<(i64, i64)>> {
        Ok(sqlx::query_as(
            "DELETE FROM timed_pins p USING groups g
             WHERE p.chat_id = g.chat_id AND g.bot_id = $1 AND p.unpin_at <= now()
             RETURNING p.chat_id, p.message_id",
        )
        .bind(bot_id)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Removes a group and everything that belongs to it (used by tests).
    #[cfg(test)]
    pub async fn delete_group(&self, chat_id: i64) -> Result<()> {
        sqlx::query("DELETE FROM groups WHERE chat_id = $1").bind(chat_id).execute(&self.pool).await?;
        sqlx::query("DELETE FROM profiles WHERE chat_id = $1").bind(chat_id).execute(&self.pool).await?;
        sqlx::query("DELETE FROM audit_log WHERE chat_id = $1").bind(chat_id).execute(&self.pool).await?;
        Ok(())
    }

    // ----------------------------------------------------------------- audit

    pub async fn audit(&self, chat_id: i64, actor: i64, target: Option<i64>, action: &str, detail: &str) -> Result<()> {
        sqlx::query("INSERT INTO audit_log (chat_id, actor, target, action, detail) VALUES ($1, $2, $3, $4, $5)")
            .bind(chat_id)
            .bind(actor)
            .bind(target)
            .bind(action)
            .bind(detail)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Needs PostgreSQL: `DATABASE_URL=postgres://… cargo test -p group-bot -- --ignored`.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::locks::Lock;
    use crate::model::MediaKind;
    use crate::text::now;

    #[tokio::test]
    #[ignore = "needs PostgreSQL (DATABASE_URL)"]
    async fn every_query_runs() {
        let url = std::env::var("DATABASE_URL").expect("set DATABASE_URL");
        let repo = Repo::connect(&url, 2).await.unwrap();
        let chat = -1_000_000_000_000 - crate::text::random(1_000_000) as i64;
        let (a, b) = (111, 222);

        // Settings: defaults, atomic update, read back.
        assert_eq!(repo.settings(chat, "en").await.unwrap().lang, "en");
        let (settings, changed) = repo.update_settings(chat, "en", |s| s.locks.insert(Lock::Photo)).await.unwrap();
        assert!(changed && settings.locked(Lock::Photo));
        assert!(repo.settings(chat, "en").await.unwrap().locked(Lock::Photo));
        // Ownership and fail-over between two bots.
        let (bot_a, bot_b) = (-chat, -chat + 1);
        repo.heartbeat(bot_a, "bot_a").await.unwrap();
        repo.heartbeat(bot_b, "bot_b").await.unwrap();
        assert!(repo.claim(chat, bot_a, "en").await.unwrap(), "an unowned group is claimed");
        assert!(repo.claim(chat, bot_a, "en").await.unwrap(), "the owner keeps it");
        assert!(!repo.claim(chat, bot_b, "en").await.unwrap(), "a live owner is not replaced");
        repo.record_membership(chat, bot_b).await.unwrap();
        assert!(repo.claim_orphans(bot_b).await.unwrap().is_empty());
        sqlx::query("UPDATE bots SET heartbeat_at = now() - interval '10 minutes' WHERE bot_id = $1")
            .bind(bot_a)
            .execute(&repo.pool)
            .await
            .unwrap();
        assert_eq!(repo.claim_orphans(bot_b).await.unwrap(), vec![chat], "a standby takes over from a dead owner");
        assert_eq!(repo.owner(chat).await.unwrap(), Some(bot_b));
        repo.heartbeat(bot_a, "bot_a").await.unwrap();
        assert!(!repo.claim(chat, bot_a, "en").await.unwrap(), "the old owner does not take it back while B is alive");
        repo.release(chat, bot_b).await.unwrap();
        assert_eq!(repo.owner(chat).await.unwrap(), None);
        assert!(repo.claim(chat, bot_a, "en").await.unwrap());
        assert!(repo.bots_overview().await.unwrap().iter().any(|(id, _, alive, groups)| *id == bot_a && *alive && *groups == 1));
        repo.set_group_info(chat, "Test", true).await.unwrap();
        assert!(repo.owned_groups(bot_a).await.unwrap().contains(&chat));

        repo.upsert_user(a, "Ann", "Lee", Some("ann_test_user"), false).await.unwrap();
        assert_eq!(repo.user_by_username("ANN_TEST_USER").await.unwrap(), Some(a));
        assert_eq!(repo.user(a).await.unwrap().map(|u| u.first_name), Some("Ann".into()));

        // Ranks, including an already expired temporary one.
        repo.set_rank(chat, a, Rank::Mod, None, 1).await.unwrap();
        repo.set_rank(chat, b, Rank::Vip, Some(now() - 10), 1).await.unwrap();
        assert_eq!(repo.ranks(chat).await.unwrap(), vec![(a, Rank::Mod, None)]);
        assert!(repo.take_expired_ranks(bot_a).await.unwrap().contains(&(chat, b, Rank::Vip)));
        assert!(!repo.remove_rank(chat, a, Rank::Vip).await.unwrap());
        assert!(repo.remove_rank(chat, a, Rank::Mod).await.unwrap());
        repo.set_rank(chat, a, Rank::Owner, None, 1).await.unwrap();
        assert_eq!(repo.clear_rank(chat, Rank::Owner).await.unwrap(), 1);

        // Sanctions.
        repo.add_sanction(chat, a, "ban", None, 1, "spam").await.unwrap();
        repo.add_sanction(chat, b, "mute", Some(now() + 600), 1, "").await.unwrap();
        assert_eq!(repo.sanctions(chat, "ban").await.unwrap(), vec![(a, None)]);
        assert_eq!(repo.sanctions(chat, "mute").await.unwrap().len(), 1);
        assert!(repo.remove_sanction(chat, a, "ban").await.unwrap());
        assert_eq!(repo.clear_sanctions(chat, "mute").await.unwrap(), vec![b]);
        repo.prune_expired_sanctions().await.unwrap();

        // Warnings never go below zero.
        assert_eq!(repo.change_warns(chat, a, 1).await.unwrap(), 1);
        assert_eq!(repo.change_warns(chat, a, 1).await.unwrap(), 2);
        assert_eq!(repo.change_warns(chat, a, -5).await.unwrap(), 0);
        repo.change_warns(chat, b, 1).await.unwrap();
        assert_eq!(repo.warns(chat).await.unwrap(), vec![(b, 1)]);
        assert_eq!(repo.warn_count(chat, b).await.unwrap(), 1);
        repo.reset_warns(chat, b).await.unwrap();
        assert_eq!(repo.clear_warns(chat).await.unwrap(), 1);

        // Filters.
        repo.add_filter(chat, "spam", Punish::Ban).await.unwrap();
        assert_eq!(repo.filters(chat).await.unwrap(), vec![("spam".into(), Punish::Ban)]);
        assert!(repo.remove_filter(chat, "spam").await.unwrap());
        assert_eq!(repo.clear_filters(chat).await.unwrap(), 0);

        // Auto answers with a response limit.
        let media = Content::Media { kind: MediaKind::Photo, file_id: "x".into(), caption: String::new() };
        assert_eq!(repo.add_answer(chat, "hi", &Content::Text("hello".into()), 2).await.unwrap(), Some(1));
        assert_eq!(repo.add_answer(chat, "hi", &media, 2).await.unwrap(), Some(2));
        assert_eq!(repo.add_answer(chat, "hi", &media, 2).await.unwrap(), None);
        assert!(repo.set_answer_audience(chat, "hi", Audience::Owners).await.unwrap());
        let answers = repo.answers(chat).await.unwrap();
        assert_eq!((answers.len(), answers[0].responses.len(), answers[0].audience), (1, 2, Audience::Owners));
        assert!(repo.remove_answer(chat, "hi").await.unwrap());
        assert_eq!(repo.clear_answers(chat).await.unwrap(), 0);

        // Members, invitations and force add.
        repo.record_join(chat, b, Some(a)).await.unwrap();
        assert_eq!(repo.count_invite(chat, a).await.unwrap(), 1);
        assert_eq!(repo.count_invite(chat, a).await.unwrap(), 2);
        repo.set_force_done(chat, a, Some(now())).await.unwrap();
        let member = repo.member(chat, a).await.unwrap().unwrap();
        assert_eq!(member.added_count, 2);
        assert!(member.force_done_at.is_some());
        repo.set_force_done(chat, a, None).await.unwrap();
        assert_eq!(repo.member(chat, a).await.unwrap().unwrap().added_count, 0);
        assert!(repo.member(chat, b).await.unwrap().unwrap().joined_at > 0);
        assert_eq!(repo.top_inviters(chat, 5).await.unwrap(), vec![(a, 2)]);
        repo.reset_force_adds(chat).await.unwrap();

        // Statistics.
        repo.add_messages(chat, a, 100, 3).await.unwrap();
        repo.add_messages(chat, a, 100, 2).await.unwrap();
        repo.add_messages(chat, b, 101, 1).await.unwrap();
        assert_eq!(repo.top_users(chat, 100, 101, 10).await.unwrap(), vec![(a, 5), (b, 1)]);
        assert_eq!(repo.daily(chat, None, 100, 101).await.unwrap().get(&100), Some(&5));
        assert_eq!(repo.daily(chat, Some(b), 100, 101).await.unwrap().get(&100), None);
        assert_eq!(repo.total_messages(chat, None).await.unwrap(), 6);
        assert_eq!(repo.total_messages(chat, Some(b)).await.unwrap(), 1);

        // Nicknames, profile cards (group first, then global), custom texts.
        repo.set_nickname(chat, a, "Boss").await.unwrap();
        assert_eq!(repo.nicknames(chat).await.unwrap().get(&a), Some(&"Boss".to_owned()));
        assert!(repo.remove_nickname(chat, a).await.unwrap());
        assert_eq!(repo.clear_nicknames(chat).await.unwrap(), 0);
        repo.set_profile(0, a, "global").await.unwrap();
        assert_eq!(repo.profile(chat, a).await.unwrap(), Some("global".into()));
        repo.set_profile(chat, a, "local").await.unwrap();
        assert_eq!(repo.profile(chat, a).await.unwrap(), Some("local".into()));
        assert!(repo.remove_profile(chat, a).await.unwrap());
        repo.set_text(chat, "lock.on", "en", "Locked {name}").await.unwrap();
        assert_eq!(repo.texts(chat, "en").await.unwrap().get("lock.on"), Some(&"Locked {name}".to_owned()));
        assert!(repo.remove_text(chat, "lock.on", "en").await.unwrap());

        // Timed pins and the audit log.
        repo.add_timed_pin(chat, 5, now() - 1).await.unwrap();
        assert!(repo.take_due_pins(bot_b).await.unwrap().is_empty(), "pins belong to the owner");
        assert!(repo.take_due_pins(bot_a).await.unwrap().contains(&(chat, 5)));
        repo.audit(chat, a, Some(b), "ban", "report").await.unwrap();

        repo.delete_group(chat).await.unwrap();
        sqlx::query("DELETE FROM bots WHERE bot_id IN ($1, $2)").bind(bot_a).bind(bot_b).execute(&repo.pool).await.unwrap();
        repo.close().await;
    }
}
