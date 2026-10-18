//! Logs in interactively and lists your most recent chats.
//!
//! ```sh
//! export TELEGRAM_API_ID=12345 TELEGRAM_API_HASH=0123456789abcdef0123456789abcdef
//! cargo run --example login
//! ```
//!
//! The session is stored in `./tdlib`, so later runs log in without prompting.

use tdrust::prelude::*;
use tdrust::types::ChatListMain;

#[tokio::main]
async fn main() -> tdrust::Result<()> {
    let (client, updates) = Client::builder().database_directory("tdlib").build()?;
    // This example only makes requests; dropping the stream discards updates.
    drop(updates);

    client.authorize(ConsoleAuthenticator::new()).await?;
    let me = client.get_me().await?;
    println!("Logged in as {} {} (id {})", me.first_name, me.last_name, me.id);

    // Make sure the main chat list is loaded before reading it. TDLib answers
    // 404 once every chat has been loaded.
    if let Err(err) = client.load_chats(Some(ChatListMain {}.into()), 20).await {
        if err.as_td().map(|e| e.code) != Some(404) {
            return Err(err);
        }
    }
    let chats = client.get_chats(Some(ChatListMain {}.into()), 20).await?;
    println!("\nYour {} most recent chats:", chats.chat_ids.len());
    for chat_id in chats.chat_ids {
        let chat = client.get_chat(chat_id).await?;
        println!("  {:>16}  {}", chat.id, chat.title);
    }

    client.shutdown().await;
    Ok(())
}
