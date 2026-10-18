use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};

use crate::functions::SetTdlibParameters;
use crate::runtime::{decode, encode, Runtime};
use crate::types::{AuthorizationState, Update};
use crate::{Error, Function, Result};

/// A TDLib client: one Telegram account or bot.
///
/// `Client` is cheap to clone and can be shared between tasks. Every TDLib
/// function is available as an async method (for example
/// [`get_me`](Client::get_me) or [`send_message`](Client::send_message));
/// [`send`](Client::send) accepts the corresponding request structs from
/// [`functions`](crate::functions), and [`send_json`](Client::send_json)
/// takes raw JSON.
///
/// When the last clone is dropped, the TDLib instance is closed. Call
/// [`shutdown`](Client::shutdown) to close it and wait until TDLib has
/// flushed its databases.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    id: i32,
    runtime: Runtime,
    auth: watch::Receiver<Option<AuthorizationState>>,
    parameters: SetTdlibParameters,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if !is_closed(&self.auth) {
            // Nobody can observe the response any more; ignore it.
            let _ = self.runtime.request(self.id, json!({"@type": "close"}));
        }
    }
}

fn is_closed(auth: &watch::Receiver<Option<AuthorizationState>>) -> bool {
    matches!(*auth.borrow(), Some(AuthorizationState::Closed(_)))
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("id", &self.inner.id)
            .field("authorization_state", &self.authorization_state().map(|state| state.type_name().to_owned()))
            .finish()
    }
}

impl Client {
    /// Starts configuring a new client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// TDLib's identifier for this client.
    pub fn id(&self) -> i32 {
        self.inner.id
    }

    /// The runtime this client belongs to.
    pub fn runtime(&self) -> &Runtime {
        &self.inner.runtime
    }

    /// Sends a request and waits for its typed response.
    ///
    /// ```no_run
    /// # async fn run(client: tdrust::Client) -> tdrust::Result<()> {
    /// use tdrust::functions::GetChats;
    ///
    /// let chats = client.send(GetChats { limit: 20, ..Default::default() }).await?;
    /// println!("{} chats", chats.chat_ids.len());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send<F: Function>(&self, function: F) -> Result<F::Output> {
        let request = encode(&function)?;
        drop(function);
        decode::<F>(self.send_json(request).await?)
    }

    /// Sends a raw JSON request and waits for the raw response.
    ///
    /// This is an escape hatch for functions newer than the schema tdrust
    /// was built with. The request must be an object with an `@type` field;
    /// any `@extra` field is replaced. TDLib `error` responses become
    /// [`Error::Td`].
    pub async fn send_json(&self, request: Value) -> Result<Value> {
        let response = self.inner.runtime.request(self.inner.id, request)?;
        response.await.map_err(|_| Error::ClientClosed(self.inner.id))?
    }

    /// The latest authorization state, or `None` before TDLib reported one.
    pub fn authorization_state(&self) -> Option<AuthorizationState> {
        self.inner.auth.borrow().clone()
    }

    /// Whether TDLib has closed this client. A closed client rejects all
    /// requests with [`Error::ClientClosed`].
    pub fn is_closed(&self) -> bool {
        is_closed(&self.inner.auth)
    }

    /// Waits until TDLib has closed this client.
    pub async fn wait_closed(&self) {
        let mut auth = self.inner.auth.clone();
        loop {
            if matches!(*auth.borrow_and_update(), Some(AuthorizationState::Closed(_))) {
                return;
            }
            if auth.changed().await.is_err() {
                return;
            }
        }
    }

    /// Closes this client and waits until TDLib has finished closing it.
    /// The session is kept, so the next run logs in without prompting.
    pub async fn shutdown(&self) {
        if !self.is_closed() {
            // `close` fails only if the client is already closing.
            let _ = self.close().await;
        }
        self.wait_closed().await;
    }

    pub(crate) fn parameters(&self) -> &SetTdlibParameters {
        &self.inner.parameters
    }

    pub(crate) fn auth_receiver(&self) -> watch::Receiver<Option<AuthorizationState>> {
        self.inner.auth.clone()
    }
}

/// The stream of updates TDLib pushes for one client.
///
/// Updates are queued without limit until they are read, so read them
/// continuously, or drop the stream to discard them. The stream ends when
/// the client is closed.
///
/// ```no_run
/// # async fn run(mut updates: tdrust::Updates) {
/// use tdrust::types::Update;
///
/// while let Some(update) = updates.next().await {
///     if let Update::NewMessage(update) = update {
///         println!("{:?}", update.message.text());
///     }
/// }
/// # }
/// ```
///
/// `Updates` also implements [`futures_core::Stream`], so stream combinators
/// from `futures` or `tokio-stream` work with it.
#[derive(Debug)]
pub struct Updates {
    client_id: i32,
    receiver: mpsc::UnboundedReceiver<Update>,
}

impl Updates {
    /// Waits for the next update. Returns `None` once the client is closed.
    pub async fn next(&mut self) -> Option<Update> {
        self.receiver.recv().await
    }

    /// Returns the next update if one is already queued.
    pub fn try_next(&mut self) -> Option<Update> {
        self.receiver.try_recv().ok()
    }

    /// The identifier of the client these updates belong to.
    pub fn client_id(&self) -> i32 {
        self.client_id
    }
}

impl futures_core::Stream for Updates {
    type Item = Update;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Update>> {
        self.receiver.poll_recv(cx)
    }
}

/// Configures and creates a [`Client`].
///
/// Only the API credentials are required; get them at
/// <https://my.telegram.org>. When they are not set on the builder, they are
/// read from the `TELEGRAM_API_ID` and `TELEGRAM_API_HASH` environment
/// variables.
///
/// ```no_run
/// # fn run() -> tdrust::Result<()> {
/// let (client, updates) = tdrust::Client::builder()
///     .api_id(12345)
///     .api_hash("0123456789abcdef0123456789abcdef")
///     .database_directory("sessions/alice")
///     .build()?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
#[must_use = "a builder does nothing until `build` is called"]
pub struct ClientBuilder {
    runtime: Option<Runtime>,
    api_id: Option<i32>,
    api_hash: Option<String>,
    parameters: SetTdlibParameters,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        ClientBuilder {
            runtime: None,
            api_id: None,
            api_hash: None,
            parameters: SetTdlibParameters {
                use_test_dc: false,
                database_directory: "tdlib".into(),
                files_directory: String::new(),
                database_encryption_key: Vec::new(),
                use_file_database: true,
                use_chat_info_database: true,
                use_message_database: true,
                use_secret_chats: false,
                api_id: 0,
                api_hash: String::new(),
                system_language_code: "en".into(),
                device_model: "tdrust".into(),
                system_version: std::env::consts::OS.into(),
                application_version: env!("CARGO_PKG_VERSION").into(),
            },
        }
    }
}

impl ClientBuilder {
    /// Application identifier from <https://my.telegram.org>.
    pub fn api_id(mut self, api_id: i32) -> Self {
        self.api_id = Some(api_id);
        self
    }

    /// Application hash from <https://my.telegram.org>.
    pub fn api_hash(mut self, api_hash: impl Into<String>) -> Self {
        self.api_hash = Some(api_hash.into());
        self
    }

    /// Where TDLib keeps its databases, including the login session.
    /// Defaults to `tdlib` in the working directory. Use a different
    /// directory for every account.
    pub fn database_directory(mut self, path: impl Into<PathBuf>) -> Self {
        self.parameters.database_directory = path_to_string(path.into());
        self
    }

    /// Where downloaded files are stored. Defaults to the database directory.
    pub fn files_directory(mut self, path: impl Into<PathBuf>) -> Self {
        self.parameters.files_directory = path_to_string(path.into());
        self
    }

    /// Key used to encrypt the local database. Empty (the default) means no
    /// encryption.
    pub fn database_encryption_key(mut self, key: impl Into<Vec<u8>>) -> Self {
        self.parameters.database_encryption_key = key.into();
        self
    }

    /// Connects to Telegram's test servers instead of production.
    pub fn use_test_dc(mut self, value: bool) -> Self {
        self.parameters.use_test_dc = value;
        self
    }

    /// Keeps information about downloaded and uploaded files between runs
    /// (default: `true`).
    pub fn use_file_database(mut self, value: bool) -> Self {
        self.parameters.use_file_database = value;
        self
    }

    /// Caches users, basic groups, supergroups, channels and secret chats
    /// between runs (default: `true`).
    pub fn use_chat_info_database(mut self, value: bool) -> Self {
        self.parameters.use_chat_info_database = value;
        self
    }

    /// Caches chats and messages between runs (default: `true`).
    pub fn use_message_database(mut self, value: bool) -> Self {
        self.parameters.use_message_database = value;
        self
    }

    /// Enables secret chats (default: `false`).
    pub fn use_secret_chats(mut self, value: bool) -> Self {
        self.parameters.use_secret_chats = value;
        self
    }

    /// IETF language tag of the user's language (default: `en`).
    pub fn system_language_code(mut self, value: impl Into<String>) -> Self {
        self.parameters.system_language_code = value.into();
        self
    }

    /// Device model shown in the account's active sessions (default: `tdrust`).
    pub fn device_model(mut self, value: impl Into<String>) -> Self {
        self.parameters.device_model = value.into();
        self
    }

    /// Operating system version shown in active sessions.
    pub fn system_version(mut self, value: impl Into<String>) -> Self {
        self.parameters.system_version = value.into();
        self
    }

    /// Application version shown in active sessions.
    pub fn application_version(mut self, value: impl Into<String>) -> Self {
        self.parameters.application_version = value.into();
        self
    }

    /// Uses `runtime` instead of [`Runtime::global`].
    pub fn runtime(mut self, runtime: Runtime) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// Creates the client and its update stream.
    ///
    /// The client starts in an unauthorized state. Call
    /// [`Client::authorize`] to log in.
    pub fn build(self) -> Result<(Client, Updates)> {
        let mut parameters = self.parameters;
        parameters.api_id = match self.api_id {
            Some(api_id) => api_id,
            None => {
                let text = env_var("TELEGRAM_API_ID", "api_id")?;
                text.trim()
                    .parse()
                    .map_err(|_| Error::Config(format!("TELEGRAM_API_ID must be a number, got `{text}`")))?
            }
        };
        parameters.api_hash = match self.api_hash {
            Some(api_hash) => api_hash,
            None => env_var("TELEGRAM_API_HASH", "api_hash")?.trim().to_owned(),
        };
        if parameters.api_hash.is_empty() {
            return Err(Error::Config("api_hash must not be empty".into()));
        }

        let runtime = match self.runtime {
            Some(runtime) => runtime,
            None => Runtime::global()?,
        };
        let registration = runtime.register_client();
        let client =
            Client { inner: Arc::new(Inner { id: registration.id, runtime, auth: registration.auth, parameters }) };
        // TDLib starts a client instance when it receives its first request.
        client.inner.runtime.request(registration.id, json!({"@type": "getOption", "name": "version"}))?;

        let updates = Updates { client_id: registration.id, receiver: registration.updates };
        Ok((client, updates))
    }
}

fn env_var(name: &str, setting: &str) -> Result<String> {
    std::env::var(name).map_err(|_| {
        Error::Config(format!(
            "{setting} is not set: call ClientBuilder::{setting} or set the {name} environment variable \
             (get your credentials at https://my.telegram.org)"
        ))
    })
}

fn path_to_string(path: PathBuf) -> String {
    path.to_string_lossy().into_owned()
}
