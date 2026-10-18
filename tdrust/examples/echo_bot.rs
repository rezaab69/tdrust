//! A bot that echoes text messages back, with graceful Ctrl+C shutdown.
//!
//! ```sh
//! export TELEGRAM_API_ID=12345 TELEGRAM_API_HASH=0123456789abcdef0123456789abcdef
//! export TELEGRAM_BOT_TOKEN=123456:ABC-DEF...
//! cargo run --example echo_bot
//! ```

use tdrust::prelude::*;
use tdrust::types::FormattedText;

#[tokio::main]
async fn main() -> tdrust::Result<()> {
    let token = std::env::var("TELEGRAM_BOT_TOKEN")
        .map_err(|_| tdrust::Error::Config("set TELEGRAM_BOT_TOKEN to a token from @BotFather".into()))?;

    let (client, mut updates) = Client::builder().database_directory("tdlib-bot").build()?;
    client.authorize(BotAuthenticator::new(token)).await?;
    let me = client.get_me().await?;
    println!("@{} is running; press Ctrl+C to stop.", me.usernames.map(|u| u.editable_username).unwrap_or_default());

    loop {
        tokio::select! {
            update = updates.next() => match update {
                Some(Update::NewMessage(update)) => {
                    let message = update.message;
                    if message.is_outgoing {
                        continue;
                    }
                    let Some(text) = message.text() else { continue };
                    let reply = if text == "/start" {
                        FormattedText::markdown("Hi\\! Send me anything and I'll *echo* it\\.")?
                    } else {
                        FormattedText::plain(text)
                    };
                    // Handle each message in its own task so a slow reply
                    // never holds up the update loop.
                    let client = client.clone();
                    tokio::spawn(async move {
                        if let Err(err) = client.reply_text(&message, reply).await {
                            eprintln!("cannot reply in chat {}: {err}", message.chat_id);
                        }
                    });
                }
                Some(_) => {}
                None => break,
            },
            _ = tokio::signal::ctrl_c() => {
                println!("Shutting down...");
                client.shutdown().await;
                break;
            }
        }
    }
    Ok(())
}
