//! Runs a real TDLib instance up to the login prompt. Needs the library and
//! no Telegram account:
//!
//! ```sh
//! TDJSON_PATH=/path/to/libtdjson.so cargo test --test live -- --ignored
//! ```

use std::time::Duration;

use tdrust::types::{AuthorizationState, Update};
use tdrust::{Authenticator, Client, Error, Login, Result};

/// Stops the login as soon as TDLib asks for credentials.
struct StopAtLogin;

impl Authenticator for StopAtLogin {
    async fn login(&mut self) -> Result<Login> {
        Err(Error::Authorization("stop here".into()))
    }
}

#[tokio::test]
#[ignore = "needs TDLib; set TDJSON_PATH and run with --ignored"]
async fn real_tdlib_lifecycle() {
    let directory = std::env::temp_dir().join(format!("tdrust-live-{}", std::process::id()));
    let (client, mut updates) = Client::builder()
        .api_id(1)
        .api_hash("00000000000000000000000000000000")
        .database_directory(&directory)
        .use_message_database(false)
        .build()
        .expect("TDLib must be loadable; set TDJSON_PATH");

    let timeout = Duration::from_secs(30);
    let err = tokio::time::timeout(timeout, client.authorize(StopAtLogin)).await.unwrap().unwrap_err();
    assert!(matches!(err, Error::Authorization(_)), "{err:?}");
    assert!(matches!(client.authorization_state(), Some(AuthorizationState::WaitPhoneNumber(_))));

    // Typed requests work before login for offline functions.
    let entities = client.get_text_entities("see https://telegram.org").await.unwrap();
    assert_eq!(entities.entities.len(), 1);
    let error = client.get_chat(1).await.unwrap_err();
    assert!(error.as_td().is_some(), "{error:?}");

    tokio::time::timeout(timeout, client.shutdown()).await.unwrap();

    // Every update TDLib produced must decode into a typed variant.
    let mut count = 0;
    while let Some(update) = tokio::time::timeout(timeout, updates.next()).await.unwrap() {
        count += 1;
        assert!(!matches!(update, Update::Unrecognized(_)), "undecodable update: {update:?}");
    }
    assert!(count > 3, "only {count} updates received");
    let _ = std::fs::remove_dir_all(directory);
}
