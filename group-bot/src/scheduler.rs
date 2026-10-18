//! Background jobs, run every few seconds by one bot instance at a time
//! (a Redis lease elects the leader when several instances share storage).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::admin::global::Flag;
use crate::admin::{self, jobs, metrics};
use crate::bot::{soft, Bot};
use crate::i18n::Texts;
use crate::text::{format_clock, local_day_minute, now};

const TICK: Duration = Duration::from_secs(20);

pub async fn run(bot: Arc<Bot>) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if bot.client.is_closed() {
            return;
        }
        // Every tick doubles as a heartbeat; missing heartbeats hand this
        // bot's groups to standby bots.
        soft(bot.repo.heartbeat(bot.me, &bot.username).await, "heartbeat");
        // Every process: pick up bot-wide changes made elsewhere, and hand
        // over its metrics and errors.
        bot.refresh_global().await;
        metrics::flush(&bot).await;
        admin::flush_errors(&bot).await;
        // One process per bot runs the jobs. The lease is slightly shorter
        // than a tick so it is free again next time.
        let lease = format!("sched:leader:{}", bot.me);
        if soft(bot.cache.once(&lease, TICK.as_secs() - 2).await, "scheduler lease") == Some(false) {
            continue;
        }
        let started = std::time::Instant::now();
        tick(&bot).await;
        metrics::TICK_AT.store(now(), Ordering::Relaxed);
        metrics::TICK_MS.store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
}

async fn tick(bot: &Arc<Bot>) {
    flush_stats(bot).await;
    jobs::poll(bot).await;
    jobs::daily_sync(bot).await;
    fleet_tick(bot).await;

    // Take over groups whose owner went down (this bot is a member there).
    if let Some(claimed) = soft(bot.repo.claim_orphans(bot.me).await, "claim orphans") {
        for chat in claimed {
            bot.forget_owner(chat);
            log::info!("now serving {chat} (its previous bot is down or left)");
        }
    }

    if let Some(expired) = soft(bot.repo.take_expired_ranks(bot.me).await, "expire ranks") {
        for (chat, user, rank) in expired {
            bot.invalidate_ranks(chat).await;
            let g = bot.group(chat).await;
            let text = g.t("user.rank_expired", &[("user", &bot.mention(chat, user).await), ("rank", &g.s(rank.key()))]);
            bot.send(chat, &text).await;
        }
    }
    soft(bot.repo.prune_expired_sanctions().await, "prune sanctions");
    if let Some(pins) = soft(bot.repo.take_due_pins(bot.me).await, "timed pins") {
        for (chat, message_id) in pins {
            let _ = bot.client.unpin_chat_message(chat, message_id).await;
        }
    }
    let captchas = crate::members::captcha_deadlines(bot);
    if let Some(due) = soft(bot.cache.take_due(&captchas, now()).await, "captcha deadlines") {
        for entry in due {
            crate::members::expire_captcha(bot, &entry).await;
        }
    }
    let Some(groups) = soft(bot.repo.owned_groups(bot.me).await, "owned groups") else { return };
    for chat in groups {
        if let Err(err) = group_jobs(bot, chat).await {
            log::warn!("scheduled jobs of {chat}: {err:#}");
        }
    }
}

/// Duties of the whole fleet, done by one process at a time.
async fn fleet_tick(bot: &Arc<Bot>) {
    if soft(bot.cache.once("sched:fleet", TICK.as_secs() - 2).await, "fleet lease") != Some(true) {
        return;
    }
    soft(bot.repo.fail_orphaned_jobs().await, "orphaned jobs");
    let settings = bot.global().settings.clone();
    let tx = Texts::plain(&bot.default_lang());
    let expired = settings.maintenance.enabled && !settings.maintenance_on(now());
    if expired && soft(admin::set_maintenance(bot, 0, false, false, None).await, "end maintenance").is_some() {
        admin::alert(bot, &tx.s("adm.alert_maint_ended")).await;
    }
    soft(jobs::scheduled(bot).await, "scheduled jobs");
    if settings.monitor_alerts {
        monitor_alerts(bot, &tx, settings.error_spike).await;
    }
}

/// Alerts when a bot goes down or comes back, and on error spikes.
async fn monitor_alerts(bot: &Bot, tx: &Texts, spike: u32) {
    for b in soft(bot.repo.fleet_status().await, "fleet").unwrap_or_default() {
        let key = format!("mon:state:{}", b.id);
        let state = if b.alive { "up" } else { "down" };
        let previous: Option<String> = soft(bot.cache.get_json(&key).await, "bot state").flatten();
        soft(bot.cache.set_json(&key, &state, 7 * 86_400).await, "bot state");
        // Bots silent for over a day are retired; do not keep alerting about them.
        if previous.is_some_and(|p| p != state) && (b.alive || b.silent_for < 86_400) {
            let key = if b.alive { "adm.alert_bot_up" } else { "adm.alert_bot_down" };
            let text = tx.t(key, &[("name", &crate::text::h(&format!("@{}", b.username))), ("groups", &b.groups.to_string())]);
            admin::alert(bot, &text).await;
        }
    }
    let t = now();
    let keys: Vec<String> = (0..10).map(|i| metrics::minute_key(t / 60 - i)).collect();
    if let Some(hashes) = soft(bot.cache.hgetall_many(&keys).await, "error spike") {
        let errors = metrics::Series::from_buckets(&hashes).total(metrics::Metric::Errors);
        if errors >= i64::from(spike.max(1)) && soft(bot.cache.once("mon:spike", 1800).await, "spike alert") == Some(true) {
            let text = tx.t("adm.alert_spike", &[("n", &errors.to_string()), ("limit", &spike.to_string())]);
            admin::alert(bot, &text).await;
        }
    }
}

/// Moves buffered message counts from Redis into PostgreSQL.
async fn flush_stats(bot: &Bot) {
    let Some(rows) = soft(bot.cache.drain_stats().await, "drain stats") else { return };
    for (chat, day, user, count) in rows {
        soft(bot.repo.add_messages(chat, user, day, count).await, "store stats");
    }
}

fn in_window(minute: u16, from: u16, to: u16) -> bool {
    if from <= to {
        (from..to).contains(&minute)
    } else {
        minute >= from || minute < to
    }
}

/// The scheduled time from `times` that is due now and has not run yet, as a run key.
fn due(times: &[u16], day: i64, minute: u16, last: i64) -> Option<i64> {
    times
        .iter()
        .filter(|t| minute >= **t && minute < **t + 3)
        .map(|t| day * 1440 + i64::from(*t))
        .find(|key| *key > last)
}

async fn group_jobs(bot: &Arc<Bot>, chat: i64) -> anyhow::Result<()> {
    let g = bot.group(chat).await;
    let s = &g.s;
    let t = now();
    let (day, minute) = local_day_minute(t, s.tz);

    if s.media_lock_until.is_some_and(|until| until <= t) {
        bot.update(chat, |s| s.media_lock_until = None).await?;
        bot.send(chat, &g.s("media.unlocked")).await;
    }

    if s.group_lock.locked && s.group_lock.until.is_some_and(|until| until <= t) {
        let auto_active = bot
            .update(chat, |s| {
                s.group_lock.locked = false;
                s.group_lock.until = None;
                s.group_lock.auto_active
            })
            .await?;
        if !auto_active {
            let _ = bot.set_chat_closed(chat, false).await;
        }
        bot.send(chat, &g.s("glock.unlocked")).await;
    }

    if let Some((from, to)) = s.group_lock.auto {
        let inside = in_window(minute, from, to);
        if inside != s.group_lock.auto_active {
            let manual = bot
                .update(chat, move |s| {
                    s.group_lock.auto_active = inside;
                    s.group_lock.locked
                })
                .await?;
            if !manual {
                let _ = bot.set_chat_closed(chat, inside).await;
            }
            let key = if inside { "glock.auto_locked" } else { "glock.auto_unlocked" };
            let text = g.t(key, &[("from", &format_clock(from)), ("to", &format_clock(to))]);
            bot.send(chat, &text).await;
        }
    }

    if !bot.flag(Flag::AutoJobs) {
        return Ok(());
    }
    if let Some(run) = due(&s.auto_clean, day, minute, s.last_auto_clean) {
        bot.update(chat, move |s| s.last_auto_clean = run).await?;
        let ids = bot.cache.take_recent(chat, None).await.unwrap_or_default();
        let deleted = bot.delete(chat, ids).await;
        bot.notice(chat, &g.t("clean.auto_done", &[("n", &crate::text::fa_digits(&deleted.to_string()))]), 60).await;
    }

    if let Some(run) = due(&s.auto_stats, day, minute, s.last_auto_stats) {
        bot.update(chat, move |s| s.last_auto_stats = run).await?;
        let text = crate::stats::summary(bot, &g, 10).await?;
        bot.send(chat, &text).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_wrap_around_midnight() {
        assert!(in_window(30, 0, 480));
        assert!(!in_window(500, 0, 480));
        assert!(in_window(1430, 1380, 360));
        assert!(in_window(100, 1380, 360));
        assert!(!in_window(700, 1380, 360));
    }

    #[test]
    fn jobs_run_once_per_slot() {
        assert_eq!(due(&[600], 10, 601, 0), Some(10 * 1440 + 600));
        assert_eq!(due(&[600], 10, 601, 10 * 1440 + 600), None);
        assert_eq!(due(&[600], 10, 700, 0), None);
    }
}
