//! Configuration commands: locks, rates, strict mode, force add/join,
//! welcome, auto answers, schedules, security, language and texts.

use anyhow::Result;
use tdrust::types::{ChatMemberStatus, ChatType, MessageSenderUser};

use super::{on_off, Ctx};
use crate::commands::C;
use crate::i18n;
use crate::locks::{self, Lock, STRICT_TYPES};
use crate::model::{Audience, Content, Punish, Rank, ReportMode, TabchiTiming};
use crate::text::{fa_digits, format_clock, format_duration, format_tz, h, now, parse_clock, parse_duration, parse_tz};
use crate::{members, panels};

pub fn lock_name(g: &crate::bot::Group, lock: Lock) -> String {
    g.s(&format!("lockname.{}", locks::info(lock).code))
}

fn on_label(ctx: &Ctx<'_>, on: bool) -> String {
    ctx.g.s(if on { "common.on" } else { "common.off" })
}

pub async fn lock(ctx: &Ctx<'_>, lock: Lock, on: bool) -> Result<()> {
    let name = lock_name(&ctx.g, lock);
    if lock == Lock::Media && on && !ctx.p.args.is_empty() {
        let words: Vec<&str> = ctx.p.args.iter().map(String::as_str).collect();
        let Some((seconds, _)) = parse_duration(&words) else {
            ctx.say_t("err.bad_duration", &[]).await;
            return Ok(());
        };
        ctx.bot.update(ctx.g.id, move |s| s.media_lock_until = Some(now() + seconds)).await?;
        ctx.changed("media.locked_for", &[("duration", &format_duration(seconds))]).await;
        return Ok(());
    }
    let changed = ctx
        .bot
        .update(ctx.g.id, move |s| {
            if lock == Lock::Media && !on {
                s.media_lock_until = None;
            }
            if on { s.locks.insert(lock) } else { s.locks.remove(&lock) }
        })
        .await?;
    let key = match (on, changed) {
        (true, true) => "lock.on",
        (false, true) => "lock.off",
        (true, false) => "lock.already_on",
        (false, false) => "lock.already_off",
    };
    ctx.changed(key, &[("name", &name)]).await;
    Ok(())
}

fn punish_arg(words: &[String]) -> Option<Punish> {
    let joined = words.join(" ");
    let minutes = words.iter().find_map(|w| w.parse::<i64>().ok());
    Some(match joined.as_str() {
        j if j.starts_with("سکوت موقت") || j.starts_with("tmute") || j.starts_with("tempmute") => {
            Punish::TempMute(minutes.unwrap_or(30) * 60)
        }
        j if j.starts_with("سکوت") || j.starts_with("mute") => match minutes {
            Some(m) => Punish::TempMute(m * 60),
            None => Punish::Mute,
        },
        j if j.starts_with("بن") || j.starts_with("ban") => Punish::Ban,
        j if j.starts_with("اخطار") || j.starts_with("warn") => Punish::Warn,
        j if j.starts_with("حذف") || j.starts_with("delete") => Punish::Delete,
        _ => return None,
    })
}

pub fn punish_label(g: &crate::bot::Group, punish: Punish) -> String {
    match punish {
        Punish::TempMute(seconds) => format!("{} ({})", g.s(punish.key()), format_duration(seconds)),
        other => g.s(other.key()),
    }
}

fn clocks(ctx: &Ctx<'_>, max: usize) -> Option<Vec<u16>> {
    let words: Vec<&str> = ctx.p.args.iter().flat_map(|a| a.split('-')).filter(|w| !w.is_empty()).collect();
    let times: Option<Vec<u16>> = words.iter().map(|w| parse_clock(w)).collect();
    times.filter(|t| !t.is_empty() && t.len() <= max)
}

pub fn clock_list(times: &[u16]) -> String {
    fa_digits(&times.iter().map(|t| format_clock(*t)).collect::<Vec<_>>().join("، "))
}

/// Handles configuration commands; returns false for commands of other modules.
pub async fn handle(ctx: &Ctx<'_>, cmd: C) -> Result<bool> {
    let chat = ctx.g.id;
    let bot = ctx.bot;
    match cmd {
        C::Help if ctx.rank >= Rank::Mod => panels::open(bot, &ctx.g, "help", ctx.msg.chat_id, Some(ctx.msg.id)).await,
        C::Help => ctx.say(&ctx.g.s("help.member")).await,
        C::Settings => panels::open(bot, &ctx.g, "main", ctx.msg.chat_id, Some(ctx.msg.id)).await,
        C::Lists => panels::open(bot, &ctx.g, "members", ctx.msg.chat_id, Some(ctx.msg.id)).await,
        C::Lang => match ctx.p.arg(0) {
            Some(code) if i18n::is_language(code) => {
                let code = code.to_owned();
                let new = code.clone();
                bot.update(chat, move |s| s.lang = new).await?;
                let texts = bot.texts(chat, &code).await;
                ctx.say(&texts.s("lang.changed")).await;
            }
            _ => {
                let list: Vec<String> =
                    i18n::languages().into_iter().map(|(code, name)| format!("<code>{code}</code> — {}", h(&name))).collect();
                ctx.say_t("lang.list", &[("current", &ctx.g.s.lang), ("list", &list.join("\n"))]).await;
            }
        },

        // ------------------------------------------------------ group lock
        C::GroupLock => {
            let words: Vec<&str> = ctx.p.args.iter().map(String::as_str).collect();
            let duration = if words.is_empty() { None } else { parse_duration(&words).map(|(s, _)| s) };
            if !words.is_empty() && duration.is_none() {
                ctx.say_t("err.bad_duration", &[]).await;
                return Ok(true);
            }
            bot.update(chat, move |s| {
                s.group_lock.locked = true;
                s.group_lock.until = duration.map(|d| now() + d);
            })
            .await?;
            if let Err(err) = bot.set_chat_closed(chat, true).await {
                log::warn!("cannot close {chat} through permissions: {err:#}");
            }
            match duration {
                Some(d) => ctx.changed("glock.locked_for", &[("duration", &format_duration(d))]).await,
                None => ctx.changed("glock.locked", &[]).await,
            }
        }
        C::GroupUnlock => {
            let auto_active = bot
                .update(chat, |s| {
                    s.group_lock.locked = false;
                    s.group_lock.until = None;
                    s.group_lock.auto_active
                })
                .await?;
            if !auto_active {
                if let Err(err) = bot.set_chat_closed(chat, false).await {
                    log::warn!("cannot reopen {chat}: {err:#}");
                }
            }
            ctx.changed("glock.unlocked", &[]).await;
        }
        C::TempMediaOn => match ctx.number(0).filter(|m| *m > 0) {
            Some(minutes) => {
                bot.update(chat, move |s| s.temp_media_minutes = Some(minutes)).await?;
                ctx.changed("media.temp_on", &[("minutes", &fa_digits(&minutes.to_string()))]).await;
            }
            None => ctx.say_t("err.need_number", &[]).await,
        },
        C::TempMediaOff => {
            bot.update(chat, |s| s.temp_media_minutes = None).await?;
            ctx.changed("media.temp_off", &[]).await;
        }
        C::AutoLockSet => match clocks(ctx, 2).filter(|t| t.len() == 2) {
            Some(times) => {
                let (from, to) = (times[0], times[1]);
                bot.update(chat, move |s| s.group_lock.auto = Some((from, to))).await?;
                ctx.changed("glock.auto_set", &[("from", &clock_list(&[from])), ("to", &clock_list(&[to]))]).await;
            }
            None => ctx.say_t("glock.auto_usage", &[]).await,
        },
        C::AutoLockRem => {
            let (was_active, manual) = bot
                .update(chat, |s| {
                    s.group_lock.auto = None;
                    let was_active = std::mem::take(&mut s.group_lock.auto_active);
                    (was_active, s.group_lock.locked)
                })
                .await?;
            if was_active && !manual {
                let _ = bot.set_chat_closed(chat, false).await;
            }
            ctx.changed("glock.auto_removed", &[]).await;
        }
        C::AutoLockStatus => match ctx.g.s.group_lock.auto {
            Some((from, to)) => {
                let state = on_label(ctx, ctx.g.s.group_lock.auto_active);
                ctx.say_t(
                    "glock.auto_status",
                    &[("from", &clock_list(&[from])), ("to", &clock_list(&[to])), ("state", &state), ("tz", &format_tz(ctx.g.s.tz))],
                )
                .await
            }
            None => ctx.say_t("glock.auto_none", &[]).await,
        },

        // --------------------------------------------------- limit and flood
        C::LimitOn | C::LimitOff => {
            let on = cmd == C::LimitOn;
            bot.update(chat, move |s| s.limit.enabled = on).await?;
            ctx.changed(if on { "limit.on" } else { "limit.off" }, &[]).await;
        }
        C::LimitCount | C::LimitTime | C::FloodCount => match ctx.number(0).filter(|n| (1..=10_000).contains(n)) {
            Some(n) => {
                bot.update(chat, move |s| match cmd {
                    C::LimitCount => s.limit.count = n,
                    C::LimitTime => s.limit.seconds = n,
                    _ => s.flood.count = n.max(2),
                })
                .await?;
                let key = match cmd {
                    C::LimitCount => "limit.count_set",
                    C::LimitTime => "limit.time_set",
                    _ => "flood.count_set",
                };
                ctx.changed(key, &[("n", &fa_digits(&n.to_string()))]).await;
            }
            None => ctx.say_t("err.need_number", &[]).await,
        },
        C::FloodOn | C::FloodOff => {
            let on = cmd == C::FloodOn;
            bot.update(chat, move |s| s.flood.enabled = on).await?;
            ctx.changed(if on { "flood.on" } else { "flood.off" }, &[]).await;
        }
        C::FloodMode => match punish_arg(&ctx.p.args).filter(|p| *p != Punish::Delete) {
            Some(punish) => {
                bot.update(chat, move |s| s.flood.punish = punish).await?;
                ctx.changed("flood.mode_set", &[("mode", &punish_label(&ctx.g, punish))]).await;
            }
            None => ctx.say_t("flood.mode_usage", &[]).await,
        },

        // --------------------------------------------------------- characters
        C::MaxChar | C::MinChar => match ctx.number(0) {
            Some(n) => {
                bot.update(chat, move |s| if cmd == C::MaxChar { s.max_chars = n } else { s.min_chars = n }).await?;
                let key = if cmd == C::MaxChar { "chars.max_set" } else { "chars.min_set" };
                ctx.changed(key, &[("n", &fa_digits(&n.to_string()))]).await;
            }
            None => ctx.say_t("err.need_number", &[]).await,
        },
        C::CharOff => {
            bot.update(chat, |s| {
                s.max_chars = 0;
                s.min_chars = 0;
            })
            .await?;
            ctx.changed("chars.off", &[]).await;
        }

        // ------------------------------------------------------------- strict
        C::StrictOn | C::StrictOff => {
            let on = cmd == C::StrictOn;
            if ctx.p.args.is_empty() {
                bot.update(chat, move |s| s.strict.enabled = on).await?;
                ctx.changed(if on { "strict.on" } else { "strict.off" }, &[]).await;
            } else {
                let joined = ctx.p.args_joined();
                match STRICT_TYPES.iter().find(|(_, names)| names.contains(&joined.as_str())) {
                    Some((lock, _)) => {
                        let lock = *lock;
                        bot.update(chat, move |s| if on { s.strict.types.insert(lock) } else { s.strict.types.remove(&lock) })
                            .await?;
                        let key = if on { "strict.type_on" } else { "strict.type_off" };
                        ctx.changed(key, &[("name", &lock_name(&ctx.g, lock))]).await;
                    }
                    None => ctx.say_t("strict.types_usage", &[]).await,
                }
            }
        }
        C::StrictMode => match punish_arg(&ctx.p.args).filter(|p| *p != Punish::Delete) {
            Some(punish) => {
                bot.update(chat, move |s| s.strict.punish = punish).await?;
                ctx.changed("strict.mode_set", &[("mode", &punish_label(&ctx.g, punish))]).await;
            }
            None => ctx.say_t("strict.mode_usage", &[]).await,
        },
        C::StrictMute => match ctx.number(0).filter(|m| *m > 0) {
            Some(minutes) => {
                let punish = Punish::TempMute(i64::from(minutes) * 60);
                bot.update(chat, move |s| s.strict.punish = punish).await?;
                ctx.changed("strict.mode_set", &[("mode", &punish_label(&ctx.g, punish))]).await;
            }
            None => ctx.say_t("err.need_number", &[]).await,
        },

        // ------------------------------------------------------------- tabchi
        C::TabchiMode => match punish_arg(&ctx.p.args).filter(|p| matches!(p, Punish::Ban | Punish::Mute)) {
            Some(punish) => {
                bot.update(chat, move |s| s.tabchi.punish = punish).await?;
                ctx.changed("tabchi.mode_set", &[("mode", &punish_label(&ctx.g, punish))]).await;
            }
            None => ctx.say_t("tabchi.mode_usage", &[]).await,
        },
        C::TabchiTime => {
            let timing = match ctx.p.arg(0) {
                Some("ورود" | "join") => TabchiTiming::Join,
                Some("پیام" | "message") => TabchiTiming::Message,
                _ => {
                    ctx.say_t("tabchi.time_usage", &[]).await;
                    return Ok(true);
                }
            };
            bot.update(chat, move |s| s.tabchi.timing = timing).await?;
            let key = if timing == TabchiTiming::Join { "tabchi.time_join" } else { "tabchi.time_message" };
            ctx.changed(key, &[]).await;
        }

        // -------------------------------------------------------------- warns
        C::SetWarnMax => match ctx.number(0).filter(|n| (1..=50).contains(n)) {
            Some(n) => {
                bot.update(chat, move |s| s.warns.max = n).await?;
                ctx.changed("warn.max_set", &[("n", &fa_digits(&n.to_string()))]).await;
            }
            None => ctx.say_t("err.need_number", &[]).await,
        },
        C::WarnBan | C::WarnMute => {
            let punish = if cmd == C::WarnBan { Punish::Ban } else { Punish::Mute };
            bot.update(chat, move |s| s.warns.punish = punish).await?;
            ctx.changed("warn.action_set", &[("mode", &punish_label(&ctx.g, punish))]).await;
        }

        // ---------------------------------------------------------- force add
        C::ForceAddOn | C::ForceAddOff => {
            let on = cmd == C::ForceAddOn;
            bot.update(chat, move |s| {
                if on && !s.force_add.enabled {
                    s.force_add.since = now();
                }
                s.force_add.enabled = on;
            })
            .await?;
            ctx.changed(if on { "fadd.on" } else { "fadd.off" }, &[]).await;
        }
        C::ForceAddCount => match ctx.number(0).filter(|n| (1..=100).contains(n)) {
            Some(n) => {
                bot.update(chat, move |s| s.force_add.count = n).await?;
                ctx.changed("fadd.count_set", &[("n", &fa_digits(&n.to_string()))]).await;
            }
            None => ctx.say_t("err.need_number", &[]).await,
        },
        C::ForceAddTime => {
            let days = match ctx.p.arg(0) {
                Some("غیرفعال" | "off") => None,
                _ => match ctx.number(0).filter(|d| *d > 0) {
                    Some(days) => Some(days),
                    None => {
                        ctx.say_t("err.need_number", &[]).await;
                        return Ok(true);
                    }
                },
            };
            bot.update(chat, move |s| s.force_add.expire_days = days).await?;
            match days {
                Some(d) => ctx.changed("fadd.time_set", &[("n", &fa_digits(&d.to_string()))]).await,
                None => ctx.changed("fadd.time_off", &[]).await,
            }
        }
        C::ForceAddText | C::ForceJoinText | C::SetGoodbye => match ctx.content_arg().await {
            Some(Content::Text(text)) => {
                let key = match cmd {
                    C::ForceAddText => "fadd.text_set",
                    C::ForceJoinText => "fjoin.text_set",
                    _ => "welcome.goodbye_set",
                };
                bot.update(chat, move |s| match cmd {
                    C::ForceAddText => s.force_add.text = Some(text),
                    C::ForceJoinText => s.force_join.text = Some(text),
                    _ => {
                        s.welcome.goodbye_text = Some(text);
                        s.welcome.goodbye_enabled = true;
                    }
                })
                .await?;
                ctx.changed(key, &[]).await;
            }
            _ => ctx.say_t("err.need_text", &[]).await,
        },
        C::ForceAddTextDel | C::ForceJoinTextDel => {
            bot.update(chat, move |s| if cmd == C::ForceAddTextDel { s.force_add.text = None } else { s.force_join.text = None })
                .await?;
            ctx.changed("common.text_reset", &[]).await;
        }
        C::ForceAddScope => {
            let new_only = match ctx.p.arg(0) {
                Some("همه" | "all") => false,
                Some("جدید" | "new") => true,
                _ => {
                    ctx.say_t("fadd.scope_usage", &[]).await;
                    return Ok(true);
                }
            };
            bot.update(chat, move |s| {
                s.force_add.new_only = new_only;
                s.force_add.since = now();
            })
            .await?;
            ctx.changed(if new_only { "fadd.scope_new" } else { "fadd.scope_all" }, &[]).await;
        }
        C::ForceAddReset => {
            bot.repo.reset_force_adds(chat).await?;
            ctx.changed("fadd.reset", &[]).await;
        }

        // --------------------------------------------------------- force join
        C::ForceJoinOn | C::ForceJoinOff => {
            let on = cmd == C::ForceJoinOn;
            if on && ctx.g.s.force_join.channel_id.is_none() {
                ctx.say_t("fjoin.no_channel", &[]).await;
                return Ok(true);
            }
            bot.update(chat, move |s| s.force_join.enabled = on).await?;
            ctx.changed(if on { "fjoin.on" } else { "fjoin.off" }, &[]).await;
        }
        C::ForceJoinStatus => {
            let fj = &ctx.g.s.force_join;
            let channel = if fj.channel_title.is_empty() { ctx.g.s("common.none") } else { h(&fj.channel_title) };
            let link = if fj.channel_link.is_empty() { ctx.g.s("common.none") } else { h(&fj.channel_link) };
            ctx.say_t("fjoin.status", &[("state", &on_label(ctx, fj.enabled)), ("channel", &channel), ("link", &link)]).await;
        }
        C::ForceJoinChannel => set_force_join_channel(ctx).await?,

        // ------------------------------------------------------------ welcome
        C::WelcomeOn | C::WelcomeOff | C::GoodbyeOn | C::GoodbyeOff => {
            let on = matches!(cmd, C::WelcomeOn | C::GoodbyeOn);
            let welcome = matches!(cmd, C::WelcomeOn | C::WelcomeOff);
            bot.update(chat, move |s| if welcome { s.welcome.enabled = on } else { s.welcome.goodbye_enabled = on }).await?;
            let key = match (welcome, on) {
                (true, true) => "welcome.on",
                (true, false) => "welcome.off",
                (false, true) => "welcome.goodbye_on",
                (false, false) => "welcome.goodbye_off",
            };
            ctx.changed(key, &[]).await;
        }
        C::SetWelcome => match ctx.content_arg().await {
            Some(Content::Text(text)) => {
                bot.update(chat, move |s| s.welcome.text = Some(text)).await?;
                ctx.changed("welcome.text_set", &[]).await;
            }
            Some(media) => {
                bot.update(chat, move |s| s.welcome.media = Some(media)).await?;
                ctx.changed("welcome.media_set", &[]).await;
            }
            None => ctx.say_t("welcome.usage", &[]).await,
        },
        C::WelcomeMedia => match ctx.replied().await.as_ref().and_then(crate::content::to_content) {
            Some(media @ Content::Media { .. }) => {
                bot.update(chat, move |s| s.welcome.media = Some(media)).await?;
                ctx.changed("welcome.media_set", &[]).await;
            }
            _ => ctx.say_t("err.need_media_reply", &[]).await,
        },
        C::WelcomeMediaDel => {
            bot.update(chat, |s| s.welcome.media = None).await?;
            ctx.changed("welcome.media_removed", &[]).await;
        }
        C::GetWelcome => members::send_welcome(bot, &ctx.g, ctx.sender, ctx.msg.chat_id, false).await,
        C::ResetWelcome => {
            bot.update(chat, |s| {
                s.welcome.text = None;
                s.welcome.media = None;
            })
            .await?;
            ctx.changed("welcome.reset", &[]).await;
        }
        C::WelcomeTime => match ctx.number(0) {
            Some(n) => {
                bot.update(chat, move |s| s.welcome.delete_after = n).await?;
                ctx.changed("welcome.time_set", &[("n", &fa_digits(&n.to_string()))]).await;
            }
            None => ctx.say_t("err.need_number", &[]).await,
        },

        // ------------------------------------------------------- auto answers
        C::SetAnswer => set_answer(ctx).await?,
        C::RemAnswer => {
            let keyword = ctx.p.args_joined();
            if bot.repo.remove_answer(chat, &keyword).await? {
                ctx.changed("answer.removed", &[("word", &h(&keyword))]).await;
            } else {
                ctx.say_t("answer.not_found", &[("word", &h(&keyword))]).await;
            }
        }
        C::AnswerList => {
            let answers = bot.repo.answers(chat).await?;
            if answers.is_empty() {
                ctx.say_t("answer.list_empty", &[]).await;
            } else {
                let lines: Vec<String> = answers
                    .iter()
                    .map(|a| {
                        let audience = ctx.g.s(&format!("answer.audience_{}", a.audience.as_db()));
                        format!("• <code>{}</code> — {} ({})", h(&a.keyword), fa_digits(&a.responses.len().to_string()), audience)
                    })
                    .collect();
                ctx.say_t("answer.list", &[("list", &lines.join("\n"))]).await;
            }
        }
        C::CleanAnswers => {
            let n = bot.repo.clear_answers(chat).await?;
            ctx.changed("answer.cleared", &[("n", &fa_digits(&n.to_string()))]).await;
        }
        C::AnswerAudience => {
            let audience = match ctx.p.args.last().map(String::as_str) {
                Some("همه" | "all") => Audience::All,
                Some("مقامداران" | "مقام" | "ranked") => Audience::Ranked,
                Some("مالکین" | "مالک" | "owners") => Audience::Owners,
                _ => {
                    ctx.say_t("answer.audience_usage", &[]).await;
                    return Ok(true);
                }
            };
            let keyword = ctx.p.args[..ctx.p.args.len() - 1].join(" ");
            if bot.repo.set_answer_audience(chat, &keyword, audience).await? {
                let label = ctx.g.s(&format!("answer.audience_{}", audience.as_db()));
                ctx.changed("answer.audience_set", &[("word", &h(&keyword)), ("audience", &label)]).await;
            } else {
                ctx.say_t("answer.not_found", &[("word", &h(&keyword))]).await;
            }
        }
        C::AnswerCooldown => match ctx.number(0) {
            Some(n) => {
                bot.update(chat, move |s| s.answer_cooldown = n).await?;
                ctx.changed("answer.cooldown_set", &[("n", &fa_digits(&n.to_string()))]).await;
            }
            None => ctx.say_t("err.need_number", &[]).await,
        },

        // ---------------------------------------------------------- schedules
        C::AutoCleanSet | C::AutoStatsSet => match clocks(ctx, 5) {
            Some(times) => {
                let list = clock_list(&times);
                let clean = cmd == C::AutoCleanSet;
                bot.update(chat, move |s| if clean { s.auto_clean = times } else { s.auto_stats = times }).await?;
                ctx.changed(if clean { "clean.auto_set" } else { "stats.auto_set" }, &[("times", &list)]).await;
            }
            None => ctx.say_t("err.need_clocks", &[]).await,
        },
        C::AutoCleanRem | C::AutoStatsRem => {
            let clean = cmd == C::AutoCleanRem;
            bot.update(chat, move |s| if clean { s.auto_clean.clear() } else { s.auto_stats.clear() }).await?;
            ctx.changed(if clean { "clean.auto_removed" } else { "stats.auto_removed" }, &[]).await;
        }
        C::AutoCleanStatus | C::AutoStatsStatus => {
            let times = if cmd == C::AutoCleanStatus { &ctx.g.s.auto_clean } else { &ctx.g.s.auto_stats };
            if times.is_empty() {
                ctx.say_t("common.schedule_none", &[]).await;
            } else {
                ctx.say_t("common.schedule", &[("times", &clock_list(times)), ("tz", &format_tz(ctx.g.s.tz))]).await;
            }
        }

        // ----------------------------------------------------------- security
        C::Captcha | C::AntiBetrayal | C::DelReason => match on_off(ctx.p.arg(0)) {
            Some(on) => {
                bot.update(chat, move |s| match cmd {
                    C::Captcha => s.security.captcha = on,
                    C::AntiBetrayal => s.security.anti_betrayal = on,
                    _ => s.security.delete_reason = on,
                })
                .await?;
                let key = match cmd {
                    C::Captcha => "captcha.state",
                    C::AntiBetrayal => "sec.betrayal_state",
                    _ => "sec.reason_state",
                };
                ctx.changed(key, &[("state", &on_label(ctx, on))]).await;
            }
            None => ctx.say_t("err.need_on_off", &[]).await,
        },
        C::CaptchaTime | C::BetrayalLimit | C::DupHours | C::DupLen => match ctx.number(0).filter(|n| *n > 0) {
            Some(n) => {
                bot.update(chat, move |s| match cmd {
                    C::CaptchaTime => s.security.captcha_minutes = n.min(60),
                    C::BetrayalLimit => s.security.betrayal_limit = n,
                    C::DupHours => s.security.duplicate_hours = n.min(168),
                    _ => s.security.duplicate_min_len = n,
                })
                .await?;
                let key = match cmd {
                    C::CaptchaTime => "captcha.time_set",
                    C::BetrayalLimit => "sec.betrayal_limit_set",
                    C::DupHours => "sec.dup_hours_set",
                    _ => "sec.dup_len_set",
                };
                ctx.changed(key, &[("n", &fa_digits(&n.to_string()))]).await;
            }
            None => ctx.say_t("err.need_number", &[]).await,
        },
        C::ReportMode => {
            let mode = match ctx.p.arg(0) {
                Some("گروه" | "group") => ReportMode::Group,
                Some("پیوی" | "pv" | "private") => ReportMode::Private,
                _ => {
                    ctx.say_t("report.mode_usage", &[]).await;
                    return Ok(true);
                }
            };
            bot.update(chat, move |s| s.security.report_mode = mode).await?;
            ctx.changed(if mode == ReportMode::Group { "report.mode_group" } else { "report.mode_private" }, &[]).await;
        }
        C::SetLog => set_log_channel(ctx).await?,
        C::RemLog => {
            bot.update(chat, |s| s.security.log_chat = None).await?;
            ctx.say_t("log.removed", &[]).await;
        }
        C::TimeZone => match ctx.p.arg(0).and_then(parse_tz) {
            Some(tz) => {
                bot.update(chat, move |s| s.tz = tz).await?;
                ctx.changed("sec.tz_set", &[("tz", &format_tz(tz))]).await;
            }
            None => ctx.say_t("sec.tz_usage", &[]).await,
        },

        // -------------------------------------------------------------- texts
        C::SetText => {
            let Some(key) = ctx.p.arg(0).filter(|k| i18n::has_key(k)).map(str::to_owned) else {
                ctx.say_t("text.unknown_key", &[]).await;
                return Ok(true);
            };
            let content = match ctx.p.raw.split_once(char::is_whitespace) {
                Some(_) => {
                    let html = ctx.rich_args();
                    html.split_once(char::is_whitespace).map(|(_, rest)| rest.trim().to_owned()).unwrap_or_default()
                }
                None => match ctx.replied().await.as_ref().and_then(crate::content::to_content) {
                    Some(Content::Text(text)) => text,
                    _ => String::new(),
                },
            };
            if content.is_empty() {
                ctx.say_t("err.need_text", &[]).await;
                return Ok(true);
            }
            bot.repo.set_text(chat, &key, &ctx.g.s.lang, &content).await?;
            bot.invalidate_texts(chat, &ctx.g.s.lang).await;
            ctx.changed("text.set", &[("key", &key)]).await;
        }
        C::RemText => {
            let key = ctx.p.arg(0).unwrap_or_default().to_owned();
            if bot.repo.remove_text(chat, &key, &ctx.g.s.lang).await? {
                bot.invalidate_texts(chat, &ctx.g.s.lang).await;
                ctx.changed("text.removed", &[("key", &h(&key))]).await;
            } else {
                ctx.say_t("text.not_overridden", &[("key", &h(&key))]).await;
            }
        }
        C::Bots => {
            let owner = bot.repo.owner(chat).await?;
            let mut lines = Vec::new();
            for (id, username, alive, groups) in bot.repo.bots_overview().await? {
                let state = ctx.g.s(if alive { "sec.bot_alive" } else { "sec.bot_down" });
                let here = if owner == Some(id) { ctx.g.s("sec.bot_here") } else { String::new() };
                let name = h(&format!("@{username}"));
                let groups = fa_digits(&groups.to_string());
                lines.push(ctx.t("sec.bot_line", &[("name", &name), ("state", &state), ("groups", &groups), ("here", &here)]));
            }
            ctx.say_t("sec.bots", &[("list", &lines.join("\n"))]).await;
        }
        C::TextKeys => {
            let mut chunk = ctx.g.s("text.keys_header");
            for key in i18n::keys() {
                if chunk.len() > 3500 {
                    ctx.say(&chunk).await;
                    chunk.clear();
                }
                chunk.push_str(&format!("\n<code>{key}</code>"));
            }
            ctx.say(&chunk).await;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

async fn set_answer(ctx: &Ctx<'_>) -> Result<()> {
    // "تنظیم پاسخ keyword | response" or "تنظیم پاسخ keyword" replying to the response.
    let (keyword, content) = if ctx.p.raw.contains('|') {
        let keyword = crate::text::normalize(ctx.p.raw.split('|').next().unwrap_or_default());
        let html = ctx.rich_args();
        let response = html.split_once('|').map(|(_, r)| r.trim().to_owned()).unwrap_or_default();
        (keyword, (!response.is_empty()).then_some(Content::Text(response)))
    } else {
        (ctx.p.args_joined(), ctx.replied().await.as_ref().and_then(crate::content::to_content))
    };
    let Some(content) = content.filter(|_| !keyword.is_empty()) else {
        ctx.say_t("answer.usage", &[]).await;
        return Ok(());
    };
    match ctx.bot.repo.add_answer(ctx.g.id, &keyword, &content, 20).await? {
        Some(count) => ctx.changed("answer.added", &[("word", &h(&keyword)), ("n", &fa_digits(&count.to_string()))]).await,
        None => ctx.say_t("answer.full", &[("word", &h(&keyword))]).await,
    }
    Ok(())
}

/// Resolves "@channel" or "-100… [link]" to a chat: (id, title, public link).
async fn resolve_channel(ctx: &Ctx<'_>) -> Option<(i64, String, String)> {
    let first = ctx.p.arg(0)?;
    let chat = if let Some(username) = first.strip_prefix('@') {
        ctx.bot.client.search_public_chat(username).await.ok()?
    } else {
        ctx.bot.client.get_chat(first.parse().ok()?).await.ok()?
    };
    let username = match &chat.r#type {
        ChatType::Supergroup(sg) => ctx
            .bot
            .client
            .get_supergroup(sg.supergroup_id)
            .await
            .ok()
            .and_then(|s| s.usernames)
            .map(|u| u.editable_username)
            .filter(|u| !u.is_empty()),
        _ => None,
    };
    let link = match (username, ctx.p.raw.split_whitespace().nth(1)) {
        (_, Some(link)) if link.starts_with("http") => link.to_owned(),
        (Some(username), _) => format!("https://t.me/{username}"),
        _ => String::new(),
    };
    Some((chat.id, chat.title, link))
}

async fn set_force_join_channel(ctx: &Ctx<'_>) -> Result<()> {
    let Some((channel_id, title, link)) = resolve_channel(ctx).await else {
        ctx.say_t("fjoin.channel_not_found", &[]).await;
        return Ok(());
    };
    let member = ctx.bot.client.get_chat_member(channel_id, MessageSenderUser { user_id: ctx.bot.me }).await;
    if !matches!(member.map(|m| m.status), Ok(ChatMemberStatus::Administrator(_) | ChatMemberStatus::Creator(_))) {
        ctx.say_t("fjoin.bot_not_admin", &[]).await;
        return Ok(());
    }
    let (t, l) = (title.clone(), link.clone());
    ctx.bot
        .update(ctx.g.id, move |s| {
            s.force_join.channel_id = Some(channel_id);
            s.force_join.channel_title = t;
            s.force_join.channel_link = l;
        })
        .await?;
    ctx.changed("fjoin.channel_set", &[("channel", &h(&title))]).await;
    if link.is_empty() {
        ctx.say_t("fjoin.no_link", &[]).await;
    }
    Ok(())
}

async fn set_log_channel(ctx: &Ctx<'_>) -> Result<()> {
    let Some((log_chat, title, _)) = resolve_channel(ctx).await else {
        ctx.say_t("log.not_found", &[]).await;
        return Ok(());
    };
    if ctx.bot.send(log_chat, &ctx.t("log.connected", &[])).await.is_none() {
        ctx.say_t("log.cannot_post", &[]).await;
        return Ok(());
    }
    ctx.bot.update(ctx.g.id, move |s| s.security.log_chat = Some(log_chat)).await?;
    ctx.say_t("log.set", &[("channel", &h(&title))]).await;
    Ok(())
}
