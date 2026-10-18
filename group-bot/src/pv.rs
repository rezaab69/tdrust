//! Private chats with the bot: the start message and the private filter panel.

use std::sync::Arc;

use anyhow::Result;
use tdrust::prelude::*;
use tdrust::types::{InlineKeyboardButton, InlineKeyboardButtonTypeUrl, ReplyMarkupInlineKeyboard};

use crate::bot::{soft, Bot};
use crate::commands::{self, Kind, C};
use crate::content::text_of;

/// Commands accepted in private chat, applied to the group chosen with "پنل پیوی".
const PRIVATE_COMMANDS: &[C] =
    &[C::AddFilter, C::FilterWarn, C::FilterBan, C::FilterMute, C::RemFilter, C::FilterList, C::CleanFilters];

pub async fn handle(bot: &Arc<Bot>, msg: &Message) -> Result<()> {
    let Some(user) = msg.sender_user_id() else { return Ok(()) };
    // Broadcast recipients; recorded at most hourly.
    if soft(bot.cache.once(&format!("pvu:{}:{user}", bot.me), 3600).await, "pv user throttle") != Some(false) {
        soft(bot.repo.record_pv_user(user, bot.me).await, "pv user");
    }
    if crate::admin::handle_private(bot, msg, user).await? {
        return Ok(());
    }
    if bot.global().gbans.contains(&user) {
        return Ok(());
    }
    let Some(text) = text_of(&msg.content).map(|t| t.text.clone()) else { return Ok(()) };
    let tx = crate::admin::texts_for(bot, user).await;
    if crate::admin::maintenance(bot).is_some() {
        crate::admin::maintenance_notice(bot, msg.chat_id, &tx).await;
        return Ok(());
    }

    let target: Option<i64> = soft(bot.cache.get_json(&format!("pvt:{user}")).await, "pv target").flatten();
    let target = target.filter(|_| bot.flag(crate::admin::global::Flag::PvPanel));
    if let (Some(group), Some(parsed)) = (target, commands::parse(&text, &bot.username)) {
        if matches!(parsed.kind, Kind::Cmd(cmd) if PRIVATE_COMMANDS.contains(&cmd)) {
            let rank = bot.rank(group, user).await;
            let g = bot.group(group).await;
            crate::exec::run(bot, msg, g, user, rank, parsed).await?;
            return Ok(());
        }
    }

    let button = InlineKeyboardButton {
        text: tx.s("pv.add_to_group"),
        icon_custom_emoji_id: 0,
        style: crate::panels::style(crate::panels::Tone::Good),
        r#type: InlineKeyboardButtonTypeUrl { url: format!("https://t.me/{}?startgroup=true", bot.username) }.into(),
    };
    let markup = ReplyMarkupInlineKeyboard { rows: vec![vec![button]], ..Default::default() }.into();
    let key = if target.is_some() { "pv.panel_help" } else { "pv.start" };
    bot.send_markup(msg.chat_id, &tx.s(key), markup, None).await;
    Ok(())
}
