//! The generated types must produce and accept exactly TDLib's JSON.

use serde_json::{json, Value};
use tdrust::functions::{SendMessage, SetTdlibParameters};
use tdrust::types::{
    AuthorizationState, FormattedText, InputMessageContent, MessageSender, RichText, RichTextBold, RichTextPlain,
    TextEntity, TextEntityTypeBold, Update,
};

fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) -> Value {
    let json = serde_json::to_value(value).unwrap();
    assert_eq!(&serde_json::from_value::<T>(json.clone()).unwrap(), value);
    json
}

#[test]
fn structs_carry_their_type_tag() {
    let text = FormattedText {
        text: "hello".into(),
        entities: vec![TextEntity { offset: 0, length: 5, r#type: TextEntityTypeBold {}.into() }],
    };
    assert_eq!(
        round_trip(&text),
        json!({
            "@type": "formattedText",
            "text": "hello",
            "entities": [{"@type": "textEntity", "offset": 0, "length": 5, "type": {"@type": "textEntityTypeBold"}}],
        })
    );
}

#[test]
fn enums_serialize_as_their_variant() {
    let content = InputMessageContent::text("hi");
    assert_eq!(
        round_trip(&content),
        json!({
            "@type": "inputMessageText",
            "text": {"@type": "formattedText", "text": "hi", "entities": []},
            "clear_draft": false,
        })
    );
}

#[test]
fn recursive_types_are_boxed() {
    let bold = RichText::from(RichTextBold { text: Box::new(RichTextPlain { text: "b".into() }.into()) });
    assert_eq!(round_trip(&bold), json!({"@type": "richTextBold", "text": {"@type": "richTextPlain", "text": "b"}}));
}

#[test]
fn functions_serialize_with_their_name_and_options() {
    let request = SendMessage {
        chat_id: -100123,
        input_message_content: InputMessageContent::text("hi"),
        topic_id: None,
        reply_to: None,
        options: None,
        reply_markup: None,
    };
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["@type"], "sendMessage");
    assert_eq!(json["chat_id"], -100123);
    assert!(json.get("reply_to").is_none(), "unset optional fields are omitted: {json}");
}

#[test]
fn bytes_are_base64() {
    let parameters = SetTdlibParameters { database_encryption_key: vec![1, 2, 3], ..Default::default() };
    let json = serde_json::to_value(&parameters).unwrap();
    assert_eq!(json["database_encryption_key"], "AQID");
}

#[test]
fn decodes_realistic_updates() {
    let update: Update = serde_json::from_value(json!({
        "@type": "updateAuthorizationState",
        "authorization_state": {
            "@type": "authorizationStateWaitOtherDeviceConfirmation",
            "link": "tg://login?token=abc",
        },
    }))
    .unwrap();
    match update {
        Update::AuthorizationState(update) => match update.authorization_state {
            AuthorizationState::WaitOtherDeviceConfirmation(state) => assert_eq!(state.link, "tg://login?token=abc"),
            other => panic!("unexpected state {other:?}"),
        },
        other => panic!("unexpected update {other:?}"),
    }

    let sender: MessageSender = serde_json::from_value(json!({"@type": "messageSenderUser", "user_id": 5})).unwrap();
    assert_eq!((sender.user_id(), sender.chat_id()), (Some(5), None));
}

#[test]
fn unknown_objects_round_trip_as_json() {
    let raw = json!({"@type": "authorizationStateSomethingNew", "detail": [1, 2]});
    let state: AuthorizationState = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(state.type_name(), "authorizationStateSomethingNew");
    assert_eq!(serde_json::to_value(&state).unwrap(), raw);
}

#[test]
fn missing_type_tags_are_rejected_for_enums() {
    let err = serde_json::from_value::<AuthorizationState>(json!({"link": "x"})).unwrap_err();
    assert!(err.to_string().contains("@type"), "{err}");
}
