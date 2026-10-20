//! A sample Telegram application built with tdrust.
//!
//! ```text
//! sample-app check   verify that TDLib and the settings are in place
//! sample-app bot     run a bot that answers commands
//! sample-app user    log in with your own account and watch incoming messages
//! ```
//!
//! See `examples/sample-app/README.md` for the setup steps.

mod bot;
mod config;
mod user;

use std::process::ExitCode;

const USAGE: &str = "\
Usage: sample-app <MODE>

Modes:
  check   Verify that TDLib can be loaded and the settings are in place
  bot     Run a bot that answers commands (needs TELEGRAM_BOT_TOKEN)
  user    Log in with your own account, list your chats and print new messages

Settings are read from the environment and from examples/sample-app/.env. In a
terminal, `check`, `bot` and `user` ask for any required setting that is missing.
See examples/sample-app/README.md for step-by-step instructions.";

fn main() -> ExitCode {
    // Read `.env` before any thread exists: it modifies the environment.
    let dotenv = config::load_dotenv();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .format_target(false)
        .init();
    match dotenv {
        Ok(true) => log::debug!("loaded {}", config::dotenv_path().display()),
        Ok(false) => log::debug!("no {} file; using the environment only", config::dotenv_path().display()),
        Err(err) => return fail(err),
    }

    let Some(mode) = std::env::args().nth(1) else {
        println!("{USAGE}");
        return ExitCode::FAILURE;
    };

    // Ask for missing settings in a terminal. Like `.env` above, this modifies
    // the environment, so it runs before the async runtime starts its threads.
    let need_bot_token = match mode.as_str() {
        "bot" => Some(true),
        "check" | "user" => Some(false),
        _ => None,
    };
    if let Some(need_bot_token) = need_bot_token {
        if let Err(err) = config::prompt_missing(need_bot_token) {
            return fail(err);
        }
    }

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => return fail(format!("cannot start the async runtime: {err}")),
    };

    let result = match mode.as_str() {
        "check" => check(),
        "bot" => match (config::data_dir("bot"), config::bot_token()) {
            (Ok(dir), Ok(token)) => runtime.block_on(bot::run(&dir, token)).map_err(explain),
            (Err(err), _) | (_, Err(err)) => Err(err),
        },
        "user" => match config::data_dir("user") {
            Ok(dir) => runtime.block_on(user::run(&dir)).map_err(explain),
            Err(err) => Err(err),
        },
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown mode `{other}`\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => fail(err),
    }
}

fn fail(message: String) -> ExitCode {
    eprintln!("\nerror: {message}");
    ExitCode::FAILURE
}

/// Adds a hint on how to fix the most common setup problems.
fn explain(err: tdrust::Error) -> String {
    let hint = match &err {
        tdrust::Error::Library(_) => {
            "\n\nhint: set TDJSON_PATH in examples/sample-app/.env to the full path of libtdjson (see README step 3)"
        }
        tdrust::Error::Config(_) => "\n\nhint: fill in examples/sample-app/.env (see README step 4)",
        tdrust::Error::Td(td) if td.message.contains("API_ID") => {
            "\n\nhint: TELEGRAM_API_ID or TELEGRAM_API_HASH is wrong; copy them again from https://my.telegram.org"
        }
        tdrust::Error::Td(td) if td.message.contains("TOKEN") => {
            "\n\nhint: the bot token was rejected; get the current one from @BotFather (/token). If you \
             revoked it, also delete the old session: rm -rf ~/.tdrust-sample/bot"
        }
        _ => "",
    };
    format!("{err}{hint}")
}

/// Checks the setup without connecting to Telegram.
fn check() -> Result<(), String> {
    let mut problems = Vec::new();
    let mut report = |ok: bool, what: &str, detail: String| {
        println!("  [{}] {what}: {detail}", if ok { " OK " } else { "FAIL" });
        if !ok {
            problems.push(what.to_owned());
        }
    };

    println!("Checking the sample app setup...\n");
    match tdrust::tdlib_version() {
        Ok(version) => {
            let matches = version == tdrust::BUNDLED_TDLIB_VERSION;
            let note = if matches {
                String::new()
            } else {
                format!(" (bindings are for {}; most things still work)", tdrust::BUNDLED_TDLIB_VERSION)
            };
            report(true, "TDLib", format!("version {version}{note}"));
        }
        Err(err) => report(false, "TDLib", err.to_string()),
    }

    let api_id = std::env::var("TELEGRAM_API_ID").unwrap_or_default();
    let api_id_ok = config::valid_api_id(&api_id);
    report(
        api_id_ok,
        "TELEGRAM_API_ID",
        if api_id_ok { api_id } else { "missing, not a number, or still the example value".into() },
    );

    let api_hash = std::env::var("TELEGRAM_API_HASH").unwrap_or_default();
    let api_hash_ok = config::valid_api_hash(&api_hash);
    report(
        api_hash_ok,
        "TELEGRAM_API_HASH",
        if api_hash_ok { "set".into() } else { "missing, not 32 characters, or still the example value".into() },
    );

    let token = config::bot_token();
    println!(
        "  [{}] TELEGRAM_BOT_TOKEN: {}",
        if token.is_ok() { " OK " } else { "----" },
        if token.is_ok() { "set" } else { "not set (only needed for bot mode)" }
    );

    for mode in ["bot", "user"] {
        if let Ok(dir) = config::data_dir(mode) {
            println!("  [info] {mode} session directory: {}", dir.display());
        }
    }

    if problems.is_empty() {
        println!("\nEverything is in place. Run `cargo run -- bot` or `-- user`.");
        Ok(())
    } else {
        Err(format!("fix these settings first: {}", problems.join(", ")))
    }
}
