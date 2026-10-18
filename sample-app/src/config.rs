//! Settings from the environment and the optional `sample-app/.env` file.

use std::env;
use std::path::{Path, PathBuf};

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
