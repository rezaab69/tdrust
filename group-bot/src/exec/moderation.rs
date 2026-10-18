//! Moderation commands: ranks, sanctions, warnings, filters, cleaning,
//! reports, pins, links, rules and the group photo.

use std::time::Duration;

use anyhow::Result;
use tdrust::types::{
    ChatAdministratorRights, ChatMember, ChatMemberStatusAdministrator, ChatMemberStatusMember, InputChatPhotoStatic,
    InputFileLocal, MessageContent, MessageSenderUser, SupergroupMembersFilter, SupergroupMembersFilterBanned,
    SupergroupMembersFilterBots, SupergroupMembersFilterRecent, SupergroupMembersFilterRestricted, UserStatus, UserType,
};

use super::{Ctx, Target};
use crate::bot::{soft, Bot};
use crate::commands::C;
use crate::model::{Punish, Rank, ReportMode};
use crate::panels;
use crate::text::{fa_digits, format_duration, h, jalali_short, normalize, now, parse_duration};

fn rank_of_cmd(cmd: C) -> Rank {
    match cmd {
        C::SetOwner | C::RemOwner | C::OwnerList | C::CleanOwners => Rank::Owner,
        C::SetTempMod | C::Promote | C::Demote | C::ModList | C::CleanMods => Rank::Mod,
        C::SetTempVip | C::SetVip | C::RemVip | C::VipList | C::CleanVips => Rank::Vip,
        _ => Rank::Exempt,
    }
}

fn duration_of(target: &Target) -> Option<i64> {
    let words: Vec<&str> = target.args.iter().map(String::as_str).collect();
    parse_duration(&words).map(|(seconds, _)| seconds)
}

pub async fn handle(ctx: &Ctx<'_>, cmd: C) -> Result<bool> {
    let chat = ctx.g.id;
    let bot = ctx.bot;
    match cmd {
        // ------------------------------------------------ global bans (sudo)
        C::GBan | C::UnGBan => {
            let Some(target) = ctx.need_target().await else { return Ok(true) };
            let report = if cmd == C::GBan {
                crate::admin::gban(bot, ctx.sender, target.user, &target.raw).await?
            } else {
                crate::admin::ungban(bot, ctx.sender, target.user).await?
            };
            ctx.say(&report.render(&ctx.g.tx)).await;
            if cmd == C::GBan {
                crate::admin::enforce_gban(bot, &ctx.g, target.user, None).await;
            }
        }
        // -------------------------------------------------------------- ranks
        C::SetOwner | C::Promote | C::SetTempMod | C::SetVip | C::SetTempVip | C::Exempt => {
            let Some(target) = ctx.need_target().await else { return Ok(true) };
            if !ctx.can_act_on(target.user).await {
                return Ok(true);
            }
            let temp = matches!(cmd, C::SetTempMod | C::SetTempVip);
            let expires = if temp {
                match duration_of(&target) {
                    Some(seconds) => Some(now() + seconds),
                    None => {
                        ctx.say_t("err.bad_duration", &[]).await;
                        return Ok(true);
                    }
                }
            } else {
                None
            };
            let rank = rank_of_cmd(cmd);
            bot.repo.set_rank(chat, target.user, rank, expires, ctx.sender).await?;
            bot.invalidate_ranks(chat).await;
            let user = ctx.mention(target.user).await;
            let rank_name = ctx.g.s(rank.key());
            let report = match expires {
                Some(until) => ctx.t(
                    "user.promoted_for",
                    &[("user", &user), ("rank", &rank_name), ("duration", &format_duration(until - now()))],
                ),
                None => ctx.t("user.promoted", &[("user", &user), ("rank", &rank_name)]),
            };
            ctx.say(&report).await;
            bot.log_event(&ctx.g, ctx.sender, Some(target.user), "promote", &report).await;
        }
        C::RemOwner | C::Demote | C::RemVip | C::RemExempt => {
            let Some(target) = ctx.need_target().await else { return Ok(true) };
            let rank = rank_of_cmd(cmd);
            let user = ctx.mention(target.user).await;
            if bot.repo.remove_rank(chat, target.user, rank).await? {
                bot.invalidate_ranks(chat).await;
                let report = ctx.t("user.demoted", &[("user", &user), ("rank", &ctx.g.s(rank.key()))]);
                ctx.say(&report).await;
                bot.log_event(&ctx.g, ctx.sender, Some(target.user), "demote", &report).await;
            } else {
                ctx.say_t("user.not_rank", &[("user", &user), ("rank", &ctx.g.s(rank.key()))]).await;
            }
        }
        C::OwnerList | C::ModList | C::VipList | C::ExemptList => {
            let rank = rank_of_cmd(cmd);
            let ranks = bot.repo.ranks(chat).await?;
            let mut lines = Vec::new();
            for (user, _, expires) in ranks.into_iter().filter(|(_, r, _)| *r == rank) {
                let until = expires
                    .map(|e| format!(" ({})", ctx.t("common.until", &[("date", &jalali_short(e, ctx.g.s.tz))])))
                    .unwrap_or_default();
                lines.push(format!("• {}{until}", ctx.mention(user).await));
            }
            send_list(ctx, &ctx.g.s(rank.key()), lines).await;
        }
        C::CleanOwners | C::CleanMods | C::CleanVips | C::CleanExempt => {
            let n = bot.repo.clear_rank(chat, rank_of_cmd(cmd)).await?;
            bot.invalidate_ranks(chat).await;
            ctx.changed("list.cleared", &[("n", &fa_digits(&n.to_string()))]).await;
        }
        C::AddAdmin | C::RemAdmin => {
            let Some(target) = ctx.need_target().await else { return Ok(true) };
            if !ctx.can_act_on(target.user).await {
                return Ok(true);
            }
            let result = if cmd == C::AddAdmin {
                let rights = ChatAdministratorRights {
                    can_manage_chat: true,
                    can_delete_messages: true,
                    can_invite_users: true,
                    can_restrict_members: true,
                    can_pin_messages: true,
                    can_manage_video_chats: true,
                    ..Default::default()
                };
                let status = ChatMemberStatusAdministrator { can_be_edited: true, rights };
                bot.client.set_chat_member_status(chat, MessageSenderUser { user_id: target.user }, status).await
            } else {
                let status = ChatMemberStatusMember { member_until_date: 0 };
                bot.client.set_chat_member_status(chat, MessageSenderUser { user_id: target.user }, status).await
            };
            bot.invalidate_admins(chat).await;
            let user = ctx.mention(target.user).await;
            match result {
                Ok(_) => {
                    let key = if cmd == C::AddAdmin { "user.admin_added" } else { "user.admin_removed" };
                    let report = ctx.t(key, &[("user", &user)]);
                    ctx.say(&report).await;
                    bot.log_event(&ctx.g, ctx.sender, Some(target.user), "admin", &report).await;
                }
                Err(err) => ctx.say_t("err.action_failed", &[("error", &h(&err.to_string()))]).await,
            }
        }
        C::AdminList => {
            let admins = bot.tg_admins(chat).await;
            let mut lines = Vec::new();
            for (user, creator) in admins {
                let badge = if creator { format!(" — {}", ctx.g.s("rank.creator")) } else { String::new() };
                lines.push(format!("• {}{badge}", ctx.mention(user).await));
            }
            send_list(ctx, &ctx.g.s("list.admins"), lines).await;
        }

        // ---------------------------------------------------------- sanctions
        C::Ban | C::Kick | C::Warn | C::Mute => {
            let Some(target) = ctx.need_target().await else { return Ok(true) };
            if !ctx.can_act_on(target.user).await {
                return Ok(true);
            }
            let reason = if cmd == C::Mute { String::new() } else { h(&target.raw) };
            let report = match cmd {
                C::Ban => bot.apply_hard(&ctx.g, target.user, Punish::Ban, ctx.sender, &reason).await,
                C::Warn => bot.warn(&ctx.g, target.user, ctx.sender, &reason).await,
                C::Mute => {
                    let punish = duration_of(&target).map_or(Punish::Mute, Punish::TempMute);
                    bot.apply_hard(&ctx.g, target.user, punish, ctx.sender, "").await
                }
                _ => match bot.kick(chat, target.user).await {
                    Ok(()) => {
                        let report = ctx.t("user.kicked", &[("user", &ctx.mention(target.user).await)]);
                        bot.log_event(&ctx.g, ctx.sender, Some(target.user), "kick", &report).await;
                        bot.record_moderation(&ctx.g, ctx.sender).await;
                        report
                    }
                    Err(err) => ctx.t("err.action_failed", &[("error", &h(&err.to_string()))]),
                },
            };
            ctx.say(&report).await;
        }
        C::BanPlus => {
            let Some(replied) = ctx.replied().await else {
                ctx.say_t("err.need_reply", &[]).await;
                return Ok(true);
            };
            let Some(user) = replied.sender_user_id() else { return Ok(true) };
            if !ctx.can_act_on(user).await {
                return Ok(true);
            }
            // Silent: the report only goes to the audit log and log channel.
            bot.apply_hard(&ctx.g, user, Punish::Ban, ctx.sender, "").await;
            bot.delete(chat, vec![replied.id, ctx.msg.id]).await;
        }
        C::UnBan | C::UnKick | C::UnMute => {
            let Some(target) = ctx.need_target().await else { return Ok(true) };
            let result = if cmd == C::UnMute { bot.unrestrict(chat, target.user).await } else { bot.unban(chat, target.user).await };
            if let Err(err) = result {
                ctx.say_t("err.action_failed", &[("error", &h(&err.to_string()))]).await;
                return Ok(true);
            }
            let kind = if cmd == C::UnMute { "mute" } else { "ban" };
            soft(bot.repo.remove_sanction(chat, target.user, kind).await, "remove sanction");
            let key = match cmd {
                C::UnBan => "user.unbanned",
                C::UnKick => "user.unkicked",
                _ => "user.unmuted",
            };
            let report = ctx.t(key, &[("user", &ctx.mention(target.user).await)]);
            ctx.say(&report).await;
            bot.log_event(&ctx.g, ctx.sender, Some(target.user), "unsanction", &report).await;
        }
        C::BanList | C::MuteList => {
            let kind = if cmd == C::BanList { "ban" } else { "mute" };
            let mut lines = Vec::new();
            for (user, until) in bot.repo.sanctions(chat, kind).await? {
                let until = until
                    .map(|u| format!(" ({})", ctx.t("common.until", &[("date", &jalali_short(u, ctx.g.s.tz))])))
                    .unwrap_or_default();
                lines.push(format!("• {}{until}", ctx.mention(user).await));
            }
            let title = ctx.g.s(if cmd == C::BanList { "list.banned" } else { "list.muted" });
            send_list(ctx, &title, lines).await;
        }
        C::CleanBans | C::CleanMutes => {
            let ban = cmd == C::CleanBans;
            let users = bot.repo.clear_sanctions(chat, if ban { "ban" } else { "mute" }).await?;
            for user in &users {
                let _ = if ban { bot.unban(chat, *user).await } else { bot.unrestrict(chat, *user).await };
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            ctx.changed("list.cleared", &[("n", &fa_digits(&users.len().to_string()))]).await;
        }

        // ------------------------------------------------------------- warns
        C::RemWarn | C::RemWarns => {
            let Some(target) = ctx.need_target().await else { return Ok(true) };
            let user = ctx.mention(target.user).await;
            if cmd == C::RemWarn {
                let count = bot.repo.change_warns(chat, target.user, -1).await?;
                ctx.say_t("warn.removed", &[("user", &user), ("count", &fa_digits(&count.to_string()))]).await;
            } else {
                bot.repo.reset_warns(chat, target.user).await?;
                ctx.say_t("warn.cleared_user", &[("user", &user)]).await;
            }
        }
        C::WarnList => {
            let max = fa_digits(&ctx.g.s.warns.max.to_string());
            let mut lines = Vec::new();
            for (user, count) in bot.repo.warns(chat).await? {
                lines.push(format!("• {} — {}/{max}", ctx.mention(user).await, fa_digits(&count.to_string())));
            }
            send_list(ctx, &ctx.g.s("list.warned"), lines).await;
        }
        C::CleanWarns => {
            let n = bot.repo.clear_warns(chat).await?;
            ctx.changed("list.cleared", &[("n", &fa_digits(&n.to_string()))]).await;
        }
        C::UserPanel => {
            let Some(target) = ctx.need_target().await else { return Ok(true) };
            let (text, markup) = panels::user_panel(bot, &ctx.g, target.user).await;
            bot.send_markup(ctx.msg.chat_id, &text, markup, Some(ctx.msg.id)).await;
        }

        // ------------------------------------------------------------ filters
        C::AddFilter | C::FilterWarn | C::FilterBan | C::FilterMute => {
            let word = normalize(&ctx.p.raw);
            let punish = match cmd {
                C::FilterWarn => Punish::Warn,
                C::FilterBan => Punish::Ban,
                C::FilterMute => Punish::Mute,
                _ => Punish::Delete,
            };
            bot.repo.add_filter(chat, &word, punish).await?;
            soft(bot.cache.del(&format!("flt:{chat}")).await, "filter cache");
            ctx.changed("filter.added", &[("word", &h(&word)), ("mode", &super::settings::punish_label(&ctx.g, punish))]).await;
        }
        C::RemFilter => {
            let word = normalize(&ctx.p.raw);
            if bot.repo.remove_filter(chat, &word).await? {
                soft(bot.cache.del(&format!("flt:{chat}")).await, "filter cache");
                ctx.changed("filter.removed", &[("word", &h(&word))]).await;
            } else {
                ctx.say_t("filter.not_found", &[("word", &h(&word))]).await;
            }
        }
        C::FilterList => {
            let lines: Vec<String> = bot
                .repo
                .filters(chat)
                .await?
                .into_iter()
                .map(|(word, punish)| format!("• <code>{}</code> — {}", h(&word), super::settings::punish_label(&ctx.g, punish)))
                .collect();
            send_list(ctx, &ctx.g.s("list.filters"), lines).await;
        }
        C::CleanFilters => {
            let n = bot.repo.clear_filters(chat).await?;
            soft(bot.cache.del(&format!("flt:{chat}")).await, "filter cache");
            ctx.changed("list.cleared", &[("n", &fa_digits(&n.to_string()))]).await;
        }
        C::FilterStart => {
            bot.cache.set_json(&format!("fsess:{chat}:{}", ctx.sender), &true, 3600).await?;
            ctx.say_t("filter.session_start", &[]).await;
        }
        C::FilterEnd => {
            bot.cache.del(&format!("fsess:{chat}:{}", ctx.sender)).await?;
            ctx.say_t("filter.session_end", &[]).await;
        }
        C::PanelPv => {
            bot.cache.set_json(&format!("pvt:{}", ctx.sender), &chat, 3600).await?;
            let title = bot.client.get_chat(chat).await.map(|c| c.title).unwrap_or_default();
            if bot.send(ctx.sender, &ctx.t("pv.panel", &[("group", &h(&title))])).await.is_some() {
                ctx.say_t("pv.panel_sent", &[]).await;
            } else {
                ctx.say_t("pv.start_first", &[("bot", &bot.username)]).await;
            }
        }

        // ----------------------------------------------------------- cleaning
        C::Del => match ctx.number(0).filter(|n| *n > 0) {
            Some(n) => {
                let mut ids = bot.cache.take_recent(chat, Some(n.min(1000) as isize)).await?;
                ids.push(ctx.msg.id);
                let deleted = bot.delete(chat, ids).await.saturating_sub(1);
                bot.notice(chat, &ctx.t("clean.deleted", &[("n", &fa_digits(&deleted.to_string()))]), 5).await;
            }
            None => ctx.say_t("err.need_number", &[]).await,
        },
        C::CleanNicknames => {
            let n = bot.repo.clear_nicknames(chat).await?;
            soft(bot.cache.del(&format!("nick:{chat}")).await, "nick cache");
            ctx.changed("list.cleared", &[("n", &fa_digits(&n.to_string()))]).await;
        }
        C::CleanBlockList | C::CleanRestrictList | C::CleanBots | C::CleanFake | C::CleanDeleted => clean_members(ctx, cmd).await?,

        // ------------------------------------------------------------ reports
        C::Report => report(ctx).await?,

        // ---------------------------------------------------- pins and links
        C::Pin => {
            let Some(replied) = ctx.replied().await else {
                ctx.say_t("err.need_reply", &[]).await;
                return Ok(true);
            };
            if let Err(err) = bot.client.pin_chat_message(chat, replied.id, false, false).await {
                ctx.say_t("err.action_failed", &[("error", &h(&err.to_string()))]).await;
                return Ok(true);
            }
            let words: Vec<&str> = ctx.p.args.iter().map(String::as_str).collect();
            match parse_duration(&words) {
                Some((seconds, _)) => {
                    bot.repo.add_timed_pin(chat, replied.id, now() + seconds).await?;
                    ctx.say_t("pin.done_for", &[("duration", &format_duration(seconds))]).await;
                }
                None => ctx.say_t("pin.done", &[]).await,
            }
        }
        C::Unpin => {
            let message_id = match ctx.replied().await {
                Some(replied) => Some(replied.id),
                None => bot.client.get_chat_pinned_message(chat).await.ok().map(|m| m.id),
            };
            match message_id {
                Some(id) => match bot.client.unpin_chat_message(chat, id).await {
                    Ok(_) => ctx.say_t("pin.removed", &[]).await,
                    Err(err) => ctx.say_t("err.action_failed", &[("error", &h(&err.to_string()))]).await,
                },
                None => ctx.say_t("pin.none", &[]).await,
            }
        }
        C::Link => {
            let link = match ctx.g.s.link.clone() {
                Some(link) => Some(link),
                None => public_link(ctx).await,
            };
            match link {
                Some(link) => ctx.say_t("link.show", &[("link", &h(&link))]).await,
                None => ctx.say_t("link.none", &[]).await,
            }
        }
        C::SetLink => {
            let link = ctx.p.raw.trim().to_owned();
            if !link.starts_with("http") && !link.starts_with("t.me/") {
                ctx.say_t("link.invalid", &[]).await;
                return Ok(true);
            }
            bot.update(chat, move |s| s.link = Some(link)).await?;
            ctx.changed("link.set", &[]).await;
        }
        C::DelLink => {
            bot.update(chat, |s| s.link = None).await?;
            ctx.changed("link.removed", &[]).await;
        }
        C::CreateLink => match bot.client.replace_primary_chat_invite_link(chat).await {
            Ok(invite) => {
                let link = invite.invite_link.clone();
                bot.update(chat, move |s| s.link = Some(link)).await?;
                ctx.changed("link.created", &[("link", &h(&invite.invite_link))]).await;
            }
            Err(err) => ctx.say_t("err.action_failed", &[("error", &h(&err.to_string()))]).await,
        },
        C::Rules => match &ctx.g.s.rules {
            Some(rules) => ctx.say_t("rules.show", &[("rules", rules)]).await,
            None => ctx.say_t("rules.none", &[]).await,
        },
        C::SetRules => match ctx.content_arg().await {
            Some(crate::model::Content::Text(rules)) => {
                bot.update(chat, move |s| s.rules = Some(rules)).await?;
                ctx.changed("rules.set", &[]).await;
            }
            _ => ctx.say_t("err.need_text", &[]).await,
        },
        C::DelRules => {
            bot.update(chat, |s| s.rules = None).await?;
            ctx.changed("rules.removed", &[]).await;
        }
        C::SetPhoto => {
            let photo = ctx.replied().await.and_then(|m| match m.content {
                MessageContent::Photo(p) => p.photo.sizes.last().map(|s| s.photo.id),
                _ => None,
            });
            let Some(file_id) = photo else {
                ctx.say_t("err.need_photo_reply", &[]).await;
                return Ok(true);
            };
            let result = async {
                let file = bot.client.download_file(file_id, 16, 0, 0, true).await?;
                let photo = InputChatPhotoStatic { photo: InputFileLocal { path: file.local.path }.into() };
                bot.client.set_chat_photo(chat, Some(photo.into())).await
            }
            .await;
            match result {
                Ok(_) => ctx.changed("photo.set", &[]).await,
                Err(err) => ctx.say_t("err.action_failed", &[("error", &h(&err.to_string()))]).await,
            }
        }
        C::Tag => tag_members(ctx).await?,
        _ => return Ok(false),
    }
    Ok(true)
}

/// Sends a titled list, split over several messages when long.
pub async fn send_list(ctx: &Ctx<'_>, title: &str, lines: Vec<String>) {
    if lines.is_empty() {
        ctx.say_t("list.empty", &[("title", title)]).await;
        return;
    }
    let count = fa_digits(&lines.len().to_string());
    let mut text = ctx.t("list.header", &[("title", title), ("n", &count)]);
    for line in lines {
        if text.len() + line.len() > 3800 {
            ctx.say(&text).await;
            text.clear();
        }
        text.push('\n');
        text.push_str(&line);
    }
    ctx.say(&text).await;
}

async fn public_link(ctx: &Ctx<'_>) -> Option<String> {
    let supergroup = ctx.bot.supergroup_id(ctx.g.id).await?;
    let username = ctx.bot.client.get_supergroup(supergroup).await.ok()?.usernames?.editable_username;
    (!username.is_empty()).then(|| format!("https://t.me/{username}"))
}

/// All members of a supergroup matching a filter (up to 10,000).
async fn members(bot: &Bot, supergroup_id: i64, filter: SupergroupMembersFilter) -> Vec<ChatMember> {
    let mut all = Vec::new();
    let mut offset = 0;
    while offset < 10_000 {
        match bot.client.get_supergroup_members(supergroup_id, Some(filter.clone()), offset, 200).await {
            Ok(page) if !page.members.is_empty() => {
                offset += page.members.len() as i32;
                all.extend(page.members);
            }
            Ok(_) => break,
            Err(err) => {
                log::warn!("cannot list members of {supergroup_id}: {err}");
                break;
            }
        }
    }
    all
}

/// Member clean-ups, shared by the commands and the panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CleanKind {
    /// Unban everyone in Telegram's banned list.
    Blocked,
    /// Lift every restriction in Telegram's restricted list.
    Restricted,
    /// Remove bots that are not administrators.
    Bots,
    /// Remove accounts without photo or username that have not been seen for a long time.
    Fake,
    /// Remove deleted accounts.
    Deleted,
}

impl CleanKind {
    pub const ALL: [CleanKind; 5] = [CleanKind::Bots, CleanKind::Deleted, CleanKind::Fake, CleanKind::Blocked, CleanKind::Restricted];

    pub fn code(self) -> &'static str {
        match self {
            CleanKind::Blocked => "blocked",
            CleanKind::Restricted => "restricted",
            CleanKind::Bots => "bots",
            CleanKind::Fake => "fake",
            CleanKind::Deleted => "deleted",
        }
    }

    pub fn from_code(code: &str) -> Option<CleanKind> {
        CleanKind::ALL.into_iter().find(|kind| kind.code() == code)
    }
}

/// Runs a clean-up and returns how many members were affected, or `None`
/// if the chat is not a supergroup.
pub async fn clean_group(bot: &Bot, chat: i64, kind: CleanKind) -> Option<usize> {
    let supergroup = bot.supergroup_id(chat).await?;
    let filter: SupergroupMembersFilter = match kind {
        CleanKind::Blocked => SupergroupMembersFilterBanned::default().into(),
        CleanKind::Restricted => SupergroupMembersFilterRestricted::default().into(),
        CleanKind::Bots => SupergroupMembersFilterBots::default().into(),
        CleanKind::Fake | CleanKind::Deleted => SupergroupMembersFilterRecent::default().into(),
    };
    let admins = bot.tg_admins(chat).await;
    let mut done = 0usize;
    for member in members(bot, supergroup, filter).await {
        let Some(user_id) = member.member_id.user_id() else { continue };
        if user_id == bot.me || admins.contains_key(&user_id) {
            continue;
        }
        let result = match kind {
            CleanKind::Blocked => Some(bot.unban(chat, user_id).await),
            CleanKind::Restricted => Some(bot.unrestrict(chat, user_id).await),
            CleanKind::Bots => Some(bot.kick(chat, user_id).await),
            CleanKind::Fake | CleanKind::Deleted => {
                let Ok(user) = bot.client.get_user(user_id).await else { continue };
                let deleted = matches!(user.r#type, UserType::Deleted(_));
                let fake = matches!(user.r#type, UserType::Regular(_))
                    && user.usernames.is_none()
                    && user.profile_photo.is_none()
                    && matches!(user.status, UserStatus::Empty(_) | UserStatus::LastMonth(_));
                let selected = if kind == CleanKind::Deleted { deleted } else { fake };
                if selected {
                    Some(bot.kick(chat, user_id).await)
                } else {
                    None
                }
            }
        };
        if matches!(result, Some(Ok(()))) {
            done += 1;
        }
        if result.is_some() {
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
    }
    match kind {
        CleanKind::Blocked => {
            soft(bot.repo.clear_sanctions(chat, "ban").await, "clear bans");
        }
        CleanKind::Restricted => {
            soft(bot.repo.clear_sanctions(chat, "mute").await, "clear mutes");
        }
        _ => {}
    }
    Some(done)
}

async fn clean_members(ctx: &Ctx<'_>, cmd: C) -> Result<()> {
    let kind = match cmd {
        C::CleanBlockList => CleanKind::Blocked,
        C::CleanRestrictList => CleanKind::Restricted,
        C::CleanBots => CleanKind::Bots,
        C::CleanFake => CleanKind::Fake,
        _ => CleanKind::Deleted,
    };
    if ctx.bot.supergroup_id(ctx.g.id).await.is_none() {
        ctx.say_t("err.supergroup_only", &[]).await;
        return Ok(());
    }
    ctx.say_t("clean.working", &[]).await;
    let done = clean_group(ctx.bot, ctx.g.id, kind).await.unwrap_or(0);
    ctx.changed("clean.members_done", &[("n", &fa_digits(&done.to_string()))]).await;
    Ok(())
}

async fn report(ctx: &Ctx<'_>) -> Result<()> {
    let chat = ctx.g.id;
    let Some(replied) = ctx.replied().await else {
        ctx.say_t("err.need_reply", &[]).await;
        return Ok(());
    };
    if !ctx.bot.cache.once(&format!("thr:report:{chat}:{}", ctx.sender), 60).await.unwrap_or(true) {
        ctx.say_t("report.too_often", &[]).await;
        return Ok(());
    }
    let reporter = ctx.mention(ctx.sender).await;
    let offender = match replied.sender_user_id() {
        Some(user) => ctx.mention(user).await,
        None => ctx.g.s("common.unknown"),
    };
    let mut admins: Vec<i64> = ctx.bot.tg_admins(chat).await.into_keys().collect();
    for (user, rank) in ctx.bot.stored_ranks(chat).await {
        if Rank::from_db(rank) >= Rank::Mod && !admins.contains(&user) {
            admins.push(user);
        }
    }
    admins.retain(|a| *a != ctx.bot.me);
    match ctx.g.s.security.report_mode {
        ReportMode::Group => {
            let mut mentions = Vec::new();
            for admin in admins.iter().take(15) {
                mentions.push(ctx.mention(*admin).await);
            }
            let text = ctx.t("report.group", &[("reporter", &reporter), ("user", &offender), ("admins", &mentions.join(" "))]);
            let content = tdrust::types::InputMessageContent::text(crate::content::html(&text));
            ctx.bot.send_input(chat, Some(replied.id), None, content).await;
        }
        ReportMode::Private => {
            let link = ctx
                .bot
                .client
                .get_message_link(chat, replied.id, 0, 0, "", false, false)
                .await
                .map(|l| l.link)
                .unwrap_or_default();
            let title = ctx.bot.client.get_chat(chat).await.map(|c| c.title).unwrap_or_default();
            let text = ctx.t(
                "report.private",
                &[("reporter", &reporter), ("user", &offender), ("group", &h(&title)), ("link", &h(&link))],
            );
            for admin in admins {
                ctx.bot.send(admin, &text).await;
            }
            ctx.say_t("report.sent", &[]).await;
        }
    }
    Ok(())
}

async fn tag_members(ctx: &Ctx<'_>) -> Result<()> {
    let chat = ctx.g.id;
    let (today, _) = crate::text::local_day_minute(now(), ctx.g.s.tz);
    let users = ctx.bot.repo.top_users(chat, today as i32 - 30, i32::MAX, 200).await?;
    let header = if ctx.p.raw.is_empty() { String::new() } else { format!("{}\n", ctx.rich_args()) };
    let mut batch = Vec::new();
    for (user, _) in users {
        if user == ctx.bot.me {
            continue;
        }
        batch.push(ctx.mention(user).await);
        if batch.len() == 10 {
            ctx.bot.send(chat, &format!("{header}{}", batch.join(" "))).await;
            batch.clear();
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    if !batch.is_empty() {
        ctx.bot.send(chat, &format!("{header}{}", batch.join(" "))).await;
    }
    Ok(())
}
