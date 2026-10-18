//! Activity statistics.

use anyhow::Result;

use crate::bot::{Bot, Group};
use crate::model::Rank;
use crate::text::{fa_digits, h, jalali_date, jalali_short, local_day_minute, now};

fn today(g: &Group) -> i32 {
    local_day_minute(now(), g.s.tz).0 as i32
}

fn num(n: impl ToString) -> String {
    fa_digits(&n.to_string())
}

async fn ranking(bot: &Bot, g: &Group, rows: Vec<(i64, i64)>) -> String {
    let medals = ["🥇", "🥈", "🥉"];
    let mut lines = Vec::new();
    for (i, (user, count)) in rows.into_iter().enumerate() {
        let place = medals.get(i).map_or_else(|| num(i + 1) + ".", |m| (*m).to_owned());
        lines.push(format!("{place} {} — {}", bot.mention(g.id, user).await, num(count)));
    }
    if lines.is_empty() {
        g.s("stats.nothing")
    } else {
        lines.join("\n")
    }
}

/// "آمار" / "Stats [N]": today's activity and the N most active members of the week.
pub async fn summary(bot: &Bot, g: &Group, top: i64) -> Result<String> {
    let day = today(g);
    let today_total: i64 = bot.repo.daily(g.id, None, day, day).await?.values().sum();
    let members = match bot.supergroup_id(g.id).await {
        Some(id) => bot.client.get_supergroup(id).await.map(|s| s.member_count).unwrap_or(0),
        None => 0,
    };
    let title = bot.client.get_chat(g.id).await.map(|c| c.title).unwrap_or_default();
    let rows = bot.repo.top_users(g.id, day - 6, day, top).await?;
    Ok(g.t(
        "stats.summary",
        &[
            ("group", &h(&title)),
            ("date", &jalali_date(now(), g.s.tz)),
            ("today", &num(today_total)),
            ("members", &num(members)),
            ("n", &num(top)),
            ("top", &ranking(bot, g, rows).await),
        ],
    ))
}

pub async fn inviters(bot: &Bot, g: &Group) -> Result<String> {
    let rows = bot.repo.top_inviters(g.id, 20).await?.into_iter().map(|(u, n)| (u, i64::from(n))).collect();
    Ok(g.t("stats.inviters", &[("list", &ranking(bot, g, rows).await)]))
}

/// Weekly activity of everyone with a rank of moderator or above.
pub async fn ranked(bot: &Bot, g: &Group) -> Result<String> {
    let mut staff: Vec<i64> = bot.tg_admins(g.id).await.into_keys().collect();
    for (user, rank) in bot.stored_ranks(g.id).await {
        if Rank::from_db(rank) >= Rank::Mod && !staff.contains(&user) {
            staff.push(user);
        }
    }
    staff.retain(|u| *u != bot.me);
    let day = today(g);
    let mut rows = Vec::new();
    for user in staff {
        let week: i64 = bot.repo.daily(g.id, Some(user), day - 6, day).await?.values().sum();
        rows.push((user, week));
    }
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(g.t("stats.ranked", &[("list", &ranking(bot, g, rows).await)]))
}

pub async fn total(bot: &Bot, g: &Group) -> Result<String> {
    let total = bot.repo.total_messages(g.id, None).await?;
    let rows = bot.repo.top_users(g.id, i32::MIN, i32::MAX, 10).await?;
    Ok(g.t("stats.total", &[("total", &num(total)), ("top", &ranking(bot, g, rows).await)]))
}

/// The last seven days, for the group or one member.
pub async fn weekly(bot: &Bot, g: &Group, user: Option<i64>) -> Result<String> {
    let day = today(g);
    let daily = bot.repo.daily(g.id, user, day - 6, day).await?;
    let max = daily.values().copied().max().unwrap_or(0).max(1);
    let mut lines = Vec::new();
    let mut week = 0;
    for d in (day - 6)..=day {
        let count = daily.get(&d).copied().unwrap_or(0);
        week += count;
        let bar = "▰".repeat(((count * 10) / max) as usize);
        // Noon of that local day, as a unix time.
        let noon = i64::from(d) * 86_400 + 43_200 - i64::from(g.s.tz) * 60;
        lines.push(format!("<code>{}</code> {bar} {}", jalali_short(noon, g.s.tz), num(count)));
    }
    let title = match user {
        Some(user) => g.t("stats.weekly_user", &[("user", &bot.mention(g.id, user).await)]),
        None => g.s("stats.weekly_group"),
    };
    let total = bot.repo.total_messages(g.id, user).await?;
    Ok(g.t("stats.weekly", &[("title", &title), ("days", &lines.join("\n")), ("week", &num(week)), ("total", &num(total))]))
}
