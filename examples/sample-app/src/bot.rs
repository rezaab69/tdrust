//! Bot mode: answers commands and echoes everything else.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tdrust::prelude::*;
use tdrust::types::{FormattedText, MessageContent};

/// The `/help` text, in MarkdownV2. Characters with a meaning in MarkdownV2
/// (`.`, `-`, `(`, `)`, `!`, ...) are escaped with a backslash.
pub const HELP: &str = "*tdrust sample bot*\n\
\n\
/help – show this message\n\
/id – show the ids of this chat and of you\n\
/whoami – show what Telegram knows about you\n\
/echo _text_ – repeat _text_\n\
/md _text_ – render _text_ as MarkdownV2, e\\.g\\. `/md *bold* _italic_`\n\
/stats – show uptime and how many messages I have handled\n\
\n\
Anything else is echoed back\\.";

struct Stats {
    started: Instant,
    messages: AtomicU64,
}

pub async fn run(data_dir: &Path, token: String) -> Result<(), tdrust::Error> {
    let (client, mut updates) = Client::builder()
        .database_directory(data_dir)
        .device_model("tdrust sample bot")
        .application_version(env!("CARGO_PKG_VERSION"))
        .build()?;

    log::info!("Logging in as a bot (session: {})", data_dir.display());
    client.authorize(BotAuthenticator::new(token)).await?;
    let me = client.get_me().await?;
    let username = me.usernames.map(|names| names.editable_username).unwrap_or_default();
    log::info!("Running as @{username}. Open https://t.me/{username} and send /start. Press Ctrl+C to stop.");

    let stats = Arc::new(Stats { started: Instant::now(), messages: AtomicU64::new(0) });
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            update = updates.next() => match update {
                Some(Update::NewMessage(update)) if !update.message.is_outgoing => {
                    // Answer in a separate task so that a slow reply never
                    // holds up the handling of other updates.
                    tokio::spawn(handle(client.clone(), update.message, stats.clone()));
                }
                Some(_) => {}
                None => break,
            },
            _ = &mut ctrl_c => {
                log::info!("Stopping...");
                client.shutdown().await;
                break;
            }
        }
    }
    log::info!("Stopped.");
    Ok(())
}

async fn handle(client: Client, message: Message, stats: Arc<Stats>) {
    stats.messages.fetch_add(1, Ordering::Relaxed);
    log::info!(
        "chat {}: {}",
        message.chat_id,
        message.text().map(str::to_owned).unwrap_or_else(|| format!("<{}>", describe(&message.content)))
    );

    let reply = match reply_to(&client, &message, &stats).await {
        Ok(reply) => reply,
        Err(err) => {
            log::warn!("cannot answer message {} in chat {}: {err}", message.id, message.chat_id);
            FormattedText::plain(format!("Sorry, something went wrong: {err}"))
        }
    };
    if let Err(err) = client.reply_text(&message, reply).await {
        log::warn!("cannot reply in chat {}: {err}", message.chat_id);
    }
}

async fn reply_to(client: &Client, message: &Message, stats: &Stats) -> tdrust::Result<FormattedText> {
    let Some(text) = message.text() else {
        return Ok(FormattedText::plain(format!(
            "I can only read text, but that was a {}.",
            describe(&message.content)
        )));
    };

    let reply = match parse_command(text) {
        None => FormattedText::plain(text),
        Some(("start" | "help", _)) => FormattedText::markdown(HELP)?,
        Some(("id", _)) => {
            let sender = match message.sender_user_id() {
                Some(user_id) => format!("Your user id: {user_id}"),
                None => "This message was sent on behalf of a chat.".to_owned(),
            };
            FormattedText::plain(format!("Chat id: {}\n{sender}", message.chat_id))
        }
        Some(("whoami", _)) => match message.sender_user_id() {
            Some(user_id) => {
                let user = client.get_user(user_id).await?;
                let username = user.usernames.map(|names| format!("@{}", names.editable_username));
                FormattedText::plain(format!(
                    "Name: {} {}\nUsername: {}\nLanguage: {}\nPremium: {}",
                    user.first_name,
                    user.last_name,
                    username.as_deref().unwrap_or("none"),
                    if user.language_code.is_empty() { "unknown" } else { &user.language_code },
                    if user.is_premium { "yes" } else { "no" },
                ))
            }
            None => FormattedText::plain("This message was sent on behalf of a chat."),
        },
        Some(("echo", "")) => FormattedText::plain("Usage: /echo some text"),
        Some(("echo", args)) => FormattedText::plain(args),
        Some(("md", "")) => FormattedText::plain("Usage: /md *bold* _italic_ `code`"),
        Some(("md", args)) => match FormattedText::markdown(args) {
            Ok(text) => text,
            Err(err) => FormattedText::plain(format!(
                "That is not valid MarkdownV2: {}\nCharacters like . - ( ) ! must be escaped with \\.",
                err.as_td().map_or_else(|| err.to_string(), |td| td.message.clone())
            )),
        },
        Some(("stats", _)) => FormattedText::plain(format!(
            "Uptime: {}\nMessages handled: {}",
            format_duration(stats.started.elapsed()),
            stats.messages.load(Ordering::Relaxed)
        )),
        Some((command, _)) => FormattedText::plain(format!("Unknown command /{command}. Send /help for the list.")),
    };
    Ok(reply)
}

/// Splits `/command@bot_name arguments` into `("command", "arguments")`.
pub fn parse_command(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix('/')?;
    let (word, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let command = word.split('@').next().unwrap_or(word);
    if command.is_empty() {
        return None;
    }
    Some((command, args.trim()))
}

/// A readable name for a kind of message content: `messageVoiceNote` becomes
/// "voice note".
pub fn describe(content: &MessageContent) -> String {
    let name = content.type_name().strip_prefix("message").unwrap_or(content.type_name());
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_uppercase() && !out.is_empty() {
            out.push(' ');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{}h {}m {}s", seconds / 3600, seconds / 60 % 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tdrust::types::{MessageVoiceNote, VoiceNote};

    #[test]
    fn parses_commands() {
        assert_eq!(parse_command("/start"), Some(("start", "")));
        assert_eq!(parse_command("/echo  hello world "), Some(("echo", "hello world")));
        assert_eq!(parse_command("/id@MyBot"), Some(("id", "")));
        assert_eq!(parse_command("/md@MyBot *x*"), Some(("md", "*x*")));
        assert_eq!(parse_command("hello"), None);
        assert_eq!(parse_command("/"), None);
    }

    #[test]
    fn describes_content() {
        let voice = MessageContent::from(MessageVoiceNote {
            voice_note: VoiceNote::default(),
            caption: FormattedText::default(),
            is_listened: false,
        });
        assert_eq!(describe(&voice), "voice note");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(Duration::from_secs(3723)), "1h 2m 3s");
    }

    /// Needs TDLib: `TDJSON_PATH=... cargo test -- --ignored`.
    #[test]
    #[ignore = "needs TDLib"]
    fn help_is_valid_markdown() {
        let help = FormattedText::markdown(HELP).expect("HELP must be valid MarkdownV2");
        assert!(help.text.starts_with("tdrust sample bot"));
        assert!(!help.entities.is_empty());
    }
}
