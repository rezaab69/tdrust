//! Text utilities: command normalization, durations, script and emoji
//! detection, calendar conversion and HTML helpers.

/// A word of the original text with its byte offset.
#[derive(Debug, Clone)]
pub struct Token {
    pub start: usize,
    pub norm: String,
}

/// Separators between words: whitespace and the zero-width non-joiner that
/// Persian keyboards put inside compound words ("خوش‌آمد").
fn is_separator(c: char) -> bool {
    c.is_whitespace() || c == '\u{200C}'
}

/// Normalizes one word: Persian and Arabic digits become ASCII, Arabic
/// variants of Persian letters are unified, invisible marks are dropped and
/// ASCII is lowercased.
pub fn normalize_word(word: &str) -> String {
    word.chars()
        .filter_map(|c| match c {
            '\u{06F0}'..='\u{06F9}' => Some(char::from(b'0' + (c as u32 - 0x06F0) as u8)),
            '\u{0660}'..='\u{0669}' => Some(char::from(b'0' + (c as u32 - 0x0660) as u8)),
            'ي' | 'ى' => Some('ی'),
            'ك' => Some('ک'),
            '\u{200E}' | '\u{200F}' | '\u{200D}' | '\u{FEFF}' => None,
            c => Some(c.to_ascii_lowercase()),
        })
        .collect()
}

/// Splits text into normalized words, remembering where each one starts.
pub fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        if is_separator(c) {
            if let Some(s) = start.take() {
                push_token(&mut tokens, text, s, i);
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        push_token(&mut tokens, text, s, text.len());
    }
    tokens
}

fn push_token(tokens: &mut Vec<Token>, text: &str, start: usize, end: usize) {
    let norm = normalize_word(&text[start..end]);
    if !norm.is_empty() {
        tokens.push(Token { start, norm });
    }
}

/// The whole text normalized, words separated by single spaces.
pub fn normalize(text: &str) -> String {
    tokenize(text).into_iter().map(|t| t.norm).collect::<Vec<_>>().join(" ")
}

/// Seconds per unit word; a missing unit means minutes.
fn unit_seconds(unit: &str) -> Option<i64> {
    Some(match unit {
        "" | "m" | "min" | "mins" | "minute" | "minutes" | "دقیقه" => 60,
        "s" | "sec" | "ثانیه" => 1,
        "h" | "hour" | "hours" | "ساعت" => 3600,
        "d" | "day" | "days" | "روز" => 86_400,
        "w" | "week" | "weeks" | "هفته" => 604_800,
        _ => return None,
    })
}

/// Parses "30", "30 m", "5h", "6 ساعت", "2 d" (normalized words) into
/// seconds, minutes by default. Returns the duration and how many words it used.
pub fn parse_duration(words: &[&str]) -> Option<(i64, usize)> {
    let first = *words.first()?;
    let digits: String = first.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let number: i64 = digits.parse().ok()?;
    let attached = &first[digits.len()..];
    if !attached.is_empty() {
        return Some((number * unit_seconds(attached)?, 1));
    }
    match words.get(1).and_then(|unit| unit_seconds(unit).filter(|_| !unit.is_empty())) {
        Some(seconds) => Some((number * seconds, 2)),
        None => Some((number * 60, 1)),
    }
}

/// Human readable Persian duration: "۲ ساعت و ۵ دقیقه".
pub fn format_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (d, h, m) = (seconds / 86_400, seconds / 3600 % 24, seconds / 60 % 60);
    let mut parts = Vec::new();
    if d > 0 {
        parts.push(format!("{d} روز"));
    }
    if h > 0 {
        parts.push(format!("{h} ساعت"));
    }
    if m > 0 || parts.is_empty() {
        parts.push(format!("{m} دقیقه"));
    }
    fa_digits(&parts.join(" و "))
}

/// Short duration with its two largest units: "3d 4h", "12m", "45s"
/// (Persian: "۳ روز ۴ ساعت").
pub fn compact_duration(seconds: i64, lang: &str) -> String {
    let s = seconds.max(0);
    let units: [(i64, &str, &str); 4] = [(86_400, "d", " روز"), (3600, "h", " ساعت"), (60, "m", " دقیقه"), (1, "s", " ثانیه")];
    let mut rest = s;
    let mut parts = Vec::new();
    for (size, en, fa) in units {
        if rest >= size || (size == 1 && parts.is_empty()) {
            parts.push(format!("{}{}", rest / size, if lang == "fa" { fa } else { en }));
            rest %= size;
        }
        if parts.len() == 2 || (!parts.is_empty() && rest == 0) {
            break;
        }
    }
    let text = parts.join(" ");
    if lang == "fa" { fa_digits(&text) } else { text }
}

/// UTC date and time: "2026-09-24 08:05:00".
pub fn iso_time(unix: i64) -> String {
    let (y, m, d) = civil_from_days(unix.div_euclid(86_400));
    let secs = unix.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// "512 B", "3.4 KB", "12.0 MB", "1.2 GB".
pub fn format_bytes(bytes: i64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Digits grouped by thousands: "12,345".
pub fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// Parses "HH:MM" (or "HH") into minutes after midnight.
pub fn parse_clock(word: &str) -> Option<u16> {
    let (h, m) = word.split_once(':').unwrap_or((word, "0"));
    let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

pub fn format_clock(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// Replaces ASCII digits with Persian ones.
pub fn fa_digits(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_digit() { char::from_u32(0x06F0 + (c as u32 - '0' as u32)).unwrap_or(c) } else { c })
        .collect()
}

pub fn is_persian_letter(c: char) -> bool {
    matches!(c, '\u{0620}'..='\u{064A}' | '\u{066E}'..='\u{06D3}' | '\u{06FA}'..='\u{06FF}' | '\u{FB50}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFC}')
}

pub fn has_persian(text: &str) -> bool {
    text.chars().any(is_persian_letter)
}

pub fn has_english(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_alphabetic())
}

/// Characters that start an emoji.
fn is_emoji_base(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2300..=0x23FF | 0x2B00..=0x2BFF | 0x2190..=0x21FF
        | 0x25A0..=0x25FF | 0x2934 | 0x2935 | 0x3030 | 0x303D | 0x3297 | 0x3299 | 0x00A9 | 0x00AE
        | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x24C2)
}

/// Characters that only modify the emoji before them.
fn is_emoji_modifier(c: char) -> bool {
    matches!(c as u32, 0xFE0E | 0xFE0F | 0x20E3 | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F)
}

/// Counts emoji (a joined sequence or a flag counts once) and other visible characters.
pub fn emoji_stats(text: &str) -> (usize, usize) {
    let (mut emoji, mut other) = (0, 0);
    let mut after_zwj = false;
    let mut pending_flag = false;
    for c in text.chars() {
        if c == '\u{200D}' {
            after_zwj = true;
            continue;
        }
        if c.is_whitespace() || is_emoji_modifier(c) {
            continue;
        }
        let regional = (0x1F1E6..=0x1F1FF).contains(&(c as u32));
        if regional {
            if pending_flag {
                pending_flag = false;
            } else {
                pending_flag = true;
                emoji += 1;
            }
        } else if is_emoji_base(c) {
            if !after_zwj {
                emoji += 1;
            }
            pending_flag = false;
        } else {
            other += 1;
            pending_flag = false;
        }
        after_zwj = false;
    }
    (emoji, other)
}

/// Whether the text contains a web or Telegram link.
pub fn has_link_text(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["http://", "https://", "www.", "t.me/", "telegram.me/", "telegram.dog/", "tg://", "joinchat/"]
        .iter()
        .any(|pattern| lower.contains(pattern))
}

/// Whether the text contains an @username.
pub fn has_username_text(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars.iter().enumerate().any(|(i, &c)| {
        c == '@'
            && (i == 0 || !chars[i - 1].is_ascii_alphanumeric())
            && chars.get(i + 1).is_some_and(|n| n.is_ascii_alphabetic())
            && chars[i + 1..].iter().take_while(|n| n.is_ascii_alphanumeric() || **n == '_').count() >= 4
    })
}

pub fn has_hashtag_text(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars.windows(2).any(|w| w[0] == '#' && (w[1].is_alphanumeric() || w[1] == '_'))
}

/// Escapes text for Telegram HTML.
pub fn h(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A clickable HTML mention of a user.
pub fn mention_html(user_id: i64, name: &str) -> String {
    let name = if name.trim().is_empty() { user_id.to_string() } else { name.to_owned() };
    format!("<a href=\"tg://user?id={user_id}\">{}</a>", h(&name))
}

/// Unix timestamp in seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Local day number and minute of the day for a UTC offset in minutes.
pub fn local_day_minute(unix: i64, tz_minutes: i32) -> (i64, u16) {
    let local = unix + i64::from(tz_minutes) * 60;
    (local.div_euclid(86_400), (local.rem_euclid(86_400) / 60) as u16)
}

/// Gregorian date of a day number (days since 1970-01-01).
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Converts a Gregorian date to the Solar Hijri (Jalali) calendar.
pub fn gregorian_to_jalali(gy: i64, gm: i64, gd: i64) -> (i64, i64, i64) {
    const G_D_M: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let gy2 = if gm > 2 { gy + 1 } else { gy };
    let mut days =
        355_666 + 365 * gy + (gy2 + 3) / 4 - (gy2 + 99) / 100 + (gy2 + 399) / 400 + gd + G_D_M[(gm - 1) as usize];
    let mut jy = -1595 + 33 * (days / 12_053);
    days %= 12_053;
    jy += 4 * (days / 1461);
    days %= 1461;
    if days > 365 {
        jy += (days - 1) / 365;
        days = (days - 1) % 365;
    }
    let (jm, jd) = if days < 186 { (1 + days / 31, 1 + days % 31) } else { (7 + (days - 186) / 30, 1 + (days - 186) % 30) };
    (jy, jm, jd)
}

const JALALI_MONTHS: [&str; 12] =
    ["فروردین", "اردیبهشت", "خرداد", "تیر", "مرداد", "شهریور", "مهر", "آبان", "آذر", "دی", "بهمن", "اسفند"];
const WEEKDAYS: [&str; 7] = ["یکشنبه", "دوشنبه", "سه‌شنبه", "چهارشنبه", "پنجشنبه", "جمعه", "شنبه"];

/// "شنبه ۱ فروردین ۱۴۰۵" for a unix time in the given time zone.
pub fn jalali_date(unix: i64, tz_minutes: i32) -> String {
    let (day, _) = local_day_minute(unix, tz_minutes);
    let (gy, gm, gd) = civil_from_days(day);
    let (jy, jm, jd) = gregorian_to_jalali(gy, gm, gd);
    let weekday = WEEKDAYS[(day + 4).rem_euclid(7) as usize];
    fa_digits(&format!("{weekday} {jd} {} {jy}", JALALI_MONTHS[(jm - 1) as usize]))
}

/// "۱۴۰۵/۰۱/۰۱" for a unix time.
pub fn jalali_short(unix: i64, tz_minutes: i32) -> String {
    let (day, _) = local_day_minute(unix, tz_minutes);
    let (gy, gm, gd) = civil_from_days(day);
    let (jy, jm, jd) = gregorian_to_jalali(gy, gm, gd);
    fa_digits(&format!("{jy}/{jm:02}/{jd:02}"))
}

/// "۱۴:۰۵" for a unix time.
pub fn local_time(unix: i64, tz_minutes: i32) -> String {
    let (_, minute) = local_day_minute(unix, tz_minutes);
    fa_digits(&format_clock(minute))
}

/// Parses a UTC offset: "+3:30", "3.5", "-5", "+4".
pub fn parse_tz(word: &str) -> Option<i32> {
    let (sign, rest) = match word.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, word.strip_prefix('+').unwrap_or(word)),
    };
    let minutes = if let Some((h, m)) = rest.split_once(':') {
        h.parse::<i32>().ok()? * 60 + m.parse::<i32>().ok()?
    } else {
        (rest.parse::<f64>().ok()? * 60.0).round() as i32
    };
    (minutes <= 14 * 60).then_some(sign * minutes)
}

pub fn format_tz(minutes: i32) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    format!("{sign}{:02}:{:02}", minutes.abs() / 60, minutes.abs() % 60)
}

/// A small non-cryptographic random number in `0..bound`.
pub fn random(bound: u64) -> u64 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = Cell::new(
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(7) | 1
        );
    }
    STATE.with(|state| {
        let mut x = state.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        state.set(x);
        if bound == 0 { 0 } else { x % bound }
    })
}

/// A stable hash of normalized text, for duplicate detection (FNV-1a).
pub fn text_hash(text: &str) -> u64 {
    normalize(text).bytes().fold(0xcbf2_9ce4_8422_2325, |hash, b| (hash ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_for_the_operator_panel() {
        assert_eq!(compact_duration(3 * 86_400 + 4 * 3600 + 5, "en"), "3d 4h");
        assert_eq!(compact_duration(720, "en"), "12m");
        assert_eq!(compact_duration(0, "en"), "0s");
        assert_eq!(compact_duration(90, "fa"), "۱ دقیقه ۳۰ ثانیه");
        assert_eq!(iso_time(0), "1970-01-01 00:00:00");
        assert_eq!(iso_time(1_790_000_000), "2026-09-21 14:13:20");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(thousands(-1000), "-1,000");
        assert_eq!(thousands(999), "999");
    }

    #[test]
    fn normalizes_persian_input() {
        assert_eq!(normalize("قفل   رسانه ۶ ساعت"), "قفل رسانه 6 ساعت");
        assert_eq!(normalize("Lock\u{200C}Media"), "lock media");
        assert_eq!(normalize("تنظيم كانال"), "تنظیم کانال");
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration(&["30"]), Some((1800, 1)));
        assert_eq!(parse_duration(&["5", "h"]), Some((18_000, 2)));
        assert_eq!(parse_duration(&["2d"]), Some((172_800, 1)));
        assert_eq!(parse_duration(&["6", "ساعت"]), Some((21_600, 2)));
        assert_eq!(parse_duration(&["1", "w"]), Some((604_800, 2)));
        assert_eq!(parse_duration(&["x"]), None);
    }

    #[test]
    fn counts_emoji() {
        assert_eq!(emoji_stats("😐😍"), (2, 0));
        assert_eq!(emoji_stats("👨‍👩‍👧"), (1, 0));
        assert_eq!(emoji_stats("🇮🇷"), (1, 0));
        assert_eq!(emoji_stats("hi 👋🏽"), (1, 2));
        assert_eq!(emoji_stats("سلام"), (0, 4));
    }

    #[test]
    fn detects_patterns() {
        assert!(has_link_text("join t.me/xyz"));
        assert!(has_username_text("ask @durov now"));
        assert!(!has_username_text("mail me a@b.c"));
        assert!(has_hashtag_text("#tag"));
        assert!(has_persian("hello سلام"));
        assert!(has_english("سلام hi"));
    }

    #[test]
    fn converts_calendars() {
        // 2025-03-21 is 1 Farvardin 1404.
        assert_eq!(gregorian_to_jalali(2025, 3, 21), (1404, 1, 1));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_168), (2025, 3, 21));
    }

    #[test]
    fn parses_time_zones_and_clocks() {
        assert_eq!(parse_tz("+3:30"), Some(210));
        assert_eq!(parse_tz("3.5"), Some(210));
        assert_eq!(parse_tz("-5"), Some(-300));
        assert_eq!(parse_clock("08:30"), Some(510));
        assert_eq!(parse_clock("24:00"), None);
    }
}
