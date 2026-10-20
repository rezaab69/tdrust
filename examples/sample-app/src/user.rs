//! User mode: logs in with your own account, lists your recent chats, and
//! prints incoming messages as they arrive.

use std::path::Path;

use tdrust::prelude::*;
use tdrust::types::ChatListMain;

pub async fn run(data_dir: &Path) -> Result<(), tdrust::Error> {
    let (client, mut updates) = Client::builder()
        .database_directory(data_dir)
        .device_model("tdrust sample app")
        .application_version(env!("CARGO_PKG_VERSION"))
        .build()?;

    log::info!("Logging in with your account (session: {})", data_dir.display());
    client.authorize(ConsoleAuthenticator::new()).await?;
    let me = client.get_me().await?;
    println!("\nLogged in as {} {} (user id {}).", me.first_name, me.last_name, me.id);

    // TDLib loads chats lazily; ask for the first page before reading the
    // list. It answers 404 when there is nothing more to load.
    if let Err(err) = client.load_chats(Some(ChatListMain {}.into()), 10).await {
        if err.as_td().map(|td| td.code) != Some(404) {
            return Err(err);
        }
    }
    let chats = client.get_chats(Some(ChatListMain {}.into()), 10).await?;
    println!("\nYour {} most recent chats:", chats.chat_ids.len());
    for chat_id in chats.chat_ids {
        let chat = client.get_chat(chat_id).await?;
        println!("  {:>16}  {}", chat.id, chat.title);
    }
    println!("\nWaiting for new messages. Press Ctrl+C to stop.\n");

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        tokio::select! {
            update = updates.next() => match update {
                Some(Update::NewMessage(update)) if !update.message.is_outgoing => {
                    let message = update.message;
                    // TDLib caches chats, so this lookup is cheap.
                    let title = match client.get_chat(message.chat_id).await {
                        Ok(chat) => chat.title,
                        Err(_) => message.chat_id.to_string(),
                    };
                    let text = message
                        .text()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("<{}>", crate::bot::describe(&message.content)));
                    println!("[{title}] {text}");
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
    Ok(())
}
