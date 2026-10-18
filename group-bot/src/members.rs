//! Joins, leaves and added members: bot lock, force-add counting, anti
//! tabchi on join, welcome/goodbye messages and captcha.

use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tdrust::prelude::*;
use tdrust::types::{
    ChatMemberStatusRestricted, ChatPermissions, InlineKeyboardButton, InlineKeyboardButtonTypeCallback,
    MessageSenderUser, ReplyMarkupInlineKeyboard, UserType,
};

use crate::admin::global::Flag;
use crate::bot::{soft, Bot, Group};
use crate::content::input_content;
use crate::locks::Lock;
use crate::model::{Content, Rank, TabchiTiming};
use crate::text::{fa_digits, has_link_text, has_username_text, h, jalali_date, local_time, mention_html, now, random};

/// Handles a service message about members joining, being added or leaving.
pub async fn on_service(bot: &Arc<Bot>, g: &Group, msg: &Message) -> Result<()> {
    let actor = msg.sender_user_id().unwrap_or(0);
    match &msg.content {
        MessageContent::ChatAddMembers(added) => {
            for &user_id in &added.member_user_ids {
                if user_id == bot.me {
                    bot.send(g.id, &g.s("common.bot_added")).await;
                    continue;
                }
                if bot.is_fleet(user_id).await {
                    continue; // another of our bots: never kicked by the bot lock
                }
                on_added(bot, g, actor, user_id).await?;
            }
        }
        MessageContent::ChatJoinByLink(_) | MessageContent::ChatJoinByRequest(_) => {
            soft(bot.repo.record_join(g.id, actor, None).await, "record join");
            on_join(bot, g, actor).await;
        }
        MessageContent::ChatDeleteMember(left) if g.s.welcome.goodbye_enabled && bot.flag(Flag::Welcome) => {
            if bot.is_fleet(left.user_id).await {
                return Ok(());
            }
            let template = g.s.welcome.goodbye_text.clone().unwrap_or_else(|| g.s("welcome.goodbye_default"));
            let text = render(bot, g, left.user_id, &template).await;
            if let Some(sent) = bot.send(g.id, &text).await {
                if g.s.welcome.delete_after > 0 {
                    bot.delete_later(sent, u64::from(g.s.welcome.delete_after));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

async fn on_added(bot: &Arc<Bot>, g: &Group, adder: i64, user_id: i64) -> Result<()> {
    let user = bot.client.get_user(user_id).await.ok();
    if let Some(user) = &user {
        bot.remember_user(user).await;
    }
    let is_bot = user.as_ref().is_some_and(|u| matches!(u.r#type, UserType::Bot(_)));

    if is_bot {
        if bot.rank(g.id, adder).await >= Rank::Mod {
            return Ok(());
        }
        if g.s.locked(Lock::Bot) {
            let _ = bot.kick(g.id, user_id).await;
            let text = g.t("lock.bot_removed", &[("user", &bot.mention(g.id, adder).await)]);
            bot.notice(g.id, &text, 30).await;
        }
        if g.s.locked(Lock::BotAdder) && adder != 0 {
            // Keep the member, but take away their right to add anyone.
            let permissions = ChatPermissions { can_invite_users: false, ..crate::bot::default_permissions() };
            let status = ChatMemberStatusRestricted { is_member: true, restricted_until_date: 0, permissions };
            let _ = bot.client.set_chat_member_status(g.id, MessageSenderUser { user_id: adder }, status).await;
        }
        if g.s.locked(Lock::Tabchi) && adder != 0 && bot.flag(Flag::AntiTabchi) {
            punish_tabchi(bot, g, adder, "tabchi.reason_adds_bots").await;
        }
        return Ok(());
    }

    let invited_by = (adder != 0 && adder != user_id).then_some(adder);
    soft(bot.repo.record_join(g.id, user_id, invited_by).await, "record join");
    if let Some(inviter) = invited_by {
        soft(bot.repo.count_invite(g.id, inviter).await, "count invite");
    }
    on_join(bot, g, user_id).await;
    Ok(())
}

/// Checks, captcha and welcome for a member who just joined.
async fn on_join(bot: &Arc<Bot>, g: &Group, user_id: i64) {
    if user_id == 0 || crate::admin::enforce_gban(bot, g, user_id, None).await {
        return;
    }
    if bot.rank(g.id, user_id).await >= Rank::Vip {
        return;
    }
    if g.s.tabchi.timing == TabchiTiming::Join {
        if let Some(reason) = suspicious(bot, g, user_id).await {
            punish_tabchi(bot, g, user_id, reason).await;
            return;
        }
    }
    // Remembered for a day: the first messages of new members get extra checks.
    soft(bot.cache.set_json(&format!("new:{}:{user_id}", g.id), &now(), 86_400).await, "newbie flag");
    if g.s.security.captcha && bot.flag(Flag::Captcha) {
        start_captcha(bot, g, user_id).await;
    }
    if g.s.welcome.enabled && bot.flag(Flag::Welcome) {
        send_welcome(bot, g, user_id, g.id, true).await;
    }
}

/// Why a member looks like a tabchi (advertising account), if they do.
pub async fn suspicious(bot: &Bot, g: &Group, user_id: i64) -> Option<&'static str> {
    if !bot.flag(Flag::AntiTabchi) {
        return None;
    }
    if g.s.locked(Lock::Tabchi) {
        if let Ok(user) = bot.client.get_user(user_id).await {
            let name = format!("{} {}", user.first_name, user.last_name);
            if has_link_text(&name) || has_username_text(&name) {
                return Some("tabchi.reason_name");
            }
        }
    }
    if g.s.locked(Lock::Bio) {
        if let Ok(info) = bot.client.get_user_full_info(user_id).await {
            let bio = info.bio.map(|b| b.text).unwrap_or_default();
            if has_link_text(&bio) || has_username_text(&bio) {
                return Some("tabchi.reason_bio");
            }
        }
    }
    None
}

pub async fn punish_tabchi(bot: &Bot, g: &Group, user_id: i64, reason_key: &str) {
    let reason = g.s(reason_key);
    let report = bot.apply_hard(g, user_id, g.s.tabchi.punish, 0, &reason).await;
    let text = g.t("tabchi.detected", &[("result", &report)]);
    bot.notice(g.id, &text, 60).await;
}

/// Fills welcome variables (English `!name` and Persian `نام_کاربر` forms).
pub async fn render(bot: &Bot, g: &Group, user_id: i64, template: &str) -> String {
    let user = bot.client.get_user(user_id).await.ok();
    let first = user.as_ref().map(|u| u.first_name.clone()).unwrap_or_default();
    let last = user.as_ref().map(|u| u.last_name.clone()).unwrap_or_default();
    let username = user
        .as_ref()
        .and_then(|u| u.usernames.as_ref())
        .map(|u| u.editable_username.clone())
        .filter(|u| !u.is_empty())
        .map(|u| format!("@{u}"))
        .unwrap_or_else(|| g.s("common.none"));
    let title = bot.client.get_chat(g.id).await.map(|c| c.title).unwrap_or_default();
    let emoji = ["🌸", "🌟", "🎉", "💐", "✨", "🌹", "🎈", "💫", "🥳", "🌺"][random(10) as usize];
    let values: [(&[&str], String); 11] = [
        (&["!mention", "منشن_کاربر", "{user}"], mention_html(user_id, format!("{first} {last}").trim())),
        (&["!firstname", "نام_کاربر"], h(&first)),
        (&["!lastname", "فامیل_کاربر"], h(&last)),
        (&["!username", "آیدی_کاربر"], h(&username)),
        (&["!userid", "آیدی_عددی_کاربر"], user_id.to_string()),
        (&["!grouplink", "لینک_گروه"], h(g.s.link.as_deref().unwrap_or_default())),
        (&["!grouprules", "قوانین_گروه"], g.s.rules.clone().unwrap_or_default()),
        (&["!groupname", "نام_گروه", "{group}"], h(&title)),
        (&["!date", "زمان_تاریخ"], jalali_date(now(), g.s.tz)),
        (&["!time", "زمان_ساعت"], local_time(now(), g.s.tz)),
        (&["!emoji", "ایموجی_خودکار"], emoji.to_owned()),
    ];
    let mut out = template.to_owned();
    // No variable name contains another, so the replacement order does not matter.
    for (names, value) in &values {
        for name in *names {
            out = out.replace(name, value);
        }
    }
    out
}

/// Sends the welcome for `user_id` to `chat_id` (the group, or a preview).
pub async fn send_welcome(bot: &Bot, g: &Group, user_id: i64, chat_id: i64, auto_delete: bool) {
    let template = g.s.welcome.text.clone().unwrap_or_else(|| g.s("welcome.default"));
    let text = render(bot, g, user_id, &template).await;
    let sent = match &g.s.welcome.media {
        Some(Content::Media { kind, file_id, caption }) => {
            let caption = if caption.is_empty() { text.clone() } else { render(bot, g, user_id, caption).await };
            let media = Content::Media { kind: *kind, file_id: file_id.clone(), caption };
            let input = input_content(&media, |c| c.to_owned());
            match bot.send_input(chat_id, None, None, input).await {
                Some(sent) => Some(sent),
                None => bot.send(chat_id, &text).await,
            }
        }
        _ => bot.send(chat_id, &text).await,
    };
    if let (Some(sent), true) = (sent, auto_delete && g.s.welcome.delete_after > 0) {
        bot.delete_later(sent, u64::from(g.s.welcome.delete_after));
    }
}

// ------------------------------------------------------------------ captcha

#[derive(Serialize, Deserialize)]
struct Captcha {
    answer: u32,
    message_id: i64,
}

fn captcha_key(chat_id: i64, user_id: i64) -> String {
    format!("cap:{chat_id}:{user_id}")
}

/// The sorted set of this bot's captcha deadlines. Per bot, because only the
/// bot that asked (the group's owner) can remove the member.
pub fn captcha_deadlines(bot: &Bot) -> String {
    format!("captcha:{}", bot.me)
}

async fn start_captcha(bot: &Arc<Bot>, g: &Group, user_id: i64) {
    if bot.restrict(g.id, user_id, 0).await.is_err() {
        return; // the bot cannot restrict here; no point in asking
    }
    let (a, b) = (1 + random(9) as u32, 1 + random(9) as u32);
    let answer = a + b;
    let mut options = vec![answer];
    while options.len() < 4 {
        let candidate = 2 + random(17) as u32;
        if !options.contains(&candidate) {
            options.push(candidate);
        }
    }
    options.sort_by_key(|_| random(100));
    let row = options
        .iter()
        .map(|value| InlineKeyboardButton {
            text: fa_digits(&value.to_string()),
            icon_custom_emoji_id: 0,
            style: crate::panels::style(crate::panels::Tone::Primary),
            r#type: InlineKeyboardButtonTypeCallback { data: format!("cap:{user_id}:{value}").into_bytes() }.into(),
        })
        .collect();
    let markup = ReplyMarkupInlineKeyboard { rows: vec![row], ..Default::default() }.into();
    let minutes = g.s.security.captcha_minutes.max(1);
    let text = g.t(
        "captcha.question",
        &[
            ("user", &bot.mention(g.id, user_id).await),
            ("a", &fa_digits(&a.to_string())),
            ("b", &fa_digits(&b.to_string())),
            ("minutes", &fa_digits(&minutes.to_string())),
        ],
    );
    let Some(sent) = bot.send_markup(g.id, &text, markup, None).await else { return };
    let message_id = if sent.sending_state.is_some() { bot.sent.confirmed(&sent).await.map_or(0, |m| m.id) } else { sent.id };
    let ttl = u64::from(minutes) * 60 + 300;
    soft(bot.cache.set_json(&captcha_key(g.id, user_id), &Captcha { answer, message_id }, ttl).await, "captcha");
    let deadline = now() + i64::from(minutes) * 60;
    soft(bot.cache.schedule(&captcha_deadlines(bot), &format!("{}:{user_id}", g.id), deadline).await, "captcha deadline");
}

/// A captcha button press. Returns the toast to show the presser.
pub async fn solve_captcha(bot: &Bot, g: &Group, presser: i64, user_id: i64, value: u32) -> String {
    if presser != user_id {
        return g.s("captcha.not_yours");
    }
    let key = captcha_key(g.id, user_id);
    let Some(Some(captcha)) = soft(bot.cache.get_json::<Captcha>(&key).await, "captcha") else {
        return g.s("captcha.expired");
    };
    soft(bot.cache.del(&key).await, "captcha");
    soft(bot.cache.unschedule(&captcha_deadlines(bot), &format!("{}:{user_id}", g.id)).await, "captcha deadline");
    if captcha.message_id != 0 {
        bot.delete(g.id, vec![captcha.message_id]).await;
    }
    if value == captcha.answer {
        let _ = bot.unrestrict(g.id, user_id).await;
        let text = g.t("captcha.passed", &[("user", &bot.mention(g.id, user_id).await)]);
        bot.notice(g.id, &text, 15).await;
        g.s("captcha.correct")
    } else {
        let _ = bot.kick(g.id, user_id).await;
        let text = g.t("captcha.failed", &[("user", &bot.mention(g.id, user_id).await)]);
        bot.notice(g.id, &text, 30).await;
        g.s("captcha.wrong")
    }
}

/// Removes a member who did not answer in time (a "chat:user" entry).
pub async fn expire_captcha(bot: &Bot, entry: &str) {
    let Some((chat, user)) = entry.split_once(':') else { return };
    let (Ok(chat_id), Ok(user_id)) = (chat.parse::<i64>(), user.parse::<i64>()) else { return };
    let key = captcha_key(chat_id, user_id);
    let Some(Some(captcha)) = soft(bot.cache.get_json::<Captcha>(&key).await, "captcha") else { return };
    soft(bot.cache.del(&key).await, "captcha");
    if captcha.message_id != 0 {
        bot.delete(chat_id, vec![captcha.message_id]).await;
    }
    let _ = bot.kick(chat_id, user_id).await;
    let g = bot.group(chat_id).await;
    let text = g.t("captcha.timeout", &[("user", &bot.mention(chat_id, user_id).await)]);
    bot.notice(chat_id, &text, 30).await;
}

/// Whether a member still has to solve a captcha.
pub async fn captcha_pending(bot: &Bot, chat_id: i64, user_id: i64) -> bool {
    bot.cache.exists(&captcha_key(chat_id, user_id)).await.unwrap_or(false)
}
