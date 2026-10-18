//! Enforcement: everything that can delete or punish a member's message,
//! plus auto answers.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tdrust::prelude::*;
use tdrust::types::{
    ChatMemberStatus, InlineKeyboardButton, InlineKeyboardButtonTypeUrl, MessageOrigin, MessageSenderUser,
    ReplyMarkup, ReplyMarkupInlineKeyboard, StickerFormat, TextEntityType,
};

use crate::admin::global::Flag;
use crate::bot::{soft, Bot, Group};
use crate::content::{input_content, is_media, text_of};
use crate::exec::lock_label;
use crate::locks::Lock;
use crate::model::{Audience, Content, Punish, Rank, TabchiTiming};
use crate::text::{
    emoji_stats, fa_digits, has_english, has_hashtag_text, has_link_text, has_persian, has_username_text, normalize, now, random,
    text_hash,
};

/// Whether `word` occurs in `text` as whole words (both normalized).
pub fn contains_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + word.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

pub fn is_member(status: &ChatMemberStatus) -> bool {
    match status {
        ChatMemberStatus::Creator(_) | ChatMemberStatus::Administrator(_) | ChatMemberStatus::Member(_) => true,
        ChatMemberStatus::Restricted(r) => r.is_member,
        _ => false,
    }
}

/// The first lock a message breaks, if any.
pub fn violation(g: &Group, msg: &Message, now: i64) -> Option<Lock> {
    let s = &g.s;
    let content = &msg.content;
    let text = text_of(content);
    let plain = text.map_or("", |t| t.text.as_str());
    let entities = text.map_or(&[][..], |t| t.entities.as_slice());
    let has = |pred: &dyn Fn(&TextEntityType) -> bool| entities.iter().any(|e| pred(&e.r#type));
    let on = |lock: Lock| s.locked(lock);

    if on(Lock::Forward) && msg.forward_info.is_some() {
        return Some(Lock::Forward);
    }
    if on(Lock::Inline) && (msg.via_bot_user_id != 0 || matches!(msg.reply_markup, Some(ReplyMarkup::InlineKeyboard(_)))) {
        return Some(Lock::Inline);
    }
    if on(Lock::Link) && (has(&|t| matches!(t, TextEntityType::Url(_) | TextEntityType::TextUrl(_))) || has_link_text(plain)) {
        return Some(Lock::Link);
    }
    if on(Lock::Hyper) && has(&|t| matches!(t, TextEntityType::TextUrl(_) | TextEntityType::MentionName(_))) {
        return Some(Lock::Hyper);
    }
    if on(Lock::Username) && (has(&|t| matches!(t, TextEntityType::Mention(_))) || has_username_text(plain)) {
        return Some(Lock::Username);
    }
    if on(Lock::Hashtag) && (has(&|t| matches!(t, TextEntityType::Hashtag(_))) || has_hashtag_text(plain)) {
        return Some(Lock::Hashtag);
    }
    if on(Lock::Command) && (has(&|t| matches!(t, TextEntityType::BotCommand(_))) || plain.trim_start().starts_with('/')) {
        return Some(Lock::Command);
    }
    let media_spoiler = match content {
        MessageContent::Photo(m) => m.has_spoiler,
        MessageContent::Video(m) => m.has_spoiler,
        MessageContent::Animation(m) => m.has_spoiler,
        _ => false,
    };
    if on(Lock::Spoiler) && (media_spoiler || has(&|t| matches!(t, TextEntityType::Spoiler(_)))) {
        return Some(Lock::Spoiler);
    }
    if on(Lock::Badword) {
        let normalized = normalize(plain);
        let words = g.s("lock.badwords");
        if words.split(',').map(str::trim).any(|w| contains_word(&normalized, &normalize(w))) {
            return Some(Lock::Badword);
        }
    }
    if on(Lock::Text) && !plain.trim().is_empty() {
        return Some(Lock::Text);
    }
    if on(Lock::Persian) && has_persian(plain) {
        return Some(Lock::Persian);
    }
    if on(Lock::English) && has_english(plain) {
        return Some(Lock::English);
    }
    let animated = matches!(content, MessageContent::AnimatedEmoji(_) | MessageContent::Dice(_));
    let (emoji, other) = if matches!(content, MessageContent::Text(_)) { emoji_stats(plain) } else { (0, 1) };
    if on(Lock::Emoji) && (emoji > 0 || animated) {
        return Some(Lock::Emoji);
    }
    if on(Lock::JustEmoji) && ((emoji > 0 && other == 0) || animated) {
        return Some(Lock::JustEmoji);
    }
    if on(Lock::OneEmoji) && ((emoji == 1 && other == 0) || animated) {
        return Some(Lock::OneEmoji);
    }
    if on(Lock::Game) && matches!(content, MessageContent::Game(_)) {
        return Some(Lock::Game);
    }
    if s.media_locked(now) && is_media(content) {
        return Some(Lock::Media);
    }
    let kind = match content {
        MessageContent::Photo(_) => Lock::Photo,
        MessageContent::Video(_) => Lock::Video,
        MessageContent::Animation(_) => Lock::Gif,
        MessageContent::Sticker(m) => match m.sticker.format {
            StickerFormat::Webp(_) => Lock::Sticker,
            _ => Lock::AnimatedSticker,
        },
        MessageContent::Document(_) => Lock::Document,
        MessageContent::Audio(_) => Lock::Audio,
        MessageContent::VoiceNote(_) => Lock::Voice,
        MessageContent::VideoNote(_) => Lock::VideoNote,
        MessageContent::Contact(_) => Lock::Contact,
        MessageContent::Location(_) | MessageContent::LiveLocation(_) | MessageContent::Venue(_) => Lock::Location,
        _ => return None,
    };
    on(kind).then_some(kind)
}

/// Runs every check on a member's message. Returns true if it was removed.
pub async fn check_message(bot: &Arc<Bot>, g: &Group, msg: &Message, user: i64, rank: Rank) -> bool {
    let chat = g.id;
    let t = now();
    let delete = || async { bot.delete(chat, vec![msg.id]).await };

    if crate::members::captcha_pending(bot, chat, user).await || g.s.group_locked(t) {
        delete().await;
        return true;
    }
    if rank < Rank::Exempt && (!force_join_ok(bot, g, user).await || !force_add_ok(bot, g, user).await) {
        delete().await;
        return true;
    }

    // The first message of a new member.
    if soft(bot.cache.take(&format!("new:{chat}:{user}")).await, "newbie flag") == Some(true) {
        if g.s.tabchi.timing == TabchiTiming::Message {
            if let Some(reason) = crate::members::suspicious(bot, g, user).await {
                delete().await;
                crate::members::punish_tabchi(bot, g, user, reason).await;
                return true;
            }
        }
        let text = text_of(&msg.content).map_or("", |t| t.text.as_str());
        let advertising = msg.forward_info.is_some() || has_link_text(text) || has_username_text(text);
        if g.s.locked(Lock::Advertiser) && advertising && bot.flag(Flag::AntiTabchi) {
            delete().await;
            crate::members::punish_tabchi(bot, g, user, "tabchi.reason_advertiser").await;
            return true;
        }
    }

    if let Some(lock) = violation(g, msg, t) {
        delete().await;
        let reason = lock_label(g, lock);
        if g.s.strict.enabled && g.s.strict.types.contains(&lock) {
            let report = bot.apply(g, user, g.s.strict.punish, 0, &reason).await;
            if !report.is_empty() {
                bot.notice(chat, &report, 30).await;
            }
        } else {
            deletion_notice(bot, g, user, &reason).await;
        }
        return true;
    }

    let text = text_of(&msg.content).map_or(String::new(), |t| t.text.clone());
    let length = text.chars().count() as u32;
    if !text.is_empty() && ((g.s.max_chars > 0 && length > g.s.max_chars) || (g.s.min_chars > 0 && length < g.s.min_chars)) {
        delete().await;
        deletion_notice(bot, g, user, &g.s("chars.reason")).await;
        return true;
    }

    if !text.is_empty() && bot.flag(Flag::Filters) {
        let normalized = normalize(&text);
        for (word, punish) in filters(bot, chat).await {
            if contains_word(&normalized, &word) {
                delete().await;
                let reason = g.s("filter.reason");
                let report = bot.apply(g, user, punish, 0, &reason).await;
                if report.is_empty() {
                    deletion_notice(bot, g, user, &reason).await;
                } else {
                    bot.notice(chat, &report, 30).await;
                }
                return true;
            }
        }
    }

    let anti_flood = bot.flag(Flag::AntiFlood);
    if g.s.flood.enabled && anti_flood {
        let window = u64::from(g.s.flood.seconds.max(1));
        let count = soft(bot.cache.hit(&format!("fl:{chat}:{user}"), window).await, "flood counter").unwrap_or(0);
        let max = i64::from(g.s.flood.count.max(2));
        if count > max {
            delete().await;
            if count == max + 1 {
                let report = bot.apply(g, user, g.s.flood.punish, 0, &g.s("flood.reason")).await;
                if !report.is_empty() {
                    bot.send(chat, &report).await;
                }
            }
            return true;
        }
    }

    if g.s.limit.enabled && anti_flood {
        let window = u64::from(g.s.limit.seconds.max(1));
        let count = soft(bot.cache.hit(&format!("lm:{chat}:{user}"), window).await, "limit counter").unwrap_or(0);
        let max = i64::from(g.s.limit.count.max(1));
        if count > max {
            delete().await;
            if count == max + 1 {
                let text = g.t(
                    "limit.notice",
                    &[
                        ("user", &bot.mention(chat, user).await),
                        ("count", &fa_digits(&max.to_string())),
                        ("seconds", &fa_digits(&window.to_string())),
                    ],
                );
                bot.notice(chat, &text, 15).await;
            }
            return true;
        }
    }

    if g.s.locked(Lock::Duplicate) && length >= g.s.security.duplicate_min_len {
        let ttl = u64::from(g.s.security.duplicate_hours.max(1)) * 3600;
        if soft(bot.cache.once(&format!("dup:{chat}:{:x}", text_hash(&text)), ttl).await, "duplicate") == Some(false) {
            delete().await;
            deletion_notice(bot, g, user, &lock_label(g, Lock::Duplicate)).await;
            return true;
        }
    }

    if let Some(minutes) = g.s.temp_media_minutes {
        if is_media(&msg.content) {
            bot.delete_message_later(chat, msg.id, u64::from(minutes) * 60);
        }
    }
    false
}

/// Tells the member why their message disappeared (at most every 20 seconds).
async fn deletion_notice(bot: &Bot, g: &Group, user: i64, reason: &str) {
    if !g.s.security.delete_reason {
        return;
    }
    if soft(bot.cache.once(&format!("thr:reason:{}:{user}", g.id), 20).await, "throttle") == Some(false) {
        return;
    }
    let text = g.t("guard.deleted", &[("user", &bot.mention(g.id, user).await), ("reason", reason)]);
    bot.notice(g.id, &text, 10).await;
}

async fn filters(bot: &Bot, chat: i64) -> Vec<(String, Punish)> {
    let key = format!("flt:{chat}");
    if let Some(Some(list)) = soft(bot.cache.get_json::<Vec<(String, Punish)>>(&key).await, "filter cache") {
        return list;
    }
    let list = soft(bot.repo.filters(chat).await, "filters").unwrap_or_default();
    soft(bot.cache.set_json(&key, &list, 3600).await, "filter cache");
    list
}

async fn force_join_ok(bot: &Bot, g: &Group, user: i64) -> bool {
    let fj = &g.s.force_join;
    let (true, Some(channel)) = (fj.enabled && bot.flag(Flag::ForceJoin), fj.channel_id) else { return true };
    let cache_key = format!("fj:{channel}:{user}");
    if soft(bot.cache.exists(&cache_key).await, "force join cache") == Some(true) {
        return true;
    }
    let joined = match bot.client.get_chat_member(channel, MessageSenderUser { user_id: user }).await {
        Ok(member) => is_member(&member.status),
        Err(err) => {
            log::debug!("cannot check membership of {user} in {channel}: {err}");
            true // cannot verify: do not punish
        }
    };
    if joined {
        soft(bot.cache.set_json(&cache_key, &true, 600).await, "force join cache");
        return true;
    }
    if soft(bot.cache.once(&format!("thr:fj:{}:{user}", g.id), 60).await, "throttle") != Some(false) {
        let template = fj.text.clone().unwrap_or_else(|| g.s("fjoin.notice"));
        let text = crate::members::render(bot, g, user, &template).await.replace("{channel}", &crate::text::h(&fj.channel_title));
        let sent = if fj.channel_link.is_empty() {
            bot.send(g.id, &text).await
        } else {
            let button = InlineKeyboardButton {
                text: g.s("fjoin.button"),
                icon_custom_emoji_id: 0,
                style: crate::panels::style(crate::panels::Tone::Primary),
                r#type: InlineKeyboardButtonTypeUrl { url: fj.channel_link.clone() }.into(),
            };
            let markup = ReplyMarkupInlineKeyboard { rows: vec![vec![button]], ..Default::default() }.into();
            bot.send_markup(g.id, &text, markup, None).await
        };
        if let Some(sent) = sent {
            bot.delete_later(sent, 60);
        }
    }
    false
}

async fn force_add_ok(bot: &Bot, g: &Group, user: i64) -> bool {
    let fa = &g.s.force_add;
    if !fa.enabled || !bot.flag(Flag::ForceAdd) {
        return true;
    }
    let member = soft(bot.repo.member(g.id, user).await, "member").flatten();
    let (joined_at, mut added, done_at) = member.map_or((0, 0, None), |m| (m.joined_at, m.added_count, m.force_done_at));
    if fa.new_only && joined_at < fa.since {
        return true;
    }
    if let Some(done_at) = done_at {
        match fa.expire_days {
            Some(days) if done_at + i64::from(days) * 86_400 <= now() => {
                soft(bot.repo.set_force_done(g.id, user, None).await, "force add reset");
                added = 0;
            }
            _ => return true,
        }
    }
    let needed = fa.count.max(1) as i32;
    if added >= needed {
        soft(bot.repo.set_force_done(g.id, user, Some(now())).await, "force add done");
        return true;
    }
    if soft(bot.cache.once(&format!("thr:fa:{}:{user}", g.id), 60).await, "throttle") != Some(false) {
        let template = fa.text.clone().unwrap_or_else(|| g.s("fadd.notice"));
        let mut text = crate::members::render(bot, g, user, &template).await;
        for (names, value) in [
            (&["needadd", "تعداد_لازم"][..], needed),
            (&["remainadd", "تعداد_مانده"][..], needed - added),
            (&["useradd", "تعداد_اد"][..], added),
        ] {
            for name in names {
                text = text.replace(name, &fa_digits(&value.to_string()));
            }
        }
        bot.notice(g.id, &text, 30).await;
    }
    false
}

/// Edited messages: the edit locks, and every other lock on the new content.
pub async fn check_edit(bot: &Arc<Bot>, chat_id: i64, message_id: i64) {
    let g = bot.group(chat_id).await;
    let Ok(msg) = bot.client.get_message(chat_id, message_id).await else { return };
    let Some(user) = msg.sender_user_id() else { return };
    if msg.is_outgoing || bot.rank(chat_id, user).await >= Rank::Vip {
        return;
    }
    let media = is_media(&msg.content);
    let edit_lock = (media && g.s.locked(Lock::EditMedia)) || (!media && g.s.locked(Lock::Edit));
    let reason = if edit_lock {
        Some(lock_label(&g, if media { Lock::EditMedia } else { Lock::Edit }))
    } else {
        violation(&g, &msg, now()).map(|lock| lock_label(&g, lock))
    };
    if let Some(reason) = reason {
        bot.delete(chat_id, vec![message_id]).await;
        deletion_notice(bot, &g, user, &reason).await;
    }
}

/// Pin lock: undo pins made automatically for posts of the linked channel.
pub async fn check_pin(bot: &Bot, chat_id: i64, message_id: i64) {
    let g = bot.group(chat_id).await;
    if !g.s.locked(Lock::Pin) {
        return;
    }
    let Ok(msg) = bot.client.get_message(chat_id, message_id).await else { return };
    let from_channel = matches!(msg.forward_info.as_ref().map(|f| &f.origin), Some(MessageOrigin::Channel(_)))
        || msg.sender_id.chat_id().is_some_and(|c| c != chat_id);
    if from_channel {
        let _ = bot.client.unpin_chat_message(chat_id, message_id).await;
    }
}

#[derive(Serialize, Deserialize)]
struct CachedAnswer {
    keyword: String,
    audience: Audience,
    responses: Vec<Content>,
}

/// Auto answers: replies when a keyword appears, with a per-keyword cooldown.
pub async fn auto_answer(bot: &Bot, g: &Group, msg: &Message, user: i64, rank: Rank) {
    if !bot.flag(Flag::AutoAnswers) {
        return;
    }
    let Some(text) = text_of(&msg.content) else { return };
    let normalized = normalize(&text.text);
    if normalized.is_empty() {
        return;
    }
    let key = format!("ans:{}", g.id);
    let answers = match soft(bot.cache.get_json::<Vec<CachedAnswer>>(&key).await, "answer cache").flatten() {
        Some(answers) => answers,
        None => {
            let answers: Vec<CachedAnswer> = soft(bot.repo.answers(g.id).await, "answers")
                .unwrap_or_default()
                .into_iter()
                .map(|a| CachedAnswer { keyword: a.keyword, audience: a.audience, responses: a.responses })
                .collect();
            soft(bot.cache.set_json(&key, &answers, 600).await, "answer cache");
            answers
        }
    };
    for answer in answers {
        if answer.responses.is_empty() || !answer.audience.allows(rank) || !contains_word(&normalized, &answer.keyword) {
            continue;
        }
        let cooldown = u64::from(g.s.answer_cooldown);
        let cooldown_key = format!("cd:{}:{:x}", g.id, text_hash(&answer.keyword));
        if cooldown > 0 && soft(bot.cache.once(&cooldown_key, cooldown).await, "cooldown") == Some(false) {
            return;
        }
        let response = &answer.responses[random(answer.responses.len() as u64) as usize];
        let rendered = match response {
            Content::Text(text) => Content::Text(crate::members::render(bot, g, user, text).await),
            Content::Media { kind, file_id, caption } => Content::Media {
                kind: *kind,
                file_id: file_id.clone(),
                caption: crate::members::render(bot, g, user, caption).await,
            },
        };
        bot.send_input(g.id, Some(msg.id), None, input_content(&rendered, |c| c.to_owned())).await;
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_whole_words() {
        assert!(contains_word("سلام خوبی", "سلام"));
        assert!(!contains_word("سلامتی", "سلام"));
        assert!(contains_word("buy cheap stuff", "cheap stuff"));
        assert!(!contains_word("cheapest", "cheap"));
    }

    #[test]
    fn lock_codes_are_unique() {
        let codes: std::collections::HashSet<_> = crate::locks::LOCKS.iter().map(|l| l.code).collect();
        assert_eq!(codes.len(), crate::locks::LOCKS.len());
    }
}
