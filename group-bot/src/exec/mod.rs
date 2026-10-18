//! Command execution: context, permissions, target resolution and dispatch.

mod fun;
mod moderation;
mod settings;

pub use moderation::{clean_group, CleanKind};
pub use settings::{lock_name as lock_label, punish_label};

use std::sync::Arc;

use anyhow::Result;
use tdrust::prelude::*;
use tdrust::types::{ChatType, FormattedText, MessageReplyTo, TextEntity, TextEntityType};

use crate::admin::global::Flag;
use crate::admin::metrics;
use crate::bot::{Bot, Group};
use crate::commands::{Kind, Parsed, C};
use crate::content::{entities_to_html, text_of, to_content};
use crate::locks::Lock;
use crate::model::{Content, Rank};

/// Everything a command handler needs.
pub struct Ctx<'a> {
    pub bot: &'a Arc<Bot>,
    /// The message that carried the command (possibly in a private chat).
    pub msg: &'a Message,
    /// The group the command applies to.
    pub g: Group,
    pub sender: i64,
    pub rank: Rank,
    pub p: Parsed,
}

/// A resolved target user and the arguments left after it.
pub struct Target {
    pub user: i64,
    pub args: Vec<String>,
    pub raw: String,
}

impl Ctx<'_> {
    pub fn t(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.g.t(key, args)
    }

    pub async fn say(&self, html: &str) {
        self.bot.reply(self.msg, html).await;
    }

    pub async fn say_t(&self, key: &str, args: &[(&str, &str)]) {
        self.say(&self.t(key, args)).await;
    }

    /// Replies and records a settings change in the audit log / log channel.
    pub async fn changed(&self, key: &str, args: &[(&str, &str)]) {
        let report = self.t(key, args);
        self.say(&report).await;
        self.bot.log_event(&self.g, self.sender, None, "settings", &report).await;
    }

    /// The message this command replies to, if any.
    pub async fn replied(&self) -> Option<Message> {
        match self.msg.reply_to.as_ref()? {
            MessageReplyTo::Message(reply) if reply.chat_id == self.msg.chat_id || reply.chat_id == 0 => {
                self.bot.client.get_message(self.msg.chat_id, reply.message_id).await.ok()
            }
            _ => None,
        }
    }

    /// The user a command is about: the replied-to sender, a text mention,
    /// an @username or a numeric id as the first argument.
    pub async fn target(&self) -> Option<Target> {
        let args = self.p.args.clone();
        if let Some(replied) = self.replied().await {
            if let Some(user) = replied.sender_user_id() {
                return Some(Target { user, args, raw: self.p.raw.clone() });
            }
        }
        if let Some(text) = text_of(&self.msg.content) {
            for entity in &text.entities {
                if let TextEntityType::MentionName(mention) = &entity.r#type {
                    let rest = args.iter().skip_while(|a| !a.chars().all(|c| c.is_ascii_digit())).cloned().collect();
                    return Some(Target { user: mention.user_id, args: rest, raw: String::new() });
                }
            }
        }
        let first = args.first()?;
        let rest_raw = self.p.raw.split_once(char::is_whitespace).map(|(_, r)| r.trim().to_owned()).unwrap_or_default();
        let rest = args[1..].to_vec();
        if let Some(username) = first.strip_prefix('@') {
            if let Ok(Some(user)) = self.bot.repo.user_by_username(username).await {
                return Some(Target { user, args: rest, raw: rest_raw });
            }
            if let Ok(chat) = self.bot.client.search_public_chat(username).await {
                if let ChatType::Private(private) = chat.r#type {
                    return Some(Target { user: private.user_id, args: rest, raw: rest_raw });
                }
            }
            return None;
        }
        if first.len() >= 5 && first.chars().all(|c| c.is_ascii_digit()) {
            let user = first.parse().ok()?;
            return Some(Target { user, args: rest, raw: rest_raw });
        }
        None
    }

    /// Like `target`, but explains what is missing when the command clearly needed one.
    pub async fn need_target(&self) -> Option<Target> {
        let target = self.target().await;
        if target.is_none() && (self.p.args.is_empty() || self.msg.reply_to.is_some()) {
            self.say_t("err.need_target", &[]).await;
        }
        target
    }

    /// Refuses to act on the bot, sudo users, or anyone of equal or higher rank.
    pub async fn can_act_on(&self, user: i64) -> bool {
        if self.bot.is_fleet(user).await {
            self.say_t("err.target_is_bot", &[]).await;
            return false;
        }
        let rank = self.bot.rank(self.g.id, user).await;
        if rank >= self.rank && self.rank != Rank::Sudo {
            self.say_t("err.higher_rank", &[]).await;
            return false;
        }
        true
    }

    /// The arguments as HTML, keeping the formatting the sender used.
    pub fn rich_args(&self) -> String {
        let Some(text) = text_of(&self.msg.content) else { return crate::text::h(&self.p.raw) };
        let offset = self.p.raw_offset.min(text.text.len());
        let prefix = text.text[..offset].encode_utf16().count() as i32;
        let rest = text.text[offset..].trim_end().to_owned();
        let end = prefix + rest.encode_utf16().count() as i32;
        let entities = text
            .entities
            .iter()
            .filter_map(|e| {
                let (start, stop) = (e.offset.max(prefix), (e.offset + e.length).min(end));
                (stop > start).then(|| TextEntity { offset: start - prefix, length: stop - start, r#type: e.r#type.clone() })
            })
            .collect();
        entities_to_html(&FormattedText { text: rest, entities })
    }

    /// Content given as arguments, or else the replied-to message.
    pub async fn content_arg(&self) -> Option<Content> {
        if !self.p.raw.is_empty() {
            return Some(Content::Text(self.rich_args()));
        }
        self.replied().await.as_ref().and_then(to_content)
    }

    pub fn number(&self, index: usize) -> Option<u32> {
        self.p.arg(index)?.parse().ok()
    }

    pub async fn mention(&self, user: i64) -> String {
        self.bot.mention(self.g.id, user).await
    }
}

pub fn on_off(word: Option<&str>) -> Option<bool> {
    match word? {
        "روشن" | "فعال" | "on" | "enable" | "yes" => Some(true),
        "خاموش" | "غیرفعال" | "off" | "disable" | "no" => Some(false),
        _ => None,
    }
}

/// Runs a parsed command. Returns false when the message should be treated
/// as ordinary chat (not a command for this sender).
pub async fn run(bot: &Arc<Bot>, msg: &Message, g: Group, sender: i64, rank: Rank, p: Parsed) -> Result<bool> {
    if rank < p.rank {
        if rank >= Rank::Mod {
            bot.reply(msg, &g.t("err.no_permission", &[("rank", &g.s(p.rank.key()))])).await;
            return Ok(true);
        }
        return Ok(false);
    }
    if p.fun && ((rank < Rank::Vip && g.s.locked(Lock::Cmd)) || !bot.flag(Flag::Fun)) {
        return Ok(false);
    }
    let ctx = Ctx { bot, msg, g, sender, rank, p };
    match ctx.p.kind {
        Kind::Lock(lock, on) => {
            metrics::command("Lock");
            settings::lock(&ctx, lock, on).await?;
        }
        Kind::Cmd(cmd) => {
            metrics::command(&format!("{cmd:?}"));
            if !(settings::handle(&ctx, cmd).await? || moderation::handle(&ctx, cmd).await? || fun::handle(&ctx, cmd).await?) {
                log::error!("command {cmd:?} has no handler");
            }
            if matches!(cmd, C::SetAnswer | C::RemAnswer | C::CleanAnswers | C::AnswerAudience) {
                crate::bot::soft(bot.cache.del(&format!("ans:{}", ctx.g.id)).await, "answer cache");
            }
        }
    }
    Ok(true)
}
