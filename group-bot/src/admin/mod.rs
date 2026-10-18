//! Bot-wide administration for the bot's operators: the creators
//! (`BOT_SUDO_IDS`) and the sudo users they add. Everything happens in a
//! private chat with the bot: `/admin` opens an inline panel (see `panel`),
//! and text commands cover what needs arguments. Sudo users see a subset;
//! everything that changes the bot itself is for creators only.
//!
//! | module | what |
//! | --- | --- |
//! | `global` | bot-wide settings, feature flags, maintenance, sudo users and global bans |
//! | `panel` | the operator panel screens and buttons |
//! | `jobs` | background jobs: broadcasts, global ban sweeps, backups, pruning, group sync |
//! | `backup` | backup files and restore |
//! | `metrics` | runtime counters for monitoring |
//! | `logs` | recent log lines and error grouping |

pub mod backup;
pub mod global;
pub mod jobs;
pub mod logs;
pub mod metrics;
pub mod panel;

use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tdrust::prelude::*;
use tdrust::types::{ChatType, MessageOrigin, TextEntityType};

use self::global::{Flag, GbanMode};
use self::jobs::{Draft, Scope, Target, Task};
use crate::bot::{soft, Bot, Group};
use crate::content::{media_of, text_of, to_content};
use crate::i18n::Texts;
use crate::text::{h, mention_html, normalize_word, now, parse_duration, tokenize};

/// What an operator may do. Creators can do everything; sudo users can look
/// at everything, manage users and global bans, but not change the bot.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Access {
    Sudo,
    Creator,
}

pub fn access(bot: &Bot, user_id: i64) -> Option<Access> {
    if bot.is_creator(user_id) {
        Some(Access::Creator)
    } else if bot.is_sudo(user_id) {
        Some(Access::Sudo)
    } else {
        None
    }
}

/// Texts in the language of a private chat user: their Telegram language
/// when the bot has it, else the bot's default.
pub async fn texts_for(bot: &Bot, user_id: i64) -> Texts {
    let lang = bot
        .client
        .get_user(user_id)
        .await
        .map(|u| u.language_code)
        .ok()
        .filter(|code| crate::i18n::is_language(code))
        .unwrap_or_else(|| bot.default_lang());
    Texts::plain(&lang)
}

// ---------------------------------------------------------------- commands

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cmd {
    /// Opens a panel screen.
    Screen(&'static str),
    GBan,
    UnGBan,
    User,
    AddSudo,
    RemSudo,
    Group,
    SendTo,
    Leave,
    Broadcast,
    Backup,
    Restore,
    Maintenance,
    AlertChat,
    Cancel,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Args {
    None,
    Optional,
    Required,
}

struct Spec {
    cmd: Cmd,
    creator: bool,
    args: Args,
    phrases: &'static [&'static str],
}

#[rustfmt::skip]
const SPECS: &[Spec] = &[
    Spec { cmd: Cmd::Screen("m"), creator: false, args: Args::None, phrases: &["admin", "panel", "start", "مدیریت", "پنل", "مدیریت ربات"] },
    Spec { cmd: Cmd::Screen("st"), creator: false, args: Args::None, phrases: &["botstats", "آمار ربات"] },
    Spec { cmd: Cmd::Screen("h"), creator: false, args: Args::None, phrases: &["health", "سلامت"] },
    Spec { cmd: Cmd::Screen("ul:recent:0"), creator: false, args: Args::None, phrases: &["users", "کاربران"] },
    Spec { cmd: Cmd::Screen("ul:gban:0"), creator: false, args: Args::None, phrases: &["gbanlist", "لیست بن سراسری"] },
    Spec { cmd: Cmd::Screen("g:a:0"), creator: false, args: Args::None, phrases: &["groups", "گروه ها", "گروهها"] },
    Spec { cmd: Cmd::Screen("e"), creator: false, args: Args::None, phrases: &["errors", "خطاها", "خطا ها"] },
    Spec { cmd: Cmd::Screen("j"), creator: false, args: Args::None, phrases: &["jobs", "کارها", "کار ها"] },
    Spec { cmd: Cmd::Screen("l:w:0"), creator: false, args: Args::None, phrases: &["logs", "لاگ", "لاگ ها"] },
    Spec { cmd: Cmd::Screen("mo:1h"), creator: false, args: Args::None, phrases: &["monitor", "مانیتورینگ", "پایش"] },
    Spec { cmd: Cmd::Screen("db"), creator: false, args: Args::None, phrases: &["dbstatus", "وضعیت دیتابیس"] },
    Spec { cmd: Cmd::Screen("au"), creator: false, args: Args::None, phrases: &["auditlog", "گزارش مدیران ربات"] },
    Spec { cmd: Cmd::Screen("f"), creator: true, args: Args::None, phrases: &["flags", "ویژگی ها", "ویژگیها"] },
    Spec { cmd: Cmd::Screen("gs"), creator: true, args: Args::None, phrases: &["globalsettings", "تنظیمات سراسری"] },
    Spec { cmd: Cmd::Screen("su"), creator: true, args: Args::None, phrases: &["sudolist", "لیست سودو"] },
    Spec { cmd: Cmd::Screen("bk"), creator: true, args: Args::None, phrases: &["backups", "پشتیبان ها", "پشتیبانها"] },
    Spec { cmd: Cmd::GBan, creator: false, args: Args::Required, phrases: &["gban", "بن سراسری"] },
    Spec { cmd: Cmd::UnGBan, creator: false, args: Args::Required, phrases: &["ungban", "حذف بن سراسری"] },
    Spec { cmd: Cmd::User, creator: false, args: Args::Required, phrases: &["user", "کاربر"] },
    Spec { cmd: Cmd::Group, creator: false, args: Args::Required, phrases: &["group", "گروه"] },
    Spec { cmd: Cmd::AddSudo, creator: true, args: Args::Required, phrases: &["addsudo", "افزودن سودو"] },
    Spec { cmd: Cmd::RemSudo, creator: true, args: Args::Required, phrases: &["remsudo", "حذف سودو"] },
    Spec { cmd: Cmd::SendTo, creator: true, args: Args::Required, phrases: &["sendto", "ارسال به"] },
    Spec { cmd: Cmd::Leave, creator: true, args: Args::Required, phrases: &["leave", "خروج از گروه"] },
    Spec { cmd: Cmd::Broadcast, creator: true, args: Args::None, phrases: &["broadcast", "ارسال همگانی"] },
    Spec { cmd: Cmd::Backup, creator: true, args: Args::Optional, phrases: &["backup", "پشتیبان گیری", "پشتیبان"] },
    Spec { cmd: Cmd::Restore, creator: true, args: Args::None, phrases: &["restore", "بازیابی"] },
    Spec { cmd: Cmd::Maintenance, creator: true, args: Args::Optional, phrases: &["maintenance", "تعمیرات"] },
    Spec { cmd: Cmd::AlertChat, creator: true, args: Args::Required, phrases: &["alertchat", "کانال هشدار"] },
    Spec { cmd: Cmd::Cancel, creator: false, args: Args::None, phrases: &["cancel", "لغو"] },
];

struct Parsed {
    spec: &'static Spec,
    /// Normalized argument words.
    args: Vec<String>,
    /// The arguments as typed.
    raw: String,
}

/// Parses an operator command (with or without `/` and `@botname`).
fn parse(text: &str, bot_username: &str) -> Option<Parsed> {
    let tokens = tokenize(text);
    let first = tokens.first()?;
    let mut words: Vec<String> = tokens.iter().map(|t| t.norm.clone()).collect();
    if let Some(rest) = first.norm.strip_prefix('/') {
        let (name, target) = rest.split_once('@').unwrap_or((rest, ""));
        if !target.is_empty() && !target.eq_ignore_ascii_case(bot_username) {
            return None;
        }
        words[0] = normalize_word(name);
    }
    let mut best: Option<(usize, &'static Spec)> = None;
    for spec in SPECS {
        for phrase in spec.phrases {
            let phrase: Vec<String> = tokenize(phrase).into_iter().map(|t| t.norm).collect();
            let k = phrase.len();
            if words.len() < k || words[..k] != phrase[..] || best.is_some_and(|(len, _)| len >= k) {
                continue;
            }
            let fits = match spec.args {
                Args::None => words.len() == k,
                Args::Required => words.len() > k,
                Args::Optional => true,
            };
            if fits {
                best = Some((k, spec));
            }
        }
    }
    let (k, spec) = best?;
    let raw = tokens.get(k).map(|t| text[t.start..].trim().to_owned()).unwrap_or_default();
    Some(Parsed { spec, args: words[k..].to_vec(), raw })
}

// ---------------------------------------------------------------- sessions

/// What the next private message of an operator is for.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "kind")]
enum Session {
    Broadcast,
    SendTo { chat: i64 },
    Restore,
}

const SESSION_SECS: u64 = 15 * 60;

fn session_key(user: i64) -> String {
    format!("adm:s:{user}")
}

pub fn draft_key(user: i64) -> String {
    format!("adm:bc:{user}")
}

pub fn restore_key(user: i64) -> String {
    format!("adm:rs:{user}")
}

async fn set_session(bot: &Bot, user: i64, session: Option<Session>) {
    let key = session_key(user);
    match session {
        Some(session) => soft(bot.cache.set_json(&key, &session, SESSION_SECS).await, "admin session"),
        None => soft(bot.cache.del(&key).await, "admin session"),
    };
}

// ------------------------------------------------------------ private chat

/// Handles an operator's private message. Returns false when it is not for
/// the operator panel (the ordinary private chat handles it then).
pub async fn handle_private(bot: &Arc<Bot>, msg: &Message, user: i64) -> Result<bool> {
    let Some(access) = access(bot, user) else { return Ok(false) };
    let tx = texts_for(bot, user).await;
    let text = text_of(&msg.content).map(|t| t.text.clone()).unwrap_or_default();
    let parsed = parse(&text, &bot.username);

    if let Some(p) = &parsed {
        if p.spec.cmd == Cmd::Cancel {
            set_session(bot, user, None).await;
            soft(bot.cache.del(&restore_key(user)).await, "restore");
            bot.reply(msg, &tx.s("adm.cancelled")).await;
            return Ok(true);
        }
    }
    let session: Option<Session> = soft(bot.cache.get_json(&session_key(user)).await, "admin session").flatten();
    if let (Some(session), None) = (session, &parsed) {
        set_session(bot, user, None).await;
        on_session(bot, msg, user, &tx, session).await?;
        return Ok(true);
    }
    let Some(p) = parsed else { return Ok(false) };
    if p.spec.creator && access < Access::Creator {
        bot.reply(msg, &tx.s("adm.creator_only")).await;
        return Ok(true);
    }
    run(bot, msg, user, &tx, p).await?;
    Ok(true)
}

async fn run(bot: &Arc<Bot>, msg: &Message, user: i64, tx: &Texts, p: Parsed) -> Result<()> {
    let say = |html: String| async move {
        bot.reply(msg, &html).await;
        Ok::<(), anyhow::Error>(())
    };
    match p.spec.cmd {
        Cmd::Screen(route) => panel::open(bot, user, msg.chat_id, route).await,
        Cmd::Cancel => {}
        Cmd::GBan | Cmd::UnGBan | Cmd::AddSudo | Cmd::RemSudo | Cmd::User => {
            let Some((target, reason)) = resolve_target(bot, msg, &p).await else {
                if p.spec.cmd == Cmd::User {
                    return search(bot, msg, tx, &p.raw).await;
                }
                return say(tx.s("adm.need_user")).await;
            };
            let report = match p.spec.cmd {
                Cmd::GBan => gban(bot, user, target, &reason).await?,
                Cmd::UnGBan => ungban(bot, user, target).await?,
                Cmd::AddSudo => add_sudo(bot, user, target).await?,
                Cmd::RemSudo => remove_sudo(bot, user, target).await?,
                _ => {
                    panel::open(bot, user, msg.chat_id, &format!("u:{target}")).await;
                    return Ok(());
                }
            };
            say(tx.t(&report.key, &report.args())).await?;
        }
        Cmd::Group | Cmd::SendTo | Cmd::Leave => {
            let Some(chat) = p.args.first().and_then(|a| a.parse::<i64>().ok()).filter(|c| *c < 0) else {
                return say(tx.s("adm.need_chat")).await;
            };
            match p.spec.cmd {
                Cmd::Group => panel::open(bot, user, msg.chat_id, &format!("gi:{chat}")).await,
                Cmd::SendTo => {
                    set_session(bot, user, Some(Session::SendTo { chat })).await;
                    say(tx.t("adm.sendto_ask", &[("chat", &chat.to_string())])).await?;
                }
                _ => {
                    let jobs = leave_group(bot, user, chat, false).await?;
                    say(tx.t("adm.leave_queued", &[("n", &jobs.to_string())])).await?;
                }
            }
        }
        Cmd::Broadcast => {
            set_session(bot, user, Some(Session::Broadcast)).await;
            say(tx.s("adm.broadcast_ask")).await?;
        }
        Cmd::Backup => {
            let full = matches!(p.args.first().map(String::as_str), Some("full" | "کامل"));
            let id = jobs::submit(bot, Task::Backup { full, notify: user }, Scope::This, user).await?;
            say(tx.t("adm.job_queued", &[("id", &id.first().copied().unwrap_or(0).to_string())])).await?;
        }
        Cmd::Restore => {
            set_session(bot, user, Some(Session::Restore)).await;
            say(tx.s("adm.restore_ask")).await?;
        }
        Cmd::Maintenance => maintenance_command(bot, msg, user, tx, &p).await?,
        Cmd::AlertChat => {
            let chat = match p.args.first().map(String::as_str) {
                Some("off" | "خاموش" | "حذف") => None,
                Some("here" | "اینجا") => Some(msg.chat_id),
                Some(word) => match word.parse::<i64>() {
                    Ok(id) => Some(id),
                    Err(_) => return say(tx.s("adm.need_chat")).await,
                },
                None => None,
            };
            if let Some(chat) = chat {
                if bot.send(chat, &tx.s("adm.alert_test")).await.is_none() {
                    return say(tx.s("adm.alert_chat_failed")).await;
                }
            }
            bot.repo.update_global(move |s| s.alert_chat = chat).await?;
            bot.refresh_global().await;
            audit(bot, user, None, "alert_chat", &chat.map(|c| c.to_string()).unwrap_or_default()).await;
            let key = if chat.is_some() { "adm.alert_chat_set" } else { "adm.alert_chat_cleared" };
            say(tx.s(key)).await?;
        }
    }
    Ok(())
}

async fn maintenance_command(bot: &Arc<Bot>, msg: &Message, user: i64, tx: &Texts, p: &Parsed) -> Result<()> {
    let words: Vec<&str> = p.args.iter().map(String::as_str).collect();
    match words.first().copied() {
        None => panel::open(bot, user, msg.chat_id, "mt").await,
        Some("msg" | "message" | "پیام") => {
            let text = p.raw.split_once(char::is_whitespace).map(|(_, rest)| rest.trim().to_owned()).filter(|t| !t.is_empty());
            let html = text.as_deref().map(h);
            bot.repo.update_global(move |s| s.maintenance.message = html).await?;
            bot.refresh_global().await;
            bot.reply(msg, &tx.s(if text.is_some() { "adm.maint_message_set" } else { "adm.maint_message_reset" })).await;
        }
        Some(word) => {
            let (on, full) = match word {
                "on" | "روشن" | "فعال" => (true, false),
                "full" | "کامل" => (true, true),
                "off" | "خاموش" | "غیرفعال" => (false, false),
                _ => {
                    bot.reply(msg, &tx.s("adm.maint_usage")).await;
                    return Ok(());
                }
            };
            let until = parse_duration(&words[1..]).map(|(seconds, _)| now() + seconds.max(60));
            set_maintenance(bot, user, on, full, until).await?;
            let key = if on { "adm.maint_on" } else { "adm.maint_off" };
            bot.reply(msg, &tx.s(key)).await;
        }
    }
    Ok(())
}

pub async fn set_maintenance(bot: &Bot, actor: i64, on: bool, full: bool, until: Option<i64>) -> Result<()> {
    bot.repo
        .update_global(move |s| {
            let m = &mut s.maintenance;
            if on && !m.enabled {
                m.since = now();
            }
            m.enabled = on;
            m.full = on && full;
            m.until = if on { until } else { None };
        })
        .await?;
    bot.refresh_global().await;
    let detail = if !on { "off" } else if full { "full" } else { "commands" };
    audit(bot, actor, None, "maintenance", detail).await;
    Ok(())
}

/// Lists users matching a search, as buttons to their profiles.
async fn search(bot: &Arc<Bot>, msg: &Message, tx: &Texts, query: &str) -> Result<()> {
    let users = bot.repo.search_users(query, 12).await?;
    if users.is_empty() {
        bot.reply(msg, &tx.t("adm.search_none", &[("query", &h(query))])).await;
        return Ok(());
    }
    let (text, markup) = panel::search_results(tx, query, &users);
    bot.send_markup(msg.chat_id, &text, markup, Some(msg.id)).await;
    Ok(())
}

/// The user a command is about: a forwarded message replied to, a text
/// mention, an @username or an id. Returns the user and the rest of the text.
async fn resolve_target(bot: &Bot, msg: &Message, p: &Parsed) -> Option<(i64, String)> {
    let rest_after_first = || p.raw.split_once(char::is_whitespace).map(|(_, r)| r.trim().to_owned()).unwrap_or_default();
    if let Some(text) = text_of(&msg.content) {
        for entity in &text.entities {
            if let TextEntityType::MentionName(mention) = &entity.r#type {
                return Some((mention.user_id, rest_after_first()));
            }
        }
    }
    if let Some(tdrust::types::MessageReplyTo::Message(reply)) = &msg.reply_to {
        if let Ok(replied) = bot.client.get_message(msg.chat_id, reply.message_id).await {
            let origin = replied.forward_info.as_ref().and_then(|f| match &f.origin {
                MessageOrigin::User(o) => Some(o.sender_user_id),
                _ => None,
            });
            if let Some(user) = origin {
                return Some((user, p.raw.clone()));
            }
        }
    }
    let first = p.args.first()?;
    if let Some(username) = first.strip_prefix('@') {
        if let Ok(Some(user)) = bot.repo.user_by_username(username).await {
            return Some((user, rest_after_first()));
        }
        if let Ok(chat) = bot.client.search_public_chat(username).await {
            if let ChatType::Private(private) = chat.r#type {
                return Some((private.user_id, rest_after_first()));
            }
        }
        return None;
    }
    let id: i64 = first.parse().ok().filter(|id| *id > 0)?;
    Some((id, rest_after_first()))
}

async fn on_session(bot: &Arc<Bot>, msg: &Message, user: i64, tx: &Texts, session: Session) -> Result<()> {
    match session {
        Session::Broadcast | Session::SendTo { .. } => {
            let Some(content) = to_content(msg) else {
                bot.reply(msg, &tx.s("adm.unsupported_content")).await;
                return Ok(());
            };
            let local_path = match media_of(&msg.content) {
                Some((_, file_id)) => keep_media(bot, &file_id).await,
                None => None,
            };
            let target = match session {
                Session::SendTo { chat } => Target::Chat(chat),
                _ => Target::Groups,
            };
            let draft = Draft { content, origin_bot: bot.me, local_path, target, pin: false };
            if let Target::Chat(chat) = target {
                let serving = bot.repo.owner(chat).await?.unwrap_or(bot.me);
                let ids = jobs::submit(bot, Task::Broadcast { draft }, Scope::Bot(serving), user).await?;
                audit(bot, user, None, "send_to", &chat.to_string()).await;
                bot.reply(msg, &tx.t("adm.job_queued", &[("id", &ids.first().copied().unwrap_or(0).to_string())])).await;
                return Ok(());
            }
            soft(bot.cache.set_json(&draft_key(user), &draft, 86_400).await, "broadcast draft");
            // The preview: the message exactly as recipients will see it.
            bot.send_input(msg.chat_id, None, None, crate::content::input_content(&draft.content, |t| t.to_owned())).await;
            panel::open(bot, user, msg.chat_id, "b").await;
        }
        Session::Restore => {
            let MessageContent::Document(doc) = &msg.content else {
                bot.reply(msg, &tx.s("adm.restore_need_file")).await;
                return Ok(());
            };
            let file = bot.client.download_file(doc.document.document.id, 32, 0, 0, true).await?;
            let path = std::path::PathBuf::from(&file.local.path);
            let parsed = tokio::task::spawn_blocking({
                let path = path.clone();
                move || backup::parse(&path)
            })
            .await?;
            let parsed = match parsed {
                Ok(parsed) => parsed,
                Err(err) => {
                    bot.reply(msg, &tx.t("adm.restore_invalid", &[("error", &h(&format!("{err:#}")))])).await;
                    return Ok(());
                }
            };
            soft(bot.cache.set_json(&restore_key(user), &file.local.path, SESSION_SECS).await, "restore");
            let (text, markup) = panel::restore_confirm(bot, tx, &parsed).await;
            bot.send_markup(msg.chat_id, &text, markup, Some(msg.id)).await;
        }
    }
    Ok(())
}

/// Copies a media file into `DATA_DIR/media`, so other bots can send it
/// (Telegram file ids belong to one bot). `None` if it cannot be downloaded.
async fn keep_media(bot: &Bot, remote_id: &str) -> Option<String> {
    let file = bot.client.get_remote_file(remote_id, None).await.ok()?;
    let file = bot.client.download_file(file.id, 16, 0, 0, true).await.ok()?;
    let source = std::path::PathBuf::from(&file.local.path);
    let dir = bot.config.data_dir.join("media");
    std::fs::create_dir_all(&dir).ok()?;
    let extension = source.extension().and_then(|e| e.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    let target = dir.join(format!("{}-{}{extension}", now(), crate::text::random(1_000_000)));
    std::fs::copy(&source, &target).ok()?;
    std::fs::canonicalize(&target).ok().map(|p| p.to_string_lossy().into_owned())
}

// --------------------------------------------------------------- operations

/// An operation's report: a text key and its arguments.
pub struct Report {
    pub key: String,
    args: Vec<(&'static str, String)>,
}

impl Report {
    fn new(key: &str, args: Vec<(&'static str, String)>) -> Report {
        Report { key: key.to_owned(), args }
    }

    pub fn args(&self) -> Vec<(&str, &str)> {
        self.args.iter().map(|(k, v)| (*k, v.as_str())).collect()
    }

    pub fn render(&self, tx: &Texts) -> String {
        tx.t(&self.key, &self.args())
    }
}

async fn user_label(bot: &Bot, user: i64) -> String {
    mention_html(user, &bot.display_name(user).await)
}

/// Records an operator action in the audit log (chat 0).
pub async fn audit(bot: &Bot, actor: i64, target: Option<i64>, action: &str, detail: &str) {
    soft(bot.repo.audit(0, actor, target, action, detail).await, "audit");
}

pub async fn gban(bot: &Arc<Bot>, actor: i64, user: i64, reason: &str) -> Result<Report> {
    let label = user_label(bot, user).await;
    if bot.is_sudo(user) || bot.is_fleet(user).await {
        return Ok(Report::new("adm.gban_protected", vec![("user", label)]));
    }
    let new = bot.repo.add_gban(user, reason, actor).await?;
    bot.refresh_global().await;
    audit(bot, actor, Some(user), "gban", reason).await;
    let mut sweeps = 0;
    if bot.global().settings.gban_mode == GbanMode::Ban {
        let task = Task::GbanSweep { user_id: user, ban: true, reason: reason.to_owned() };
        sweeps = jobs::submit(bot, task, Scope::EachBot, actor).await?.len();
    }
    let key = if new { "adm.gban_done" } else { "adm.gban_updated" };
    Ok(Report::new(key, vec![("user", label), ("n", sweeps.to_string())]))
}

pub async fn ungban(bot: &Arc<Bot>, actor: i64, user: i64) -> Result<Report> {
    let label = user_label(bot, user).await;
    if !bot.repo.remove_gban(user).await? {
        return Ok(Report::new("adm.gban_absent", vec![("user", label)]));
    }
    bot.refresh_global().await;
    audit(bot, actor, Some(user), "ungban", "").await;
    let task = Task::GbanSweep { user_id: user, ban: false, reason: String::new() };
    let sweeps = jobs::submit(bot, task, Scope::EachBot, actor).await?.len();
    Ok(Report::new("adm.ungban_done", vec![("user", label), ("n", sweeps.to_string())]))
}

pub async fn add_sudo(bot: &Bot, actor: i64, user: i64) -> Result<Report> {
    let label = user_label(bot, user).await;
    if bot.is_creator(user) || bot.is_fleet(user).await {
        return Ok(Report::new("adm.sudo_is_creator", vec![("user", label)]));
    }
    let new = bot.repo.add_sudo(user, actor).await?;
    bot.refresh_global().await;
    if new {
        audit(bot, actor, Some(user), "add_sudo", "").await;
    }
    let key = if new { "adm.sudo_added" } else { "adm.sudo_exists" };
    Ok(Report::new(key, vec![("user", label)]))
}

pub async fn remove_sudo(bot: &Bot, actor: i64, user: i64) -> Result<Report> {
    let label = user_label(bot, user).await;
    let removed = bot.repo.remove_sudo(user).await?;
    bot.refresh_global().await;
    if removed {
        audit(bot, actor, Some(user), "remove_sudo", "").await;
    }
    let key = if removed { "adm.sudo_removed" } else { "adm.sudo_absent" };
    Ok(Report::new(key, vec![("user", label)]))
}

/// Makes every bot in a group leave it; `block` also keeps them out.
/// Returns the number of leave jobs queued.
pub async fn leave_group(bot: &Arc<Bot>, actor: i64, chat: i64, block: bool) -> Result<usize> {
    if block {
        bot.repo.set_blocked(chat, true).await?;
    }
    audit(bot, actor, None, if block { "block_group" } else { "leave_group" }, &chat.to_string()).await;
    let mut bots = bot.repo.group_bots(chat).await?;
    if let Some(owner) = bot.repo.owner(chat).await? {
        if !bots.contains(&owner) {
            bots.push(owner);
        }
    }
    if bots.is_empty() {
        bots.push(bot.me);
    }
    let mut queued = 0;
    for id in bots {
        queued += jobs::submit(bot, Task::Leave { chat_id: chat }, Scope::Bot(id), actor).await?.len();
    }
    Ok(queued)
}

/// Members of a group, from Telegram.
pub async fn member_count(bot: &Bot, chat: i64) -> Option<i32> {
    match bot.client.get_chat(chat).await.ok()?.r#type {
        ChatType::Supergroup(s) => {
            let count = bot.client.get_supergroup(s.supergroup_id).await.ok()?.member_count;
            if count > 0 {
                return Some(count);
            }
            bot.client.get_supergroup_full_info(s.supergroup_id).await.ok().map(|f| f.member_count)
        }
        ChatType::BasicGroup(b) => bot.client.get_basic_group(b.basic_group_id).await.ok().map(|g| g.member_count),
        _ => None,
    }
}

// -------------------------------------------------------------- enforcement

/// Deals with a globally banned user seen in a group: deletes their message
/// and (in ban mode) bans them once. Returns true if the user is banned.
pub async fn enforce_gban(bot: &Bot, g: &Group, user: i64, message_id: Option<i64>) -> bool {
    let global = bot.global();
    if !global.gbans.contains(&user) || bot.is_sudo(user) {
        return false;
    }
    if let Some(id) = message_id {
        bot.delete(g.id, vec![id]).await;
    }
    let first_time = soft(bot.cache.once(&format!("gbe:{}:{user}", g.id), 3600).await, "gban throttle") != Some(false);
    if global.settings.gban_mode == GbanMode::Ban && first_time && bot.ban(g.id, user, 0).await.is_ok() {
        soft(bot.repo.add_sanction(g.id, user, "ban", None, 0, "[gban]").await, "store sanction");
        metrics::inc(metrics::Metric::Punished);
        let text = g.t("adm.gban_enforced", &[("user", &bot.mention(g.id, user).await)]);
        bot.notice(g.id, &text, 60).await;
    }
    true
}

/// `Some(full)` while maintenance mode is on.
pub fn maintenance(bot: &Bot) -> Option<bool> {
    let global = bot.global();
    global.settings.maintenance_on(now()).then_some(global.settings.maintenance.full)
}

/// Tells a chat (at most every 10 minutes) that the bot is under maintenance.
pub async fn maintenance_notice(bot: &Bot, chat_id: i64, tx: &Texts) {
    if soft(bot.cache.once(&format!("mtn:{chat_id}"), 600).await, "maintenance notice") == Some(false) {
        return;
    }
    let global = bot.global();
    let text = match &global.settings.maintenance.message {
        Some(custom) => custom.clone(),
        None => tx.s("adm.maint_notice"),
    };
    bot.notice(chat_id, &text, 120).await;
}

/// The bot joined or left a group. Leaves blocked groups, and new groups
/// while joining is switched off or when they are too small; tells the operators.
pub async fn on_bot_membership(bot: &Arc<Bot>, chat: i64, actor: i64, joined: bool) {
    let global = bot.global();
    let tx = Texts::plain(&bot.default_lang());
    let title = bot.client.get_chat(chat).await.map(|c| c.title).unwrap_or_default();
    let actor_label = if actor == 0 { tx.s("common.unknown") } else { user_label(bot, actor).await };
    if !joined {
        if global.settings.notify_groups {
            let args = [("group", h(&title)), ("id", chat.to_string()), ("user", actor_label), ("bot", format!("@{}", bot.username))];
            alert(bot, &tx.t("adm.alert_removed", &args.iter().map(|(k, v)| (*k, v.as_str())).collect::<Vec<_>>())).await;
        }
        return;
    }
    let members = member_count(bot, chat).await;
    if let Some(n) = members {
        soft(bot.repo.set_group_members(chat, n).await, "member count");
    }
    let by_operator = bot.is_sudo(actor);
    let refusal = if soft(bot.repo.is_blocked(chat).await, "blocked group") == Some(true) {
        Some("adm.group_blocked")
    } else if !by_operator && !global.flag(Flag::NewGroups) {
        Some("adm.new_groups_closed")
    } else if !by_operator && global.settings.min_members > 0 && members.is_some_and(|n| n < global.settings.min_members as i32) {
        Some("adm.group_too_small")
    } else {
        None
    };
    if let Some(key) = refusal {
        let g = bot.group(chat).await;
        bot.send(chat, &g.t(key, &[("n", &global.settings.min_members.to_string())])).await;
        if let Err(err) = bot.client.leave_chat(chat).await {
            log::warn!("cannot leave {chat}: {err}");
        }
        soft(bot.repo.release(chat, bot.me).await, "release group");
        soft(bot.repo.deactivate_if_unserved(chat).await, "deactivate group");
    }
    if global.settings.notify_groups {
        let state = refusal.map(|key| tx.s(&format!("{key}_short"))).unwrap_or_default();
        let members = members.map_or_else(|| tx.s("common.unknown"), |n| n.to_string());
        let args = [
            ("group", h(&title)),
            ("id", chat.to_string()),
            ("user", actor_label),
            ("members", members),
            ("bot", format!("@{}", bot.username)),
            ("state", state),
        ];
        alert(bot, &tx.t("adm.alert_added", &args.iter().map(|(k, v)| (*k, v.as_str())).collect::<Vec<_>>())).await;
    }
}

// ------------------------------------------------------------------- alerts

/// Sends an alert to the alert chat, or to the creators who started this
/// bot. At most 30 alerts an hour.
pub async fn alert(bot: &Bot, html: &str) {
    let hour = now() / 3600;
    if soft(bot.cache.hit(&format!("alerts:{hour}"), 3600).await, "alert limit").is_some_and(|n| n > 30) {
        return;
    }
    if let Some(chat) = bot.global().settings.alert_chat {
        bot.send(chat, html).await;
        return;
    }
    for &creator in &bot.config.sudo_ids {
        if soft(bot.repo.has_pv(creator, bot.me).await, "alert recipient") == Some(true) {
            bot.send(creator, html).await;
        }
    }
}

/// Stores the errors logged since the last tick and alerts about new ones
/// (or ones that came back after being resolved).
pub async fn flush_errors(bot: &Bot) {
    let errors = logs::take_errors();
    if errors.is_empty() {
        return;
    }
    let alerts = bot.global().settings.error_alerts;
    let tx = Texts::plain(&bot.default_lang());
    for (fingerprint, group) in errors {
        let recorded = bot.repo.record_error(&fingerprint, "error", &group.target, &group.message, bot.me, group.count).await;
        let Some((id, previous)) = soft(recorded, "record error") else { continue };
        if !alerts || previous == Some(false) {
            continue;
        }
        if soft(bot.cache.once(&format!("erralert:{id}"), 1800).await, "error alert throttle") == Some(false) {
            continue;
        }
        let key = if previous.is_none() { "adm.alert_error_new" } else { "adm.alert_error_back" };
        let message: String = group.message.chars().take(600).collect();
        let text = tx.t(
            key,
            &[
                ("id", &id.to_string()),
                ("target", &h(&group.target)),
                ("message", &h(&message)),
                ("bot", &format!("@{}", bot.username)),
            ],
        );
        alert(bot, &text).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(text: &str) -> Option<Cmd> {
        parse(text, "MyBot").map(|p| p.spec.cmd)
    }

    #[test]
    fn parses_operator_commands() {
        assert_eq!(cmd("/admin"), Some(Cmd::Screen("m")));
        assert_eq!(cmd("/start"), Some(Cmd::Screen("m")));
        assert_eq!(cmd("gban 12345 spam"), Some(Cmd::GBan));
        assert_eq!(cmd("بن سراسری ۱۲۳۴۵"), Some(Cmd::GBan));
        assert_eq!(cmd("حذف بن سراسری 12345"), Some(Cmd::UnGBan));
        assert_eq!(cmd("gban"), None, "gban needs a user");
        assert_eq!(cmd("/gban@OtherBot 1"), None);
        assert_eq!(cmd("maintenance"), Some(Cmd::Maintenance));
        assert_eq!(cmd("maintenance on 2 h"), Some(Cmd::Maintenance));
        assert_eq!(cmd("hello there"), None);
        assert_eq!(cmd("گروه ها"), Some(Cmd::Screen("g:a:0")));
        assert_eq!(cmd("گروه -1001234"), Some(Cmd::Group));
        let p = parse("sendto -100123 hi", "b").unwrap();
        assert_eq!((p.args[0].as_str(), p.raw.as_str()), ("-100123", "-100123 hi"));
    }

    #[test]
    fn phrases_are_unique_and_screens_exist() {
        let mut seen = std::collections::HashSet::new();
        for spec in SPECS {
            for phrase in spec.phrases {
                let norm: Vec<String> = tokenize(phrase).into_iter().map(|t| t.norm).collect();
                assert!(seen.insert(norm), "phrase {phrase:?} is used twice");
            }
        }
    }
}
