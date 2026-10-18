//! The control panel: one inline menu with a screen per section. Every
//! screen explains what it controls, shows the current values and offers
//! buttons to change them. Navigation edits the same message in place.
//!
//! Callback data (at most 64 bytes):
//!
//! | data | meaning |
//! | --- | --- |
//! | `m:<screen>` | open a screen |
//! | `t:<lock>:<page>` | switch a lock on locks page `page` |
//! | `s:<screen>:<switch>` | switch a system on or off |
//! | `v:<screen>:<field>:<value>` | change a value (`+`/`-` or an option) |
//! | `l:<list>` | show a list |
//! | `h:<section>` | show a help page |
//! | `st:<report>` | show a statistics report |
//! | `c:<kind>` / `cc:<kind>` | confirm / run a member clean-up |
//! | `g:<lang>` | set the language |
//! | `u:<action>:<user>` | user panel |
//! | `cap:<user>:<answer>` | captcha answer |
//! | `n` | label (does nothing) |
//! | `x` | close |

use std::sync::Arc;

use anyhow::Result;
use tdrust::prelude::*;
use tdrust::types::{
    ButtonStyle, ButtonStyleDanger, ButtonStyleDefault, ButtonStylePrimary, ButtonStyleSuccess, CallbackQueryPayload,
    InlineKeyboardButton, InlineKeyboardButtonTypeCallback, ReplyMarkup, ReplyMarkupInlineKeyboard, UpdateNewCallbackQuery,
};

use crate::bot::{soft, Bot, Group};
use crate::exec::{clean_group, lock_label, CleanKind};
use crate::i18n;
use crate::locks::{self, Lock, PANEL_PAGES, STRICT_TYPES};
use crate::model::{Content, Punish, Rank, ReportMode, TabchiTiming};
use crate::text::{fa_digits, format_clock, format_duration, format_tz, h, now};

pub(crate) type Row = Vec<InlineKeyboardButton>;
pub(crate) type Screen = (String, ReplyMarkup);

/// Sections of the command help (`help.<section>` texts).
const HELP_SECTIONS: &[&str] = &["locks", "protect", "ranks", "punish", "words", "join", "stats", "clean", "tools", "texts"];

// ------------------------------------------------------------ small builders

/// Button colors by function: navigation is blue, active or restoring
/// actions green, closing and destructive or punitive actions red.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Plain,
    Primary,
    Good,
    Bad,
}

pub fn style(tone: Tone) -> ButtonStyle {
    match tone {
        Tone::Plain => ButtonStyleDefault {}.into(),
        Tone::Primary => ButtonStylePrimary {}.into(),
        Tone::Good => ButtonStyleSuccess {}.into(),
        Tone::Bad => ButtonStyleDanger {}.into(),
    }
}

pub(crate) fn colored(text: impl Into<String>, data: impl Into<String>, tone: Tone) -> InlineKeyboardButton {
    InlineKeyboardButton {
        text: text.into(),
        icon_custom_emoji_id: 0,
        style: style(tone),
        r#type: InlineKeyboardButtonTypeCallback { data: data.into().into_bytes() }.into(),
    }
}

pub(crate) fn button(text: impl Into<String>, data: impl Into<String>) -> InlineKeyboardButton {
    colored(text, data, Tone::Plain)
}

/// Blue: opens a screen, list, report or help page.
pub(crate) fn link(text: impl Into<String>, data: impl Into<String>) -> InlineKeyboardButton {
    colored(text, data, Tone::Primary)
}

/// Green when the thing is on, neutral when off.
fn state_button(on: bool, text: impl Into<String>, data: impl Into<String>) -> InlineKeyboardButton {
    colored(text, data, if on { Tone::Good } else { Tone::Plain })
}

/// A button that only labels a group of buttons.
fn label(text: impl Into<String>) -> InlineKeyboardButton {
    button(text, "n")
}

pub(crate) fn keyboard(rows: Vec<Row>) -> ReplyMarkup {
    ReplyMarkupInlineKeyboard { rows, ..Default::default() }.into()
}

fn num(n: impl ToString) -> String {
    fa_digits(&n.to_string())
}

fn icon(on: bool) -> &'static str {
    if on { "✅" } else { "❌" }
}

/// "✅ on" / "❌ off", for screen texts.
fn state(g: &Group, on: bool) -> String {
    format!("{} {}", icon(on), g.s(if on { "common.on" } else { "common.off" }))
}

fn header(g: &Group, key: &str) -> Row {
    vec![label(format!("┄┄ {} ┄┄", g.s(key)))]
}

/// The on/off switch of a feature: green while on, red while off.
fn switch_row(g: &Group, on: bool, data: &str) -> Row {
    let tone = if on { Tone::Good } else { Tone::Bad };
    vec![colored(g.s(if on { "ui.on_tap" } else { "ui.off_tap" }), data, tone)]
}

fn close(g: &Group) -> InlineKeyboardButton {
    colored(g.s("ui.close"), "x", Tone::Bad)
}

fn nav(g: &Group, back: &str) -> Row {
    vec![link(g.s("ui.back"), format!("m:{back}")), close(g)]
}

fn stepper(g: &Group, screen: &str, field: &str, value: impl ToString) -> Row {
    vec![
        button("➖", format!("v:{screen}:{field}:-")),
        label(g.t(&format!("ui.v_{field}"), &[("n", &num(value))])),
        button("➕", format!("v:{screen}:{field}:+")),
    ]
}

pub(crate) fn option(text: String, selected: bool, data: String) -> InlineKeyboardButton {
    let tone = if selected { Tone::Primary } else { Tone::Plain };
    colored(format!("{} {text}", if selected { "🔘" } else { "⚪️" }), data, tone)
}

fn punish_code(punish: Punish) -> &'static str {
    match punish {
        Punish::Delete => "delete",
        Punish::Warn => "warn",
        Punish::Mute => "mute",
        Punish::TempMute(_) => "tmute",
        Punish::Ban => "ban",
    }
}

fn punish_from(code: &str, current: Punish) -> Option<Punish> {
    Some(match code {
        "warn" => Punish::Warn,
        "mute" => Punish::Mute,
        "tmute" => match current {
            Punish::TempMute(seconds) => Punish::TempMute(seconds),
            _ => Punish::TempMute(30 * 60),
        },
        "ban" => Punish::Ban,
        _ => return None,
    })
}

fn punish_rows(g: &Group, screen: &str, field: &str, current: Punish, options: &[Punish]) -> Vec<Row> {
    let buttons: Vec<_> = options
        .iter()
        .map(|p| {
            let code = punish_code(*p);
            option(g.s(p.key()), punish_code(current) == code, format!("v:{screen}:{field}:{code}"))
        })
        .collect();
    buttons.chunks(2).map(<[_]>::to_vec).collect()
}

fn punish_text(g: &Group, punish: Punish) -> String {
    crate::exec::punish_label(g, punish)
}

fn clocks(times: &[u16]) -> String {
    fa_digits(&times.iter().map(|t| format_clock(*t)).collect::<Vec<_>>().join("، "))
}

// ------------------------------------------------------------------ screens

/// Renders a screen by name; unknown names show the main menu.
pub async fn screen(bot: &Bot, g: &Group, name: &str) -> Screen {
    match name {
        "words" => words_screen(g, bot.repo.filters(g.id).await.map_or(0, |f| f.len()), bot.repo.answers(g.id).await.map_or(0, |a| a.len())),
        other => static_screen(g, other),
    }
}

/// Screens that need nothing but the group's settings.
fn static_screen(g: &Group, name: &str) -> Screen {
    match name {
        "locks0" | "locks1" | "locks2" => locks_screen(g, name[5..].parse().unwrap_or(0)),
        "protect" => protect_screen(g),
        "glock" => glock_screen(g),
        "flood" => flood_screen(g),
        "limit" => limit_screen(g),
        "chars" => chars_screen(g),
        "strict" => strict_screen(g),
        "captcha" => captcha_screen(g),
        "tabchi" => tabchi_screen(g),
        "dup" => dup_screen(g),
        "betray" => betray_screen(g),
        "reason" => reason_screen(g),
        "members" => members_screen(g),
        "join" => join_screen(g),
        "welcome" => welcome_screen(g),
        "fadd" => fadd_screen(g),
        "fjoin" => fjoin_screen(g),
        "stats" => stats_screen(g),
        "clean" => clean_screen(g),
        "lang" => lang_screen(g),
        "help" => help_screen(g),
        _ => main_screen(g),
    }
}

/// Sends a screen as a new message (replying to the command).
pub async fn open(bot: &Bot, g: &Group, name: &str, chat_id: i64, reply_to: Option<i64>) {
    let (text, markup) = screen(bot, g, name).await;
    bot.send_markup(chat_id, &text, markup, reply_to).await;
}

fn main_screen(g: &Group) -> Screen {
    let b = |key: &str, to: &str| link(g.s(key), format!("m:{to}"));
    let rows = vec![
        vec![b("ui.b_locks", "locks0"), b("ui.b_protect", "protect")],
        vec![b("ui.b_members", "members"), b("ui.b_join", "join")],
        vec![b("ui.b_words", "words"), b("ui.b_stats", "stats")],
        vec![b("ui.b_clean", "clean"), b("ui.b_lang", "lang")],
        vec![b("ui.b_help", "help")],
        vec![close(g)],
    ];
    (g.s("ui.main"), keyboard(rows))
}

fn locks_screen(g: &Group, page: usize) -> Screen {
    let page = page.min(PANEL_PAGES.len() - 1);
    let mut rows: Vec<Row> = PANEL_PAGES[page]
        .chunks(2)
        .map(|pair| {
            pair.iter()
                .map(|lock| {
                    let code = locks::info(*lock).code;
                    let on = g.s.locked(*lock);
                    state_button(on, format!("{} {}", icon(on), lock_label(g, *lock)), format!("t:{code}:{page}"))
                })
                .collect()
        })
        .collect();
    let mut pager = Vec::new();
    if page > 0 {
        pager.push(link(format!("◀️ {}", g.s(&format!("ui.locks_title_{}", page - 1))), format!("m:locks{}", page - 1)));
    }
    if page + 1 < PANEL_PAGES.len() {
        pager.push(link(format!("{} ▶️", g.s(&format!("ui.locks_title_{}", page + 1))), format!("m:locks{}", page + 1)));
    }
    rows.push(pager);
    rows.push(nav(g, "main"));
    let text = g.t(
        "ui.locks",
        &[
            ("page", &g.s(&format!("ui.locks_title_{page}"))),
            ("n", &num(page + 1)),
            ("total", &num(PANEL_PAGES.len())),
            ("hint", &g.s(&format!("ui.locks_hint_{page}"))),
        ],
    );
    (text, keyboard(rows))
}

fn protect_screen(g: &Group) -> Screen {
    let s = &g.s;
    let item = |on: bool, key: &str, to: &str| state_button(on, format!("{} {}", icon(on), g.s(key)), format!("m:{to}"));
    let tabchi =s.locked(Lock::Tabchi) || s.locked(Lock::Advertiser) || s.locked(Lock::Bio);
    let rows = vec![
        vec![item(s.group_locked(now()), "ui.b_glock", "glock"), item(s.flood.enabled, "ui.b_flood", "flood")],
        vec![item(s.limit.enabled, "ui.b_limit", "limit"), item(s.max_chars > 0 || s.min_chars > 0, "ui.b_chars", "chars")],
        vec![item(s.strict.enabled, "ui.b_strict", "strict"), item(s.security.captcha, "ui.b_captcha", "captcha")],
        vec![item(tabchi, "ui.b_tabchi", "tabchi"), item(s.locked(Lock::Duplicate), "ui.b_dup", "dup")],
        vec![item(s.security.anti_betrayal, "ui.b_betray", "betray"), item(s.security.delete_reason, "ui.b_reason", "reason")],
        nav(g, "main"),
    ];
    (g.s("ui.protect"), keyboard(rows))
}

fn glock_screen(g: &Group) -> Screen {
    let lock = &g.s.group_lock;
    let auto = match lock.auto {
        Some((from, to)) => fa_digits(&format!("{} → {} (UTC{})", format_clock(from), format_clock(to), format_tz(g.s.tz))),
        None => g.s("common.none"),
    };
    let until = match lock.until {
        Some(until) if lock.locked && until > now() => format_duration(until - now()),
        _ => g.s("common.none"),
    };
    let text = g.t("ui.glock", &[("state", &state(g, g.s.group_locked(now()))), ("until", &until), ("auto", &auto)]);
    let mut rows = vec![vec![
        colored(g.s("ui.lock_now"), "v:glock:gl:on", Tone::Bad),
        colored(g.s("ui.unlock_now"), "v:glock:gl:off", Tone::Good),
    ]];
    if lock.auto.is_some() {
        rows.push(vec![button(g.s("ui.auto_off"), "v:glock:ga:off")]);
    }
    rows.push(nav(g, "protect"));
    (text, keyboard(rows))
}

fn flood_screen(g: &Group) -> Screen {
    let f = &g.s.flood;
    let text = g.t(
        "ui.flood",
        &[("state", &state(g, f.enabled)), ("count", &num(f.count)), ("seconds", &num(f.seconds)), ("mode", &punish_text(g, f.punish))],
    );
    let mut rows = vec![switch_row(g, f.enabled, "s:flood:flood"), header(g, "ui.l_count"), stepper(g, "flood", "fc", f.count)];
    rows.push(header(g, "ui.l_punish"));
    rows.extend(punish_rows(g, "flood", "fm", f.punish, &[Punish::TempMute(0), Punish::Mute, Punish::Warn, Punish::Ban]));
    rows.push(nav(g, "protect"));
    (text, keyboard(rows))
}

fn limit_screen(g: &Group) -> Screen {
    let l = &g.s.limit;
    let text = g.t("ui.limit", &[("state", &state(g, l.enabled)), ("count", &num(l.count)), ("seconds", &num(l.seconds))]);
    let rows = vec![
        switch_row(g, l.enabled, "s:limit:limit"),
        header(g, "ui.l_count"),
        stepper(g, "limit", "lc", l.count),
        header(g, "ui.l_period"),
        stepper(g, "limit", "ls", l.seconds),
        nav(g, "protect"),
    ];
    (text, keyboard(rows))
}

fn chars_screen(g: &Group) -> Screen {
    let show = |n: u32| if n == 0 { g.s("ui.no_limit") } else { num(n) };
    let text = g.t("ui.chars", &[("max", &show(g.s.max_chars)), ("min", &show(g.s.min_chars))]);
    let rows = vec![
        header(g, "ui.l_max"),
        stepper(g, "chars", "mx", g.s.max_chars),
        header(g, "ui.l_min"),
        stepper(g, "chars", "mn", g.s.min_chars),
        vec![colored(g.s("ui.reset"), "v:chars:cr:0", Tone::Bad)],
        nav(g, "protect"),
    ];
    (text, keyboard(rows))
}

fn strict_screen(g: &Group) -> Screen {
    let st = &g.s.strict;
    let text = g.t("ui.strict", &[("state", &state(g, st.enabled)), ("mode", &punish_text(g, st.punish))]);
    let mut rows = vec![switch_row(g, st.enabled, "s:strict:strict"), header(g, "ui.l_punish")];
    rows.extend(punish_rows(g, "strict", "sm", st.punish, &[Punish::Warn, Punish::TempMute(0), Punish::Mute, Punish::Ban]));
    rows.push(header(g, "ui.l_types"));
    let types: Vec<_> = STRICT_TYPES
        .iter()
        .map(|(lock, _)| {
            let code = locks::info(*lock).code;
            let on = st.types.contains(lock);
            state_button(on, format!("{} {}", icon(on), lock_label(g, *lock)), format!("v:strict:sx:{code}"))
        })
        .collect();
    rows.extend(types.chunks(2).map(<[_]>::to_vec));
    rows.push(nav(g, "protect"));
    (text, keyboard(rows))
}

fn captcha_screen(g: &Group) -> Screen {
    let sec = &g.s.security;
    let text = g.t("ui.captcha", &[("state", &state(g, sec.captcha)), ("minutes", &num(sec.captcha_minutes))]);
    let rows = vec![
        switch_row(g, sec.captcha, "s:captcha:captcha"),
        header(g, "ui.l_time"),
        stepper(g, "captcha", "cm", sec.captcha_minutes),
        nav(g, "protect"),
    ];
    (text, keyboard(rows))
}

fn tabchi_screen(g: &Group) -> Screen {
    let t = &g.s.tabchi;
    let timing = g.s(if t.timing == TabchiTiming::Join { "ui.opt_join" } else { "ui.opt_message" });
    let text = g.t("ui.tabchi", &[("mode", &punish_text(g, t.punish)), ("timing", &timing)]);
    let mut rows = vec![header(g, "ui.l_detect")];
    for lock in [Lock::Tabchi, Lock::Advertiser, Lock::Bio] {
        let code = locks::info(lock).code;
        let on = g.s.locked(lock);
        rows.push(vec![state_button(on, format!("{} {}", icon(on), lock_label(g, lock)), format!("v:tabchi:lk:{code}"))]);
    }
    rows.push(header(g, "ui.l_punish"));
    rows.extend(punish_rows(g, "tabchi", "tp", t.punish, &[Punish::Ban, Punish::Mute]));
    rows.push(header(g, "ui.l_timing"));
    rows.push(vec![
        option(g.s("ui.opt_join"), t.timing == TabchiTiming::Join, "v:tabchi:tt:join".into()),
        option(g.s("ui.opt_message"), t.timing == TabchiTiming::Message, "v:tabchi:tt:message".into()),
    ]);
    rows.push(nav(g, "protect"));
    (text, keyboard(rows))
}

fn dup_screen(g: &Group) -> Screen {
    let sec = &g.s.security;
    let on = g.s.locked(Lock::Duplicate);
    let text = g.t("ui.dup", &[("state", &state(g, on)), ("hours", &num(sec.duplicate_hours)), ("len", &num(sec.duplicate_min_len))]);
    let rows = vec![
        switch_row(g, on, "s:dup:dup"),
        header(g, "ui.l_window"),
        stepper(g, "dup", "dh", sec.duplicate_hours),
        header(g, "ui.l_min"),
        stepper(g, "dup", "dl", sec.duplicate_min_len),
        nav(g, "protect"),
    ];
    (text, keyboard(rows))
}

fn betray_screen(g: &Group) -> Screen {
    let sec = &g.s.security;
    let text = g.t("ui.betray", &[("state", &state(g, sec.anti_betrayal)), ("limit", &num(sec.betrayal_limit))]);
    let rows = vec![
        switch_row(g, sec.anti_betrayal, "s:betray:betray"),
        header(g, "ui.l_limit"),
        stepper(g, "betray", "bl", sec.betrayal_limit),
        nav(g, "protect"),
    ];
    (text, keyboard(rows))
}

fn reason_screen(g: &Group) -> Screen {
    let sec = &g.s.security;
    let report = g.s(if sec.report_mode == ReportMode::Group { "ui.opt_group" } else { "ui.opt_private" });
    let text = g.t("ui.reason", &[("state", &state(g, sec.delete_reason)), ("report", &report)]);
    let rows = vec![
        switch_row(g, sec.delete_reason, "s:reason:reason"),
        header(g, "ui.l_report"),
        vec![
            option(g.s("ui.opt_group"), sec.report_mode == ReportMode::Group, "v:reason:rm:group".into()),
            option(g.s("ui.opt_private"), sec.report_mode == ReportMode::Private, "v:reason:rm:private".into()),
        ],
        nav(g, "protect"),
    ];
    (text, keyboard(rows))
}

fn members_screen(g: &Group) -> Screen {
    let w = &g.s.warns;
    let text = g.t("ui.members", &[("max", &num(w.max)), ("action", &punish_text(g, w.punish))]);
    let list = |name: &str| link(g.s(&format!("panel.list_{name}")), format!("l:{name}"));
    let rows = vec![
        header(g, "ui.l_lists"),
        vec![list("owners"), list("mods")],
        vec![list("vips"), list("exempt")],
        vec![list("admins"), list("warned")],
        vec![list("muted"), list("banned")],
        header(g, "ui.l_warns"),
        stepper(g, "members", "wm", w.max),
        vec![
            option(g.s("punish.mute"), w.punish != Punish::Ban, "v:members:wa:mute".into()),
            option(g.s("punish.ban"), w.punish == Punish::Ban, "v:members:wa:ban".into()),
        ],
        nav(g, "main"),
    ];
    (text, keyboard(rows))
}

fn join_screen(g: &Group) -> Screen {
    let s = &g.s;
    let text = g.t(
        "ui.join",
        &[
            ("welcome", &state(g, s.welcome.enabled)),
            ("goodbye", &state(g, s.welcome.goodbye_enabled)),
            ("captcha", &state(g, s.security.captcha)),
            ("fadd", &state(g, s.force_add.enabled)),
            ("fjoin", &state(g, s.force_join.enabled)),
        ],
    );
    let item = |on: bool, key: &str, to: &str| state_button(on, format!("{} {}", icon(on), g.s(key)), format!("m:{to}"));
    let rows = vec![
        vec![item(s.welcome.enabled, "ui.b_welcome", "welcome"), item(s.security.captcha, "ui.b_captcha", "captcha")],
        vec![item(s.force_add.enabled, "ui.b_fadd", "fadd"), item(s.force_join.enabled, "ui.b_fjoin", "fjoin")],
        nav(g, "main"),
    ];
    (text, keyboard(rows))
}

const WELCOME_DELETE: [u32; 6] = [0, 30, 60, 300, 900, 3600];

fn seconds_label(g: &Group, seconds: u32) -> String {
    match seconds {
        0 => g.s("ui.never"),
        s if s % 60 == 0 => g.t("ui.v_cm", &[("n", &num(s / 60))]),
        s => g.t("ui.v_ls", &[("n", &num(s))]),
    }
}

fn welcome_screen(g: &Group) -> Screen {
    let w = &g.s.welcome;
    let media = g.s(if matches!(w.media, Some(Content::Media { .. })) { "common.yes" } else { "common.no" });
    let custom = g.s(if w.text.is_some() { "ui.custom" } else { "ui.default" });
    let text = g.t(
        "ui.welcome",
        &[
            ("state", &state(g, w.enabled)),
            ("goodbye", &state(g, w.goodbye_enabled)),
            ("delete", &seconds_label(g, w.delete_after)),
            ("media", &media),
            ("text", &custom),
        ],
    );
    let delete: Vec<_> =
        WELCOME_DELETE.iter().map(|s| option(seconds_label(g, *s), w.delete_after == *s, format!("v:welcome:wd:{s}"))).collect();
    let mut rows = vec![
        switch_row(g, w.enabled, "s:welcome:welcome"),
        vec![state_button(w.goodbye_enabled, format!("{} {}", icon(w.goodbye_enabled), g.s("ui.b_goodbye")), "s:welcome:goodbye")],
        header(g, "ui.l_delete"),
    ];
    rows.extend(delete.chunks(3).map(<[_]>::to_vec));
    rows.push(vec![link(g.s("ui.preview"), "v:welcome:wp:0"), colored(g.s("ui.reset"), "v:welcome:wr:0", Tone::Bad)]);
    rows.push(nav(g, "join"));
    (text, keyboard(rows))
}

fn fadd_screen(g: &Group) -> Screen {
    let fa = &g.s.force_add;
    let expire = match fa.expire_days {
        Some(days) => g.t("ui.days", &[("n", &num(days))]),
        None => g.s("ui.never"),
    };
    let scope = g.s(if fa.new_only { "ui.opt_new" } else { "ui.opt_all" });
    let text = g.t("ui.fadd", &[("state", &state(g, fa.enabled)), ("count", &num(fa.count)), ("scope", &scope), ("expire", &expire)]);
    let rows = vec![
        switch_row(g, fa.enabled, "s:fadd:fadd"),
        header(g, "ui.l_count"),
        stepper(g, "fadd", "fa", fa.count),
        header(g, "ui.l_scope"),
        vec![
            option(g.s("ui.opt_all"), !fa.new_only, "v:fadd:fs:all".into()),
            option(g.s("ui.opt_new"), fa.new_only, "v:fadd:fs:new".into()),
        ],
        vec![colored(g.s("ui.reset_history"), "v:fadd:fr:0", Tone::Bad)],
        nav(g, "join"),
    ];
    (text, keyboard(rows))
}

fn fjoin_screen(g: &Group) -> Screen {
    let fj = &g.s.force_join;
    let channel = if fj.channel_title.is_empty() { g.s("common.none") } else { h(&fj.channel_title) };
    let text = g.t("ui.fjoin", &[("state", &state(g, fj.enabled)), ("channel", &channel)]);
    let rows = vec![switch_row(g, fj.enabled, "s:fjoin:fjoin"), nav(g, "join")];
    (text, keyboard(rows))
}

fn words_screen(g: &Group, filters: usize, answers: usize) -> Screen {
    let text = g.t("ui.words", &[("filters", &num(filters)), ("answers", &num(answers)), ("cooldown", &num(g.s.answer_cooldown))]);
    let bad = g.s.locked(Lock::Badword);
    let rows = vec![
        vec![link(g.s("panel.list_filters"), "l:filters"), link(g.s("panel.list_answers"), "l:answers")],
        vec![state_button(bad, format!("{} {}", icon(bad), lock_label(g, Lock::Badword)), "v:words:lk:bw")],
        header(g, "ui.l_cooldown"),
        stepper(g, "words", "ac", g.s.answer_cooldown),
        nav(g, "main"),
    ];
    (text, keyboard(rows))
}

fn stats_screen(g: &Group) -> Screen {
    let auto = if g.s.auto_stats.is_empty() { g.s("common.none") } else { clocks(&g.s.auto_stats) };
    let text = g.t("ui.stats", &[("auto", &auto)]);
    let b = |key: &str, kind: &str| link(g.s(key), format!("st:{kind}"));
    let rows = vec![
        vec![b("ui.st_summary", "summary"), b("ui.st_weekly", "weekly")],
        vec![b("ui.st_total", "total"), b("ui.st_inviters", "inviters")],
        vec![b("ui.st_staff", "staff")],
        nav(g, "main"),
    ];
    (text, keyboard(rows))
}

fn clean_screen(g: &Group) -> Screen {
    let auto = if g.s.auto_clean.is_empty() { g.s("common.none") } else { clocks(&g.s.auto_clean) };
    let text = g.t("ui.clean", &[("auto", &auto)]);
    let b = |kind: CleanKind| colored(g.s(&format!("ui.cl_{}", kind.code())), format!("c:{}", kind.code()), Tone::Bad);
    let rows = vec![
        vec![b(CleanKind::Bots), b(CleanKind::Deleted)],
        vec![b(CleanKind::Fake)],
        vec![b(CleanKind::Blocked), b(CleanKind::Restricted)],
        nav(g, "main"),
    ];
    (text, keyboard(rows))
}

fn lang_screen(g: &Group) -> Screen {
    let languages = i18n::languages();
    let current = languages.iter().find(|(code, _)| *code == g.s.lang).map_or_else(|| g.s.lang.clone(), |(_, name)| name.clone());
    let text = g.t("ui.lang", &[("current", &h(&current)), ("tz", &format_tz(g.s.tz))]);
    let options: Vec<_> = languages.into_iter().map(|(code, name)| option(name, code == g.s.lang, format!("g:{code}"))).collect();
    let mut rows: Vec<Row> = options.chunks(2).map(<[_]>::to_vec).collect();
    rows.push(nav(g, "main"));
    (text, keyboard(rows))
}

fn help_screen(g: &Group) -> Screen {
    let buttons: Vec<_> = HELP_SECTIONS.iter().map(|s| link(g.s(&format!("ui.h_{s}")), format!("h:{s}"))).collect();
    let mut rows: Vec<Row> = buttons.chunks(2).map(<[_]>::to_vec).collect();
    rows.push(nav(g, "main"));
    (g.s("ui.help"), keyboard(rows))
}

// -------------------------------------------------------------- list views

async fn list_text(bot: &Bot, g: &Group, name: &str) -> Result<String> {
    let rank = match name {
        "owners" => Some(Rank::Owner),
        "mods" => Some(Rank::Mod),
        "vips" => Some(Rank::Vip),
        "exempt" => Some(Rank::Exempt),
        _ => None,
    };
    let mut lines = Vec::new();
    if let Some(rank) = rank {
        for (user, _, _) in bot.repo.ranks(g.id).await?.into_iter().filter(|(_, r, _)| *r == rank) {
            lines.push(format!("• {}", bot.mention(g.id, user).await));
        }
    } else {
        match name {
            "admins" => {
                for (user, creator) in bot.tg_admins(g.id).await {
                    let badge = if creator { format!(" — {}", g.s("rank.creator")) } else { String::new() };
                    lines.push(format!("• {}{badge}", bot.mention(g.id, user).await));
                }
            }
            "muted" | "banned" => {
                let kind = if name == "muted" { "mute" } else { "ban" };
                for (user, _) in bot.repo.sanctions(g.id, kind).await? {
                    lines.push(format!("• {}", bot.mention(g.id, user).await));
                }
            }
            "warned" => {
                for (user, count) in bot.repo.warns(g.id).await? {
                    lines.push(format!("• {} — {}/{}", bot.mention(g.id, user).await, num(count), num(g.s.warns.max)));
                }
            }
            "filters" => {
                for (word, punish) in bot.repo.filters(g.id).await? {
                    lines.push(format!("• <code>{}</code> — {}", h(&word), punish_text(g, punish)));
                }
            }
            "answers" => {
                for answer in bot.repo.answers(g.id).await? {
                    lines.push(format!("• <code>{}</code> ({})", h(&answer.keyword), num(answer.responses.len())));
                }
            }
            _ => {}
        }
    }
    let title = g.s(&format!("panel.list_{name}"));
    if lines.is_empty() {
        return Ok(g.t("list.empty", &[("title", &title)]));
    }
    let total = lines.len();
    lines.truncate(60);
    let header = g.t("list.header", &[("title", &title), ("n", &num(total))]);
    Ok(format!("{header}\n{}", lines.join("\n")))
}

// --------------------------------------------------------------- user panel

pub async fn user_panel(bot: &Bot, g: &Group, user: i64) -> Screen {
    let rank = bot.rank(g.id, user).await;
    let warns = bot.repo.warn_count(g.id, user).await.unwrap_or(0);
    let text = g.t(
        "panel.user",
        &[
            ("user", &bot.mention(g.id, user).await),
            ("id", &user.to_string()),
            ("rank", &g.s(rank.key())),
            ("warns", &num(format!("{warns}/{}", g.s.warns.max))),
        ],
    );
    let pair = |(on, a, on_tone): (&str, &str, Tone), (off, b, off_tone): (&str, &str, Tone)| {
        vec![colored(g.s(on), format!("u:{a}:{user}"), on_tone), colored(g.s(off), format!("u:{b}:{user}"), off_tone)]
    };
    let rows = vec![
        header(g, "ui.l_rank"),
        pair(("panel.u_mod", "mod", Tone::Good), ("panel.u_unmod", "unmod", Tone::Plain)),
        pair(("panel.u_vip", "vip", Tone::Good), ("panel.u_unvip", "unvip", Tone::Plain)),
        pair(("panel.u_ex", "ex", Tone::Good), ("panel.u_unex", "unex", Tone::Plain)),
        header(g, "ui.l_punish"),
        pair(("panel.u_ban", "ban", Tone::Bad), ("panel.u_unban", "unban", Tone::Good)),
        pair(("panel.u_mute", "mute", Tone::Bad), ("panel.u_unmute", "unmute", Tone::Good)),
        pair(("panel.u_warn", "warn", Tone::Bad), ("panel.u_clrw", "clrw", Tone::Good)),
        vec![close(g)],
    ];
    (text, keyboard(rows))
}

async fn user_action(bot: &Bot, g: &Group, actor: i64, actor_rank: Rank, action: &str, user: i64) -> Result<String> {
    let chat = g.id;
    let target_rank = bot.rank(chat, user).await;
    if bot.is_fleet(user).await || (target_rank >= actor_rank && actor_rank != Rank::Sudo) {
        return Ok(g.s("err.higher_rank"));
    }
    let needs_owner = matches!(action, "mod" | "unmod" | "vip" | "unvip" | "ex" | "unex");
    if needs_owner && actor_rank < Rank::Owner {
        return Ok(g.t("err.no_permission", &[("rank", &g.s(Rank::Owner.key()))]));
    }
    let mention = bot.mention(chat, user).await;
    let rank_for = |action: &str| match action.trim_start_matches("un") {
        "mod" => Rank::Mod,
        "vip" => Rank::Vip,
        _ => Rank::Exempt,
    };
    let report = match action {
        "mod" | "vip" | "ex" => {
            let rank = rank_for(action);
            bot.repo.set_rank(chat, user, rank, None, actor).await?;
            bot.invalidate_ranks(chat).await;
            g.t("user.promoted", &[("user", &mention), ("rank", &g.s(rank.key()))])
        }
        "unmod" | "unvip" | "unex" => {
            let rank = rank_for(action);
            bot.repo.remove_rank(chat, user, rank).await?;
            bot.invalidate_ranks(chat).await;
            g.t("user.demoted", &[("user", &mention), ("rank", &g.s(rank.key()))])
        }
        "ban" => return Ok(bot.apply_hard(g, user, Punish::Ban, actor, "").await),
        "mute" => return Ok(bot.apply_hard(g, user, Punish::Mute, actor, "").await),
        "warn" => return Ok(bot.warn(g, user, actor, "").await),
        "unban" | "unmute" => {
            let (result, kind, key) = if action == "unban" {
                (bot.unban(chat, user).await, "ban", "user.unbanned")
            } else {
                (bot.unrestrict(chat, user).await, "mute", "user.unmuted")
            };
            if let Err(err) = result {
                return Ok(g.t("err.action_failed", &[("error", &h(&err.to_string()))]));
            }
            soft(bot.repo.remove_sanction(chat, user, kind).await, "remove sanction");
            g.t(key, &[("user", &mention)])
        }
        "clrw" => {
            bot.repo.reset_warns(chat, user).await?;
            g.t("warn.cleared_user", &[("user", &mention)])
        }
        _ => return Ok(String::new()),
    };
    bot.log_event(g, actor, Some(user), action, &report).await;
    Ok(report)
}

// ------------------------------------------------------------------ changes

fn stepped(value: u32, up: bool, by: u32, min: u32, max: u32) -> u32 {
    if up { value.saturating_add(by).min(max) } else { value.saturating_sub(by).max(min) }
}

fn owner_only(g: &Group) -> Option<String> {
    Some(g.t("err.no_permission", &[("rank", &g.s(Rank::Owner.key()))]))
}

/// Switches a system on or off. Returns a toast when refused.
async fn switch(bot: &Bot, g: &Group, rank: Rank, which: &str) -> Result<Option<String>> {
    let s = &g.s;
    let value = match which {
        "flood" => !s.flood.enabled,
        "limit" => !s.limit.enabled,
        "strict" => !s.strict.enabled,
        "captcha" => !s.security.captcha,
        "welcome" => !s.welcome.enabled,
        "goodbye" => !s.welcome.goodbye_enabled,
        "fadd" => !s.force_add.enabled,
        "fjoin" if s.force_join.channel_id.is_none() => return Ok(Some(g.s("panel.fjoin_needs_channel"))),
        "fjoin" => !s.force_join.enabled,
        "betray" if rank < Rank::Owner => return Ok(owner_only(g)),
        "betray" => !s.security.anti_betrayal,
        "reason" => !s.security.delete_reason,
        "dup" => !s.locked(Lock::Duplicate),
        _ => return Ok(None),
    };
    let which = which.to_owned();
    bot.update(g.id, move |s| match which.as_str() {
        "flood" => s.flood.enabled = value,
        "limit" => s.limit.enabled = value,
        "strict" => s.strict.enabled = value,
        "captcha" => s.security.captcha = value,
        "welcome" => s.welcome.enabled = value,
        "goodbye" => s.welcome.goodbye_enabled = value,
        "fadd" => {
            if value && !s.force_add.enabled {
                s.force_add.since = now();
            }
            s.force_add.enabled = value;
        }
        "fjoin" => s.force_join.enabled = value,
        "betray" => s.security.anti_betrayal = value,
        "reason" => s.security.delete_reason = value,
        _ => {
            if value {
                s.locks.insert(Lock::Duplicate);
            } else {
                s.locks.remove(&Lock::Duplicate);
            }
        }
    })
    .await?;
    Ok(None)
}

/// Changes one value. Returns a toast to show, if any.
async fn set_value(bot: &Bot, g: &Group, rank: Rank, presser: i64, field: &str, value: &str) -> Result<Option<String>> {
    let chat = g.id;
    let s = &g.s;
    let up = value == "+";
    match field {
        "fc" => {
            let n = stepped(s.flood.count, up, 1, 2, 50);
            bot.update(chat, move |s| s.flood.count = n).await?;
        }
        "fm" | "sm" | "tp" => {
            let current = match field {
                "fm" => s.flood.punish,
                "sm" => s.strict.punish,
                _ => s.tabchi.punish,
            };
            let Some(punish) = punish_from(value, current) else { return Ok(None) };
            let field = field.to_owned();
            bot.update(chat, move |s| match field.as_str() {
                "fm" => s.flood.punish = punish,
                "sm" => s.strict.punish = punish,
                _ => s.tabchi.punish = punish,
            })
            .await?;
        }
        "lc" => {
            let n = stepped(s.limit.count, up, 1, 1, 100);
            bot.update(chat, move |s| s.limit.count = n).await?;
        }
        "ls" => {
            let n = stepped(s.limit.seconds, up, 10, 10, 3600);
            bot.update(chat, move |s| s.limit.seconds = n).await?;
        }
        "mx" => {
            let n = stepped(s.max_chars, up, 100, 0, 4000);
            bot.update(chat, move |s| s.max_chars = n).await?;
        }
        "mn" => {
            let n = stepped(s.min_chars, up, 1, 0, 100);
            bot.update(chat, move |s| s.min_chars = n).await?;
        }
        "cr" => {
            bot.update(chat, |s| {
                s.max_chars = 0;
                s.min_chars = 0;
            })
            .await?;
        }
        "sx" | "lk" => {
            let Some(lock) = locks::by_code(value) else { return Ok(None) };
            let strict = field == "sx";
            bot.update(chat, move |s| {
                let set = if strict { &mut s.strict.types } else { &mut s.locks };
                if !set.remove(&lock) {
                    set.insert(lock);
                }
            })
            .await?;
        }
        "cm" => {
            let n = stepped(s.security.captcha_minutes, up, 1, 1, 30);
            bot.update(chat, move |s| s.security.captcha_minutes = n).await?;
        }
        "tt" => {
            let timing = if value == "join" { TabchiTiming::Join } else { TabchiTiming::Message };
            bot.update(chat, move |s| s.tabchi.timing = timing).await?;
        }
        "dh" => {
            let n = stepped(s.security.duplicate_hours, up, 1, 1, 168);
            bot.update(chat, move |s| s.security.duplicate_hours = n).await?;
        }
        "dl" => {
            let n = stepped(s.security.duplicate_min_len, up, 10, 10, 1000);
            bot.update(chat, move |s| s.security.duplicate_min_len = n).await?;
        }
        "bl" if rank < Rank::Owner => return Ok(owner_only(g)),
        "bl" => {
            let n = stepped(s.security.betrayal_limit, up, 1, 1, 100);
            bot.update(chat, move |s| s.security.betrayal_limit = n).await?;
        }
        "rm" => {
            let mode = if value == "group" { ReportMode::Group } else { ReportMode::Private };
            bot.update(chat, move |s| s.security.report_mode = mode).await?;
        }
        "wm" => {
            let n = stepped(s.warns.max, up, 1, 1, 20);
            bot.update(chat, move |s| s.warns.max = n).await?;
        }
        "wa" => {
            let punish = if value == "ban" { Punish::Ban } else { Punish::Mute };
            bot.update(chat, move |s| s.warns.punish = punish).await?;
        }
        "wd" => {
            let Ok(seconds) = value.parse::<u32>() else { return Ok(None) };
            bot.update(chat, move |s| s.welcome.delete_after = seconds).await?;
        }
        "wp" => {
            crate::members::send_welcome(bot, g, presser, chat, true).await;
            return Ok(Some(g.s("ui.preview_sent")));
        }
        "wr" => {
            bot.update(chat, |s| {
                s.welcome.text = None;
                s.welcome.media = None;
            })
            .await?;
        }
        "ac" => {
            let n = stepped(s.answer_cooldown, up, 10, 0, 3600);
            bot.update(chat, move |s| s.answer_cooldown = n).await?;
        }
        "fa" => {
            let n = stepped(s.force_add.count, up, 1, 1, 50);
            bot.update(chat, move |s| s.force_add.count = n).await?;
        }
        "fs" => {
            let new_only = value == "new";
            bot.update(chat, move |s| {
                s.force_add.new_only = new_only;
                s.force_add.since = now();
            })
            .await?;
        }
        "fr" => {
            bot.repo.reset_force_adds(chat).await?;
            return Ok(Some(g.s("fadd.reset")));
        }
        "gl" => {
            let lock = value == "on";
            let auto_active = bot
                .update(chat, move |s| {
                    s.group_lock.locked = lock;
                    s.group_lock.until = None;
                    s.group_lock.auto_active
                })
                .await?;
            if lock || !auto_active {
                if let Err(err) = bot.set_chat_closed(chat, lock).await {
                    log::warn!("cannot change permissions of {chat}: {err:#}");
                }
            }
        }
        "ga" => {
            let (was_active, manual) = bot
                .update(chat, |s| {
                    s.group_lock.auto = None;
                    (std::mem::take(&mut s.group_lock.auto_active), s.group_lock.locked)
                })
                .await?;
            if was_active && !manual {
                let _ = bot.set_chat_closed(chat, false).await;
            }
        }
        _ => {}
    }
    Ok(None)
}

// ------------------------------------------------------------------ buttons

pub(crate) async fn edit(bot: &Bot, chat_id: i64, message_id: i64, (text, markup): Screen) {
    let content = InputMessageContent::text(crate::content::html(&text));
    if let Err(err) = bot.client.edit_message_text(chat_id, message_id, Some(markup), content).await {
        if !err.to_string().contains("MESSAGE_NOT_MODIFIED") {
            log::debug!("cannot edit panel: {err}");
        }
    }
}

/// Handles a button press. Returns the toast text (may be empty).
async fn handle(bot: &Arc<Bot>, q: &UpdateNewCallbackQuery, data: &str) -> Result<String> {
    let chat = q.chat_id;
    let g = bot.group(chat).await;
    let parts: Vec<&str> = data.split(':').collect();

    match parts.as_slice() {
        ["n"] => return Ok(String::new()),
        ["cap", user, value] => {
            let (Ok(user), Ok(value)) = (user.parse(), value.parse()) else { return Ok(String::new()) };
            return Ok(crate::members::solve_captcha(bot, &g, q.sender_user_id, user, value).await);
        }
        _ => {}
    }

    let rank = bot.rank(chat, q.sender_user_id).await;
    if rank < Rank::Mod {
        return Ok(g.s("panel.no_access"));
    }
    let message = q.message_id;
    match parts.as_slice() {
        ["x"] => {
            bot.delete(chat, vec![message]).await;
        }
        ["m", name] => edit(bot, chat, message, screen(bot, &g, name).await).await,
        ["t", code, page] => {
            let Some(lock) = locks::by_code(code) else { return Ok(String::new()) };
            bot.update(chat, move |s| {
                if !s.locks.remove(&lock) {
                    s.locks.insert(lock);
                }
            })
            .await?;
            let g = bot.group(chat).await;
            edit(bot, chat, message, locks_screen(&g, page.parse().unwrap_or(0))).await;
            let state = g.s(if g.s.locked(lock) { "common.on" } else { "common.off" });
            let report = g.t("panel.toggled", &[("name", &lock_label(&g, lock)), ("state", &state)]);
            bot.log_event(&g, q.sender_user_id, None, "settings", &report).await;
            return Ok(report);
        }
        ["s", name, which] => {
            if let Some(toast) = switch(bot, &g, rank, which).await? {
                return Ok(toast);
            }
            let g = bot.group(chat).await;
            edit(bot, chat, message, screen(bot, &g, name).await).await;
            log_change(bot, &g, q.sender_user_id, name).await;
        }
        ["v", name, field, value] => {
            let toast = set_value(bot, &g, rank, q.sender_user_id, field, value).await?;
            let g = bot.group(chat).await;
            edit(bot, chat, message, screen(bot, &g, name).await).await;
            if *field != "wp" && toast.is_none() {
                log_change(bot, &g, q.sender_user_id, name).await;
            }
            return Ok(toast.unwrap_or_default());
        }
        ["l", name] => {
            let back = if matches!(*name, "filters" | "answers") { "words" } else { "members" };
            let text = list_text(bot, &g, name).await?;
            edit(bot, chat, message, (text, keyboard(vec![nav(&g, back)]))).await;
        }
        ["h", section] => {
            let text = g.s(&format!("help.{section}"));
            edit(bot, chat, message, (text, keyboard(vec![nav(&g, "help")]))).await;
        }
        ["st", kind] => {
            let text = match *kind {
                "weekly" => crate::stats::weekly(bot, &g, None).await?,
                "total" => crate::stats::total(bot, &g).await?,
                "inviters" => crate::stats::inviters(bot, &g).await?,
                "staff" => crate::stats::ranked(bot, &g).await?,
                _ => crate::stats::summary(bot, &g, 10).await?,
            };
            edit(bot, chat, message, (text, keyboard(vec![nav(&g, "stats")]))).await;
        }
        ["c", kind] => {
            let Some(kind) = CleanKind::from_code(kind) else { return Ok(String::new()) };
            let action = g.s(&format!("ui.cl_{}", kind.code()));
            let text = g.t("ui.clean_confirm", &[("action", &action)]);
            let rows = vec![vec![
                colored(g.s("ui.yes"), format!("cc:{}", kind.code()), Tone::Bad),
                button(g.s("ui.no"), "m:clean"),
            ]];
            edit(bot, chat, message, (text, keyboard(rows))).await;
        }
        ["cc", kind] => {
            let Some(kind) = CleanKind::from_code(kind) else { return Ok(String::new()) };
            edit(bot, chat, message, (g.s("clean.working"), keyboard(vec![]))).await;
            let bot = bot.clone();
            let actor = q.sender_user_id;
            // Clean-ups can take minutes: answer the button now and report when done.
            tokio::spawn(async move {
                let text = match clean_group(&bot, chat, kind).await {
                    Some(done) => g.t("clean.members_done", &[("n", &num(done))]),
                    None => g.s("err.supergroup_only"),
                };
                bot.log_event(&g, actor, None, "clean", &text).await;
                edit(&bot, chat, message, (text, keyboard(vec![nav(&g, "clean")]))).await;
            });
        }
        ["g", _] if rank < Rank::Owner => return Ok(owner_only(&g).unwrap_or_default()),
        ["g", code] => {
            if i18n::is_language(code) {
                let code = (*code).to_owned();
                bot.update(chat, move |s| s.lang = code).await?;
            }
            let g = bot.group(chat).await;
            edit(bot, chat, message, lang_screen(&g)).await;
            return Ok(g.s("lang.changed"));
        }
        ["u", action, user] => {
            let Ok(user) = user.parse::<i64>() else { return Ok(String::new()) };
            let result = user_action(bot, &g, q.sender_user_id, rank, action, user).await?;
            let (text, markup) = user_panel(bot, &g, user).await;
            let text = if result.is_empty() { text } else { format!("{text}\n\n{result}") };
            edit(bot, chat, message, (text, markup)).await;
        }
        _ => return Ok(g.s("ui.expired")),
    }
    Ok(String::new())
}

async fn log_change(bot: &Bot, g: &Group, actor: i64, screen: &str) {
    let section = g.s(&format!("ui.b_{screen}"));
    bot.log_event(g, actor, None, "settings", &g.t("ui.log_change", &[("section", &section)])).await;
}

pub async fn on_callback(bot: &Arc<Bot>, q: UpdateNewCallbackQuery) {
    let CallbackQueryPayload::Data(payload) = &q.payload else { return };
    let data = String::from_utf8_lossy(&payload.data).into_owned();
    crate::admin::metrics::inc(crate::admin::metrics::Metric::Callbacks);
    let toast = if data.starts_with("a:") {
        // The operator panel, in private chats.
        crate::admin::panel::on_callback(bot, &q, &data).await
    } else if q.chat_id > 0 {
        String::new()
    } else {
        match crate::admin::maintenance(bot).filter(|_| !bot.is_sudo(q.sender_user_id)) {
            // Captcha answers still work unless everything is paused.
            Some(full) if full || !data.starts_with("cap:") => bot.group(q.chat_id).await.s("adm.maint_toast"),
            _ => match handle(bot, &q, &data).await {
                Ok(toast) => toast,
                Err(err) => {
                    log::error!("callback {data} in {}: {err:#}", q.chat_id);
                    bot.group(q.chat_id).await.s("err.internal")
                }
            },
        }
    };
    if let Err(err) = bot.client.answer_callback_query(q.id, strip_tags(&toast), false, "", 0).await {
        log::debug!("cannot answer callback: {err}");
    }
}

/// Toasts are plain text: removes the HTML of translated texts.
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&amp;", "&");
    out.chars().take(190).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREENS: &[&str] = &[
        "main", "locks0", "locks1", "locks2", "protect", "glock", "flood", "limit", "chars", "strict", "captcha", "tabchi",
        "dup", "betray", "reason", "members", "join", "welcome", "fadd", "fjoin", "words", "stats", "clean", "lang", "help",
    ];

    fn group(lang: &str) -> Group {
        Group { id: -100, s: crate::model::Settings::default(), tx: crate::i18n::Texts::plain(lang) }
    }

    fn render(g: &Group, name: &str) -> Screen {
        if name == "words" { words_screen(g, 3, 2) } else { static_screen(g, name) }
    }

    fn buttons(markup: &ReplyMarkup) -> Vec<(String, String)> {
        let ReplyMarkup::InlineKeyboard(keyboard) = markup else { return vec![] };
        keyboard
            .rows
            .iter()
            .flatten()
            .filter_map(|b| match &b.r#type {
                tdrust::types::InlineKeyboardButtonType::Callback(c) => {
                    Some((b.text.clone(), String::from_utf8_lossy(&c.data).into_owned()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_screen_is_complete_and_linked() {
        for lang in ["fa", "en"] {
            let g = group(lang);
            for name in SCREENS {
                let (text, markup) = render(&g, name);
                assert!(!text.contains('{') && !text.contains("ui."), "{lang}/{name}: unfilled text: {text}");
                for (label, data) in buttons(&markup) {
                    assert!(!label.contains("ui.") && !label.contains("panel.") && !label.contains('{'), "{lang}/{name}: {label}");
                    assert!(data.len() <= 64, "{name}: {data}");
                    if let Some(target) = data.strip_prefix("m:") {
                        assert!(SCREENS.contains(&target), "{name} links to unknown screen {target}");
                    }
                }
            }
        }
    }

    #[test]
    fn help_sections_exist_and_fit_a_message() {
        for lang in ["fa", "en"] {
            let g = group(lang);
            for section in HELP_SECTIONS {
                let text = g.s(&format!("help.{section}"));
                assert!(!text.starts_with("help."), "{lang}: help.{section} missing");
                assert!(text.chars().count() < 3500, "{lang}: help.{section} is too long");
            }
        }
    }

    #[test]
    fn buttons_are_colored_by_function() {
        let styles = |markup: &ReplyMarkup| -> Vec<(String, ButtonStyle)> {
            let ReplyMarkup::InlineKeyboard(keyboard) = markup else { return vec![] };
            keyboard
                .rows
                .iter()
                .flatten()
                .filter_map(|b| match &b.r#type {
                    tdrust::types::InlineKeyboardButtonType::Callback(c) => {
                        Some((String::from_utf8_lossy(&c.data).into_owned(), b.style.clone()))
                    }
                    _ => None,
                })
                .collect()
        };
        for lang in ["fa", "en"] {
            let g = group(lang);
            for name in SCREENS {
                for (data, style) in styles(&render(&g, name).1) {
                    if data == "x" {
                        assert_eq!(style, super::style(Tone::Bad), "{name}: close must be red");
                    }
                    if data.starts_with("m:") && !name.starts_with("protect") && !name.starts_with("join") {
                        assert_eq!(style, super::style(Tone::Primary), "{name}: navigation {data} must be blue");
                    }
                }
            }
        }
    }

    #[test]
    fn steppers_stay_in_range() {
        assert_eq!(stepped(2, false, 1, 2, 50), 2);
        assert_eq!(stepped(50, true, 1, 2, 50), 50);
        assert_eq!(stepped(10, true, 10, 10, 3600), 20);
        assert_eq!(punish_from("tmute", Punish::TempMute(600)), Some(Punish::TempMute(600)));
    }

    #[test]
    fn strips_html_for_toasts() {
        assert_eq!(strip_tags("<b>Done</b> &amp; saved"), "Done & saved");
    }
}
