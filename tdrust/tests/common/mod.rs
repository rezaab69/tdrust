//! A scripted stand-in for TDLib.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tdrust::{Backend, Client, Runtime, Updates};

/// What the mock does in reaction to a request.
pub enum Reply {
    /// Answers the request with this object.
    Response(Value),
    /// Pushes an update for the requesting client.
    Update(Value),
}

type Handler = dyn Fn(i32, &Value) -> Vec<Reply> + Send + Sync;

#[derive(Clone)]
pub struct MockTd {
    inner: Arc<Inner>,
}

struct Inner {
    next_id: AtomicI32,
    outbox: Mutex<VecDeque<String>>,
    ready: Condvar,
    handler: Box<Handler>,
    requests: Mutex<Vec<(i32, Value)>>,
}

impl MockTd {
    pub fn new(handler: impl Fn(i32, &Value) -> Vec<Reply> + Send + Sync + 'static) -> Self {
        MockTd {
            inner: Arc::new(Inner {
                next_id: AtomicI32::new(1),
                outbox: Mutex::new(VecDeque::new()),
                ready: Condvar::new(),
                handler: Box::new(handler),
                requests: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Pushes an update for `client_id` as if TDLib produced it spontaneously.
    pub fn push_update(&self, client_id: i32, mut update: Value) {
        update["@client_id"] = client_id.into();
        self.push(update);
    }

    /// All requests received so far, with the `@extra` field removed.
    pub fn requests(&self) -> Vec<(i32, Value)> {
        self.inner.requests.lock().unwrap().clone()
    }

    /// The first request of the given type.
    pub fn request(&self, ty: &str) -> Option<Value> {
        self.requests().into_iter().map(|(_, request)| request).find(|request| request["@type"] == ty)
    }

    fn push(&self, message: Value) {
        self.inner.outbox.lock().unwrap().push_back(message.to_string());
        self.inner.ready.notify_all();
    }
}

impl Backend for MockTd {
    fn create_client_id(&self) -> i32 {
        self.inner.next_id.fetch_add(1, Ordering::SeqCst)
    }

    fn send(&self, client_id: i32, request: &str) {
        let mut request: Value = serde_json::from_str(request).unwrap();
        let extra = request.as_object_mut().unwrap().remove("@extra");
        self.inner.requests.lock().unwrap().push((client_id, request.clone()));

        for reply in (self.inner.handler)(client_id, &request) {
            match reply {
                Reply::Response(mut response) => {
                    response["@client_id"] = client_id.into();
                    if let Some(extra) = &extra {
                        response["@extra"] = extra.clone();
                    }
                    self.push(response);
                }
                Reply::Update(update) => self.push_update(client_id, update),
            }
        }
    }

    fn receive(&self, timeout: Duration) -> Option<String> {
        let outbox = self.inner.outbox.lock().unwrap();
        let (mut outbox, _) = self.inner.ready.wait_timeout_while(outbox, timeout, |queue| queue.is_empty()).unwrap();
        outbox.pop_front()
    }

    fn execute(&self, request: &str) -> Option<String> {
        let request: Value = serde_json::from_str(request).unwrap();
        let response = match request["@type"].as_str() {
            Some("getOption") => json!({"@type": "optionValueString", "value": "1.8.67"}),
            _ => json!({"@type": "error", "code": 400, "message": "Unsupported"}),
        };
        Some(response.to_string())
    }
}

pub fn ok() -> Reply {
    Reply::Response(json!({"@type": "ok"}))
}

pub fn error(code: i32, message: &str) -> Reply {
    Reply::Response(json!({"@type": "error", "code": code, "message": message}))
}

pub fn auth_state(state: &str) -> Reply {
    Reply::Update(json!({
        "@type": "updateAuthorizationState",
        "authorization_state": {"@type": state},
    }))
}

/// A mock that behaves like a fresh TDLib instance up to `WaitPhoneNumber`,
/// closes on `close`, and delegates everything else to `rest`.
pub fn tdlib_like(rest: impl Fn(i32, &Value) -> Vec<Reply> + Send + Sync + 'static) -> MockTd {
    MockTd::new(move |client_id, request| match request["@type"].as_str().unwrap() {
        "getOption" => vec![
            auth_state("authorizationStateWaitTdlibParameters"),
            Reply::Response(json!({"@type": "optionValueString", "value": "1.8.67"})),
        ],
        "setTdlibParameters" => vec![ok(), auth_state("authorizationStateWaitPhoneNumber")],
        "close" => vec![ok(), auth_state("authorizationStateClosing"), auth_state("authorizationStateClosed")],
        _ => rest(client_id, request),
    })
}

pub fn client(mock: &MockTd) -> (Client, Updates) {
    Client::builder()
        .api_id(94575)
        .api_hash("a3406de8d171bb422bb6ddf3bbd800e2")
        .database_directory("test-db")
        .runtime(Runtime::with_backend(mock.clone()))
        .build()
        .unwrap()
}

/// Fails a test that would otherwise hang.
pub async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future).await.expect("timed out")
}
