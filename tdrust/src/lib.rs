//! An async, fully typed Rust framework for [TDLib](https://core.telegram.org/tdlib),
//! Telegram's official client library.
//!
//! * Every TDLib type and function is generated from `td_api.tl`, with
//!   documentation: [`types`], [`functions`], and one async method per
//!   function on [`Client`].
//! * One background thread serves any number of clients; requests are plain
//!   `async` calls that work with any executor.
//! * Login is a single call with a pluggable [`Authenticator`].
//! * `tdjson` is loaded at run time, so building needs no TDLib installation.
//!
//! # Quick start
//!
//! ```no_run
//! use tdrust::prelude::*;
//!
//! #[tokio::main]
//! async fn main() -> tdrust::Result<()> {
//!     // Reads TELEGRAM_API_ID and TELEGRAM_API_HASH from the environment.
//!     let (client, mut updates) = Client::builder().database_directory("tdlib").build()?;
//!     client.authorize(ConsoleAuthenticator::new()).await?;
//!
//!     let me = client.get_me().await?;
//!     println!("Logged in as {} (id {})", me.first_name, me.id);
//!
//!     while let Some(update) = updates.next().await {
//!         if let Update::NewMessage(update) = update {
//!             if let Some(text) = update.message.text() {
//!                 println!("{}: {text}", update.message.chat_id);
//!             }
//!         }
//!     }
//!     Ok(())
//! }
//! ```
//!
//! # Finding TDLib
//!
//! [`Tdjson::load`] looks for the library in `TDJSON_PATH`, next to the
//! executable, and on the system library path. Enable the `link` feature to
//! link against it at build time instead.
//!
//! # Versions
//!
//! The bindings are generated from the schema of TDLib
//! [`BUNDLED_TDLIB_VERSION`]. Using a different TDLib version mostly works,
//! but new or changed objects may arrive as `Unrecognized` or fail to
//! decode; set `TDRUST_TL_SCHEMA` at build time to the `td_api.tl` of the
//! version you run to generate matching bindings.

#![cfg_attr(docsrs, feature(doc_auto_cfg))]
#![warn(missing_docs, missing_debug_implementations)]

mod auth;
mod backend;
mod client;
mod error;
mod ext;
mod runtime;
#[doc(hidden)]
pub mod serde_helpers;

pub use auth::{AuthStep, Authenticator, BotAuthenticator, ConsoleAuthenticator, Login, Registration};
pub use backend::{Backend, Tdjson};
pub use client::{Client, ClientBuilder, Updates};
pub use error::{Error, Result, TdError};
pub use runtime::Runtime;

/// The TDLib version whose `td_api.tl` is bundled with this crate.
pub const BUNDLED_TDLIB_VERSION: &str = "1.8.67";

/// Every TDLib object: one struct per constructor and one enum per abstract
/// type.
///
/// Enums such as [`Update`](types::Update) have a variant per constructor,
/// named without the type prefix (`updateNewMessage` becomes
/// `Update::NewMessage`), and an `Unrecognized` variant that keeps objects
/// unknown to this version as raw JSON. Each variant struct converts into
/// its enum with `.into()`.
#[allow(missing_docs, clippy::all, rustdoc::all)]
pub mod types {
    include!(concat!(env!("OUT_DIR"), "/types.rs"));
}

/// Every TDLib function as a request struct, for use with [`Client::send`]
/// and [`execute`].
///
/// Structs whose fields all have sensible defaults implement [`Default`], so
/// optional parameters can be left out:
///
/// ```
/// let request = tdrust::functions::GetChatHistory { chat_id: 42, limit: 50, ..Default::default() };
/// ```
#[allow(missing_docs, clippy::all, rustdoc::all)]
pub mod functions {
    include!(concat!(env!("OUT_DIR"), "/functions.rs"));
}

#[allow(missing_docs, clippy::all, rustdoc::all)]
mod methods {
    include!(concat!(env!("OUT_DIR"), "/methods.rs"));
}

/// A TDLib function: a request that can be sent with [`Client::send`].
pub trait Function: serde::Serialize + Send {
    /// The type TDLib answers with.
    type Output: serde::de::DeserializeOwned + Send + 'static;
    /// The function's name in TDLib (its `@type`).
    const TYPE: &'static str;
}

/// A function that TDLib can execute synchronously, without a client (see
/// [`execute`]).
pub trait SyncFunction: Function {}

/// Executes a function synchronously on the global runtime.
///
/// ```no_run
/// use tdrust::functions::GetTextEntities;
///
/// let entities = tdrust::execute(GetTextEntities { text: "Visit https://telegram.org".into() })?;
/// assert_eq!(entities.entities.len(), 1);
/// # Ok::<(), tdrust::Error>(())
/// ```
pub fn execute<F: SyncFunction>(function: F) -> Result<F::Output> {
    Runtime::global()?.execute(function)
}

/// Sets how much TDLib logs to stderr: 0 fatal errors, 1 errors (tdrust's
/// default), 2 warnings, 3 info, 4 debug, 5 verbose debug.
pub fn set_log_verbosity(level: i32) -> Result<()> {
    execute(functions::SetLogVerbosityLevel { new_verbosity_level: level }).map(drop)
}

/// The version of the loaded TDLib library, such as `"1.8.67"`.
pub fn tdlib_version() -> Result<String> {
    match execute(functions::GetOption { name: "version".into() })? {
        types::OptionValue::String(value) => Ok(value.value),
        other => Err(Error::InvalidRequest(format!("unexpected version option `{}`", other.type_name()))),
    }
}

/// The most commonly used items, for `use tdrust::prelude::*`.
pub mod prelude {
    pub use crate::types::{
        AuthorizationState, Chat, FormattedText, InputMessageContent, Message, MessageContent, Update, User,
    };
    pub use crate::{
        Authenticator, BotAuthenticator, Client, ClientBuilder, ConsoleAuthenticator, Error, Login, Result, TdError,
        Updates,
    };
}
