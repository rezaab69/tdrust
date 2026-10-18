//! The machinery shared by all clients: one thread that receives everything
//! TDLib produces and routes it to the right request or update stream.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once, Weak};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, watch};

use crate::backend::{Backend, Tdjson};
use crate::types::{AuthorizationState, Update};
use crate::{Error, Function, Result, SyncFunction, TdError};

/// How long one `td_receive` call may block. Also bounds how long the
/// receiver thread outlives a dropped runtime.
const RECEIVE_TIMEOUT: Duration = Duration::from_millis(500);

/// A connection to a TDLib backend, shared by any number of clients.
///
/// Most programs never touch this type: [`ClientBuilder::build`](crate::ClientBuilder::build)
/// uses [`Runtime::global`], which loads `tdjson` on first use. Create a
/// runtime yourself to load the library from a specific path, or to plug in
/// a custom [`Backend`]:
///
/// ```no_run
/// use tdrust::{Runtime, Tdjson};
///
/// # fn main() -> tdrust::Result<()> {
/// Runtime::with_backend(Tdjson::load_from("/opt/tdlib/lib/libtdjson.so")?)
///     .set_global()
///     .expect("the global runtime was already initialized");
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Runtime {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = lock(&self.shared.state);
        f.debug_struct("Runtime")
            .field("clients", &state.clients.len())
            .field("pending_requests", &state.pending.len())
            .finish()
    }
}

struct Shared {
    backend: Arc<dyn Backend>,
    state: Mutex<State>,
    next_extra: AtomicU64,
    receiver: Once,
}

#[derive(Default)]
struct State {
    clients: HashMap<i32, Slot>,
    pending: HashMap<u64, Pending>,
}

struct Slot {
    updates: Option<mpsc::UnboundedSender<Update>>,
    auth: watch::Sender<Option<AuthorizationState>>,
}

struct Pending {
    client_id: i32,
    reply: oneshot::Sender<Result<Value>>,
}

/// What a newly registered client needs to operate.
pub(crate) struct Registration {
    pub id: i32,
    pub updates: mpsc::UnboundedReceiver<Update>,
    pub auth: watch::Receiver<Option<AuthorizationState>>,
}

static GLOBAL: Mutex<Option<Runtime>> = Mutex::new(None);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The protected data stays consistent even if a holder panicked.
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Runtime {
    /// Creates a runtime on top of `backend`.
    pub fn with_backend(backend: impl Backend) -> Self {
        Runtime {
            shared: Arc::new(Shared {
                backend: Arc::new(backend),
                state: Mutex::new(State::default()),
                next_extra: AtomicU64::new(1),
                receiver: Once::new(),
            }),
        }
    }

    /// The process-wide runtime, created on first use with [`Tdjson::load`].
    ///
    /// When tdrust creates it, TDLib's log verbosity is lowered to 1 (errors
    /// only), because the default floods stderr. Use
    /// [`set_log_verbosity`](crate::set_log_verbosity) to change it.
    pub fn global() -> Result<Runtime> {
        let mut global = lock(&GLOBAL);
        if let Some(runtime) = global.as_ref() {
            return Ok(runtime.clone());
        }
        let runtime = Runtime::with_backend(Tdjson::load()?);
        if let Err(err) = runtime.execute_json(json!({"@type": "setLogVerbosityLevel", "new_verbosity_level": 1})) {
            log::warn!("cannot lower TDLib log verbosity: {err}");
        }
        *global = Some(runtime.clone());
        Ok(runtime)
    }

    /// Makes this runtime the one returned by [`Runtime::global`]. Fails,
    /// returning the runtime, if the global runtime already exists.
    pub fn set_global(self) -> std::result::Result<(), Runtime> {
        let mut global = lock(&GLOBAL);
        if global.is_some() {
            return Err(self);
        }
        *global = Some(self);
        Ok(())
    }

    /// Executes a function synchronously, without a client. Only functions
    /// documented as "Can be called synchronously" implement [`SyncFunction`].
    pub fn execute<F: SyncFunction>(&self, function: F) -> Result<F::Output> {
        decode::<F>(self.execute_json(encode(&function)?)?)
    }

    /// Executes a raw JSON request synchronously.
    pub fn execute_json(&self, request: Value) -> Result<Value> {
        check_request(&request)?;
        let response = self.shared.backend.execute(&request.to_string()).ok_or_else(|| {
            Error::InvalidRequest("TDLib returned no response; the function cannot be executed synchronously".into())
        })?;
        let value = serde_json::from_str(&response)
            .map_err(|err| Error::InvalidRequest(format!("TDLib returned invalid JSON: {err}")))?;
        into_result(value)
    }

    pub(crate) fn register_client(&self) -> Registration {
        self.start_receiver();
        let id = self.shared.backend.create_client_id();
        let (updates_tx, updates) = mpsc::unbounded_channel();
        let (auth_tx, auth) = watch::channel(None);
        lock(&self.shared.state).clients.insert(id, Slot { updates: Some(updates_tx), auth: auth_tx });
        Registration { id, updates, auth }
    }

    /// Sends a request and returns a receiver for its response.
    pub(crate) fn request(&self, client_id: i32, mut request: Value) -> Result<oneshot::Receiver<Result<Value>>> {
        check_request(&request)?;
        let extra = self.shared.next_extra.fetch_add(1, Ordering::Relaxed);
        if let Some(object) = request.as_object_mut() {
            object.insert("@extra".into(), extra.into());
        }

        let (reply, response) = oneshot::channel();
        {
            let mut state = lock(&self.shared.state);
            if !state.clients.contains_key(&client_id) {
                return Err(Error::ClientClosed(client_id));
            }
            state.pending.insert(extra, Pending { client_id, reply });
        }
        self.shared.backend.send(client_id, &request.to_string());
        Ok(response)
    }

    fn start_receiver(&self) {
        let shared = Arc::downgrade(&self.shared);
        self.shared.receiver.call_once(|| {
            std::thread::Builder::new()
                .name("tdrust-receiver".into())
                .spawn(move || receive_loop(shared))
                .expect("failed to spawn the TDLib receiver thread");
        });
    }
}

/// Runs until the runtime is dropped.
fn receive_loop(shared: Weak<Shared>) {
    while let Some(shared) = shared.upgrade() {
        if let Some(message) = shared.backend.receive(RECEIVE_TIMEOUT) {
            shared.dispatch(&message);
        }
    }
}

impl Shared {
    fn dispatch(&self, message: &str) {
        let mut value: Value = match serde_json::from_str(message) {
            Ok(value) => value,
            Err(err) => {
                log::error!("TDLib sent invalid JSON ({err}): {message}");
                return;
            }
        };
        let Some(object) = value.as_object_mut() else {
            log::error!("TDLib sent a non-object message: {message}");
            return;
        };
        let client_id = object.remove("@client_id").and_then(|id| id.as_i64()).and_then(|id| i32::try_from(id).ok());

        if let Some(extra) = object.remove("@extra") {
            let pending = extra.as_u64().and_then(|extra| lock(&self.state).pending.remove(&extra));
            match pending {
                // The requester may have given up waiting; that is fine.
                Some(pending) => drop(pending.reply.send(into_result(value))),
                None => log::debug!("dropping a response nobody is waiting for: {message}"),
            }
            return;
        }

        match client_id {
            Some(client_id) => self.dispatch_update(client_id, value),
            None => log::debug!("dropping an update without @client_id: {message}"),
        }
    }

    fn dispatch_update(&self, client_id: i32, value: Value) {
        let update = match Update::deserialize(&value) {
            Ok(update) => update,
            Err(err) => {
                log::warn!("cannot decode update {value}: {err}; passing it on as Update::Unrecognized");
                Update::Unrecognized(value)
            }
        };

        let mut state = lock(&self.state);
        let Some(slot) = state.clients.get_mut(&client_id) else {
            return;
        };
        let mut closed = false;
        if let Update::AuthorizationState(update) = &update {
            closed = matches!(update.authorization_state, AuthorizationState::Closed(_));
            slot.auth.send_replace(Some(update.authorization_state.clone()));
        }
        if let Some(updates) = &slot.updates {
            if updates.send(update).is_err() {
                // The `Updates` stream was dropped; stop queueing updates.
                slot.updates = None;
            }
        }
        if closed {
            // Dropping the slot ends the update stream, and dropping pending
            // reply senders fails their requests with `ClientClosed`.
            state.clients.remove(&client_id);
            state.pending.retain(|_, pending| pending.client_id != client_id);
        }
    }
}

fn check_request(request: &Value) -> Result<()> {
    match request.get("@type") {
        Some(Value::String(_)) => Ok(()),
        _ => Err(Error::InvalidRequest("a request must be a JSON object with a string `@type` field".into())),
    }
}

/// Turns a TDLib `error` object into `Err`.
pub(crate) fn into_result(value: Value) -> Result<Value> {
    if value.get("@type").and_then(Value::as_str) != Some("error") {
        return Ok(value);
    }
    let code = value.get("code").and_then(Value::as_i64).and_then(|code| i32::try_from(code).ok());
    let message = value.get("message").and_then(Value::as_str).unwrap_or_default();
    Err(TdError { code: code.unwrap_or_default(), message: message.to_owned() }.into())
}

pub(crate) fn encode<F: Function>(function: &F) -> Result<Value> {
    serde_json::to_value(function).map_err(|err| Error::InvalidRequest(format!("cannot encode `{}`: {err}", F::TYPE)))
}

pub(crate) fn decode<F: Function>(value: Value) -> Result<F::Output> {
    F::Output::deserialize(&value).map_err(|source| Error::Decode { function: F::TYPE, source, json: Box::new(value) })
}
