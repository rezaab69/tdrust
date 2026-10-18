//! The operator panel: inline screens in a private chat with the bot.
//! Navigation edits the same message; every screen can be refreshed.
//!
//! Callback data is `a:<route>` (at most 64 bytes). Screens:
//!
//! | route | screen |
//! | --- | --- |
//! | `m` | main menu |
//! | `st` | bot statistics |
//! | `h` | system health |
//! | `ul:<recent\|top\|gban>:<page>` | user lists |
//! | `u:<user>` | a user's global profile |
//! | `g:<a\|i\|b>:<page>` | groups (active, inactive, blocked) |
//! | `gi:<chat>` | one group |
//! | `b` | broadcast |
//! | `su` | sudo users |
//! | `f` | feature flags |
//! | `gs` | global settings |
//! | `mt` | maintenance mode |
//! | `j` | jobs |
//! | `e`, `ed:<id>` | errors, one error |
//! | `l:<e\|w\|i\|d>:<page>` | recent log lines |
//! | `mo:<1h\|24h\|7d>` | monitoring |
//! | `db` | database and migrations |
//! | `bk` | backups |
//! | `au` | operator audit log |
//!
//! Actions (then a screen is shown again): `ua:<act>:<user>`, `ga:<act>:<chat>`
//! (`gc:` asks first), `bn`, `bt:<target>`, `bp`, `bs`, `by`, `bx`, `sr:<user>`,
//! `ft:<flag>`, `gv:<field>:<value>`, `mv:<field>:<value>`, `jc:<id>`, `jr:<id>`,
//! `jn:<kind>`, `ex:<id>`, `ea`, `dba`, `bkn:<c\|f>`, `bkf:<n>`, `rs`, `rsy`,
//! `rsn`, `lf:<level>`, `n` (label), `x` (close).

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tdrust::types::{InlineKeyboardButton, OptionValue, UpdateNewCallbackQuery};

use super::backup::{self, Parsed};
use super::global::{Flag, GbanMode, FLAGS};
use super::jobs::{self, Draft, Scope, Target, Task};
use super::metrics::{self, Metric, Range, Series};
use super::{access, audit, logs, Access, Session};
use crate::bot::{soft, Bot};
use crate::db::admin::{GroupFilter, UserRef};
use crate::i18n::{self, Texts};
use crate::model::{Content, Rank, Settings};
use crate::panels::{colored, edit, keyboard, link, option, Row, Screen, Tone};
use crate::text::{compact_duration, fa_digits, format_bytes, h, iso_time, mention_html, now, thousands};

/// Rows per page in lists.
const PAGE: i64 = 8;

/// Who is looking, in which language.
struct View<'a> {
    bot: &'a Arc<Bot>,
    user: i64,
    access: Access,
    tx: Texts,
}

impl View<'_> {
    fn t(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.tx.t(key, args)
    }

    fn s(&self, key: &str) -> String {
        self.tx.s(key)
    }

    fn creator(&self) -> bool {
        self.access == Access::Creator
    }

    /// A number grouped by thousands, in the viewer's digits.
    fn n(&self, value: i64) -> String {
        let text = thousands(value);
        if self.tx.lang == "fa" { fa_digits(&text) } else { text }
    }

    fn dur(&self, seconds: i64) -> String {
        compact_duration(seconds, &self.tx.lang)
    }

    fn ago(&self, unix: i64) -> String {
        if unix <= 0 {
            return self.s("common.unknown");
        }
        self.t("adm.ago", &[("time", &self.dur(now() - unix))])
    }

    fn state(&self, on: bool) -> String {
        format!("{} {}", if on { "✅" } else { "❌" }, self.s(if on { "common.on" } else { "common.off" }))
    }

    fn close(&self) -> InlineKeyboardButton {
        colored(self.s("ui.close"), "a:x", Tone::Bad)
    }

    fn nav(&self, back: &str) -> Row {
        vec![link(self.s("ui.back"), format!("a:{back}")), self.close()]
    }

    fn refresh(&self, route: &str) -> InlineKeyboardButton {
        link(self.s("adm.refresh"), format!("a:{route}"))
    }

    fn header(&self, key: &str) -> Row {
        vec![label(format!("┄┄ {} ┄┄", self.s(key)))]
    }
}

/// A button that only labels a group of buttons.
fn label(text: impl Into<String>) -> InlineKeyboardButton {
    colored(text, "a:n", Tone::Plain)
}

fn toggle(on: bool, text: String, data: String) -> InlineKeyboardButton {
    colored(format!("{} {text}", if on { "✅" } else { "❌" }), data, if on { Tone::Good } else { Tone::Bad })
}

fn today() -> i32 {
    (now() / 86_400) as i32
}

fn date_of_day(day: i32) -> String {
    iso_time(i64::from(day) * 86_400)[..10].to_owned()
}

fn short(text: &str, chars: usize) -> String {
    let mut out: String = text.chars().take(chars).collect();
    if text.chars().count() > chars {
        out.push('…');
    }
    out
}

fn user_line(user: &UserRef) -> String {
    let name = if user.name.is_empty() { user.id.to_string() } else { user.name.clone() };
    let username = user.username.as_ref().map(|u| format!(" @{}", h(u))).unwrap_or_default();
    format!("{}{username} <code>{}</code>", mention_html(user.id, &name), user.id)
}

fn user_button(user: &UserRef) -> InlineKeyboardButton {
    let name = if user.name.is_empty() { user.id.to_string() } else { short(&user.name, 20) };
    link(format!("👤 {name}"), format!("a:u:{}", user.id))
}

fn pager(route: impl Fn(i64) -> String, page: i64, total: i64) -> Row {
    let mut row = Vec::new();
    if page > 0 {
        row.push(link("◀️", route(page - 1)));
    }
    if (page + 1) * PAGE < total {
        row.push(link("▶️", route(page + 1)));
    }
    row
}

/// Routes (first part) that only creators may open or use.
fn creator_only(head: &str) -> bool {
    matches!(
        head,
        "b" | "bn" | "bt" | "bp" | "bs" | "by" | "bx" | "su" | "sr" | "f" | "ft" | "gs" | "gv" | "mt" | "mv" | "bk" | "bkn" | "bkf"
            | "rs" | "rsy" | "rsn" | "jc" | "jr" | "jn" | "dba" | "gc"
    )
}

// ------------------------------------------------------------ entry points

/// Sends a screen as a new message.
pub async fn open(bot: &Arc<Bot>, user: i64, chat: i64, route: &str) {
    let Some(access) = access(bot, user) else { return };
    let v = View { bot, user, access, tx: super::texts_for(bot, user).await };
    match render(&v, route).await {
        Ok((text, markup)) => {
            bot.send_markup(chat, &text, markup, None).await;
        }
        Err(err) => {
            log::error!("operator screen {route}: {err:#}");
            bot.send(chat, &v.s("err.internal")).await;
        }
    }
}

/// Handles a button of the operator panel; returns the toast.
pub async fn on_callback(bot: &Arc<Bot>, q: &UpdateNewCallbackQuery, data: &str) -> String {
    let Some(access) = access(bot, q.sender_user_id) else {
        return Texts::plain(&bot.default_lang()).s("adm.no_access");
    };
    let v = View { bot, user: q.sender_user_id, access, tx: super::texts_for(bot, q.sender_user_id).await };
    let route = data.strip_prefix("a:").unwrap_or("m");
    match act(&v, q.chat_id, q.message_id, route).await {
        Ok(toast) => toast,
        Err(err) => {
            log::error!("operator button {route}: {err:#}");
            v.s("err.internal")
        }
    }
}

/// Runs a button's action, then shows the next screen. Returns the toast.
async fn act(v: &View<'_>, chat: i64, message: i64, route: &str) -> Result<String> {
    let bot = v.bot;
    let parts: Vec<&str> = route.split(':').collect();
    if creator_only(parts[0]) && !v.creator() {
        return Ok(v.s("adm.creator_only"));
    }
    let id = |i: usize| parts.get(i).and_then(|p| p.parse::<i64>().ok()).unwrap_or(0);
    let (toast, next): (String, String) = match parts.as_slice() {
        ["n"] => return Ok(String::new()),
        ["x"] => {
            bot.delete(chat, vec![message]).await;
            return Ok(String::new());
        }
        ["ua", action, _] => {
            let user = id(2);
            let toast = user_action(v, action, user).await?;
            (toast, format!("u:{user}"))
        }
        ["gc", action, _] => {
            let group = id(2);
            let title = bot.client.get_chat(group).await.map(|c| c.title).unwrap_or_else(|_| group.to_string());
            let text = v.t("adm.confirm_group", &[("action", &v.s(&format!("adm.ga_{action}"))), ("group", &h(&title))]);
            let rows = vec![vec![
                colored(v.s("ui.yes"), format!("a:ga:{action}:{group}"), Tone::Bad),
                link(v.s("ui.no"), format!("a:gi:{group}")),
            ]];
            edit(bot, chat, message, (text, keyboard(rows))).await;
            return Ok(String::new());
        }
        ["ga", action, _] => {
            let group = id(2);
            let toast = group_action(v, action, group).await?;
            (toast, format!("gi:{group}"))
        }
        ["bn"] => {
            super::set_session(bot, v.user, Some(Session::Broadcast)).await;
            bot.send(chat, &v.s("adm.broadcast_ask")).await;
            (v.s("adm.broadcast_ask_short"), "b".into())
        }
        ["bt", target] => {
            let target = match *target {
                "users" => Target::Users,
                "all" => Target::All,
                _ => Target::Groups,
            };
            change_draft(bot, v.user, |d| d.target = target).await;
            (String::new(), "b".into())
        }
        ["bp"] => {
            change_draft(bot, v.user, |d| d.pin = !d.pin).await;
            (String::new(), "b".into())
        }
        ["bx"] => {
            soft(bot.cache.del(&super::draft_key(v.user)).await, "broadcast draft");
            (v.s("adm.broadcast_discarded"), "b".into())
        }
        ["bs"] => {
            let Some(draft) = draft(bot, v.user).await else { return Ok(v.s("adm.broadcast_none")) };
            let recipients = recipients(bot, draft.target).await?;
            let text = v.t("adm.broadcast_confirm", &[("n", &v.n(recipients))]);
            let rows = vec![vec![colored(v.s("adm.send_now"), "a:by", Tone::Good), link(v.s("ui.no"), "a:b")]];
            edit(bot, chat, message, (text, keyboard(rows))).await;
            return Ok(String::new());
        }
        ["by"] => {
            let Some(draft) = draft(bot, v.user).await else { return Ok(v.s("adm.broadcast_none")) };
            let target = draft.target.code();
            let ids = jobs::submit(bot, Task::Broadcast { draft }, Scope::EachBot, v.user).await?;
            soft(bot.cache.del(&super::draft_key(v.user)).await, "broadcast draft");
            audit(bot, v.user, None, "broadcast", target).await;
            let text = v.t("adm.broadcast_queued", &[("n", &v.n(ids.len() as i64))]);
            let rows = vec![vec![link(v.s("adm.b_jobs"), "a:j")], v.nav("m")];
            edit(bot, chat, message, (text, keyboard(rows))).await;
            return Ok(String::new());
        }
        ["sr", _] => {
            let report = super::remove_sudo(bot, v.user, id(1)).await?;
            (report.render(&v.tx), "su".into())
        }
        ["ft", code] => {
            let Some(flag) = Flag::from_code(code) else { return Ok(String::new()) };
            let on = !bot.flag(flag);
            bot.repo.update_global(move |s| s.set_flag(flag, on)).await?;
            bot.refresh_global().await;
            audit(bot, v.user, None, "flag", &format!("{}={on}", flag.code())).await;
            let name = v.s(&format!("adm.flag_{code}"));
            (v.t("adm.flag_changed", &[("name", &name), ("state", &v.state(on))]), "f".into())
        }
        ["gv", field, value] => {
            let toast = set_global(v, field, value).await?;
            (toast, "gs".into())
        }
        ["mv", field, value] => {
            maintenance_action(v, field, value).await?;
            (String::new(), "mt".into())
        }
        ["jc", _] => {
            let toast = if bot.repo.cancel_job(id(1)).await? { v.s("adm.job_cancelled") } else { v.s("adm.job_not_running") };
            (toast, "j".into())
        }
        ["jr", _] => {
            let toast = match bot.repo.job(id(1)).await? {
                Some(job) => {
                    let new = jobs::retry(bot, &job, v.user).await?;
                    v.t("adm.job_queued", &[("id", &new.to_string())])
                }
                None => String::new(),
            };
            (toast, "j".into())
        }
        ["jn", kind] => {
            let (task, scope) = match *kind {
                "sync" => (Task::SyncGroups, Scope::EachBot),
                "backup" => (Task::Backup { full: false, notify: v.user }, Scope::This),
                _ => {
                    let days = match bot.global().settings.retention_days {
                        0 => 90,
                        days => days,
                    };
                    (Task::Prune { days }, Scope::Any)
                }
            };
            let ids = jobs::submit(bot, task, scope, v.user).await?;
            audit(bot, v.user, None, "job", kind).await;
            (v.t("adm.jobs_queued", &[("n", &v.n(ids.len() as i64))]), "j".into())
        }
        ["ex", _] => {
            bot.repo.resolve_error(id(1)).await?;
            (v.s("adm.error_resolved"), "e".into())
        }
        ["ea"] => {
            let n = bot.repo.resolve_all_errors().await?;
            (v.t("adm.errors_resolved", &[("n", &v.n(n as i64))]), "e".into())
        }
        ["dba"] => {
            let started = Instant::now();
            bot.repo.analyze().await?;
            audit(bot, v.user, None, "analyze", "").await;
            (v.t("adm.analyzed", &[("ms", &v.n(started.elapsed().as_millis() as i64))]), "db".into())
        }
        ["bkn", kind] => {
            let ids = jobs::submit(bot, Task::Backup { full: *kind == "f", notify: v.user }, Scope::This, v.user).await?;
            audit(bot, v.user, None, "backup", kind).await;
            (v.t("adm.job_queued", &[("id", &ids.first().copied().unwrap_or(0).to_string())]), "bk".into())
        }
        ["bkf", _] => {
            let files = backup::list(bot);
            let toast = match files.get(id(1) as usize) {
                Some(file) => {
                    let caption = v.t("adm.backup_file", &[("name", &h(&file.name)), ("size", &format_bytes(file.size as i64))]);
                    if backup::send_file(bot, v.user, &file.path, &caption).await { v.s("adm.file_sent") } else { v.s("adm.file_failed") }
                }
                None => String::new(),
            };
            (toast, "bk".into())
        }
        ["rs"] => {
            super::set_session(bot, v.user, Some(Session::Restore)).await;
            bot.send(chat, &v.s("adm.restore_ask")).await;
            (v.s("adm.restore_ask_short"), "bk".into())
        }
        ["rsn"] => {
            soft(bot.cache.del(&super::restore_key(v.user)).await, "restore");
            edit(bot, chat, message, (v.s("adm.cancelled"), keyboard(vec![v.nav("bk")]))).await;
            return Ok(String::new());
        }
        ["rsy"] => {
            let path: Option<String> = soft(bot.cache.get_json(&super::restore_key(v.user)).await, "restore").flatten();
            let Some(path) = path else { return Ok(v.s("adm.restore_expired")) };
            soft(bot.cache.del(&super::restore_key(v.user)).await, "restore");
            edit(bot, chat, message, (v.s("adm.restoring"), keyboard(vec![]))).await;
            let parsed = tokio::task::spawn_blocking(move || backup::parse(std::path::Path::new(&path))).await?;
            let text = match parsed {
                Ok(parsed) => match backup::restore(bot, &parsed).await {
                    Ok(safety) => {
                        audit(bot, v.user, None, "restore", &parsed.rows().to_string()).await;
                        v.t("adm.restored", &[("rows", &v.n(parsed.rows() as i64)), ("safety", &h(&safety.name))])
                    }
                    Err(err) => v.t("adm.restore_failed", &[("error", &h(&format!("{err:#}")))]),
                },
                Err(err) => v.t("adm.restore_invalid", &[("error", &h(&format!("{err:#}")))]),
            };
            edit(bot, chat, message, (text, keyboard(vec![v.nav("bk")]))).await;
            return Ok(String::new());
        }
        ["lf", level] => {
            let dump = logs::dump(level_of(level));
            let path = bot.config.data_dir.join(format!("logs-{}.txt", now()));
            let sent = std::fs::create_dir_all(&bot.config.data_dir).is_ok()
                && std::fs::write(&path, dump).is_ok()
                && backup::send_file(bot, v.user, &path, &v.s("adm.log_file")).await;
            // The upload keeps its own copy.
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let _ = std::fs::remove_file(&path);
            (v.s(if sent { "adm.file_sent" } else { "adm.file_failed" }), format!("l:{level}:0"))
        }
        _ => (String::new(), route.to_owned()),
    };
    let screen = render(v, &next).await?;
    edit(bot, chat, message, screen).await;
    Ok(toast)
}

/// Draws a screen by route; unknown routes show the main menu.
async fn render(v: &View<'_>, route: &str) -> Result<Screen> {
    let parts: Vec<&str> = route.split(':').collect();
    if creator_only(parts[0]) && !v.creator() {
        return main(v).await;
    }
    let number = |i: usize| parts.get(i).and_then(|p| p.parse::<i64>().ok()).unwrap_or(0);
    match parts.as_slice() {
        ["st"] => stats(v).await,
        ["h"] => health(v).await,
        ["ul", kind, _] => users(v, kind, number(2).max(0)).await,
        ["u", _] => user(v, number(1)).await,
        ["g", filter, _] => groups(v, GroupFilter::from_code(filter), number(2).max(0)).await,
        ["gi", _] => group(v, number(1)).await,
        ["b"] => broadcast(v).await,
        ["su"] => sudo(v).await,
        ["f"] => Ok(flags(v)),
        ["gs"] => Ok(global_settings(v)),
        ["mt"] => Ok(maintenance(v)),
        ["j"] => jobs_screen(v).await,
        ["e"] => errors(v).await,
        ["ed", _] => error(v, number(1)).await,
        ["l", level, _] => Ok(log_lines(v, level, number(2).max(0) as usize)),
        ["mo", range] => monitor(v, Range::from_code(range)).await,
        ["db"] => database(v).await,
        ["bk"] => Ok(backups(v)),
        ["au"] => operator_audit(v).await,
        _ => main(v).await,
    }
}

// ------------------------------------------------------------------ screens

async fn main(v: &View<'_>) -> Result<Screen> {
    let bot = v.bot;
    let global = bot.global();
    let fleet = bot.repo.fleet_status().await?;
    let alive = fleet.iter().filter(|b| b.alive).count() as i64;
    let (groups, users) = bot.repo.audience().await?;
    let maintenance = if global.settings.maintenance_on(now()) { v.s("adm.main_maint_on") } else { String::new() };
    let role = v.s(if v.creator() { "adm.role_creator" } else { "adm.role_sudo" });
    let text = v.t(
        "adm.main",
        &[
            ("role", &role),
            ("groups", &v.n(groups)),
            ("users", &v.n(users)),
            ("alive", &v.n(alive)),
            ("bots", &v.n(fleet.len() as i64)),
            ("maintenance", &maintenance),
        ],
    );
    let items: [(&str, &str); 17] = [
        ("adm.b_stats", "st"),
        ("adm.b_health", "h"),
        ("adm.b_users", "ul:recent:0"),
        ("adm.b_groups", "g:a:0"),
        ("adm.b_broadcast", "b"),
        ("adm.b_gbans", "ul:gban:0"),
        ("adm.b_sudo", "su"),
        ("adm.b_flags", "f"),
        ("adm.b_settings", "gs"),
        ("adm.b_maintenance", "mt"),
        ("adm.b_jobs", "j"),
        ("adm.b_errors", "e"),
        ("adm.b_logs", "l:w:0"),
        ("adm.b_monitor", "mo:1h"),
        ("adm.b_db", "db"),
        ("adm.b_backup", "bk"),
        ("adm.b_audit", "au"),
    ];
    let buttons: Vec<_> = items
        .iter()
        .filter(|(_, route)| v.creator() || !creator_only(route.split(':').next().unwrap_or_default()))
        .map(|(key, route)| link(v.s(key), format!("a:{route}")))
        .collect();
    let mut rows: Vec<Row> = buttons.chunks(2).map(<[_]>::to_vec).collect();
    rows.push(vec![v.refresh("m"), v.close()]);
    Ok((text, keyboard(rows)))
}

async fn stats(v: &View<'_>) -> Result<Screen> {
    let bot = v.bot;
    let (o, m) = bot.repo.overview(today()).await?;
    let top: Vec<String> = bot
        .repo
        .top_groups(today() - 6, 5)
        .await?
        .into_iter()
        .map(|(chat, title, messages)| {
            let title = if title.is_empty() { chat.to_string() } else { short(&title, 40) };
            format!("• {} — {}", h(&title), v.n(messages))
        })
        .collect();
    let actions: Vec<String> =
        bot.repo.audit_actions(24).await?.into_iter().map(|(action, n)| format!("• {} — {}", h(&action), v.n(n))).collect();
    let fleet = bot.repo.fleet_status().await?;
    let none = v.s("adm.empty");
    let text = v.t(
        "adm.stats",
        &[
            ("groups", &v.n(o.groups_active)),
            ("groups_total", &v.n(o.groups)),
            ("blocked", &v.n(o.groups_blocked)),
            ("groups_1d", &v.n(o.groups_1d)),
            ("groups_7d", &v.n(o.groups_7d)),
            ("members", &v.n(o.members)),
            ("users", &v.n(o.users)),
            ("users_1d", &v.n(o.users_1d)),
            ("users_7d", &v.n(o.users_7d)),
            ("active_1d", &v.n(o.active_1d)),
            ("active_7d", &v.n(o.active_7d)),
            ("pv", &v.n(o.pv)),
            ("pv_blocked", &v.n(o.pv_blocked)),
            ("msg_1d", &v.n(m.msg_1d)),
            ("msg_7d", &v.n(m.msg_7d)),
            ("msg_total", &v.n(m.msg_total)),
            ("bans", &v.n(m.bans)),
            ("mutes", &v.n(m.mutes)),
            ("warns", &v.n(m.warns)),
            ("gbans", &v.n(m.gbans)),
            ("sudo", &v.n(m.sudo)),
            ("filters", &v.n(m.filters)),
            ("answers", &v.n(m.answers)),
            ("ranks", &v.n(m.ranks)),
            ("top", &if top.is_empty() { none.clone() } else { top.join("\n") }),
            ("actions", &if actions.is_empty() { none } else { actions.join("\n") }),
            ("alive", &v.n(fleet.iter().filter(|b| b.alive).count() as i64)),
            ("bots", &v.n(fleet.len() as i64)),
            ("uptime", &v.dur(now() - bot.booted)),
        ],
    );
    let rows = vec![
        vec![link(v.s("adm.ul_top"), "a:ul:top:0"), link(v.s("adm.b_groups"), "a:g:a:0")],
        vec![link(v.s("adm.b_monitor"), "a:mo:24h"), v.refresh("st")],
        v.nav("m"),
    ];
    Ok((text, keyboard(rows)))
}

/// Resident memory (bytes) and threads of this process, where the OS tells.
fn process_memory() -> Option<(i64, i64)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| {
        status
            .lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|n| n.parse::<i64>().ok())
    };
    Some((field("VmRSS:")? * 1024, field("Threads:").unwrap_or(0)))
}

async fn health(v: &View<'_>) -> Result<Screen> {
    let bot = v.bot;
    let ms = |ms: u128| format!("{} ms", v.n(ms as i64));
    let db = bot.repo.ping().await;
    let started = Instant::now();
    let redis = bot.cache.ping().await.map(|()| started.elapsed().as_millis());
    let started = Instant::now();
    let tdlib = bot.client.get_option("version").await.map(|value| {
        let version = match value {
            OptionValue::String(s) => s.value,
            _ => String::new(),
        };
        (version, started.elapsed().as_millis())
    });
    let fleet = soft(bot.repo.fleet_status().await, "fleet").unwrap_or_default();
    let errors = soft(bot.repo.error_counts().await, "error counts");
    let buffered = soft(bot.cache.set_size("st:keys").await, "stats buffer").unwrap_or(0);
    let tick_at = metrics::TICK_AT.load(Ordering::Relaxed);

    let mut warnings = 0;
    let db_line = match &db {
        Ok(t) => {
            warnings += usize::from(*t > 200);
            format!("🟢 {}", ms(*t))
        }
        Err(err) => format!("🔴 {}", h(&short(&err.to_string(), 80))),
    };
    let redis_line = match &redis {
        Ok(t) => {
            warnings += usize::from(*t > 50);
            format!("🟢 {}", ms(*t))
        }
        Err(err) => format!("🔴 {}", h(&short(&err.to_string(), 80))),
    };
    let tdlib_line = match &tdlib {
        Ok((version, t)) => format!("🟢 {} · {}", h(version), ms(*t)),
        Err(err) => format!("🔴 {}", h(&short(&err.to_string(), 80))),
    };
    let mut bots = Vec::new();
    for b in &fleet {
        // Bots silent for over a day are retired, not down.
        if !b.alive && b.silent_for < 86_400 {
            warnings += 1;
        }
        let state = if b.alive { "🟢" } else { "🔴" };
        let host = if b.host.is_empty() { String::new() } else { format!(" · {}", h(&b.host)) };
        let version = if b.version.is_empty() { String::new() } else { format!(" · v{}", h(&b.version)) };
        bots.push(v.t(
            "adm.health_bot",
            &[
                ("state", state),
                ("name", &h(&format!("@{}", b.username))),
                ("groups", &v.n(b.groups)),
                ("uptime", &v.dur(now() - b.booted_at)),
                ("beat", &v.dur(b.silent_for)),
                ("info", &format!("{version}{host}")),
            ],
        ));
    }
    let tick_age = now() - tick_at;
    warnings += usize::from(tick_at > 0 && tick_age > 120);
    let open_errors = errors.as_ref().map_or(0, |e| e.last_hour);
    warnings += usize::from(open_errors > 0);
    let verdict = if db.is_err() || redis.is_err() || tdlib.is_err() {
        v.s("adm.health_down")
    } else if warnings > 0 {
        v.s("adm.health_degraded")
    } else {
        v.s("adm.health_ok")
    };
    let (memory, threads) = match process_memory() {
        Some((rss, threads)) => (format_bytes(rss), v.n(threads)),
        None => (v.s("common.unknown"), v.s("common.unknown")),
    };
    let (open, idle, max) = bot.repo.pool_stats();
    let tick = if tick_at == 0 {
        v.s("adm.tick_none")
    } else {
        v.t("adm.tick", &[("ago", &v.ago(tick_at)), ("ms", &v.n(metrics::TICK_MS.load(Ordering::Relaxed) as i64))])
    };
    let text = v.t(
        "adm.health",
        &[
            ("verdict", &verdict),
            ("db", &db_line),
            ("redis", &redis_line),
            ("tdlib", &tdlib_line),
            ("uptime", &v.dur(now() - bot.booted)),
            ("memory", &memory),
            ("threads", &threads),
            ("pool", &format!("{}/{} ({} {})", v.n(i64::from(open)), v.n(i64::from(max)), v.n(idle as i64), v.s("adm.idle"))),
            ("tick", &tick),
            ("job", &v.s(if jobs::busy() { "adm.job_busy" } else { "adm.job_idle" })),
            ("buffer", &v.n(buffered)),
            ("errors", &v.n(open_errors)),
            ("bots", &bots.join("\n")),
        ],
    );
    let rows = vec![
        vec![link(v.s("adm.b_monitor"), "a:mo:1h"), link(v.s("adm.b_errors"), "a:e")],
        vec![link(v.s("adm.b_db"), "a:db"), v.refresh("h")],
        v.nav("m"),
    ];
    Ok((text, keyboard(rows)))
}

async fn users(v: &View<'_>, kind: &str, page: i64) -> Result<Screen> {
    let bot = v.bot;
    let offset = page * PAGE;
    let (list, total) = match kind {
        "top" => bot.repo.top_users_global(today() - 6, PAGE, offset).await?,
        "gban" => bot.repo.gban_list(PAGE, offset).await?,
        _ => bot.repo.recent_users(PAGE, offset).await?,
    };
    let lines: Vec<String> = list
        .iter()
        .map(|u| {
            let extra = match kind {
                "top" => v.t("adm.n_messages", &[("n", &v.n(u.value))]),
                "gban" if !u.note.is_empty() => h(&short(&u.note, 60)),
                "gban" => v.s("common.no_reason"),
                _ => v.ago(u.value),
            };
            format!("• {} — {extra}", user_line(u))
        })
        .collect();
    let kind = if matches!(kind, "top" | "gban") { kind } else { "recent" };
    let text = v.t(
        "adm.users",
        &[
            ("title", &v.s(&format!("adm.ul_{kind}"))),
            ("n", &v.n(total)),
            ("list", &if lines.is_empty() { v.s("adm.empty") } else { lines.join("\n") }),
        ],
    );
    let mut rows: Vec<Row> = list.iter().map(user_button).collect::<Vec<_>>().chunks(2).map(<[_]>::to_vec).collect();
    rows.push(pager(|p| format!("a:ul:{kind}:{p}"), page, total));
    rows.push(
        ["recent", "top", "gban"]
            .iter()
            .map(|k| option(v.s(&format!("adm.ul_{k}")), *k == kind, format!("a:ul:{k}:0")))
            .collect(),
    );
    rows.push(v.nav("m"));
    Ok((text, keyboard(rows)))
}

async fn user(v: &View<'_>, id: i64) -> Result<Screen> {
    let bot = v.bot;
    let card = bot.repo.user_card(id).await?;
    let live = bot.client.get_user(id).await.ok();
    let name = match (&live, &card) {
        (Some(u), _) => format!("{} {}", u.first_name, u.last_name).trim().to_owned(),
        (None, Some(c)) => c.name.clone(),
        _ => id.to_string(),
    };
    let username = live
        .as_ref()
        .and_then(|u| u.usernames.as_ref().map(|n| n.editable_username.clone()))
        .filter(|u| !u.is_empty())
        .or_else(|| card.as_ref().and_then(|c| c.username.clone()));
    let totals = bot.repo.user_totals(id, today()).await?;
    let groups: Vec<String> = bot
        .repo
        .user_groups(id, 8)
        .await?
        .into_iter()
        .map(|(chat, title, messages, rank)| {
            let title = if title.is_empty() { chat.to_string() } else { short(&title, 32) };
            let rank = rank.map(|r| format!(" · {}", v.s(Rank::from_db(r).key()))).unwrap_or_default();
            format!("• {} — {}{rank} <code>{chat}</code>", h(&title), v.t("adm.n_messages", &[("n", &v.n(messages))]))
        })
        .collect();
    let bots: Vec<String> = bot
        .repo
        .user_bots(id)
        .await?
        .into_iter()
        .map(|(name, blocked)| if blocked { format!("@{} ({})", h(&name), v.s("adm.blocked_bot")) } else { format!("@{}", h(&name)) })
        .collect();
    let gban = bot.repo.gban(id).await?;
    let is_bot = live.as_ref().map(|u| matches!(u.r#type, tdrust::types::UserType::Bot(_))).or(card.as_ref().map(|c| c.is_bot));
    let status = if bot.is_creator(id) {
        v.s("adm.status_creator")
    } else if bot.is_sudo(id) {
        v.s("adm.status_sudo")
    } else if gban.is_some() {
        v.s("adm.status_gbanned")
    } else if is_bot == Some(true) {
        v.s("adm.status_bot")
    } else {
        v.s("adm.status_user")
    };
    let gban_text = match &gban {
        Some((reason, actor, at)) => {
            let reason = if reason.is_empty() { v.s("common.no_reason") } else { h(reason) };
            let actor = if *actor == 0 { v.s("common.system") } else { mention_html(*actor, &bot.display_name(*actor).await) };
            v.t("adm.user_gban", &[("reason", &reason), ("actor", &actor), ("ago", &v.ago(*at))])
        }
        None => String::new(),
    };
    let unknown = v.s("common.unknown");
    let text = v.t(
        "adm.user",
        &[
            ("user", &mention_html(id, &name)),
            ("id", &id.to_string()),
            ("username", &username.map(|u| format!("@{}", h(&u))).unwrap_or_else(|| v.s("common.none"))),
            ("status", &status),
            ("gban", &gban_text),
            ("first_seen", &card.as_ref().map_or(unknown.clone(), |c| v.ago(c.first_seen))),
            ("updated", &card.as_ref().map_or(unknown.clone(), |c| v.ago(c.updated_at))),
            ("groups_n", &v.n(totals.groups)),
            ("messages", &v.n(totals.messages)),
            ("messages_7d", &v.n(totals.messages_7d)),
            ("last_active", &totals.last_day.map_or(unknown, date_of_day)),
            ("warns", &v.n(totals.warns)),
            ("sanctions", &v.n(totals.sanctions)),
            ("bots", &if bots.is_empty() { v.s("common.none") } else { bots.join(", ") }),
            ("groups", &if groups.is_empty() { v.s("adm.empty") } else { groups.join("\n") }),
        ],
    );
    let mut rows = Vec::new();
    let protected = bot.is_sudo(id);
    if !protected {
        rows.push(if gban.is_some() {
            vec![colored(v.s("adm.u_ungban"), format!("a:ua:ungban:{id}"), Tone::Good)]
        } else {
            vec![colored(v.s("adm.u_gban"), format!("a:ua:gban:{id}"), Tone::Bad)]
        });
    }
    if v.creator() && !bot.is_creator(id) {
        rows.push(if bot.global().sudoers.contains(&id) {
            vec![colored(v.s("adm.u_unsudo"), format!("a:ua:unsudo:{id}"), Tone::Bad)]
        } else {
            vec![colored(v.s("adm.u_sudo"), format!("a:ua:sudo:{id}"), Tone::Good)]
        });
    }
    let mut cleanup = vec![colored(v.s("adm.u_clear_warns"), format!("a:ua:clrw:{id}"), Tone::Plain)];
    if v.creator() {
        cleanup.push(colored(v.s("adm.u_unrank"), format!("a:ua:unrank:{id}"), Tone::Plain));
    }
    rows.push(cleanup);
    rows.push(vec![v.refresh(&format!("u:{id}")), link(v.s("adm.b_users"), "a:ul:recent:0")]);
    rows.push(v.nav("m"));
    Ok((text, keyboard(rows)))
}

async fn user_action(v: &View<'_>, action: &str, user: i64) -> Result<String> {
    let bot = v.bot;
    if matches!(action, "sudo" | "unsudo" | "unrank") && !v.creator() {
        return Ok(v.s("adm.creator_only"));
    }
    Ok(match action {
        "gban" => super::gban(bot, v.user, user, "").await?.render(&v.tx),
        "ungban" => super::ungban(bot, v.user, user).await?.render(&v.tx),
        "sudo" => super::add_sudo(bot, v.user, user).await?.render(&v.tx),
        "unsudo" => super::remove_sudo(bot, v.user, user).await?.render(&v.tx),
        "clrw" => {
            let n = bot.repo.clear_warns_everywhere(user).await?;
            audit(bot, v.user, Some(user), "clear_warns", &n.to_string()).await;
            v.t("adm.warns_cleared", &[("n", &v.n(n as i64))])
        }
        "unrank" => {
            let chats = bot.repo.remove_ranks_everywhere(user).await?;
            for chat in &chats {
                bot.invalidate_ranks(*chat).await;
            }
            audit(bot, v.user, Some(user), "remove_ranks", &chats.len().to_string()).await;
            v.t("adm.ranks_removed", &[("n", &v.n(chats.len() as i64))])
        }
        _ => String::new(),
    })
}

/// Search results as buttons to the users' profiles.
pub fn search_results(tx: &Texts, query: &str, users: &[UserRef]) -> Screen {
    let lines: Vec<String> = users.iter().map(|u| format!("• {}", user_line(u))).collect();
    let text = tx.t("adm.search", &[("query", &h(query)), ("list", &lines.join("\n"))]);
    let mut rows: Vec<Row> = users.iter().map(user_button).collect::<Vec<_>>().chunks(2).map(<[_]>::to_vec).collect();
    rows.push(vec![colored(tx.s("ui.close"), "a:x", Tone::Bad)]);
    (text, keyboard(rows))
}

async fn groups(v: &View<'_>, filter: GroupFilter, page: i64) -> Result<Screen> {
    let bot = v.bot;
    let (list, total) = bot.repo.groups_page(filter, PAGE, page * PAGE).await?;
    let fleet = bot.fleet().await;
    let lines: Vec<String> = list
        .iter()
        .map(|g| {
            let title = if g.title.is_empty() { g.chat_id.to_string() } else { short(&g.title, 36) };
            let served = g.bot_id.and_then(|id| fleet.get(&id)).map(|name| format!(" · @{}", h(name))).unwrap_or_default();
            format!("• {} — 👥 {}{served}", h(&title), v.n(i64::from(g.members)))
        })
        .collect();
    let text = v.t(
        "adm.groups",
        &[
            ("filter", &v.s(&format!("adm.gf_{}", filter.code()))),
            ("n", &v.n(total)),
            ("list", &if lines.is_empty() { v.s("adm.empty") } else { lines.join("\n") }),
        ],
    );
    let buttons: Vec<_> = list
        .iter()
        .map(|g| {
            let title = if g.title.is_empty() { g.chat_id.to_string() } else { short(&g.title, 22) };
            link(title, format!("a:gi:{}", g.chat_id))
        })
        .collect();
    let mut rows: Vec<Row> = buttons.chunks(2).map(<[_]>::to_vec).collect();
    rows.push(pager(|p| format!("a:g:{}:{p}", filter.code()), page, total));
    rows.push(
        [GroupFilter::Active, GroupFilter::Inactive, GroupFilter::Blocked]
            .iter()
            .map(|f| option(v.s(&format!("adm.gf_{}", f.code())), *f == filter, format!("a:g:{}:0", f.code())))
            .collect(),
    );
    rows.push(v.nav("m"));
    Ok((text, keyboard(rows)))
}

async fn group(v: &View<'_>, chat: i64) -> Result<Screen> {
    let bot = v.bot;
    let Some(card) = bot.repo.group_card(chat, today()).await? else {
        return Ok((v.t("adm.group_unknown", &[("id", &chat.to_string())]), keyboard(vec![v.nav("g:a:0")])));
    };
    let settings: Settings = serde_json::from_value(card.settings.clone()).unwrap_or_default();
    let fleet = bot.fleet().await;
    let bot_name = |id: i64| fleet.get(&id).map_or_else(|| id.to_string(), |name| format!("@{name}"));
    let members: Vec<String> = bot.repo.group_bots(chat).await?.into_iter().map(|id| h(&bot_name(id))).collect();
    let state = if card.blocked {
        v.s("adm.gf_b")
    } else if card.active {
        v.s("adm.gf_a")
    } else {
        v.s("adm.gf_i")
    };
    let language = i18n::languages().into_iter().find(|(code, _)| *code == settings.lang).map_or(settings.lang.clone(), |(_, n)| n);
    let title = if card.title.is_empty() { chat.to_string() } else { card.title.clone() };
    let text = v.t(
        "adm.group",
        &[
            ("title", &h(&title)),
            ("id", &chat.to_string()),
            ("state", &state),
            ("members", &v.n(i64::from(card.members))),
            ("bot", &card.bot_id.map_or_else(|| v.s("common.none"), |id| h(&bot_name(id)))),
            ("bots", &if members.is_empty() { v.s("common.none") } else { members.join(", ") }),
            ("lang", &h(&language)),
            ("created", &v.ago(card.created_at)),
            ("last_active", &card.last_day.map_or_else(|| v.s("common.unknown"), date_of_day)),
            ("messages_7d", &v.n(card.messages_7d)),
            ("ranks", &v.n(card.ranks)),
            ("bans", &v.n(card.bans)),
            ("mutes", &v.n(card.mutes)),
            ("warns", &v.n(card.warns)),
            ("filters", &v.n(card.filters)),
            ("answers", &v.n(card.answers)),
            ("locks", &v.n(settings.locks.len() as i64)),
            ("log", &v.state(settings.security.log_chat.is_some())),
        ],
    );
    let mut rows = vec![vec![
        link(v.s("adm.ga_link"), format!("a:ga:link:{chat}")),
        link(v.s("adm.ga_sync"), format!("a:ga:sync:{chat}")),
    ]];
    if v.creator() {
        rows.push(vec![
            colored(v.s("adm.ga_send"), format!("a:ga:send:{chat}"), Tone::Plain),
            colored(v.s("adm.ga_reset"), format!("a:gc:reset:{chat}"), Tone::Bad),
        ]);
        rows.push(if card.blocked {
            vec![colored(v.s("adm.ga_unblock"), format!("a:ga:unblock:{chat}"), Tone::Good)]
        } else {
            vec![
                colored(v.s("adm.ga_leave"), format!("a:gc:leave:{chat}"), Tone::Bad),
                colored(v.s("adm.ga_block"), format!("a:gc:block:{chat}"), Tone::Bad),
            ]
        });
    }
    rows.push(vec![v.refresh(&format!("gi:{chat}")), link(v.s("adm.b_groups"), "a:g:a:0")]);
    rows.push(v.nav("m"));
    Ok((text, keyboard(rows)))
}

async fn group_action(v: &View<'_>, action: &str, chat: i64) -> Result<String> {
    let bot = v.bot;
    if !matches!(action, "link" | "sync") && !v.creator() {
        return Ok(v.s("adm.creator_only"));
    }
    Ok(match action {
        "link" => {
            let link = match bot.client.create_chat_invite_link(chat, "operator", 0, 0, false).await {
                Ok(link) => Some(link.invite_link),
                Err(_) => bot.settings(chat).await.link,
            };
            match link {
                Some(link) => {
                    bot.send(v.user, &v.t("adm.group_link", &[("link", &h(&link))])).await;
                    v.s("adm.file_sent")
                }
                None => v.s("adm.link_failed"),
            }
        }
        "sync" => match bot.client.get_chat(chat).await {
            Ok(info) => {
                bot.repo.set_group_info(chat, &info.title, true).await?;
                if let Some(n) = super::member_count(bot, chat).await {
                    bot.repo.set_group_members(chat, n).await?;
                }
                v.s("adm.synced")
            }
            Err(_) => v.s("adm.sync_failed"),
        },
        "send" => {
            super::set_session(bot, v.user, Some(Session::SendTo { chat })).await;
            bot.send(v.user, &v.t("adm.sendto_ask", &[("chat", &chat.to_string())])).await;
            v.s("adm.broadcast_ask_short")
        }
        "reset" => {
            bot.update(chat, |s| *s = Settings { lang: s.lang.clone(), ..Settings::default() }).await?;
            audit(bot, v.user, None, "reset_group", &chat.to_string()).await;
            v.s("adm.group_reset")
        }
        "leave" | "block" => {
            let n = super::leave_group(bot, v.user, chat, action == "block").await?;
            v.t("adm.leave_queued", &[("n", &v.n(n as i64))])
        }
        "unblock" => {
            bot.repo.set_blocked(chat, false).await?;
            audit(bot, v.user, None, "unblock_group", &chat.to_string()).await;
            v.s("adm.unblocked")
        }
        _ => String::new(),
    })
}

async fn draft(bot: &Bot, user: i64) -> Option<Draft> {
    soft(bot.cache.get_json::<Draft>(&super::draft_key(user)).await, "broadcast draft").flatten()
}

async fn change_draft(bot: &Bot, user: i64, change: impl FnOnce(&mut Draft)) {
    if let Some(mut d) = draft(bot, user).await {
        change(&mut d);
        soft(bot.cache.set_json(&super::draft_key(user), &d, 86_400).await, "broadcast draft");
    }
}

/// Fleet-wide recipients of a broadcast target.
async fn recipients(bot: &Bot, target: Target) -> Result<i64> {
    let (groups, users) = bot.repo.audience().await?;
    Ok(match target {
        Target::Groups => groups,
        Target::Users => users,
        Target::All => groups + users,
        Target::Chat(_) => 1,
    })
}

async fn broadcast(v: &View<'_>) -> Result<Screen> {
    let bot = v.bot;
    let (groups, users) = bot.repo.audience().await?;
    let Some(draft) = draft(bot, v.user).await else {
        let text = v.t("adm.broadcast_empty", &[("groups", &v.n(groups)), ("users", &v.n(users))]);
        let rows = vec![vec![colored(v.s("adm.broadcast_new"), "a:bn", Tone::Good)], v.nav("m")];
        return Ok((text, keyboard(rows)));
    };
    let kind = match &draft.content {
        Content::Text(_) => v.s("adm.content_text"),
        Content::Media { kind, .. } => format!("{kind:?}"),
    };
    let media_note = match (&draft.content, &draft.local_path) {
        (Content::Media { .. }, None) => v.s("adm.broadcast_media_local"),
        _ => String::new(),
    };
    let text = v.t(
        "adm.broadcast",
        &[
            ("kind", &h(&kind)),
            ("target", &v.s(&format!("adm.target_{}", draft.target.code()))),
            ("n", &v.n(recipients(bot, draft.target).await?)),
            ("pin", &v.state(draft.pin)),
            ("note", &media_note),
        ],
    );
    let targets: Row = [Target::Groups, Target::Users, Target::All]
        .iter()
        .map(|t| option(v.s(&format!("adm.target_{}", t.code())), *t == draft.target, format!("a:bt:{}", t.code())))
        .collect();
    let rows = vec![
        v.header("adm.l_target"),
        targets,
        vec![toggle(draft.pin, v.s("adm.pin_groups"), "a:bp".into())],
        vec![colored(v.s("adm.send"), "a:bs", Tone::Good), colored(v.s("adm.discard"), "a:bx", Tone::Bad)],
        v.nav("m"),
    ];
    Ok((text, keyboard(rows)))
}

async fn sudo(v: &View<'_>) -> Result<Screen> {
    let bot = v.bot;
    let mut creators = Vec::new();
    for &id in &bot.config.sudo_ids {
        creators.push(format!("• {} <code>{id}</code>", mention_html(id, &bot.display_name(id).await)));
    }
    let list = bot.repo.sudo_list().await?;
    let lines: Vec<String> = list.iter().map(|u| format!("• {}", user_line(u))).collect();
    let text = v.t(
        "adm.sudo",
        &[
            ("creators", &if creators.is_empty() { v.s("common.none") } else { creators.join("\n") }),
            ("list", &if lines.is_empty() { v.s("adm.empty") } else { lines.join("\n") }),
        ],
    );
    let mut rows: Vec<Row> = list
        .iter()
        .map(|u| {
            let name = if u.name.is_empty() { u.id.to_string() } else { short(&u.name, 24) };
            vec![link(format!("👤 {name}"), format!("a:u:{}", u.id)), colored(v.s("adm.remove"), format!("a:sr:{}", u.id), Tone::Bad)]
        })
        .collect();
    rows.push(v.nav("m"));
    Ok((text, keyboard(rows)))
}

fn flags(v: &View<'_>) -> Screen {
    let global = v.bot.global();
    let lines: Vec<String> = FLAGS
        .iter()
        .map(|f| {
            let on = global.flag(*f);
            let name = v.s(&format!("adm.flag_{}", f.code()));
            let about = v.s(&format!("adm.flagd_{}", f.code()));
            format!("{} <b>{name}</b> — {about}", if on { "✅" } else { "❌" })
        })
        .collect();
    let text = v.t("adm.flags", &[("list", &lines.join("\n"))]);
    let buttons: Vec<_> =
        FLAGS.iter().map(|f| toggle(global.flag(*f), v.s(&format!("adm.flag_{}", f.code())), format!("a:ft:{}", f.code()))).collect();
    let mut rows: Vec<Row> = buttons.chunks(2).map(<[_]>::to_vec).collect();
    rows.push(v.nav("m"));
    (text, keyboard(rows))
}

const MIN_MEMBERS: [u32; 5] = [0, 10, 50, 100, 500];
const BACKUP_HOURS: [u32; 5] = [0, 6, 12, 24, 72];
const RETENTION_DAYS: [u32; 5] = [0, 30, 90, 180, 365];
const LOG_LEVELS: [&str; 5] = ["env", "error", "warn", "info", "debug"];

fn global_settings(v: &View<'_>) -> Screen {
    let global = v.bot.global();
    let s = &global.settings;
    let off = v.s("common.off");
    let languages = i18n::languages();
    let lang = match &s.default_lang {
        Some(code) => languages.iter().find(|(c, _)| c == code).map_or(code.clone(), |(_, n)| n.clone()),
        None => v.t("adm.lang_env", &[("lang", &v.bot.config.default_lang)]),
    };
    let text = v.t(
        "adm.settings",
        &[
            ("lang", &h(&lang)),
            ("gban", &v.s(if s.gban_mode == GbanMode::Ban { "adm.gban_mode_ban" } else { "adm.gban_mode_delete" })),
            ("notify", &v.state(s.notify_groups)),
            ("min", &if s.min_members == 0 { off.clone() } else { v.n(i64::from(s.min_members)) }),
            ("delay", &v.n(i64::from(s.broadcast_delay_ms))),
            ("errors", &v.state(s.error_alerts)),
            ("monitor", &v.state(s.monitor_alerts)),
            ("spike", &v.n(i64::from(s.error_spike))),
            ("alert", &s.alert_chat.map_or_else(|| v.s("adm.alert_creators"), |c| format!("<code>{c}</code>"))),
            ("backup", &if s.auto_backup_hours == 0 { off.clone() } else { v.t("adm.every_hours", &[("n", &v.n(i64::from(s.auto_backup_hours)))]) }),
            ("keep", &v.n(i64::from(s.backup_keep))),
            ("retention", &if s.retention_days == 0 { v.s("adm.forever") } else { v.t("adm.n_days", &[("n", &v.n(i64::from(s.retention_days)))]) }),
            ("level", &s.log_level.clone().unwrap_or_else(|| v.s("adm.level_env"))),
        ],
    );
    let stepper = |field: &str, text: String| {
        vec![colored("➖", format!("a:gv:{field}:-"), Tone::Plain), label(text), colored("➕", format!("a:gv:{field}:+"), Tone::Plain)]
    };
    let options = |field: &str, values: &[u32], current: u32, show: &dyn Fn(u32) -> String| -> Row {
        values.iter().map(|n| option(show(*n), *n == current, format!("a:gv:{field}:{n}"))).collect()
    };
    let mut lang_row: Row = vec![option(v.s("adm.lang_default"), s.default_lang.is_none(), "a:gv:lang:-".into())];
    lang_row.extend(languages.iter().map(|(code, name)| option(name.clone(), s.default_lang.as_deref() == Some(code), format!("a:gv:lang:{code}"))));
    let mut rows = vec![
        v.header("adm.l_lang"),
        lang_row,
        v.header("adm.l_gban"),
        vec![
            option(v.s("adm.gban_mode_ban"), s.gban_mode == GbanMode::Ban, "a:gv:gban:ban".into()),
            option(v.s("adm.gban_mode_delete"), s.gban_mode == GbanMode::Delete, "a:gv:gban:delete".into()),
        ],
        v.header("adm.l_alerts"),
        vec![toggle(s.notify_groups, v.s("adm.s_notify"), "a:gv:notify:t".into()), toggle(s.error_alerts, v.s("adm.s_errors"), "a:gv:erralert:t".into())],
        vec![toggle(s.monitor_alerts, v.s("adm.s_monitor"), "a:gv:monalert:t".into())],
        stepper("spike", v.t("adm.v_spike", &[("n", &v.n(i64::from(s.error_spike)))])),
        v.header("adm.l_min_members"),
        options("min", &MIN_MEMBERS, s.min_members, &|n| if n == 0 { off.clone() } else { v.n(i64::from(n)) }),
        v.header("adm.l_broadcast"),
        stepper("delay", v.t("adm.v_delay", &[("n", &v.n(i64::from(s.broadcast_delay_ms)))])),
        v.header("adm.l_backup"),
        options("backup", &BACKUP_HOURS, s.auto_backup_hours, &|n| if n == 0 { off.clone() } else { v.t("adm.v_hours", &[("n", &v.n(i64::from(n)))]) }),
        stepper("keep", v.t("adm.v_keep", &[("n", &v.n(i64::from(s.backup_keep)))])),
        v.header("adm.l_retention"),
        options("ret", &RETENTION_DAYS, s.retention_days, &|n| if n == 0 { v.s("adm.forever") } else { v.t("adm.v_days", &[("n", &v.n(i64::from(n)))]) }),
        v.header("adm.l_log_level"),
        LOG_LEVELS
            .iter()
            .map(|l| {
                let selected = s.log_level.as_deref().unwrap_or("env") == *l;
                option(if *l == "env" { v.s("adm.level_env") } else { (*l).to_owned() }, selected, format!("a:gv:log:{l}"))
            })
            .collect(),
    ];
    if s.alert_chat.is_some() {
        rows.push(vec![colored(v.s("adm.clear_alert_chat"), "a:gv:alert:-", Tone::Bad)]);
    }
    rows.push(v.nav("m"));
    (text, keyboard(rows))
}

fn stepped(value: u32, up: bool, by: u32, min: u32, max: u32) -> u32 {
    if up { value.saturating_add(by).min(max) } else { value.saturating_sub(by).max(min) }
}

async fn set_global(v: &View<'_>, field: &str, value: &str) -> Result<String> {
    let bot = v.bot;
    let current = bot.global().settings.clone();
    let up = value == "+";
    let number = value.parse::<u32>().ok();
    let (field, value) = (field.to_owned(), value.to_owned());
    let detail = format!("{field}={value}");
    let known = bot
        .repo
        .update_global(move |s| {
            match field.as_str() {
                "lang" => s.default_lang = (value != "-" && i18n::is_language(&value)).then(|| value.clone()),
                "gban" => s.gban_mode = if value == "delete" { GbanMode::Delete } else { GbanMode::Ban },
                "notify" => s.notify_groups = !current.notify_groups,
                "erralert" => s.error_alerts = !current.error_alerts,
                "monalert" => s.monitor_alerts = !current.monitor_alerts,
                "spike" => s.error_spike = stepped(current.error_spike, up, 10, 10, 1000),
                "min" => s.min_members = number.filter(|n| MIN_MEMBERS.contains(n)).unwrap_or(current.min_members),
                "delay" => s.broadcast_delay_ms = stepped(current.broadcast_delay_ms, up, 50, 0, 2000),
                "backup" => s.auto_backup_hours = number.filter(|n| BACKUP_HOURS.contains(n)).unwrap_or(current.auto_backup_hours),
                "keep" => s.backup_keep = stepped(current.backup_keep, up, 1, 1, 30),
                "ret" => s.retention_days = number.filter(|n| RETENTION_DAYS.contains(n)).unwrap_or(current.retention_days),
                "log" => s.log_level = (value != "env" && LOG_LEVELS.contains(&value.as_str())).then(|| value.clone()),
                "alert" => s.alert_chat = None,
                _ => return false,
            }
            true
        })
        .await?;
    if !known {
        return Ok(String::new());
    }
    bot.refresh_global().await;
    audit(bot, v.user, None, "global_setting", &detail).await;
    Ok(v.s("adm.saved"))
}

fn maintenance(v: &View<'_>) -> Screen {
    let global = v.bot.global();
    let m = &global.settings.maintenance;
    let on = global.settings.maintenance_on(now());
    let until = match m.until {
        Some(until) if on => v.t("adm.maint_ends", &[("time", &v.dur(until - now()))]),
        _ if on => v.s("adm.maint_until_off"),
        _ => "—".to_owned(),
    };
    let message = match &m.message {
        Some(custom) => custom.clone(),
        None => v.s("adm.maint_notice"),
    };
    let text = v.t(
        "adm.maint",
        &[
            ("state", &v.state(on)),
            ("mode", &v.s(if m.full { "adm.maint_full" } else { "adm.maint_commands" })),
            ("since", &if on { v.ago(m.since) } else { "—".to_owned() }),
            ("until", &until),
            ("message", &message),
        ],
    );
    let mut rows = Vec::new();
    if on {
        rows.push(vec![colored(v.s("adm.maint_turn_off"), "a:mv:off:0", Tone::Good)]);
        rows.push(v.header("adm.l_mode"));
        rows.push(vec![
            option(v.s("adm.maint_commands"), !m.full, "a:mv:mode:c".into()),
            option(v.s("adm.maint_full"), m.full, "a:mv:mode:f".into()),
        ]);
        rows.push(v.header("adm.l_duration"));
        rows.push(
            [1800, 3600, 3 * 3600, 6 * 3600, 0]
                .iter()
                .map(|secs: &i64| {
                    let text = if *secs == 0 { "∞".to_owned() } else { v.dur(*secs) };
                    colored(text, format!("a:mv:for:{secs}"), Tone::Plain)
                })
                .collect(),
        );
    } else {
        rows.push(vec![
            colored(v.s("adm.maint_commands"), "a:mv:on:c", Tone::Bad),
            colored(v.s("adm.maint_full"), "a:mv:on:f", Tone::Bad),
        ]);
    }
    if m.message.is_some() {
        rows.push(vec![colored(v.s("adm.maint_default_message"), "a:mv:msg:-", Tone::Plain)]);
    }
    rows.push(v.nav("m"));
    (text, keyboard(rows))
}

async fn maintenance_action(v: &View<'_>, field: &str, value: &str) -> Result<()> {
    let bot = v.bot;
    let m = bot.global().settings.maintenance.clone();
    match field {
        "on" => super::set_maintenance(bot, v.user, true, value == "f", None).await?,
        "off" => super::set_maintenance(bot, v.user, false, false, None).await?,
        "mode" => super::set_maintenance(bot, v.user, true, value == "f", m.until).await?,
        "for" => {
            let seconds = value.parse::<i64>().unwrap_or(0);
            let until = (seconds > 0).then(|| now() + seconds);
            super::set_maintenance(bot, v.user, true, m.full, until).await?;
        }
        "msg" => {
            bot.repo.update_global(|s| s.maintenance.message = None).await?;
            bot.refresh_global().await;
        }
        _ => {}
    }
    Ok(())
}

async fn jobs_screen(v: &View<'_>) -> Result<Screen> {
    let bot = v.bot;
    let counts = bot.repo.job_counts().await?;
    let count = |status: &str| v.n(counts.get(status).copied().unwrap_or(0));
    let recent = bot.repo.recent_jobs(10).await?;
    let fleet = bot.fleet().await;
    let lines: Vec<String> = recent
        .iter()
        .map(|job| {
            let on = job.bot_id.and_then(|id| fleet.get(&id)).map(|n| format!(" · @{}", h(n))).unwrap_or_default();
            let when = v.ago(job.finished_at.unwrap_or(job.created_at));
            let result = if job.result.is_empty() { String::new() } else { format!("\n   <i>{}</i>", h(&short(&job.result, 90))) };
            format!("• {}{on} · {when}{result}", h(&jobs::describe(&v.tx, job)))
        })
        .collect();
    let tick_at = metrics::TICK_AT.load(Ordering::Relaxed);
    let text = v.t(
        "adm.jobs",
        &[
            ("pending", &count("pending")),
            ("running", &count("running")),
            ("done", &count("done")),
            ("failed", &count("failed")),
            ("cancelled", &count("cancelled")),
            ("list", &if lines.is_empty() { v.s("adm.empty") } else { lines.join("\n") }),
            ("tick", &if tick_at == 0 { v.s("adm.tick_none") } else { v.ago(tick_at) }),
        ],
    );
    let mut rows = Vec::new();
    if v.creator() {
        let controls: Vec<_> = recent
            .iter()
            .filter_map(|job| match job.status.as_str() {
                "pending" | "running" => Some(colored(format!("✖ #{}", job.id), format!("a:jc:{}", job.id), Tone::Bad)),
                "failed" | "cancelled" => Some(colored(format!("🔁 #{}", job.id), format!("a:jr:{}", job.id), Tone::Plain)),
                _ => None,
            })
            .collect();
        rows.extend(controls.chunks(3).map(<[_]>::to_vec));
        rows.push(vec![
            colored(v.s("adm.jn_prune"), "a:jn:prune", Tone::Plain),
            colored(v.s("adm.jn_sync"), "a:jn:sync", Tone::Plain),
            colored(v.s("adm.jn_backup"), "a:jn:backup", Tone::Plain),
        ]);
    }
    rows.push(vec![v.refresh("j")]);
    rows.push(v.nav("m"));
    Ok((text, keyboard(rows)))
}

async fn errors(v: &View<'_>) -> Result<Screen> {
    let bot = v.bot;
    let counts = bot.repo.error_counts().await?;
    let list = bot.repo.open_errors(8).await?;
    let lines: Vec<String> = list
        .iter()
        .map(|e| {
            format!(
                "• <b>#{}</b> ×{} <code>{}</code>\n   {} · {}",
                e.id,
                v.n(e.count),
                h(&short(&e.target, 40)),
                h(&short(&e.message, 140)),
                v.ago(e.last_seen)
            )
        })
        .collect();
    let text = v.t(
        "adm.errors",
        &[
            ("open", &v.n(counts.open)),
            ("events", &v.n(counts.open_events)),
            ("hour", &v.n(counts.last_hour)),
            ("alerts", &v.state(bot.global().settings.error_alerts)),
            ("list", &if lines.is_empty() { v.s("adm.no_errors") } else { lines.join("\n") }),
        ],
    );
    let buttons: Vec<_> = list.iter().map(|e| link(format!("🔎 #{}", e.id), format!("a:ed:{}", e.id))).collect();
    let mut rows: Vec<Row> = buttons.chunks(4).map(<[_]>::to_vec).collect();
    if !list.is_empty() {
        rows.push(vec![colored(v.s("adm.resolve_all"), "a:ea", Tone::Good)]);
    }
    rows.push(vec![v.refresh("e"), link(v.s("adm.b_logs"), "a:l:e:0")]);
    rows.push(v.nav("m"));
    Ok((text, keyboard(rows)))
}

async fn error(v: &View<'_>, id: i64) -> Result<Screen> {
    let bot = v.bot;
    let Some(e) = bot.repo.error(id).await? else { return errors(v).await };
    let fleet = bot.fleet().await;
    let text = v.t(
        "adm.error",
        &[
            ("id", &e.id.to_string()),
            ("level", &h(&e.level)),
            ("target", &h(&e.target)),
            ("message", &h(&short(&e.message, 3000))),
            ("count", &v.n(e.count)),
            ("first", &v.ago(e.first_seen)),
            ("last", &v.ago(e.last_seen)),
            ("bot", &fleet.get(&e.bot_id).map_or_else(|| e.bot_id.to_string(), |n| format!("@{}", h(n)))),
            ("state", &v.s(if e.resolved { "adm.error_state_resolved" } else { "adm.error_state_open" })),
        ],
    );
    let mut rows = Vec::new();
    if !e.resolved {
        rows.push(vec![colored(v.s("adm.resolve"), format!("a:ex:{id}"), Tone::Good)]);
    }
    rows.push(v.nav("e"));
    Ok((text, keyboard(rows)))
}

fn level_of(code: &str) -> log::Level {
    match code {
        "e" => log::Level::Error,
        "w" => log::Level::Warn,
        "i" => log::Level::Info,
        _ => log::Level::Debug,
    }
}

fn log_lines(v: &View<'_>, level: &str, page: usize) -> Screen {
    const LINES: usize = 12;
    let level = if matches!(level, "e" | "w" | "i" | "d") { level } else { "w" };
    let (lines, total) = logs::recent(level_of(level), page * LINES, LINES);
    let mut list = String::new();
    for line in &lines {
        let icon = match line.level {
            log::Level::Error => "🔴",
            log::Level::Warn => "🟡",
            log::Level::Info => "🔵",
            _ => "⚪️",
        };
        let entry = format!(
            "{icon} <code>{}</code> <i>{}</i>\n{}\n",
            &iso_time(line.at)[11..],
            h(&short(line.target.trim_start_matches("group_bot::"), 30)),
            h(&short(&line.text, 220))
        );
        if list.len() + entry.len() > 3300 {
            break;
        }
        list.push_str(&entry);
    }
    let text = v.t(
        "adm.logs",
        &[
            ("level", &v.s(&format!("adm.lv_{level}"))),
            ("n", &v.n(total as i64)),
            ("current", &log::max_level().to_string().to_lowercase()),
            ("list", &if list.is_empty() { v.s("adm.empty") } else { list }),
        ],
    );
    let page = page as i64;
    let mut pages = Vec::new();
    if page > 0 {
        pages.push(link("◀️", format!("a:l:{level}:{}", page - 1)));
    }
    if (page as usize + 1) * LINES < total {
        pages.push(link("▶️", format!("a:l:{level}:{}", page + 1)));
    }
    let rows = vec![
        ["e", "w", "i", "d"].iter().map(|l| option(v.s(&format!("adm.lv_{l}")), *l == level, format!("a:l:{l}:0"))).collect(),
        pages,
        vec![link(v.s("adm.log_download"), format!("a:lf:{level}")), v.refresh(&format!("l:{level}:{page}"))],
        v.nav("m"),
    ];
    (text, keyboard(rows))
}

async fn monitor(v: &View<'_>, range: Range) -> Result<Screen> {
    let bot = v.bot;
    let t = now();
    let hashes = bot.cache.hgetall_many(&range.keys(t)).await?;
    let series = Series::from_buckets(&hashes);
    let mut commands: BTreeMap<String, i64> = BTreeMap::new();
    for hash in bot.cache.hgetall_many(&range.command_keys(t)).await? {
        for (name, n) in hash {
            *commands.entry(name).or_default() += n;
        }
    }
    let mut top: Vec<(String, i64)> = commands.into_iter().collect();
    top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let charted = [
        Metric::Updates,
        Metric::Messages,
        Metric::Commands,
        Metric::Callbacks,
        Metric::Deleted,
        Metric::Punished,
        Metric::SendFailed,
        Metric::Warnings,
        Metric::Errors,
    ];
    let charts: Vec<String> = charted
        .iter()
        .map(|m| {
            format!(
                "{} <b>{}</b>\n<code>{}</code>",
                v.s(&format!("adm.m_{}", m.code())),
                v.n(series.total(*m)),
                metrics::sparkline(series.values(*m), 24)
            )
        })
        .collect();
    let handled = series.total(Metric::Handled);
    let latency = if handled > 0 { series.total(Metric::HandlerMs) / handled } else { 0 };
    let fleet = bot.fleet().await;
    let mut per_bot: BTreeMap<i64, (i64, i64, i64)> = BTreeMap::new();
    for ((id, code), n) in &series.per_bot {
        let entry = per_bot.entry(*id).or_default();
        match *code {
            "upd" => entry.0 += n,
            "msg" => entry.1 += n,
            "err" => entry.2 += n,
            _ => {}
        }
    }
    let bots: Vec<String> = per_bot
        .iter()
        .map(|(id, (updates, messages, errors))| {
            let name = fleet.get(id).map_or_else(|| id.to_string(), |n| format!("@{n}"));
            v.t("adm.monitor_bot", &[("name", &h(&name)), ("updates", &v.n(*updates)), ("messages", &v.n(*messages)), ("errors", &v.n(*errors))])
        })
        .collect();
    let commands: Vec<String> = top.iter().take(8).map(|(name, n)| format!("• {} — {}", h(name), v.n(*n))).collect();
    let text = v.t(
        "adm.monitor",
        &[
            ("range", &v.s(&format!("adm.range_{}", range.code()))),
            ("charts", &charts.join("\n")),
            ("latency", &v.n(latency)),
            ("bots", &if bots.is_empty() { v.s("adm.empty") } else { bots.join("\n") }),
            ("commands", &if commands.is_empty() { v.s("adm.empty") } else { commands.join("\n") }),
            ("live_updates", &v.n(metrics::since_start(Metric::Updates) as i64)),
            ("live_messages", &v.n(metrics::since_start(Metric::Messages) as i64)),
            ("live_errors", &v.n(metrics::since_start(Metric::Errors) as i64)),
        ],
    );
    let rows = vec![
        [Range::Hour, Range::Day, Range::Week]
            .iter()
            .map(|r| option(v.s(&format!("adm.range_{}", r.code())), *r == range, format!("a:mo:{}", r.code())))
            .collect(),
        vec![v.refresh(&format!("mo:{}", range.code())), link(v.s("adm.b_health"), "a:h")],
        v.nav("m"),
    ];
    Ok((text, keyboard(rows)))
}

async fn database(v: &View<'_>) -> Result<Screen> {
    let bot = v.bot;
    let info = bot.repo.db_info().await?;
    let (open, idle, max) = bot.repo.pool_stats();
    let mut migrations = Vec::new();
    let (mut pending, mut problems) = (0, 0);
    for m in crate::db::MIGRATOR.iter() {
        let applied = info.migrations.iter().find(|a| a.version == m.version);
        let (icon, detail) = match applied {
            Some(a) if !a.success => {
                problems += 1;
                ("❌", v.s("adm.mig_failed"))
            }
            Some(a) if a.checksum.as_slice() != &*m.checksum => {
                problems += 1;
                ("⚠️", v.s("adm.mig_changed"))
            }
            Some(a) => ("✅", format!("{} · {} ms", v.ago(a.installed_on), v.n(a.execution_ms))),
            None => {
                pending += 1;
                ("⏳", v.s("adm.mig_pending"))
            }
        };
        migrations.push(format!("{icon} {:04} {} — {detail}", m.version, h(&m.description)));
    }
    for a in info.migrations.iter().filter(|a| !crate::db::MIGRATOR.iter().any(|m| m.version == a.version)) {
        problems += 1;
        migrations.push(format!("❓ {:04} {} — {}", a.version, h(&a.description), v.s("adm.mig_unknown")));
    }
    let schema = if problems > 0 {
        v.s("adm.schema_problem")
    } else if pending > 0 {
        v.s("adm.schema_pending")
    } else {
        v.s("adm.schema_ok")
    };
    let tables: Vec<String> = info
        .tables
        .iter()
        .map(|(name, rows, bytes)| format!("• {} — {} · {}", h(name), v.n(*rows), format_bytes(*bytes)))
        .collect();
    let redis = match bot.cache.info().await {
        Ok(info) => {
            let get = |k: &str| h(info.get(k).map_or("?", String::as_str));
            let keys = soft(bot.cache.dbsize().await, "dbsize").unwrap_or(0);
            v.t(
                "adm.db_redis",
                &[
                    ("version", &get("redis_version")),
                    ("memory", &get("used_memory_human")),
                    ("clients", &get("connected_clients")),
                    ("days", &get("uptime_in_days")),
                    ("keys", &v.n(keys)),
                ],
            )
        }
        Err(err) => format!("🔴 {}", h(&err.to_string())),
    };
    let text = v.t(
        "adm.db",
        &[
            ("server", &h(&info.server)),
            ("size", &format_bytes(info.size)),
            ("connections", &v.n(info.connections)),
            ("max", &h(&info.max_connections)),
            ("pool", &format!("{}/{} ({} {})", v.n(i64::from(open)), v.n(i64::from(max)), v.n(idle as i64), v.s("adm.idle"))),
            ("schema", &schema),
            ("migrations", &migrations.join("\n")),
            ("tables", &tables.join("\n")),
            ("redis", &redis),
        ],
    );
    let mut rows = Vec::new();
    if v.creator() {
        rows.push(vec![colored(v.s("adm.analyze"), "a:dba", Tone::Plain)]);
    }
    rows.push(vec![v.refresh("db"), link(v.s("adm.b_backup"), "a:bk")]);
    rows.push(v.nav("m"));
    Ok((text, keyboard(rows)))
}

fn backups(v: &View<'_>) -> Screen {
    let bot = v.bot;
    let files = backup::list(bot);
    let settings = bot.global().settings.clone();
    let lines: Vec<String> =
        files.iter().take(8).enumerate().map(|(i, f)| format!("{}. <code>{}</code> — {}", i + 1, h(&f.name), format_bytes(f.size as i64))).collect();
    let dir = std::fs::canonicalize(backup::dir(bot)).unwrap_or_else(|_| backup::dir(bot));
    let text = v.t(
        "adm.backups",
        &[
            ("dir", &h(&dir.display().to_string())),
            ("auto", &if settings.auto_backup_hours == 0 { v.s("common.off") } else { v.t("adm.every_hours", &[("n", &v.n(i64::from(settings.auto_backup_hours)))]) }),
            ("keep", &v.n(i64::from(settings.backup_keep))),
            ("list", &if lines.is_empty() { v.s("adm.empty") } else { lines.join("\n") }),
        ],
    );
    let downloads: Vec<_> = files.iter().take(8).enumerate().map(|(i, _)| link(format!("📥 {}", i + 1), format!("a:bkf:{i}"))).collect();
    let mut rows = vec![vec![
        colored(v.s("adm.backup_config"), "a:bkn:c", Tone::Good),
        colored(v.s("adm.backup_full"), "a:bkn:f", Tone::Good),
    ]];
    rows.extend(downloads.chunks(4).map(<[_]>::to_vec));
    rows.push(vec![colored(v.s("adm.restore"), "a:rs", Tone::Bad)]);
    rows.push(vec![v.refresh("bk"), link(v.s("adm.b_settings"), "a:gs")]);
    rows.push(v.nav("m"));
    (text, keyboard(rows))
}

/// Asks to confirm restoring a backup that was just uploaded.
pub async fn restore_confirm(bot: &Bot, tx: &Texts, parsed: &Parsed) -> Screen {
    let current = soft(bot.repo.schema_version().await, "schema version").unwrap_or(0);
    let tables: Vec<String> = parsed.tables.iter().map(|(t, rows)| format!("• {t}: {}", rows.len())).collect();
    let warning = if parsed.schema > current { tx.s("adm.restore_newer") } else { String::new() };
    let text = tx.t(
        "adm.restore_confirm",
        &[
            ("created", &iso_time(parsed.created_at)),
            ("bot", &h(&parsed.bot)),
            ("kind", &tx.s(if parsed.full { "adm.backup_full" } else { "adm.backup_config" })),
            ("rows", &parsed.rows().to_string()),
            ("tables", &tables.join("\n")),
            ("warning", &warning),
        ],
    );
    let rows = vec![vec![colored(tx.s("adm.restore_now"), "a:rsy", Tone::Bad), link(tx.s("ui.no"), "a:rsn")]];
    (text, keyboard(rows))
}

async fn operator_audit(v: &View<'_>) -> Result<Screen> {
    let bot = v.bot;
    let mut lines = Vec::new();
    for (actor, target, action, detail, at) in bot.repo.operator_audit(15).await? {
        let actor = mention_html(actor, &bot.display_name(actor).await);
        let target = match target {
            Some(id) => format!(" → {}", mention_html(id, &bot.display_name(id).await)),
            None => String::new(),
        };
        let detail = if detail.is_empty() { String::new() } else { format!(" <i>{}</i>", h(&short(&detail, 60))) };
        lines.push(format!("• {} {actor}: <b>{}</b>{target}{detail}", v.ago(at), h(&action)));
    }
    let text = v.t("adm.audit", &[("list", &if lines.is_empty() { v.s("adm.empty") } else { lines.join("\n") })]);
    Ok((text, keyboard(vec![vec![v.refresh("au")], v.nav("m")])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creator_routes_are_guarded() {
        for head in ["b", "su", "f", "gs", "mt", "bk", "rsy", "gc", "jn", "dba"] {
            assert!(creator_only(head), "{head}");
        }
        for head in ["m", "st", "h", "u", "ul", "g", "gi", "j", "e", "l", "mo", "db", "au", "ua", "ga"] {
            assert!(!creator_only(head), "{head}");
        }
    }

    #[test]
    fn routes_fit_callback_data() {
        let longest = format!("a:gc:unblock:{}", i64::MIN);
        assert!(longest.len() <= 64);
        assert_eq!(date_of_day(0), "1970-01-01");
        assert_eq!(short("abcdef", 3), "abc…");
    }
}
