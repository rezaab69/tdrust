mod common;

use common::{client, error, tdlib_like, within, MockTd, Reply};
use serde_json::json;
use tdrust::functions::GetChats;
use tdrust::types::{AuthorizationState, ChatListMain, OptionValue, Update};
use tdrust::{Error, Runtime};

fn chats() -> Reply {
    Reply::Response(json!({"@type": "chats", "total_count": 2, "chat_ids": [10, -1001234567890i64]}))
}

#[tokio::test]
async fn generated_methods_send_typed_requests() {
    let mock = tdlib_like(|_, request| match request["@type"].as_str() {
        Some("getChats") => vec![chats()],
        _ => vec![],
    });
    let (client, _updates) = client(&mock);

    let result = within(client.get_chats(Some(ChatListMain {}.into()), 20)).await.unwrap();
    assert_eq!(result.total_count, 2);
    assert_eq!(result.chat_ids, [10, -1001234567890]);

    assert_eq!(
        mock.request("getChats").unwrap(),
        json!({"@type": "getChats", "chat_list": {"@type": "chatListMain"}, "limit": 20})
    );
}

#[tokio::test]
async fn request_structs_and_raw_json() {
    let mock = tdlib_like(|_, request| match request["@type"].as_str() {
        Some("getChats") => vec![chats()],
        _ => vec![],
    });
    let (client, _updates) = client(&mock);

    let typed = within(client.send(GetChats { limit: 5, ..Default::default() })).await.unwrap();
    assert_eq!(typed.total_count, 2);
    assert_eq!(mock.request("getChats").unwrap(), json!({"@type": "getChats", "limit": 5}));

    let raw = within(client.send_json(json!({"@type": "getChats", "limit": 1}))).await.unwrap();
    assert_eq!(raw["@type"], "chats");
    assert!(raw.get("@extra").is_none() && raw.get("@client_id").is_none());

    let invalid = client.send_json(json!({"limit": 1})).await.unwrap_err();
    assert!(matches!(invalid, Error::InvalidRequest(_)), "{invalid:?}");
}

#[tokio::test]
async fn tdlib_errors_become_td_errors() {
    let mock = tdlib_like(|_, _| vec![error(400, "CHAT_NOT_FOUND")]);
    let (client, _updates) = client(&mock);

    let err = within(client.get_chat(42)).await.unwrap_err();
    let td = err.as_td().expect("a TDLib error");
    assert_eq!((td.code, td.message.as_str()), (400, "CHAT_NOT_FOUND"));
}

#[tokio::test]
async fn mismatched_responses_report_decode_errors() {
    let mock = tdlib_like(|_, _| vec![Reply::Response(json!({"@type": "chats", "total_count": "many"}))]);
    let (client, _updates) = client(&mock);

    match within(client.get_chats(None, 1)).await.unwrap_err() {
        Error::Decode { function, json, .. } => {
            assert_eq!(function, "getChats");
            assert_eq!(json["total_count"], "many");
        }
        other => panic!("unexpected error {other:?}"),
    }
}

#[tokio::test]
async fn updates_are_routed_to_their_client() {
    let mock = tdlib_like(|_, _| vec![]);
    let runtime = Runtime::with_backend(mock.clone());
    let build = || tdrust::Client::builder().api_id(1).api_hash("hash").runtime(runtime.clone()).build().unwrap();
    let (first, mut first_updates) = build();
    let (second, mut second_updates) = build();
    assert_ne!(first.id(), second.id());

    // Each client first sees its own WaitTdlibParameters.
    for updates in [&mut first_updates, &mut second_updates] {
        let update = within(updates.next()).await.unwrap();
        assert!(matches!(update, Update::AuthorizationState(_)), "{update:?}");
    }

    mock.push_update(
        second.id(),
        json!({"@type": "updateOption", "name": "my_id", "value": {"@type": "optionValueInteger", "value": "77"}}),
    );
    match within(second_updates.next()).await.unwrap() {
        Update::Option(update) => {
            assert_eq!(update.name, "my_id");
            assert!(matches!(update.value, OptionValue::Integer(ref value) if value.value == 77));
        }
        other => panic!("unexpected update {other:?}"),
    }
    assert!(first_updates.try_next().is_none());
}

#[tokio::test]
async fn unknown_updates_are_kept_as_json() {
    let mock = tdlib_like(|_, _| vec![]);
    let (client, mut updates) = client(&mock);
    within(updates.next()).await.unwrap(); // WaitTdlibParameters

    mock.push_update(client.id(), json!({"@type": "updateFromTheFuture", "answer": 42}));
    let update = within(updates.next()).await.unwrap();
    assert_eq!(update.type_name(), "updateFromTheFuture");
    match update {
        Update::Unrecognized(value) => assert_eq!(value["answer"], 42),
        other => panic!("unexpected update {other:?}"),
    }
}

#[tokio::test]
async fn shutdown_closes_the_client_and_ends_the_stream() {
    let mock = tdlib_like(|_, _| vec![]);
    let (client, mut updates) = client(&mock);

    within(client.shutdown()).await;
    assert!(client.is_closed());
    assert!(matches!(client.authorization_state(), Some(AuthorizationState::Closed(_))));

    let mut states = Vec::new();
    while let Some(update) = within(updates.next()).await {
        if let Update::AuthorizationState(update) = update {
            states.push(update.authorization_state.type_name().to_owned());
        }
    }
    assert_eq!(states.last().map(String::as_str), Some("authorizationStateClosed"));

    let err = client.get_chats(None, 1).await.unwrap_err();
    assert!(matches!(err, Error::ClientClosed(id) if id == client.id()), "{err:?}");
}

#[tokio::test]
async fn pending_requests_fail_when_the_client_closes() {
    // `getChats` is never answered.
    let mock = tdlib_like(|_, _| vec![]);
    let (client, _updates) = client(&mock);

    let pending = tokio::spawn({
        let client = client.clone();
        async move { client.get_chats(None, 1).await }
    });
    within(async {
        while mock.request("getChats").is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    mock.push_update(
        client.id(),
        json!({"@type": "updateAuthorizationState", "authorization_state": {"@type": "authorizationStateClosed"}}),
    );

    let err = within(pending).await.unwrap().unwrap_err();
    assert!(matches!(err, Error::ClientClosed(_)), "{err:?}");
}

#[tokio::test]
async fn dropping_the_last_handle_closes_the_client() {
    let mock = tdlib_like(|_, _| vec![]);
    let (client, mut updates) = client(&mock);
    let copy = client.clone();
    drop(client);
    assert!(mock.request("close").is_none(), "a clone is still alive");
    drop(copy);
    assert!(mock.request("close").is_some());
    while within(updates.next()).await.is_some() {}
}

#[tokio::test]
async fn missing_credentials_are_reported() {
    let runtime = Runtime::with_backend(MockTd::new(|_, _| vec![]));
    let err = tdrust::Client::builder().api_hash("hash").runtime(runtime).build();
    // TELEGRAM_API_ID may be set in a developer's environment; only check
    // the message when it is not.
    if std::env::var_os("TELEGRAM_API_ID").is_none() {
        let err = err.unwrap_err();
        assert!(err.to_string().contains("TELEGRAM_API_ID"), "{err}");
    }
}

#[test]
fn execute_runs_synchronously() {
    let runtime = Runtime::with_backend(MockTd::new(|_, _| vec![]));
    let version = runtime.execute(tdrust::functions::GetOption { name: "version".into() }).unwrap();
    assert!(matches!(version, OptionValue::String(ref value) if value.value == "1.8.67"));

    let err = runtime.execute(tdrust::functions::GetTextEntities { text: "x".into() }).unwrap_err();
    assert_eq!(err.as_td().map(|e| e.code), Some(400));
}
