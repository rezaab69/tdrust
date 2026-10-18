//! Public tools: time, echo, jokes, user info, profile cards, nicknames,
//! profile photos, fonts, and the statistics commands.

use anyhow::Result;
use tdrust::types::{InputFileId, InputMessageContent, InputMessagePhoto, InputPhoto, MessageSenderUser};

use super::Ctx;
use crate::bot::soft;
use crate::commands::C;
use crate::content;
use crate::stats;
use crate::text::{fa_digits, format_tz, h, jalali_date, local_time, now, random};

pub async fn handle(ctx: &Ctx<'_>, cmd: C) -> Result<bool> {
    let chat = ctx.g.id;
    let bot = ctx.bot;
    match cmd {
        C::Time => {
            let t = now();
            ctx.say_t(
                "fun.time",
                &[("time", &local_time(t, ctx.g.s.tz)), ("date", &jalali_date(t, ctx.g.s.tz)), ("tz", &format_tz(ctx.g.s.tz))],
            )
            .await;
        }
        C::Echo => {
            let html = ctx.rich_args();
            let reply_to = ctx.replied().await.map(|m| m.id);
            bot.send_input(ctx.msg.chat_id, reply_to, None, InputMessageContent::text(content::html(&html))).await;
            bot.delete(ctx.msg.chat_id, vec![ctx.msg.id]).await;
        }
        C::Joke => {
            let jokes = ctx.g.s("fun.jokes");
            let jokes: Vec<&str> = jokes.split("\n---\n").filter(|j| !j.trim().is_empty()).collect();
            if let Some(joke) = jokes.get(random(jokes.len() as u64) as usize) {
                ctx.say(joke).await;
            }
        }
        C::Id => {
            let user_id = match ctx.target().await {
                Some(target) => target.user,
                None if ctx.p.args.is_empty() => ctx.sender,
                None => return Ok(true),
            };
            send_info(ctx, user_id).await;
        }
        C::JoinDate => {
            let user_id = ctx.target().await.map_or(ctx.sender, |t| t.user);
            let joined = bot.client.get_chat_member(chat, MessageSenderUser { user_id }).await.map(|m| m.joined_chat_date).unwrap_or(0);
            let user = ctx.mention(user_id).await;
            if joined > 0 {
                ctx.say_t("fun.join_date", &[("user", &user), ("date", &jalali_date(i64::from(joined), ctx.g.s.tz))]).await;
            } else {
                ctx.say_t("fun.join_date_unknown", &[("user", &user)]).await;
            }
        }
        C::SetAsl | C::SetGlobalAsl => {
            let scope = if cmd == C::SetGlobalAsl { 0 } else { chat };
            bot.repo.set_profile(scope, ctx.sender, &ctx.rich_args()).await?;
            ctx.say_t("fun.asl_set", &[]).await;
        }
        C::MyAsl | C::Asl => {
            let user_id = if cmd == C::MyAsl { ctx.sender } else { ctx.target().await.map_or(ctx.sender, |t| t.user) };
            let user = ctx.mention(user_id).await;
            match bot.repo.profile(chat, user_id).await? {
                Some(card) => ctx.say_t("fun.asl_show", &[("user", &user), ("card", &card)]).await,
                None => ctx.say_t("fun.asl_none", &[("user", &user)]).await,
            }
        }
        C::DelAsl => {
            bot.repo.remove_profile(chat, ctx.sender).await?;
            ctx.say_t("fun.asl_removed", &[]).await;
        }
        C::SetNickname => {
            let Some(target) = ctx.need_target().await else { return Ok(true) };
            let nickname = target.raw.trim().to_owned();
            if nickname.is_empty() || nickname.chars().count() > 64 {
                ctx.say_t("err.need_text", &[]).await;
                return Ok(true);
            }
            bot.repo.set_nickname(chat, target.user, &nickname).await?;
            soft(bot.cache.del(&format!("nick:{chat}")).await, "nick cache");
            ctx.say_t("fun.nickname_set", &[("user", &ctx.mention(target.user).await)]).await;
        }
        C::Nickname => {
            let user_id = match ctx.target().await {
                Some(target) => target.user,
                None if ctx.p.args.is_empty() => ctx.sender,
                None => return Ok(true),
            };
            match bot.nicknames(chat).await.get(&user_id) {
                Some(nickname) => ctx.say_t("fun.nickname_show", &[("nickname", &h(nickname))]).await,
                None => ctx.say_t("fun.nickname_none", &[]).await,
            }
        }
        C::DelNickname => {
            let Some(target) = ctx.need_target().await else { return Ok(true) };
            bot.repo.remove_nickname(chat, target.user).await?;
            soft(bot.cache.del(&format!("nick:{chat}")).await, "nick cache");
            ctx.say_t("fun.nickname_removed", &[]).await;
        }
        C::NicknameList => {
            let lines: Vec<String> = bot
                .nicknames(chat)
                .await
                .into_iter()
                .map(|(user, nickname)| format!("• {}", crate::text::mention_html(user, &nickname)))
                .collect();
            super::moderation::send_list(ctx, &ctx.g.s("list.nicknames"), lines).await;
        }
        C::GetPro => {
            let (user_id, index) = match ctx.target().await {
                Some(target) => (target.user, target.args.first().and_then(|a| a.parse::<i32>().ok()).unwrap_or(1)),
                None => (ctx.sender, ctx.number(0).map_or(1, |n| n as i32)),
            };
            let photos = bot.client.get_user_profile_photos(user_id, (index - 1).max(0), 1).await;
            let file = photos.ok().and_then(|p| p.photos.into_iter().next()).and_then(|p| p.sizes.last().map(|s| s.photo.id));
            match file {
                Some(file_id) => {
                    let caption =
                        ctx.t("fun.profile_photo", &[("user", &ctx.mention(user_id).await), ("n", &fa_digits(&index.to_string()))]);
                    send_photo(ctx, file_id, &caption).await;
                }
                None => ctx.say_t("fun.no_profile_photo", &[]).await,
            }
        }
        C::Font => {
            let text = ctx.p.raw.trim().to_owned();
            if text.chars().count() > 50 {
                ctx.say_t("fun.font_too_long", &[]).await;
                return Ok(true);
            }
            let lines: Vec<String> = fonts(&text).into_iter().map(|styled| format!("<code>{}</code>", h(&styled))).collect();
            ctx.say(&lines.join("\n")).await;
        }

        C::Stats => {
            let top = ctx.number(0).map_or(10, |n| n.clamp(1, 100));
            ctx.say(&stats::summary(bot, &ctx.g, i64::from(top)).await?).await;
        }
        C::AddStats => ctx.say(&stats::inviters(bot, &ctx.g).await?).await,
        C::RankStats => ctx.say(&stats::ranked(bot, &ctx.g).await?).await,
        C::TotalStats => ctx.say(&stats::total(bot, &ctx.g).await?).await,
        C::WeeklyStats => ctx.say(&stats::weekly(bot, &ctx.g, None).await?).await,
        C::UserStats => {
            let user_id = ctx.target().await.map_or(ctx.sender, |t| t.user);
            ctx.say(&stats::weekly(bot, &ctx.g, Some(user_id)).await?).await;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

async fn send_photo(ctx: &Ctx<'_>, file_id: i32, caption: &str) {
    let photo = InputMessagePhoto {
        photo: InputPhoto {
            photo: InputFileId { id: file_id }.into(),
            thumbnail: None,
            video: None,
            added_sticker_file_ids: vec![],
            width: 0,
            height: 0,
        },
        caption: Some(content::html(caption)),
        show_caption_above_media: false,
        self_destruct_type: None,
        has_spoiler: false,
    };
    if ctx.bot.send_input(ctx.msg.chat_id, Some(ctx.msg.id), None, photo).await.is_none() {
        ctx.say(caption).await;
    }
}

async fn send_info(ctx: &Ctx<'_>, user_id: i64) {
    let bot = ctx.bot;
    let Ok(user) = bot.client.get_user(user_id).await else {
        ctx.say_t("err.user_not_found", &[]).await;
        return;
    };
    let username = user
        .usernames
        .as_ref()
        .map(|u| u.editable_username.clone())
        .filter(|u| !u.is_empty())
        .map(|u| format!("@{u}"))
        .unwrap_or_else(|| ctx.g.s("common.none"));
    let rank = bot.rank(ctx.g.id, user_id).await;
    let warns = bot.repo.warn_count(ctx.g.id, user_id).await.unwrap_or(0);
    let messages = bot.repo.total_messages(ctx.g.id, Some(user_id)).await.unwrap_or(0);
    let name = format!("{} {}", user.first_name, user.last_name).trim().to_owned();
    let text = ctx.t(
        "fun.info",
        &[
            ("name", &crate::text::mention_html(user_id, &name)),
            ("id", &user_id.to_string()),
            ("username", &h(&username)),
            ("rank", &ctx.g.s(rank.key())),
            ("warns", &fa_digits(&format!("{warns}/{}", ctx.g.s.warns.max))),
            ("messages", &fa_digits(&messages.to_string())),
            ("chat_id", &ctx.g.id.to_string()),
            ("premium", &ctx.g.s(if user.is_premium { "common.yes" } else { "common.no" })),
        ],
    );
    match user.profile_photo.map(|p| p.big.id) {
        Some(file_id) => send_photo(ctx, file_id, &text).await,
        None => ctx.say(&text).await,
    }
}

/// Mathematical alphanumeric styles: (uppercase A, lowercase a, digit 0).
const LATIN_STYLES: &[(u32, u32, Option<u32>)] = &[
    (0x1D400, 0x1D41A, Some(0x1D7CE)), // bold
    (0x1D468, 0x1D482, None),          // bold italic
    (0x1D4D0, 0x1D4EA, None),          // bold script
    (0x1D56C, 0x1D586, None),          // bold fraktur
    (0x1D5D4, 0x1D5EE, Some(0x1D7EC)), // sans bold
    (0x1D670, 0x1D68A, Some(0x1D7F6)), // monospace
    (0xFF21, 0xFF41, Some(0xFF10)),    // full width
    (0x24B6, 0x24D0, None),            // circled
];

const SMALL_CAPS: &str = "ᴀʙᴄᴅᴇғɢʜɪᴊᴋʟᴍɴᴏᴘǫʀsᴛᴜᴠᴡxʏᴢ";

fn latin(text: &str, (upper, lower, digit): (u32, u32, Option<u32>)) -> String {
    text.chars()
        .map(|c| {
            let code = match c {
                'A'..='Z' => upper + (c as u32 - 'A' as u32),
                'a'..='z' => lower + (c as u32 - 'a' as u32),
                '0'..='9' => match digit {
                    Some(d) => d + (c as u32 - '0' as u32),
                    None => c as u32,
                },
                _ => c as u32,
            };
            char::from_u32(code).unwrap_or(c)
        })
        .collect()
}

/// Letters that connect to the next one, where a kashida (ـ) can stretch the word.
fn joins_forward(c: char) -> bool {
    crate::text::is_persian_letter(c) && !"اآأإدذرزژوؤة".contains(c)
}

/// Styled variants of a text.
pub fn fonts(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    if crate::text::has_english(text) {
        out.extend(LATIN_STYLES.iter().map(|style| latin(text, *style)));
        let small: Vec<char> = SMALL_CAPS.chars().collect();
        out.push(
            text.chars()
                .map(|c| if c.is_ascii_alphabetic() { small[(c.to_ascii_lowercase() as u8 - b'a') as usize] } else { c })
                .collect(),
        );
    }
    if crate::text::has_persian(text) {
        let chars: Vec<char> = text.chars().collect();
        let stretched = |marker: &str| -> String {
            let mut s = String::new();
            for (i, c) in chars.iter().enumerate() {
                s.push(*c);
                if joins_forward(*c) && chars.get(i + 1).is_some_and(|n| crate::text::is_persian_letter(*n)) {
                    s.push_str(marker);
                }
            }
            s
        };
        out.push(stretched("ـ"));
        out.push(stretched("ـــ"));
        out.push(format!("꧁ {text} ꧂"));
        out.push(format!("✿ {text} ✿"));
        out.push(format!("『{text}』"));
    }
    if out.is_empty() {
        out.push(text.to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_latin_and_persian() {
        assert!(fonts("Ab1").contains(&"𝐀𝐛𝟏".to_owned()));
        assert!(fonts("سلام").contains(&"سـلـام".to_owned()));
    }
}
