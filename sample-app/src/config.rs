//! Settings from the environment and the optional `sample-app/.env` file.

use std::env;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

/// How often a value is asked for before giving up.
const MAX_ATTEMPTS: usize = 5;

/// The placeholders in `.env.example`. A copy of that file that was never
/// edited counts as "not filled in".
const EXAMPLE_API_ID: &str = "12345678";
const EXAMPLE_API_HASH: &str = "0123456789abcdef0123456789abcdef";
const EXAMPLE_BOT_TOKEN: &str = "123456789:AAAbbbCCCdddEEEfffGGGhhhIIIjjjKKKll";

/// Where the `.env` file is looked for, regardless of the working directory.
pub fn dotenv_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(".env")
}

/// Loads `sample-app/.env` into the environment, without overriding
/// variables that are already set. Returns whether the file was found.
///
/// Must run before any other thread starts, because it modifies the
/// process environment.
pub fn load_dotenv() -> Result<bool, String> {
    let path = dotenv_path();
    let found = match dotenvy::from_path(&path) {
        Ok(()) => true,
        Err(dotenvy::Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => return Err(format!("cannot read {}: {err}", path.display())),
    };

    // Allow `TDJSON_PATH=~/...` in the file; the shell would expand it, but
    // `.env` files are not processed by a shell.
    if let Some(path) = env::var("TDJSON_PATH").ok().and_then(|path| expand_home(&path)) {
        env::set_var("TDJSON_PATH", path);
    }
    Ok(found)
}

/// The directory where TDLib keeps the session of `mode` ("bot" or "user").
///
/// Defaults to `~/.tdrust-sample/<mode>`. It lives in the home directory
/// rather than the project because TDLib's databases need a native Linux
/// file system when running under WSL.
pub fn data_dir(mode: &str) -> Result<PathBuf, String> {
    let base = match env::var("DATA_DIR") {
        Ok(dir) => PathBuf::from(expand_home(&dir).unwrap_or(dir)),
        Err(_) => home_dir()?.join(".tdrust-sample"),
    };
    Ok(base.join(mode))
}

/// The bot token for bot mode.
pub fn bot_token() -> Result<String, String> {
    match env::var("TELEGRAM_BOT_TOKEN") {
        Ok(token) if !token.trim().is_empty() => Ok(token.trim().to_owned()),
        _ => Err(format!(
            "TELEGRAM_BOT_TOKEN is not set. Create a bot with @BotFather and put its token in {}",
            dotenv_path().display()
        )),
    }
}

/// Whether `value` is a usable Telegram API id.
pub fn valid_api_id(value: &str) -> bool {
    let value = value.trim();
    value != EXAMPLE_API_ID && value.parse::<i32>().is_ok_and(|id| id > 0)
}

/// Whether `value` is a usable Telegram API hash.
pub fn valid_api_hash(value: &str) -> bool {
    let value = value.trim();
    value != EXAMPLE_API_HASH && value.len() == 32
}

/// Whether `value` looks like a bot token (`<digits>:<letters, digits, - or _>`).
pub fn valid_bot_token(value: &str) -> bool {
    let value = value.trim();
    let Some((id, secret)) = value.split_once(':') else {
        return false;
    };
    value != EXAMPLE_BOT_TOKEN
        && !id.is_empty()
        && id.bytes().all(|byte| byte.is_ascii_digit())
        && !secret.is_empty()
        && secret.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// A setting the app cannot start without.
struct Setting {
    key: &'static str,
    help: &'static str,
    /// Typed without echo, and never printed.
    secret: bool,
    is_valid: fn(&str) -> bool,
    expected: &'static str,
}

impl Setting {
    /// Whether the environment already holds a valid value.
    fn is_set(&self) -> bool {
        env::var(self.key).is_ok_and(|value| (self.is_valid)(&value))
    }
}

static API_ID: Setting = Setting {
    key: "TELEGRAM_API_ID",
    help: "Your Telegram API id: a number from https://my.telegram.org > API development tools.",
    secret: false,
    is_valid: valid_api_id,
    expected: "a number such as 1234567",
};

static API_HASH: Setting = Setting {
    key: "TELEGRAM_API_HASH",
    help: "Your Telegram API hash: 32 characters, from the same page.",
    secret: true,
    is_valid: valid_api_hash,
    expected: "exactly 32 characters",
};

static BOT_TOKEN: Setting = Setting {
    key: "TELEGRAM_BOT_TOKEN",
    help: "Your bot's token from @BotFather (send /newbot, or /token for an existing bot).",
    secret: true,
    is_valid: valid_bot_token,
    expected: "a token like 123456789:AAAbbbCCC...",
};

/// Asks for the required settings that are missing or malformed, so the app
/// works without a complete `.env`. `need_bot_token` adds the bot token.
///
/// Does nothing when standard input is not a terminal (for example under
/// systemd); the later steps then report what is missing. Offers to save the
/// answers to `.env` so they are asked for only once.
///
/// Must run before any other thread starts, because it modifies the process
/// environment.
pub fn prompt_missing(need_bot_token: bool) -> Result<(), String> {
    let mut wanted = vec![&API_ID, &API_HASH];
    if need_bot_token {
        wanted.push(&BOT_TOKEN);
    }
    let missing: Vec<&Setting> = wanted.into_iter().filter(|setting| !setting.is_set()).collect();
    if missing.is_empty() || !io::stdin().is_terminal() {
        return Ok(());
    }

    println!("Some required settings are missing. Enter them now (Ctrl+C to cancel).\n");
    let mut entered = Vec::new();
    for setting in missing {
        let value = ask(setting)?;
        env::set_var(setting.key, &value);
        entered.push((setting.key, value));
    }
    offer_to_save(&entered);
    Ok(())
}

/// Asks for one value until it is valid.
fn ask(setting: &Setting) -> Result<String, String> {
    println!("{}", setting.help);
    for _ in 0..MAX_ATTEMPTS {
        let input = if setting.secret {
            rpassword::prompt_password(format!("{} (input is hidden): ", setting.key))
                .map_err(|err| format!("cannot read {}: {err}", setting.key))?
        } else {
            read_line(&format!("{}: ", setting.key))?
        };
        let value = input.trim().to_owned();
        if (setting.is_valid)(&value) {
            println!();
            return Ok(value);
        }
        println!("  That does not look right; expected {}.", setting.expected);
    }
    Err(format!("no valid value for {} after {MAX_ATTEMPTS} attempts", setting.key))
}

fn read_line(prompt: &str) -> Result<String, String> {
    print!("{prompt}");
    io::stdout().flush().map_err(|err| format!("cannot write to the terminal: {err}"))?;
    let mut line = String::new();
    match io::stdin().lock().read_line(&mut line) {
        Ok(0) => Err("input closed before all settings were entered".to_owned()),
        Ok(_) => Ok(line),
        Err(err) => Err(format!("cannot read input: {err}")),
    }
}

/// Asks whether to keep the answers in `.env`. A failure to save is only a
/// warning: the app can still run with the values it has.
fn offer_to_save(entered: &[(&str, String)]) {
    let path = dotenv_path();
    let answer = read_line(&format!("Save these to {} so you are not asked again? [Y/n] ", path.display()))
        .unwrap_or_else(|_| "n".to_owned());
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no") {
        println!("Not saved; you will be asked again next time.\n");
        return;
    }
    match save_dotenv(&path, entered) {
        Ok(()) => println!("Saved to {}.\n", path.display()),
        Err(err) => eprintln!("warning: {err}\n"),
    }
}

/// Writes `entered` into the `.env` file at `path`, keeping its other lines.
/// A new file is readable by its owner only, since it holds secrets.
fn save_dotenv(path: &Path, entered: &[(&str, String)]) -> Result<(), String> {
    let existing = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("cannot read {}: {err}", path.display())),
    };
    let updated = upsert_env(&existing, entered);

    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .and_then(|mut file| file.write_all(updated.as_bytes()))
        .map_err(|err| format!("cannot write {}: {err}", path.display()))
}

/// Sets each `KEY=value` in the text of an `.env` file: replaces the existing
/// assignment, or appends one. Comments and other lines are left alone.
fn upsert_env(existing: &str, entered: &[(&str, String)]) -> String {
    let mut lines: Vec<String> = existing.lines().map(str::to_owned).collect();
    for (key, value) in entered {
        let assignment = format!("{key}={value}");
        let slot = lines.iter().position(|line| {
            let line = line.trim_start();
            !line.starts_with('#') && line.split_once('=').is_some_and(|(name, _)| name.trim() == *key)
        });
        match slot {
            Some(index) => lines[index] = assignment,
            None => lines.push(assignment),
        }
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

fn home_dir() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| "cannot find the home directory; set DATA_DIR".to_owned())
}

fn expand_home(path: &str) -> Option<String> {
    let rest = path.strip_prefix("~/")?;
    Some(home_dir().ok()?.join(rest).to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_id_must_be_a_positive_number_and_not_the_example() {
        assert!(valid_api_id("1234567"));
        assert!(valid_api_id(" 987654 "));
        assert!(!valid_api_id(""));
        assert!(!valid_api_id("abc"));
        assert!(!valid_api_id("-5"));
        assert!(!valid_api_id(EXAMPLE_API_ID));
    }

    #[test]
    fn api_hash_must_have_32_characters_and_not_be_the_example() {
        assert!(valid_api_hash(&"a".repeat(32)));
        assert!(!valid_api_hash(&"a".repeat(31)));
        assert!(!valid_api_hash(""));
        assert!(!valid_api_hash(EXAMPLE_API_HASH));
    }

    #[test]
    fn bot_token_needs_digits_a_colon_and_a_secret() {
        assert!(valid_bot_token("123456789:AAEhBP0-xx_yy"));
        assert!(valid_bot_token("  123456789:AAEhBP0-xx_yy\n"));
        assert!(!valid_bot_token(""));
        assert!(!valid_bot_token("123456789"));
        assert!(!valid_bot_token("123456789:"));
        assert!(!valid_bot_token(":AAEhBP0"));
        assert!(!valid_bot_token("abc:AAEhBP0"));
        assert!(!valid_bot_token("123456789:has space"));
        assert!(!valid_bot_token(EXAMPLE_BOT_TOKEN));
    }

    #[test]
    fn upsert_replaces_an_empty_assignment_and_keeps_other_lines() {
        let existing = "# comment\nTELEGRAM_API_ID=\nTDJSON_PATH=/x\n";
        let entered = [("TELEGRAM_API_ID", "42".to_owned()), ("TELEGRAM_API_HASH", "h".to_owned())];
        assert_eq!(
            upsert_env(existing, &entered),
            "# comment\nTELEGRAM_API_ID=42\nTDJSON_PATH=/x\nTELEGRAM_API_HASH=h\n"
        );
    }

    #[test]
    fn upsert_does_not_touch_commented_assignments() {
        let entered = [("TELEGRAM_API_ID", "42".to_owned())];
        assert_eq!(upsert_env("# TELEGRAM_API_ID=1\n", &entered), "# TELEGRAM_API_ID=1\nTELEGRAM_API_ID=42\n");
    }

    #[test]
    fn upsert_into_an_empty_file() {
        let entered = [("TELEGRAM_BOT_TOKEN", "1:a".to_owned())];
        assert_eq!(upsert_env("", &entered), "TELEGRAM_BOT_TOKEN=1:a\n");
    }
}
