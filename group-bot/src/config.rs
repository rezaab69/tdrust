//! Settings read from the environment (and the optional `.env` file).

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub bot_token: String,
    pub database_url: String,
    pub database_max_connections: u32,
    pub redis_url: String,
    /// Prefix for every Redis key, so several bots can share one Redis.
    pub redis_prefix: String,
    /// The bot's creators: full control in every group and of the bot
    /// itself; they can add more sudo users from the operator panel.
    pub sudo_ids: Vec<i64>,
    /// TDLib session directory.
    pub tdlib_dir: String,
    /// Language for new groups.
    pub default_lang: String,
    /// Backups and broadcast media. Bots on one machine should share it, so
    /// every bot can send media of a broadcast created on another.
    pub data_dir: std::path::PathBuf,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

impl Config {
    pub fn from_env() -> Result<Config> {
        let Some(bot_token) = var("TELEGRAM_BOT_TOKEN") else {
            bail!("TELEGRAM_BOT_TOKEN is not set; create a bot with @BotFather");
        };
        let Some(database_url) = var("DATABASE_URL") else {
            bail!("DATABASE_URL is not set, e.g. postgres://bot:bot@localhost:5432/groupbot");
        };
        let sudo_ids = var("BOT_SUDO_IDS")
            .unwrap_or_default()
            .split([',', ' '])
            .filter(|id| !id.is_empty())
            .map(|id| id.parse::<i64>().with_context(|| format!("BOT_SUDO_IDS: `{id}` is not a user id")))
            .collect::<Result<Vec<_>>>()?;
        Ok(Config {
            bot_token,
            database_url,
            database_max_connections: var("DATABASE_MAX_CONNECTIONS")
                .map(|v| v.parse().context("DATABASE_MAX_CONNECTIONS must be a number"))
                .transpose()?
                .unwrap_or(10),
            redis_url: var("REDIS_URL").unwrap_or_else(|| "redis://127.0.0.1:6379".into()),
            redis_prefix: var("REDIS_PREFIX").unwrap_or_else(|| "gb:".into()),
            sudo_ids,
            tdlib_dir: var("TDLIB_DATA_DIR").unwrap_or_else(|| "tdlib-group-bot".into()),
            default_lang: var("DEFAULT_LANG").unwrap_or_else(|| "fa".into()),
            data_dir: var("DATA_DIR").unwrap_or_else(|| "group-bot-data".into()).into(),
        })
    }
}
