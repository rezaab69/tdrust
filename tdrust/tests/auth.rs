mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use common::{auth_state, client, error, ok, tdlib_like, within, MockTd, Reply};
use serde_json::json;
use tdrust::types::{AuthenticationCodeInfo, AuthorizationState, AuthorizationStateWaitPassword};
use tdrust::{AuthStep, Authenticator, BotAuthenticator, Error, Login, Result, TdError};

#[tokio::test]
async fn bot_login_sends_parameters_and_token() {
    let mock = tdlib_like(|_, request| match request["@type"].as_str() {
        Some("checkAuthenticationBotToken") if request["token"] == "123:good" => {
            vec![ok(), auth_state("authorizationStateReady")]
        }
        Some("checkAuthenticationBotToken") => vec![error(400, "ACCESS_TOKEN_INVALID")],
        _ => vec![],
    });
    let (client, _updates) = client(&mock);

    within(client.authorize(BotAuthenticator::new("123:good"))).await.unwrap();
    assert!(matches!(client.authorization_state(), Some(AuthorizationState::Ready(_))));

    let parameters = mock.request("setTdlibParameters").unwrap();
    assert_eq!(parameters["api_id"], 94575);
    assert_eq!(parameters["api_hash"], "a3406de8d171bb422bb6ddf3bbd800e2");
    assert_eq!(parameters["database_directory"], "test-db");
    assert_eq!(parameters["database_encryption_key"], "");
    assert_eq!(parameters["use_message_database"], true);
}

#[tokio::test]
async fn bot_login_fails_on_rejected_token() {
    let mock = tdlib_like(|_, request| match request["@type"].as_str() {
        Some("checkAuthenticationBotToken") => vec![error(400, "ACCESS_TOKEN_INVALID")],
        _ => vec![],
    });
    let (client, _updates) = client(&mock);

    let err = within(client.authorize(BotAuthenticator::new("123:bad"))).await.unwrap_err();
    assert_eq!(err.as_td().map(|e| e.message.as_str()), Some("ACCESS_TOKEN_INVALID"));
}

/// Answers with scripted values and counts rejected codes.
struct Scripted {
    codes: Vec<&'static str>,
    rejected: Arc<AtomicUsize>,
}

impl Authenticator for Scripted {
    async fn login(&mut self) -> Result<Login> {
        Ok(Login::PhoneNumber("+15550001111".into()))
    }

    async fn code(&mut self, info: &AuthenticationCodeInfo) -> Result<String> {
        assert_eq!(info.phone_number, "+15550001111");
        Ok(self.codes.remove(0).to_owned())
    }

    async fn password(&mut self, state: &AuthorizationStateWaitPassword) -> Result<String> {
        assert_eq!(state.password_hint, "pet");
        Ok("hunter2".into())
    }

    async fn on_error(&mut self, step: AuthStep, error: &TdError) -> Result<()> {
        assert_eq!((step, error.message.as_str()), (AuthStep::Code, "PHONE_CODE_INVALID"));
        self.rejected.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn user_login_retries_rejected_codes_and_asks_for_password() {
    let mock = tdlib_like(|_, request| match request["@type"].as_str() {
        Some("setAuthenticationPhoneNumber") => vec![
            ok(),
            Reply::Update(json!({
                "@type": "updateAuthorizationState",
                "authorization_state": {
                    "@type": "authorizationStateWaitCode",
                    "code_info": {
                        "@type": "authenticationCodeInfo",
                        "phone_number": request["phone_number"],
                        "type": {"@type": "authenticationCodeTypeTelegramMessage", "length": 5},
                        "timeout": 60,
                    },
                },
            })),
        ],
        Some("checkAuthenticationCode") if request["code"] == "12345" => vec![
            ok(),
            Reply::Update(json!({
                "@type": "updateAuthorizationState",
                "authorization_state": {
                    "@type": "authorizationStateWaitPassword",
                    "password_hint": "pet",
                    "has_recovery_email_address": false,
                    "has_passport_data": false,
                    "recovery_email_address_pattern": "",
                },
            })),
        ],
        Some("checkAuthenticationCode") => vec![error(400, "PHONE_CODE_INVALID")],
        Some("checkAuthenticationPassword") if request["password"] == "hunter2" => {
            vec![ok(), auth_state("authorizationStateReady")]
        }
        _ => vec![error(400, "UNEXPECTED")],
    });
    let (client, _updates) = client(&mock);
    let rejected = Arc::new(AtomicUsize::new(0));

    let authenticator = Scripted { codes: vec!["00000", "12345"], rejected: rejected.clone() };
    within(client.authorize(authenticator)).await.unwrap();

    assert_eq!(rejected.load(Ordering::SeqCst), 1);
    let codes: Vec<_> = mock
        .requests()
        .into_iter()
        .filter(|(_, request)| request["@type"] == "checkAuthenticationCode")
        .map(|(_, request)| request["code"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(codes, ["00000", "12345"]);
}

#[tokio::test]
async fn existing_session_needs_no_credentials() {
    // TDLib found a saved session: the parameters lead straight to Ready.
    let mock = MockTd::new(|_, request| match request["@type"].as_str() {
        Some("getOption") => vec![auth_state("authorizationStateWaitTdlibParameters")],
        Some("setTdlibParameters") => vec![ok(), auth_state("authorizationStateReady")],
        _ => vec![],
    });
    let (client, _updates) = client(&mock);

    struct NeverAsked;
    impl Authenticator for NeverAsked {
        async fn login(&mut self) -> Result<Login> {
            panic!("login must not be requested when a session exists");
        }
    }
    within(client.authorize(NeverAsked)).await.unwrap();
}

#[tokio::test]
async fn unsupported_steps_fail_with_a_clear_message() {
    let mock = tdlib_like(|_, request| match request["@type"].as_str() {
        Some("setAuthenticationPhoneNumber") => vec![ok(), auth_state("authorizationStateWaitEmailAddress")],
        _ => vec![],
    });
    let (client, _updates) = client(&mock);

    struct PhoneOnly;
    impl Authenticator for PhoneOnly {
        async fn login(&mut self) -> Result<Login> {
            Ok(Login::PhoneNumber("+15550001111".into()))
        }
    }
    let err = within(client.authorize(PhoneOnly)).await.unwrap_err();
    assert!(matches!(err, Error::Authorization(ref message) if message.contains("email address")), "{err:?}");
}
