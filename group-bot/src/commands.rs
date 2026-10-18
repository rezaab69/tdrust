//! Command table and parser. Every command has Persian and English phrases;
//! both work in every group regardless of its language, with or without a
//! leading `/` (and `@botname`).

use std::sync::OnceLock;

use crate::locks::{Lock, LOCKS};
use crate::model::Rank;
use crate::text::{normalize_word, tokenize};

#[rustfmt::skip]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum C {
    Help, Settings, Lists, Lang,
    GroupLock, GroupUnlock, TempMediaOn, TempMediaOff, AutoLockSet, AutoLockRem, AutoLockStatus,
    LimitOn, LimitOff, LimitCount, LimitTime, FloodOn, FloodOff, FloodCount, FloodMode,
    MaxChar, MinChar, CharOff, StrictOn, StrictOff, StrictMode, StrictMute, TabchiMode, TabchiTime,
    SetOwner, RemOwner, OwnerList, CleanOwners, SetTempMod, Promote, Demote, ModList, CleanMods,
    SetTempVip, SetVip, RemVip, VipList, CleanVips, Exempt, RemExempt, ExemptList, CleanExempt,
    AddAdmin, RemAdmin, AdminList,
    BanPlus, Ban, UnBan, BanList, CleanBans, Mute, UnMute, MuteList, CleanMutes, Kick, UnKick,
    Warn, RemWarn, RemWarns, SetWarnMax, WarnBan, WarnMute, WarnList, CleanWarns, UserPanel,
    AddFilter, FilterWarn, FilterBan, FilterMute, RemFilter, FilterList, CleanFilters, FilterStart, FilterEnd, PanelPv,
    Del, AutoCleanSet, AutoCleanRem, AutoCleanStatus,
    CleanNicknames, CleanBlockList, CleanRestrictList, CleanBots, CleanFake, CleanDeleted,
    ForceAddOn, ForceAddOff, ForceAddCount, ForceAddTime, ForceAddText, ForceAddTextDel, ForceAddScope, ForceAddReset,
    ForceJoinOn, ForceJoinOff, ForceJoinText, ForceJoinTextDel, ForceJoinStatus, ForceJoinChannel,
    WelcomeOn, WelcomeOff, SetWelcome, GetWelcome, ResetWelcome, WelcomeTime, WelcomeMedia, WelcomeMediaDel,
    GoodbyeOn, GoodbyeOff, SetGoodbye,
    SetAnswer, RemAnswer, AnswerList, CleanAnswers, AnswerAudience, AnswerCooldown,
    Stats, AddStats, RankStats, TotalStats, WeeklyStats, UserStats, AutoStatsSet, AutoStatsRem, AutoStatsStatus,
    Captcha, CaptchaTime, AntiBetrayal, BetrayalLimit, DupHours, DupLen, Report, ReportMode, DelReason,
    SetLog, RemLog, TimeZone, SetText, RemText, TextKeys, Bots,
    Time, Echo, Joke, Id, JoinDate, SetGlobalAsl, SetAsl, MyAsl, Asl, DelAsl, Tag,
    SetNickname, Nickname, DelNickname, NicknameList, GetPro, Rules, SetRules, DelRules, Pin, Unpin,
    Link, SetLink, DelLink, CreateLink, SetPhoto, Font,
    GBan, UnGBan,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Args {
    /// The phrase must be the whole message.
    None,
    Optional,
    Required,
}

pub struct Spec {
    pub cmd: C,
    pub rank: Rank,
    pub args: Args,
    /// Public "fun" command, restricted to VIPs and above by the `Cmd` lock.
    pub fun: bool,
    pub phrases: &'static [&'static str],
}

macro_rules! specs {
    ($($cmd:ident $rank:ident $args:ident $fun:literal [$($phrase:literal),+];)+) => {
        pub const SPECS: &[Spec] = &[$(Spec { cmd: C::$cmd, rank: Rank::$rank, args: Args::$args, fun: $fun, phrases: &[$($phrase),+] }),+];
    };
}

specs! {
    Help Member None false ["راهنما", "help", "start"];
    Settings Mod None false ["تنظیمات", "پنل", "پنل مدیریت", "settings", "panel"];
    Lists Mod None false ["لیست ها", "لیستها", "lists"];
    Lang Owner Optional false ["زبان", "lang", "language"];

    GroupLock Mod Optional false ["قفل گروه", "lock group"];
    GroupUnlock Mod None false ["بازکردن گروه", "باز کردن گروه", "unlock group"];
    TempMediaOn Mod Required false ["قفل رسانه موقت", "lock tmedia"];
    TempMediaOff Mod None false ["بازکردن رسانه موقت", "باز کردن رسانه موقت", "unlock tmedia"];
    AutoLockSet Mod Required false ["تنظیم قفل خودکار", "setautolock"];
    AutoLockRem Mod None false ["حذف قفل خودکار", "remautolock"];
    AutoLockStatus Mod None false ["وضعیت قفل خودکار", "autolockstatus"];

    LimitOn Mod None false ["قفل لیمیت", "limit on"];
    LimitOff Mod None false ["بازکردن لیمیت", "باز کردن لیمیت", "limit off"];
    LimitCount Mod Required false ["تنظیم تعداد لیمیت", "limitcount"];
    LimitTime Mod Required false ["تنظیم زمان لیمیت", "limittime"];
    FloodOn Mod None false ["قفل رگبار", "flood on"];
    FloodOff Mod None false ["بازکردن رگبار", "باز کردن رگبار", "flood off"];
    FloodCount Mod Required false ["تعداد رگبار", "floodcount"];
    FloodMode Mod Required false ["تنظیم رگبار", "floodmode"];
    MaxChar Mod Required false ["حداکثر کاراکتر", "setmaxchar"];
    MinChar Mod Required false ["حداقل کاراکتر", "setminchar"];
    CharOff Mod None false ["بازکردن کاراکتر", "باز کردن کاراکتر", "unlock char"];
    StrictOn Mod Optional false ["قفل سختگیرانه", "strict on", "lock strict"];
    StrictOff Mod Optional false ["بازکردن سختگیرانه", "باز کردن سختگیرانه", "strict off", "unlock strict"];
    StrictMode Mod Required false ["وضعیت سختگیرانه", "strictmode"];
    StrictMute Mod Required false ["strictmute"];
    TabchiMode Mod Required false ["حالت تبچی", "tabchimode"];
    TabchiTime Mod Required false ["زمان تبچی", "tabchitime"];

    SetOwner Creator Optional false ["تنظیم مالک", "setowner"];
    RemOwner Creator Optional false ["حذف مالک", "remowner"];
    OwnerList Mod None false ["لیست مالکین", "لیست مالکان", "ownerlist"];
    CleanOwners Creator None false ["پاکسازی لیست مالکین", "clean ownerlist"];
    SetTempMod Owner Required false ["تنظیم مدیر موقت", "settempmod"];
    Promote Owner Optional false ["تنظیم مدیر", "promote"];
    Demote Owner Optional false ["حذف مدیر", "demote"];
    ModList Mod None false ["لیست مدیران", "modlist"];
    CleanMods Owner None false ["پاکسازی لیست مدیران", "clean modlist"];
    SetTempVip Owner Required false ["تنظیم ویژه موقت", "settempvip"];
    SetVip Owner Optional false ["تنظیم ویژه", "setvip"];
    RemVip Owner Optional false ["حذف ویژه", "remvip"];
    VipList Mod None false ["لیست ویژه", "viplist"];
    CleanVips Owner None false ["پاکسازی لیست ویژه", "clean viplist"];
    Exempt Owner Optional false ["معاف", "exempt"];
    RemExempt Owner Optional false ["حذف معاف", "remexempt"];
    ExemptList Mod None false ["لیست معاف", "exemptlist"];
    CleanExempt Owner None false ["پاکسازی لیست معاف", "clean exemptlist"];
    AddAdmin Owner Optional false ["افزودن ادمین", "addadmin"];
    RemAdmin Owner Optional false ["حذف ادمین", "remadmin"];
    AdminList Mod None false ["لیست ادمین ها", "لیست ادمینها", "adminlist"];

    BanPlus Owner Optional false ["بن+", "ban+"];
    Ban Mod Optional false ["بن", "ban"];
    UnBan Mod Optional false ["حذف بن", "unban"];
    BanList Mod None false ["لیست بن", "banlist"];
    CleanBans Mod None false ["پاکسازی لیست بن", "clean banlist"];
    Mute Mod Optional false ["سکوت", "silent", "mute"];
    UnMute Mod Optional false ["حذف سکوت", "unsilent", "unmute"];
    MuteList Mod None false ["لیست سکوت", "silentlist", "mutelist"];
    CleanMutes Mod None false ["پاکسازی لیست سکوت", "clean silentlist", "clean mutelist"];
    Kick Mod Optional false ["اخراج", "kick"];
    UnKick Mod Optional false ["حذف اخراج", "unkick"];
    Warn Mod Optional false ["اخطار", "warn"];
    RemWarns Mod Optional false ["حذف اخطارها", "حذف اخطار ها", "remwarns"];
    RemWarn Mod Optional false ["حذف اخطار", "remwarn"];
    SetWarnMax Mod Required false ["تعداد اخطار", "setwarn"];
    WarnBan Mod None false ["تنظیم اخطار بن", "setwarnban"];
    WarnMute Mod None false ["تنظیم اخطار سکوت", "setwarnmute"];
    WarnList Mod None false ["لیست اخطار", "warnlist"];
    CleanWarns Mod None false ["پاکسازی لیست اخطار", "clean warnlist"];
    UserPanel Mod Optional false ["پنل کاربر", "userpanel"];

    AddFilter Mod Required false ["فیلتر کردن", "addfilter"];
    FilterWarn Mod Required false ["فیلتر اخطار", "filterwarn"];
    FilterBan Mod Required false ["فیلتر بن", "filterban"];
    FilterMute Mod Required false ["فیلتر سکوت", "filtermute"];
    RemFilter Mod Required false ["حذف فیلتر", "remfilter"];
    FilterList Mod None false ["لیست فیلتر", "filterlist"];
    CleanFilters Mod None false ["پاکسازی لیست فیلتر", "clean filterlist"];
    FilterStart Mod None false ["فیلتر", "filter"];
    FilterEnd Mod None false ["پایان فیلتر", "filterend"];
    PanelPv Mod None false ["پنل پیوی", "panel pv"];

    Del Mod Required false ["حذف", "del"];
    AutoCleanSet Mod Required false ["تنظیم پاکسازی خودکار", "setautoclean"];
    AutoCleanRem Mod None false ["حذف پاکسازی خودکار", "remautoclean"];
    AutoCleanStatus Mod None false ["وضعیت پاکسازی خودکار", "autoclean stats", "autocleanstats"];
    CleanNicknames Mod None false ["پاکسازی لیست لقب", "clean nicknames"];
    CleanBlockList Mod None false ["پاکسازی لیست مسدود", "clean blocklist"];
    CleanRestrictList Mod None false ["پاکسازی لیست محدود", "clean restrictlist"];
    CleanBots Mod None false ["پاکسازی ربات ها", "پاکسازی رباتها", "clean bots"];
    CleanFake Mod None false ["پاکسازی لیست فیک", "clean fake"];
    CleanDeleted Mod None false ["پاکسازی لیست دیلیت", "clean deletelist"];

    ForceAddOn Mod None false ["اد اجباری فعال", "forceadd on"];
    ForceAddOff Mod None false ["اد اجباری غیرفعال", "forceadd off"];
    ForceAddCount Mod Required false ["تعداد اد اجباری", "setforceadd"];
    ForceAddTime Mod Required false ["زمان اد اجباری", "forceaddtime"];
    ForceAddText Mod Optional false ["تنظیم متن اد اجباری", "setforceaddtext"];
    ForceAddTextDel Mod None false ["حذف متن اد اجباری", "delforceaddtext"];
    ForceAddScope Mod Required false ["وضعیت اد اجباری", "forceaddstatus"];
    ForceAddReset Mod None false ["پاکسازی سابقه اد اجباری", "cleanforceadd"];
    ForceJoinOn Mod None false ["عضویت اجباری فعال", "forcejoin on"];
    ForceJoinOff Mod None false ["عضویت اجباری غیرفعال", "forcejoin off"];
    ForceJoinText Mod Optional false ["تنظیم متن عضویت اجباری", "setforcejointext"];
    ForceJoinTextDel Mod None false ["حذف متن عضویت اجباری", "delforcejointext"];
    ForceJoinStatus Mod None false ["وضعیت عضویت اجباری", "forcejoinstatus"];
    ForceJoinChannel Mod Required false ["تنظیم کانال عضویت اجباری", "setforcejoinchannel"];

    WelcomeOn Mod None false ["خوشامد روشن", "welcome on"];
    WelcomeOff Mod None false ["خوشامد خاموش", "welcome off"];
    SetWelcome Mod Optional false ["تنظیم خوشامد", "setwelcome"];
    GetWelcome Mod None false ["دریافت خوشامد", "getwelcome"];
    ResetWelcome Mod None false ["ریست خوشامد", "resetwelcome"];
    WelcomeTime Mod Required false ["تنظیم زمان خوشامد", "setwelcometime"];
    WelcomeMedia Mod None false ["تنظیم رسانه خوشامد", "setwelcomemedia"];
    WelcomeMediaDel Mod None false ["حذف رسانه خوشامد", "delwelcomemedia"];
    GoodbyeOn Mod None false ["خداحافظی روشن", "goodbye on"];
    GoodbyeOff Mod None false ["خداحافظی خاموش", "goodbye off"];
    SetGoodbye Mod Optional false ["تنظیم خداحافظی", "setgoodbye"];

    SetAnswer Mod Required false ["تنظیم پاسخ", "setanswer"];
    RemAnswer Mod Required false ["حذف پاسخ", "remanswer"];
    AnswerList Mod None false ["لیست پاسخ", "answerlist"];
    CleanAnswers Mod None false ["پاکسازی لیست پاسخ", "clean answerlist"];
    AnswerAudience Mod Required false ["دسترسی پاسخ", "answeraudience"];
    AnswerCooldown Mod Required false ["زمان پاسخ", "answercooldown"];

    Stats Mod Optional false ["آمار", "stats"];
    AddStats Mod None false ["آمار اد", "addstats"];
    RankStats Mod None false ["آمار مقامداران", "rankstats"];
    TotalStats Mod None false ["آمار کل", "totalstats"];
    WeeklyStats Mod None false ["آمار هفتگی", "weeklystats"];
    UserStats Member Optional true ["آمار کاربر", "userstats"];
    AutoStatsSet Mod Required false ["تنظیم آمار خودکار", "setautostats"];
    AutoStatsRem Mod None false ["حذف آمار خودکار", "remautostats"];
    AutoStatsStatus Mod None false ["وضعیت آمار خودکار", "statsstatus"];

    Captcha Mod Required false ["کپچا", "captcha"];
    CaptchaTime Mod Required false ["زمان کپچا", "captchatime"];
    AntiBetrayal Owner Required false ["ضد خیانت", "antibetrayal"];
    BetrayalLimit Owner Required false ["سقف خیانت", "betrayallimit"];
    DupHours Mod Required false ["زمان تکراری", "duptime"];
    DupLen Mod Required false ["طول تکراری", "duplen"];
    Report Member None false ["گزارش", "report"];
    ReportMode Mod Required false ["حالت گزارش", "reportmode"];
    DelReason Mod Required false ["دلیل حذف", "delreason"];
    SetLog Owner Required false ["تنظیم کانال لاگ", "setlog"];
    RemLog Owner None false ["حذف کانال لاگ", "remlog"];
    TimeZone Owner Required false ["تنظیم منطقه زمانی", "settimezone", "timezone"];
    SetText Owner Required false ["تنظیم متن", "settext"];
    RemText Owner Required false ["حذف متن", "deltext", "remtext"];
    TextKeys Owner None false ["لیست متن ها", "لیست متنها", "textkeys"];
    Bots Owner None false ["ربات ها", "رباتها", "لیست ربات ها", "bots"];

    Time Member None true ["ساعت", "time"];
    Echo Member Required true ["اکو", "بگوو", "echo"];
    Joke Member None true ["جوک", "joke"];
    Id Member Optional true ["آیدی", "ایدی", "اطلاعات", "id", "info"];
    JoinDate Member Optional true ["تاریخ عضویت", "joindate"];
    SetGlobalAsl Member Required true ["ثبت اصل سراسری", "setglobalasl"];
    SetAsl Member Required true ["ثبت اصل", "setasl"];
    MyAsl Member None true ["اصل من", "myasl"];
    Asl Member Optional true ["اصل", "asl"];
    DelAsl Member None true ["حذف اصل", "delasl"];
    Tag Mod Optional false ["تگ", "tag"];
    SetNickname Mod Required false ["تنظیم لقب", "setnickname"];
    Nickname Member Optional true ["لقب", "nickname"];
    DelNickname Mod Optional false ["حذف لقب", "delnickname"];
    NicknameList Member None true ["لیست لقب", "nicknamelist"];
    GetPro Member Optional true ["پروفایل کاربر", "getpro"];
    Rules Member None true ["قوانین", "rules"];
    SetRules Mod Optional false ["ثبت قوانین", "setrules"];
    DelRules Mod None false ["حذف قوانین", "delrules"];
    Pin Mod Optional false ["سنجاق", "pin"];
    Unpin Mod Optional false ["حذف سنجاق", "unpin"];
    Link Member None true ["لینک", "link"];
    SetLink Mod Required false ["ثبت لینک", "setlink"];
    DelLink Mod None false ["حذف لینک", "dellink"];
    CreateLink Mod None false ["ساخت لینک", "createlink"];
    SetPhoto Mod None false ["تنظیم عکس", "setphoto"];
    Font Member Required true ["فونت متن", "فونت", "font text", "font"];

    // Bot operators (see src/admin/): by reply, @username or id.
    GBan Sudo Optional false ["بن سراسری", "gban"];
    UnGBan Sudo Optional false ["حذف بن سراسری", "ungban"];
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Cmd(C),
    /// "قفل X" / "lock X" (true) or "بازکردن X" / "unlock X" (false).
    Lock(Lock, bool),
}

#[derive(Debug, Clone)]
pub struct Parsed {
    pub kind: Kind,
    pub rank: Rank,
    pub fun: bool,
    /// Normalized argument words.
    pub args: Vec<String>,
    /// The arguments as typed (original case, digits and line breaks).
    pub raw: String,
    /// Byte offset of `raw` in the original text, to keep its formatting.
    pub raw_offset: usize,
}

impl Parsed {
    pub fn arg(&self, index: usize) -> Option<&str> {
        self.args.get(index).map(String::as_str)
    }

    pub fn args_joined(&self) -> String {
        self.args.join(" ")
    }
}

fn phrase_index() -> &'static Vec<(Vec<String>, usize)> {
    static INDEX: OnceLock<Vec<(Vec<String>, usize)>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index: Vec<(Vec<String>, usize)> = SPECS
            .iter()
            .enumerate()
            .flat_map(|(i, spec)| spec.phrases.iter().map(move |p| (tokenize(p).into_iter().map(|t| t.norm).collect(), i)))
            .collect();
        index.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        index
    })
}

fn lock_index() -> &'static Vec<(Vec<String>, Lock)> {
    static INDEX: OnceLock<Vec<(Vec<String>, Lock)>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index: Vec<(Vec<String>, Lock)> = LOCKS
            .iter()
            .flat_map(|info| info.names.iter().map(move |n| (tokenize(n).into_iter().map(|t| t.norm).collect(), info.lock)))
            .collect();
        index.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        index
    })
}

/// Parses a message into a command, or `None` for ordinary chat.
/// The bot a slash command is addressed to: `bot` in `/cmd@bot ...`.
pub fn addressee(text: &str) -> Option<&str> {
    let (_, target) = text.split_whitespace().next()?.strip_prefix('/')?.split_once('@')?;
    Some(target).filter(|t| !t.is_empty())
}

pub fn parse(text: &str, bot_username: &str) -> Option<Parsed> {
    let tokens = tokenize(text);
    let first = tokens.first()?;
    let mut words: Vec<String> = tokens.iter().map(|t| t.norm.clone()).collect();
    if let Some(rest) = first.norm.strip_prefix('/') {
        let (name, target) = rest.split_once('@').unwrap_or((rest, ""));
        if !target.is_empty() && !target.eq_ignore_ascii_case(bot_username) {
            return None; // addressed to another bot
        }
        words[0] = normalize_word(name);
    }
    let raw_from = |k: usize| tokens.get(k).map(|t| text[t.start..].trim_end().to_owned()).unwrap_or_default();
    let offset_of = |k: usize| tokens.get(k).map_or(text.len(), |t| t.start);

    for (phrase, spec_index) in phrase_index() {
        let k = phrase.len();
        if words.len() < k || words[..k] != phrase[..] {
            continue;
        }
        let spec = &SPECS[*spec_index];
        let ok = match spec.args {
            Args::None => words.len() == k,
            Args::Required => words.len() > k,
            Args::Optional => true,
        };
        if ok {
            return Some(Parsed {
                kind: Kind::Cmd(spec.cmd),
                rank: spec.rank,
                fun: spec.fun,
                args: words[k..].to_vec(),
                raw: raw_from(k),
                raw_offset: offset_of(k),
            });
        }
    }

    // Generic lock grammar: "قفل X [duration]" / "lock X", "بازکردن X" / "unlock X".
    let (on, skip) = match (words.first().map(String::as_str), words.get(1).map(String::as_str)) {
        (Some("قفل" | "lock"), _) => (true, 1),
        (Some("بازکردن" | "unlock"), _) => (false, 1),
        (Some("باز"), Some("کردن")) => (false, 2),
        _ => return None,
    };
    let rest = &words[skip..];
    for (name, lock) in lock_index() {
        let k = name.len();
        if rest.len() >= k && rest[..k] == name[..] {
            // Only media accepts a duration after the name.
            if rest.len() > k && *lock != Lock::Media {
                continue;
            }
            return Some(Parsed {
                kind: Kind::Lock(*lock, on),
                rank: Rank::Mod,
                fun: false,
                args: rest[k..].to_vec(),
                raw: raw_from(skip + k),
                raw_offset: offset_of(skip + k),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_addressed_bot() {
        assert_eq!(addressee("/panel@OtherBot"), Some("OtherBot"));
        assert_eq!(addressee("/ban@MyBot 12345"), Some("MyBot"));
        assert_eq!(addressee("/panel"), None);
        assert_eq!(addressee("/panel@"), None);
        assert_eq!(addressee("hi @OtherBot"), None);
        assert_eq!(addressee(""), None);
    }

    fn cmd(text: &str) -> Option<C> {
        match parse(text, "MyBot")?.kind {
            Kind::Cmd(c) => Some(c),
            Kind::Lock(..) => None,
        }
    }

    #[test]
    fn matches_longest_phrase() {
        assert_eq!(cmd("حذف بن"), Some(C::UnBan));
        assert_eq!(cmd("حذف 50"), Some(C::Del));
        assert_eq!(cmd("حذف ۵۰"), Some(C::Del));
        assert_eq!(cmd("حذف اخطارها"), Some(C::RemWarns));
        assert_eq!(cmd("حذف اخطار"), Some(C::RemWarn));
        assert_eq!(cmd("تنظیم مدیر موقت 2 d"), Some(C::SetTempMod));
        assert_eq!(cmd("تنظیم مدیر"), Some(C::Promote));
        assert_eq!(cmd("آمار اد"), Some(C::AddStats));
        assert_eq!(cmd("آمار 30 نفر"), Some(C::Stats));
        assert_eq!(cmd("بن+"), Some(C::BanPlus));
        assert_eq!(cmd("پاکسازی لیست بن"), Some(C::CleanBans));
    }

    #[test]
    fn english_and_slash_commands() {
        assert_eq!(cmd("Ban"), Some(C::Ban));
        assert_eq!(cmd("/ban@MyBot"), Some(C::Ban));
        assert_eq!(cmd("/ban@OtherBot"), None);
        assert_eq!(cmd("Clean BanList"), Some(C::CleanBans));
        assert_eq!(cmd("Limit On"), Some(C::LimitOn));
        assert_eq!(cmd("FloodMode TMute"), Some(C::FloodMode));
    }

    #[test]
    fn exact_commands_ignore_chat() {
        assert_eq!(cmd("لینک"), Some(C::Link));
        assert_eq!(cmd("لینک رو بفرست"), None);
        assert_eq!(cmd("ساعت"), Some(C::Time));
        assert_eq!(cmd("ساعت چنده"), None);
    }

    #[test]
    fn parses_locks() {
        let lock = |text: &str| parse(text, "b").map(|p| (p.kind, p.args));
        assert_eq!(lock("قفل لینک"), Some((Kind::Lock(Lock::Link, true), vec![])));
        assert_eq!(lock("Unlock Link"), Some((Kind::Lock(Lock::Link, false), vec![])));
        assert_eq!(lock("باز‌کردن استیکر متحرک"), Some((Kind::Lock(Lock::AnimatedSticker, false), vec![])));
        assert_eq!(lock("قفل رسانه ۶ ساعت"), Some((Kind::Lock(Lock::Media, true), vec!["6".into(), "ساعت".into()])));
        assert_eq!(lock("قفل لینک در بیو"), Some((Kind::Lock(Lock::Bio, true), vec![])));
        assert_eq!(lock("Lock JustEmoji"), Some((Kind::Lock(Lock::JustEmoji, true), vec![])));
        assert_eq!(cmd("قفل گروه 10 دقیقه"), Some(C::GroupLock));
        assert_eq!(cmd("قفل رسانه موقت 15"), Some(C::TempMediaOn));
        assert_eq!(cmd("قفل سختگیرانه لینک"), Some(C::StrictOn));
    }

    #[test]
    fn keeps_raw_arguments() {
        let parsed = parse("تنظیم خوشامد سلام !mention\nخوش آمدی", "b").unwrap();
        assert_eq!(parsed.raw, "سلام !mention\nخوش آمدی");
    }

    #[test]
    fn phrases_are_unique() {
        let mut seen = std::collections::HashMap::new();
        for (phrase, index) in phrase_index() {
            if let Some(other) = seen.insert(phrase.clone(), *index) {
                panic!("phrase {phrase:?} is used by {:?} and {:?}", SPECS[other].cmd, SPECS[*index].cmd);
            }
        }
    }
}
