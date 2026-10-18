//! A multilingual Telegram group management bot on tdrust, PostgreSQL and Redis.
//!
//! See README.md for setup and the command reference.

mod admin;
mod bot;
mod cache;
mod commands;
mod config;
mod content;
mod db;
mod exec;
mod guard;
mod i18n;
mod locks;
mod members;
mod model;
mod panels;
mod pv;
mod scheduler;
mod stats;
mod text;

use std::sync::Arc;

use anyhow::{Context, Result};
use tdrust::prelude::*;
use tdrust::types::{ChatMemberStatus, MessageSender, UpdateChatMember};

use crate::admin::global::Flag;
use crate::admin::metrics::{self, Metric};
use crate::bot::{soft, Bot, SentMessages, UPDATE_CLAIM_SECS};
use crate::locks::Lock;
use crate::model::{Punish, Rank};

fn main() {
    // .env must be read before any thread exists: it modifies the environment.
    load_dotenv();
    admin::logs::init();
    let runtime = tokio::runtime::Runtime::new().expect("cannot start the async runtime");
    if let Err(err) = runtime.block_on(run()) {
        log::error!("{err:#}");
        std::process::exit(1);
    }
}

/// Loads `.env` from the working directory, else from this crate's directory,
/// without overriding variables that are already set. `.env` files are not
/// read by a shell, so a leading `~/` in paths is expanded here.
fn load_dotenv() {
    if dotenvy::dotenv().is_err() {
        let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env"));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    for name in ["TDJSON_PATH", "TDLIB_DATA_DIR", "LOCALES_DIR"] {
        let (Ok(value), Some(home)) = (std::env::var(name), home.as_ref()) else { continue };
        if let Some(rest) = value.strip_prefix("~/") {
            std::env::set_var(name, std::path::Path::new(home).join(rest));
        }
    }
}

async fn run() -> Result<()> {
    let config = config::Config::from_env()?;
    i18n::init(std::env::var("LOCALES_DIR").ok().as_deref())?;

    let repo = db::Repo::connect(&config.database_url, config.database_max_connections).await?;
    log::info!("PostgreSQL ready");
    let cache = cache::Cache::connect(&config.redis_url, &config.redis_prefix).await?;
    cache.ping().await.context("Redis does not answer")?;
    log::info!("Redis ready");

    let (client, mut updates) = Client::builder().database_directory(&config.tdlib_dir).device_model("group-bot").build()?;
    client.authorize(BotAuthenticator::new(config.bot_token.clone())).await?;
    let me = client.get_me().await?;
    let username = me.usernames.as_ref().map(|u| u.editable_username.clone()).unwrap_or_default();
    log::info!("running as @{username} ({})", me.id);

    // Announce this bot before claiming groups, so other bots see it alive.
    repo.heartbeat(me.id, &username).await?;
    soft(repo.boot(me.id, env!("CARGO_PKG_VERSION"), &hostname()).await, "boot record");
    soft(repo.fail_interrupted_jobs(me.id).await, "interrupted jobs");
    // Bot-wide settings, sudo users and global bans must be known before any update.
    let global = repo.load_global().await.context("cannot load the global settings")?;
    admin::logs::set_level(global.settings.log_level.as_deref());
    let bot = Arc::new(Bot {
        client: client.clone(),
        repo,
        cache,
        config,
        me: me.id,
        username,
        sent: SentMessages::default(),
        owners: Default::default(),
        fleet: Default::default(),
        global: std::sync::RwLock::new(Arc::new(global)),
        booted: text::now(),
    });
    let scheduler = tokio::spawn(scheduler::run(bot.clone()));

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            update = updates.next() => match update {
                Some(update) => route(&bot, update),
                None => break,
            },
            _ = &mut shutdown => {
                log::info!("shutting down");
                break;
            }
        }
    }
    scheduler.abort();
    client.shutdown().await;
    bot.repo.close().await;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// This machine's name, for the health screen.
fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|name| name.trim().to_owned())
        .unwrap_or_default()
}

/// Runs in the update loop: picks the updates the bot cares about and spawns their handlers.
fn route(bot: &Arc<Bot>, update: Update) {
    metrics::inc(Metric::Updates);
    match update {
        Update::NewMessage(u) if !u.message.is_outgoing => {
            metrics::inc(Metric::Messages);
            let bot = bot.clone();
            tokio::spawn(async move {
                let (chat, id) = (u.message.chat_id, u.message.id);
                let started = std::time::Instant::now();
                if let Err(err) = on_message(&bot, u.message).await {
                    log::error!("message {id} in {chat}: {err:#}");
                }
                metrics::inc(Metric::Handled);
                metrics::add(Metric::HandlerMs, started.elapsed().as_millis() as u64);
            });
        }
        Update::MessageSendSucceeded(u) => bot.sent.succeeded(u.old_message_id, u.message),
        Update::MessageSendFailed(u) => {
            metrics::inc(Metric::SendFailed);
            log::warn!("sending to {} failed: {}", u.message.chat_id, u.error.message);
            bot.sent.failed(u.old_message_id);
        }
        Update::MessageEdited(u) if u.chat_id < 0 => {
            let bot = bot.clone();
            tokio::spawn(async move {
                let key = format!("e:{}:{}:{}", u.chat_id, u.message_id, u.edit_date);
                if claim_by_id(&bot, u.chat_id, &key, UPDATE_CLAIM_SECS).await {
                    guard::check_edit(&bot, u.chat_id, u.message_id).await;
                }
            });
        }
        Update::MessageIsPinned(u) if u.is_pinned => {
            let bot = bot.clone();
            tokio::spawn(async move {
                // Short, so pinning the same message again later is checked again.
                let key = format!("p:{}:{}", u.chat_id, u.message_id);
                if claim_by_id(&bot, u.chat_id, &key, 60).await {
                    guard::check_pin(&bot, u.chat_id, u.message_id).await;
                }
            });
        }
        Update::NewCallbackQuery(q) => {
            let bot = bot.clone();
            tokio::spawn(async move { panels::on_callback(&bot, q).await });
        }
        Update::ChatMember(u) => {
            let bot = bot.clone();
            tokio::spawn(async move { on_chat_member(&bot, u).await });
        }
        _ => {}
    }
}

async fn on_message(bot: &Arc<Bot>, msg: Message) -> Result<()> {
    if msg.chat_id > 0 {
        return pv::handle(bot, &msg).await;
    }
    if msg.is_channel_post {
        return Ok(());
    }
    let chat = msg.chat_id;
    // Every bot in the group gets this message and any of them may handle it,
    // but only the first to claim it does. One bot also owns the group for its
    // scheduled jobs; the others remember they are here, to take those over.
    bot.note_membership(chat).await;
    if let Some(sender) = msg.sender_user_id() {
        if bot.is_fleet(sender).await {
            return Ok(()); // sent by another of our bots
        }
    }
    if let Some(target) = content::text_of(&msg.content).and_then(|t| commands::addressee(&t.text)) {
        if bot.is_other_fleet_bot(target).await {
            return Ok(()); // `/cmd@otherbot`: that bot handles it
        }
    }
    if !bot.first_to_handle(chat, &message_key(&msg), UPDATE_CLAIM_SECS).await {
        return Ok(());
    }
    let g = bot.group(chat).await;
    let (user, rank) = match &msg.sender_id {
        MessageSender::User(sender) => (sender.user_id, bot.rank(chat, sender.user_id).await),
        // Anonymous group admins post as the group itself.
        MessageSender::Chat(sender) if sender.chat_id == chat => (0, Rank::Mod),
        _ => (0, Rank::Member),
    };
    // Maintenance: `Some(true)` pauses everything, `Some(false)` only commands.
    let maintenance = if rank < Rank::Sudo { admin::maintenance(bot) } else { None };
    if maintenance == Some(true) {
        return Ok(());
    }
    if user != 0 && rank < Rank::Sudo && admin::enforce_gban(bot, &g, user, Some(msg.id)).await {
        return Ok(());
    }

    soft(bot.cache.push_recent(chat, msg.id).await, "recent messages");
    if user != 0 && bot.flag(Flag::Stats) {
        let (day, _) = text::local_day_minute(text::now(), g.s.tz);
        soft(bot.cache.incr_stat(chat, day as i32, user).await, "message stats");
    }
    if user != 0 {
        if let Ok(profile) = bot.client.get_user(user).await {
            bot.remember_user(&profile).await;
        }
    }

    if content::is_service(&msg.content) {
        members::on_service(bot, &g, &msg).await?;
        if g.s.locked(Lock::TgService) {
            bot.delete(chat, vec![msg.id]).await;
        }
        return Ok(());
    }

    if let MessageContent::Text(content) = &msg.content {
        let text = &content.text.text;
        let parsed = commands::parse(text, &bot.username);
        // During maintenance, commands wait (with a notice for staff); protection below keeps running.
        let paused = maintenance.is_some();
        if paused && parsed.as_ref().is_some_and(|p| rank >= p.rank && rank >= Rank::Mod) {
            admin::maintenance_notice(bot, chat, &g.tx).await;
            return Ok(());
        }
        // The "فیلتر" session: every message of the admin becomes a filtered word.
        let ends_session = matches!(parsed.as_ref().map(|p| p.kind), Some(commands::Kind::Cmd(commands::C::FilterEnd)));
        let session = format!("fsess:{chat}:{user}");
        if !paused
            && user != 0
            && rank >= Rank::Mod
            && !ends_session
            && soft(bot.cache.exists(&session).await, "filter session") == Some(true)
        {
            let word = text::normalize(text);
            bot.repo.add_filter(chat, &word, Punish::Delete).await?;
            soft(bot.cache.del(&format!("flt:{chat}")).await, "filter cache");
            let mode = exec::punish_label(&g, Punish::Delete);
            bot.reply(&msg, &g.t("filter.added", &[("word", &text::h(&word)), ("mode", &mode)])).await;
            return Ok(());
        }
        if let Some(parsed) = parsed.filter(|_| !paused) {
            if exec::run(bot, &msg, bot.group(chat).await, user, rank, parsed).await? {
                return Ok(());
            }
        }
    }

    if rank < Rank::Vip {
        let removed = if user != 0 {
            guard::check_message(bot, &g, &msg, user, rank).await
        } else if guard::violation(&g, &msg, text::now()).is_some() {
            // Posts on behalf of other channels: only the locks apply.
            bot.delete(chat, vec![msg.id]).await;
            true
        } else {
            false
        };
        if removed {
            return Ok(());
        }
    }
    if user != 0 {
        guard::auto_answer(bot, &g, &msg, user, rank).await;
    }
    Ok(())
}

/// Identifies a group message the same way for every bot that receives it.
fn message_key(msg: &Message) -> String {
    if bot::is_supergroup(msg.chat_id) {
        return format!("m:{}:{}", msg.chat_id, msg.id);
    }
    // Basic group message ids differ between bots: use sender, time and text.
    let sender = msg.sender_id.user_id().or(msg.sender_id.chat_id()).unwrap_or(0);
    let text = content::text_of(&msg.content).map_or("", |t| t.text.as_str());
    format!("m:{}:{sender}:{}:{:x}", msg.chat_id, msg.date, fnv1a(text.as_bytes()))
}

/// A hash that is the same in every build, unlike `DefaultHasher`, since
/// bots of different versions may share one Redis.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// Claims an update that names a message by id. Basic group message ids differ
/// between bots, so there the group's owner handles it alone.
async fn claim_by_id(bot: &Bot, chat_id: i64, key: &str, ttl_secs: u64) -> bool {
    if bot::is_supergroup(chat_id) {
        bot.first_to_handle(chat_id, key, ttl_secs).await
    } else {
        bot.owns(chat_id).await
    }
}

async fn on_chat_member(bot: &Arc<Bot>, u: UpdateChatMember) {
    let admin = |status: &ChatMemberStatus| matches!(status, ChatMemberStatus::Administrator(_) | ChatMemberStatus::Creator(_));
    if admin(&u.old_chat_member.status) || admin(&u.new_chat_member.status) {
        bot.invalidate_admins(u.chat_id).await;
    }
    let target = u.new_chat_member.member_id.user_id();
    if target == Some(bot.me) {
        let gone = |status: &ChatMemberStatus| matches!(status, ChatMemberStatus::Left(_) | ChatMemberStatus::Banned(_));
        let present = !gone(&u.new_chat_member.status);
        let was_present = !gone(&u.old_chat_member.status);
        bot.forget_owner(u.chat_id);
        if present {
            // Joined (or rights changed): take the group if nobody serves it.
            if bot.owns(u.chat_id).await {
                let title = bot.client.get_chat(u.chat_id).await.map(|c| c.title).unwrap_or_default();
                soft(bot.repo.set_group_info(u.chat_id, &title, true).await, "group info");
            }
            soft(bot.repo.record_membership(u.chat_id, bot.me).await, "record membership");
            if !was_present {
                admin::on_bot_membership(bot, u.chat_id, u.actor_user_id, true).await;
            }
        } else {
            // Removed: hand the group over to a standby bot, if there is one.
            soft(bot.repo.release(u.chat_id, bot.me).await, "release group");
            if was_present && u.actor_user_id != bot.me {
                admin::on_bot_membership(bot, u.chat_id, u.actor_user_id, false).await;
            }
        }
        return;
    }
    // User ids are the same for every bot, so this works in basic groups too.
    let member = target.or(u.new_chat_member.member_id.chat_id()).unwrap_or(0);
    let key = format!("c:{}:{member}:{}:{}", u.chat_id, u.actor_user_id, u.date);
    if !bot.first_to_handle(u.chat_id, &key, UPDATE_CLAIM_SECS).await {
        return;
    }
    // Anti-betrayal also counts bans and mutes done with Telegram's own tools;
    // actions through the bot are counted where they happen.
    let sanction = matches!(u.new_chat_member.status, ChatMemberStatus::Banned(_))
        || matches!(&u.new_chat_member.status, ChatMemberStatus::Restricted(r) if !r.permissions.can_send_basic_messages);
    if sanction && u.actor_user_id != bot.me && Some(u.actor_user_id) != target {
        let g = bot.group(u.chat_id).await;
        bot.record_moderation(&g, u.actor_user_id).await;
    }
}
