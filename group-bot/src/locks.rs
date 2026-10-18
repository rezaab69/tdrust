//! The on/off locks of a group, with their command names and panel pages.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum Lock {
    Hashtag,
    Username,
    Link,
    Hyper,
    Text,
    Persian,
    English,
    Game,
    Badword,
    TgService,
    Inline,
    Bot,
    BotAdder,
    Edit,
    EditMedia,
    Forward,
    Gif,
    Sticker,
    AnimatedSticker,
    Photo,
    Document,
    Contact,
    Location,
    Spoiler,
    Command,
    Pin,
    Video,
    VideoNote,
    Audio,
    Voice,
    Cmd,
    JustEmoji,
    OneEmoji,
    Emoji,
    Media,
    Tabchi,
    Advertiser,
    Bio,
    Duplicate,
}

pub struct LockInfo {
    pub lock: Lock,
    /// Short identifier used in callback data.
    pub code: &'static str,
    /// Command names after "قفل"/"بازکردن" and "lock"/"unlock". Display
    /// names are translations (`lockname.<code>`).
    pub names: &'static [&'static str],
}

macro_rules! locks {
    ($(($lock:ident, $code:literal, [$($name:literal),+])),+ $(,)?) => {
        pub const LOCKS: &[LockInfo] = &[$(LockInfo { lock: Lock::$lock, code: $code, names: &[$($name),+] }),+];
    };
}

locks![
    (Hashtag, "hs", ["هشتگ", "hashtag"]),
    (Username, "us", ["یوزرنیم", "تگ", "username"]),
    (Link, "ln", ["لینک", "link"]),
    (Hyper, "hy", ["هایپر", "هایپرلینک", "hyper"]),
    (Text, "tx", ["متن", "text"]),
    (Persian, "fa", ["فارسی", "persian"]),
    (English, "en", ["انگلیسی", "english"]),
    (Game, "gm", ["بازی", "game"]),
    (Badword, "bw", ["فحش", "badword"]),
    (TgService, "sv", ["سرویس تلگرام", "سرویس", "tgservice"]),
    (Inline, "il", ["اینلاین", "inline"]),
    (Bot, "bt", ["ربات", "bot"]),
    (BotAdder, "ba", ["اد ربات", "botadder"]),
    (Edit, "ed", ["ویرایش", "edit"]),
    (EditMedia, "em", ["ویرایش رسانه", "editmedia"]),
    (Forward, "fw", ["فروارد", "فوروارد", "forward"]),
    (Gif, "gf", ["گیف", "gif"]),
    (Sticker, "st", ["استیکر", "sticker"]),
    (AnimatedSticker, "as", ["استیکر متحرک", "animatedsticker"]),
    (Photo, "ph", ["عکس", "photo"]),
    (Document, "dc", ["فایل", "document", "file"]),
    (Contact, "ct", ["مخاطب", "contact"]),
    (Location, "lc", ["مکان", "موقعیت مکانی", "location"]),
    (Spoiler, "sp", ["اسپویلر", "spoiler"]),
    (Command, "cm", ["کامند", "commend", "command"]),
    (Pin, "pn", ["سنجاق", "pin"]),
    (Video, "vd", ["فیلم", "video"]),
    (VideoNote, "vn", ["فیلم سلفی", "videonote"]),
    (Audio, "au", ["آهنگ", "audio", "music"]),
    (Voice, "vc", ["ویس", "voice"]),
    (Cmd, "cd", ["دستورات عمومی", "cmd"]),
    (JustEmoji, "je", ["ایموجی تنها", "justemoji"]),
    (OneEmoji, "oe", ["تک ایموجی", "oneemoji"]),
    (Emoji, "ej", ["ایموجی", "emoji"]),
    (Media, "md", ["رسانه", "media"]),
    (Tabchi, "tb", ["تبچی", "tabchi"]),
    (Advertiser, "ad", ["تبلیغ گر", "تبلیغگر", "advertiser"]),
    (Bio, "bi", ["لینک در بیو", "bio"]),
    (Duplicate, "dp", ["تکراری", "پیام تکراری", "duplicate"]),
];

pub fn info(lock: Lock) -> &'static LockInfo {
    LOCKS.iter().find(|info| info.lock == lock).expect("every lock has an entry")
}

pub fn by_code(code: &str) -> Option<Lock> {
    LOCKS.iter().find(|info| info.code == code).map(|info| info.lock)
}

/// Pages of the settings panel (titles are the translations `panel.page_<n>`).
pub const PANEL_PAGES: &[&[Lock]] = &[
    &[
        Lock::Link, Lock::Username, Lock::Hashtag, Lock::Hyper, Lock::Text, Lock::Persian, Lock::English, Lock::Badword,
        Lock::Command, Lock::Spoiler, Lock::Emoji, Lock::JustEmoji, Lock::OneEmoji, Lock::Duplicate,
    ],
    &[
        Lock::Media, Lock::Photo, Lock::Video, Lock::Gif, Lock::Sticker, Lock::AnimatedSticker, Lock::Document, Lock::Audio,
        Lock::Voice, Lock::VideoNote, Lock::Contact, Lock::Location, Lock::Game,
    ],
    &[
        Lock::Forward, Lock::Inline, Lock::TgService, Lock::Bot, Lock::BotAdder, Lock::Edit, Lock::EditMedia, Lock::Pin,
        Lock::Cmd, Lock::Tabchi, Lock::Advertiser, Lock::Bio,
    ],
];

/// Strict-mode message types and the lock each one corresponds to.
pub const STRICT_TYPES: &[(Lock, &[&str])] = &[
    (Lock::Link, &["لینک", "link"]),
    (Lock::Forward, &["فروارد", "فوروارد", "forward"]),
    (Lock::AnimatedSticker, &["استیکر متحرک", "animatedsticker"]),
    (Lock::Sticker, &["استیکر", "sticker"]),
    (Lock::Voice, &["ویس", "voice"]),
    (Lock::Audio, &["آهنگ", "audio"]),
    (Lock::Photo, &["عکس", "photo"]),
    (Lock::VideoNote, &["فیلم سلفی", "videonote"]),
    (Lock::Gif, &["گیف", "gif"]),
    (Lock::English, &["انگلیسی", "english"]),
    (Lock::Username, &["آیدی", "یوزرنیم", "id", "username"]),
];
