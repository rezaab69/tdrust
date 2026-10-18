//! Background jobs, stored in the `jobs` table so every bot of the fleet can
//! see and run them. Work that concerns groups is split per bot (each bot
//! acts in the groups it serves); the rest runs on any bot. Each process
//! runs one job at a time, reports progress, and stops when cancelled.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tdrust::prelude::*;
use tdrust::types::InputFileLocal;

use crate::bot::{soft, Bot};
use crate::content::{input_content, input_media};
use crate::db::admin::JobRow;
use crate::i18n::Texts;
use crate::model::Content;
use crate::text::{h, now};

/// A message to broadcast.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Draft {
    pub content: Content,
    /// The bot that received the message; media file ids only work there.
    pub origin_bot: i64,
    /// A copy of the media in `DATA_DIR/media`, for the other bots.
    pub local_path: Option<String>,
    pub target: Target,
    /// Pin the message in groups.
    pub pin: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Groups,
    Users,
    All,
    Chat(i64),
}

impl Target {
    pub fn code(self) -> &'static str {
        match self {
            Target::Groups => "groups",
            Target::Users => "users",
            Target::All => "all",
            Target::Chat(_) => "chat",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Task {
    Broadcast { draft: Draft },
    Leave { chat_id: i64 },
    GbanSweep { user_id: i64, ban: bool, reason: String },
    Backup { full: bool, notify: i64 },
    Prune { days: u32 },
    SyncGroups,
}

impl Task {
    /// Stored in `jobs.kind`; also the text key `adm.job_<kind>`.
    pub fn kind(&self) -> &'static str {
        match self {
            Task::Broadcast { .. } => "broadcast",
            Task::Leave { .. } => "leave",
            Task::GbanSweep { ban: true, .. } => "gban",
            Task::GbanSweep { ban: false, .. } => "ungban",
            Task::Backup { .. } => "backup",
            Task::Prune { .. } => "prune",
            Task::SyncGroups => "sync",
        }
    }
}

/// Which bots run a job.
#[derive(Clone, Copy, Debug)]
pub enum Scope {
    /// Whichever bot is free first.
    Any,
    /// This bot.
    This,
    Bot(i64),
    /// One job for every running bot.
    EachBot,
}

/// Queues a job; returns the ids of the rows created.
pub async fn submit(bot: &Bot, task: Task, scope: Scope, by: i64) -> Result<Vec<i64>> {
    let payload = serde_json::to_value(&task)?;
    let bots: Vec<Option<i64>> = match scope {
        Scope::Any => vec![None],
        Scope::This => vec![Some(bot.me)],
        Scope::Bot(id) => vec![Some(id)],
        Scope::EachBot => bot.repo.fleet_status().await?.into_iter().filter(|b| b.alive).map(|b| Some(b.id)).collect(),
    };
    let mut ids = Vec::new();
    for target in bots {
        ids.push(bot.repo.insert_job(task.kind(), &payload, target, bot.me, by).await?);
    }
    Ok(ids)
}

/// Runs the same task again as a new job, on the same bot.
pub async fn retry(bot: &Bot, job: &JobRow, by: i64) -> Result<i64> {
    bot.repo.insert_job(&job.kind, &job.payload, job.bot_id, bot.me, by).await
}

static RUNNING: AtomicBool = AtomicBool::new(false);

/// Whether this process is running a job now.
pub fn busy() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

/// Starts the next job for this bot, unless one is already running here.
pub async fn poll(bot: &Arc<Bot>) {
    if RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let job = match bot.repo.claim_job(bot.me).await {
        Ok(Some(job)) => job,
        Ok(None) => {
            RUNNING.store(false, Ordering::Release);
            return;
        }
        Err(err) => {
            log::warn!("cannot claim a job: {err:#}");
            RUNNING.store(false, Ordering::Release);
            return;
        }
    };
    let bot = bot.clone();
    tokio::spawn(async move {
        log::info!("job {} ({}) started", job.id, job.kind);
        let outcome = match serde_json::from_value::<Task>(job.payload.clone()) {
            Ok(task) => run(&bot, &job, task).await,
            Err(err) => Err(anyhow::Error::from(err).context("unreadable job")),
        };
        let (status, result) = match outcome {
            Ok(Outcome { text, cancelled }) => (if cancelled { "cancelled" } else { "done" }, text),
            Err(err) => ("failed", format!("{err:#}")),
        };
        log::info!("job {} ({}) {status}: {result}", job.id, job.kind);
        soft(bot.repo.finish_job(job.id, status, &result).await, "finish job");
        report(&bot, &job, status, &result).await;
        RUNNING.store(false, Ordering::Release);
    });
}

/// Tells the operator who created a job how it ended (from the bot they used).
async fn report(bot: &Bot, job: &JobRow, status: &str, result: &str) {
    if job.created_by == 0 || job.origin_bot != bot.me || matches!(job.kind.as_str(), "backup") && status == "done" {
        return;
    }
    let tx = super::texts_for(bot, job.created_by).await;
    let text = tx.t(
        "adm.job_finished",
        &[
            ("id", &job.id.to_string()),
            ("kind", &tx.s(&format!("adm.job_{}", job.kind))),
            ("status", &tx.s(&format!("adm.status_{status}"))),
            ("result", &h(result)),
        ],
    );
    bot.send(job.created_by, &text).await;
}

struct Outcome {
    text: String,
    cancelled: bool,
}

impl Outcome {
    fn done(text: String) -> Outcome {
        Outcome { text, cancelled: false }
    }
}

/// Progress reporting and cancellation, checked every few seconds.
struct Progress<'a> {
    bot: &'a Bot,
    id: i64,
    total: i32,
    done: i32,
    reported: Instant,
}

impl<'a> Progress<'a> {
    async fn new(bot: &'a Bot, id: i64, total: usize) -> Progress<'a> {
        let total = i32::try_from(total).unwrap_or(i32::MAX);
        soft(bot.repo.job_progress(id, 0, total).await, "job progress");
        Progress { bot, id, total, done: 0, reported: Instant::now() }
    }

    /// Counts one item; returns true if the job should stop.
    async fn step(&mut self) -> bool {
        self.done += 1;
        if self.done < self.total && self.reported.elapsed() < Duration::from_secs(3) {
            return false;
        }
        self.reported = Instant::now();
        soft(self.bot.repo.job_progress(self.id, self.done, self.total).await, "job progress").unwrap_or(false)
    }
}

async fn run(bot: &Arc<Bot>, job: &JobRow, task: Task) -> Result<Outcome> {
    match task {
        Task::Broadcast { draft } => broadcast(bot, job.id, &draft).await,
        Task::Leave { chat_id } => {
            let left = bot.client.leave_chat(chat_id).await;
            bot.repo.release(chat_id, bot.me).await?;
            bot.repo.deactivate_if_unserved(chat_id).await?;
            bot.forget_owner(chat_id);
            Ok(Outcome::done(match left {
                Ok(_) => format!("left {chat_id}"),
                Err(err) => format!("{chat_id}: {err}"),
            }))
        }
        Task::GbanSweep { user_id, ban, reason } => gban_sweep(bot, job.id, user_id, ban, &reason).await,
        Task::Backup { full, notify } => {
            let (file, counts) = super::backup::create(bot, full).await?;
            let rows: u64 = counts.values().sum();
            let text = format!("{} ({} rows, {})", file.name, rows, crate::text::format_bytes(file.size as i64));
            if notify != 0 {
                let tx = super::texts_for(bot, notify).await;
                let caption = tx.t(
                    "adm.backup_ready",
                    &[("name", &h(&file.name)), ("rows", &rows.to_string()), ("size", &crate::text::format_bytes(file.size as i64))],
                );
                super::backup::send_file(bot, notify, &file.path, &caption).await;
            }
            Ok(Outcome::done(text))
        }
        Task::Prune { days } => {
            let today = (now() / 86_400) as i32;
            let (stats, audit, errors, jobs) = bot.repo.prune(days, today).await?;
            Ok(Outcome::done(format!("older than {days} days: {stats} stats rows, {audit} audit entries, {errors} errors, {jobs} jobs")))
        }
        Task::SyncGroups => sync_groups(bot, job.id).await,
    }
}

async fn broadcast(bot: &Bot, id: i64, draft: &Draft) -> Result<Outcome> {
    let mut chats = Vec::new();
    match draft.target {
        Target::Groups => chats.extend(bot.repo.owned_groups(bot.me).await?),
        Target::Users => chats.extend(bot.repo.broadcast_users(bot.me).await?),
        Target::All => {
            chats.extend(bot.repo.owned_groups(bot.me).await?);
            chats.extend(bot.repo.broadcast_users(bot.me).await?);
        }
        Target::Chat(chat) => chats.push(chat),
    }
    let input = match (&draft.content, &draft.local_path) {
        (Content::Media { kind, caption, .. }, Some(path)) if draft.origin_bot != bot.me => {
            let caption = (!caption.is_empty()).then(|| crate::content::html(caption));
            input_media(*kind, InputFileLocal { path: path.clone() }.into(), caption)
        }
        (Content::Media { caption, .. }, None) if draft.origin_bot != bot.me => {
            // The media cannot be sent from this bot: send its caption.
            if caption.is_empty() {
                return Ok(Outcome::done("skipped: the media is not available to this bot".into()));
            }
            InputMessageContent::text(crate::content::html(caption))
        }
        (content, _) => input_content(content, |t| t.to_owned()),
    };
    let delay = Duration::from_millis(u64::from(bot.global().settings.broadcast_delay_ms));
    let mut progress = Progress::new(bot, id, chats.len()).await;
    let (mut sent, mut failed) = (0, 0);
    for chat in chats {
        match bot.client.send_message(chat, None, None, None, None, input.clone()).await {
            Ok(message) => {
                sent += 1;
                if draft.pin && chat < 0 {
                    let message = if message.sending_state.is_some() { bot.sent.confirmed(&message).await } else { Some(message) };
                    if let Some(message) = message {
                        let _ = bot.client.pin_chat_message(chat, message.id, true, false).await;
                    }
                }
            }
            Err(err) => {
                failed += 1;
                let error = err.to_string();
                log::debug!("broadcast to {chat}: {error}");
                if chat > 0 && is_unreachable(&error) {
                    soft(bot.repo.set_pv_blocked(chat, bot.me).await, "blocked user");
                }
            }
        }
        if progress.step().await {
            return Ok(Outcome { text: format!("sent {sent}, failed {failed} (stopped)"), cancelled: true });
        }
        tokio::time::sleep(delay).await;
    }
    Ok(Outcome::done(format!("sent {sent}, failed {failed}")))
}

/// Errors meaning a private chat can never be reached again.
fn is_unreachable(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    ["blocked", "deactivated", "user_is_deleted", "chat not found", "forbidden"].iter().any(|e| error.contains(e))
}

async fn gban_sweep(bot: &Bot, id: i64, user: i64, ban: bool, reason: &str) -> Result<Outcome> {
    let chats = bot.repo.gban_sweep_chats(bot.me, user, ban).await?;
    let mut progress = Progress::new(bot, id, chats.len()).await;
    let mut done = 0;
    for chat in chats {
        let result = if ban { bot.ban(chat, user, 0).await } else { bot.unban(chat, user).await };
        if result.is_ok() {
            done += 1;
            if ban {
                let reason = if reason.is_empty() { "[gban]".to_owned() } else { format!("[gban] {reason}") };
                soft(bot.repo.add_sanction(chat, user, "ban", None, 0, &reason).await, "store sanction");
            } else {
                soft(bot.repo.remove_sanction(chat, user, "ban").await, "remove sanction");
            }
        }
        if progress.step().await {
            return Ok(Outcome { text: format!("{done} groups (stopped)"), cancelled: true });
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(Outcome::done(format!("{} {user} in {done} groups", if ban { "banned" } else { "unbanned" })))
}

/// Refreshes titles and member counts of the groups this bot serves, and
/// releases groups it is no longer in.
async fn sync_groups(bot: &Bot, id: i64) -> Result<Outcome> {
    let chats = bot.repo.owned_groups(bot.me).await?;
    let mut progress = Progress::new(bot, id, chats.len()).await;
    let (mut synced, mut gone) = (0, 0);
    for chat in chats {
        match bot.client.get_chat(chat).await {
            Ok(info) => {
                let present = bot.client.get_chat_member(chat, tdrust::types::MessageSenderUser { user_id: bot.me }).await;
                if present.is_ok_and(|m| crate::guard::is_member(&m.status)) {
                    soft(bot.repo.set_group_info(chat, &info.title, true).await, "group info");
                    if let Some(n) = super::member_count(bot, chat).await {
                        soft(bot.repo.set_group_members(chat, n).await, "member count");
                    }
                    synced += 1;
                } else {
                    release(bot, chat).await;
                    gone += 1;
                }
            }
            Err(err) if is_unreachable(&err.to_string()) || err.to_string().contains("CHANNEL_PRIVATE") => {
                release(bot, chat).await;
                gone += 1;
            }
            Err(err) => log::debug!("sync {chat}: {err}"),
        }
        if progress.step().await {
            return Ok(Outcome { text: format!("{synced} synced, {gone} left (stopped)"), cancelled: true });
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(Outcome::done(format!("{synced} synced, {gone} no longer joined")))
}

async fn release(bot: &Bot, chat: i64) {
    soft(bot.repo.release(chat, bot.me).await, "release group");
    soft(bot.repo.deactivate_if_unserved(chat).await, "deactivate group");
    bot.forget_owner(chat);
}

/// Work the scheduler starts by itself: automatic backups, pruning and a
/// daily group sync. Runs on one process of the fleet at a time.
pub async fn scheduled(bot: &Bot) -> Result<()> {
    let settings = bot.global().settings.clone();
    let t = now();
    if settings.auto_backup_hours > 0 && t - settings.last_backup >= i64::from(settings.auto_backup_hours) * 3600 {
        bot.repo.update_global(move |s| s.last_backup = t).await?;
        submit(bot, Task::Backup { full: false, notify: 0 }, Scope::Any, 0).await.context("auto backup")?;
    }
    if settings.retention_days > 0 && t - settings.last_prune >= 86_400 {
        bot.repo.update_global(move |s| s.last_prune = t).await?;
        submit(bot, Task::Prune { days: settings.retention_days }, Scope::Any, 0).await.context("auto prune")?;
    }
    Ok(())
}

/// Once a day per bot: refresh the groups it serves.
pub async fn daily_sync(bot: &Bot) {
    if soft(bot.cache.once(&format!("sync:{}", bot.me), 86_400).await, "daily sync") == Some(true) {
        soft(submit(bot, Task::SyncGroups, Scope::This, 0).await, "daily sync");
    }
}

/// Text for a job's status line.
pub fn describe(tx: &Texts, job: &JobRow) -> String {
    let status = tx.s(&format!("adm.status_{}", job.status));
    let kind = tx.s(&format!("adm.job_{}", job.kind));
    let progress = if job.total > 0 { format!(" {}/{}", job.progress, job.total) } else { String::new() };
    let stopping = if job.cancel && job.status == "running" { format!(" ({})", tx.s("adm.stopping")) } else { String::new() };
    format!("#{} {kind} — {status}{progress}{stopping}", job.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_round_trip() {
        let draft = Draft { content: Content::Text("<b>hi</b>".into()), origin_bot: 1, local_path: None, target: Target::Chat(-5), pin: true };
        for task in [
            Task::Broadcast { draft },
            Task::Leave { chat_id: -1 },
            Task::GbanSweep { user_id: 7, ban: false, reason: String::new() },
            Task::Backup { full: true, notify: 3 },
            Task::Prune { days: 30 },
            Task::SyncGroups,
        ] {
            let back: Task = serde_json::from_value(serde_json::to_value(&task).unwrap()).unwrap();
            assert_eq!(back.kind(), task.kind());
        }
    }

    #[test]
    fn detects_unreachable_chats() {
        assert!(is_unreachable("403: Forbidden: bot was blocked by the user"));
        assert!(is_unreachable("400: USER_IS_DELETED"));
        assert!(!is_unreachable("429: Too Many Requests: retry after 5"));
    }
}
