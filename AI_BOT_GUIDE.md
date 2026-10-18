# tdrust: complete guide for writing Telegram bots (for AI code generators)

This document is self-contained. It describes everything needed to write a
Telegram bot (or user-account automation) in Rust with the **tdrust**
framework, without reading tdrust's source. All signatures and type shapes
below come from the actual generated code (TDLib schema 1.8.67).

Read sections 1–4 before writing any code. Sections 5–12 are reference
material to consult as needed. Section 13 is a checklist to run before
handing code back.

---

## 0. Contents

1. Hard rules (read first)
2. Mental model
3. Project setup
4. Complete bot template
5. How the generated API maps from TDLib (naming and type rules)
6. Hand-written API reference
7. Generated method signatures most bots need
8. Type reference (fields of the important structs and enums)
9. Recipes
10. Errors
11. Gotchas
12. Testing without Telegram
13. Pre-delivery checklist

---

## 1. Hard rules

1. **Import with `use tdrust::prelude::*;`** plus explicit `use tdrust::types::{...};`
   for anything else. All TDLib objects live in `tdrust::types`, all request
   structs in `tdrust::functions`.
2. **Every TDLib function is an `async` method on `Client`** named in
   `snake_case` (`sendMessage` → `client.send_message(...)`). Parameters are
   positional, in schema order. See §7 for exact signatures.
3. **Always ignore your own messages:** `Update::NewMessage` also fires for
   messages the bot itself sends. Skip them with `if message.is_outgoing { ... }`.
4. **Never block or `await` slow work inside the update loop.** Spawn a task
   per relevant update (`tokio::spawn`) and give it a `client.clone()`.
5. **Read the `Updates` stream continuously** until it returns `None`.
   Updates are buffered in an unbounded queue.
6. **Keep at least one `Client` clone alive.** When the last clone is dropped,
   TDLib closes the client. Call `client.shutdown().await` on exit.
7. **Every `match` on a TDLib enum needs a `_ =>` arm.** Every enum has an
   extra `Unrecognized(serde_json::Value)` variant, and new variants appear
   with TDLib upgrades.
8. **Field named `type` is `r#type`** (e.g. `chat.r#type`, `entity.r#type`).
   The same goes for other Rust keywords.
9. **IDs:** chat, user, message and supergroup IDs are `i64`. File IDs are `i32`.
   Group and channel chat IDs are negative (`-100…`).
10. **`Option` is used only for object-typed fields that TDLib documents as
    nullable.** Strings, numbers, bools and vectors are never `Option`. For
    those, "not set" is `""`, `0`, `false` or `vec![]`.
11. **Enum-typed parameters take `impl Into<Enum>`:** pass the variant
    *struct* directly (e.g. `MessageSenderUser { user_id }`), no need to
    wrap it. **Optional** enum parameters (`Option<Enum>`) need
    `Some(VariantStruct { .. }.into())`.
12. **String parameters take `impl Into<String>`:** `&str`, `String` and
    `format!(..)` all work.
13. **Use `..Default::default()` only on structs that implement `Default`.**
    §8 marks them `[Default]`. Structs with a required (non-`Option`) enum
    field do *not* implement `Default` (e.g. `InlineKeyboardButton`,
    `InputMessagePhoto`, `SendMessage`, `Message`, `Chat`, `User`).
14. **The message returned by `send_message`/`send_text`/`reply_text` has a
    temporary ID.** To edit, delete or pin it later, wait for its final ID
    in `Update::MessageSendSucceeded` (recipe 9.12).
15. **Error type:** all fallible calls return `tdrust::Result<T>` =
    `Result<T, tdrust::Error>`. Use `?`.

---

## 2. Mental model

```
your code ──async method──▶ Client ──JSON──▶ TDLib (libtdjson, loaded at run time)
   ▲                                              │
   └──────────── Updates stream ◀── receiver thread┘
```

- **TDLib** is Telegram's official client library, a native shared
  library (`libtdjson.so` / `tdjson.dll` / `libtdjson.dylib`). It talks
  to Telegram's MTProto API, so a bot built on it is a full client, **not**
  a Bot-API HTTP client. The Bot API concepts (commands, inline keyboards,
  callback queries, inline queries) all exist, but under TDLib's names and
  shapes.
- **`Client`** = one logged-in account (a bot or a user). It is `Clone +
  Send + Sync`, and clones are cheap because it wraps an `Arc`. Requests are
  plain `async fn`s and work on any executor. tokio is used in all examples.
- **`Updates`** = the stream of events TDLib pushes for that client (new
  messages, callback queries, chat changes, ...). It is an enum,
  `tdrust::types::Update`, with about 190 variants plus `Unrecognized`.
- **Login** is a single call, `client.authorize(authenticator).await?`. The
  session is stored in the client's database directory, so later runs log in
  without asking.
- TDLib keeps a **local cache** of chats, users and messages. `get_chat`,
  `get_user` and `get_message` are cheap once TDLib has seen the object.

---

## 3. Project setup

### 3.1 Cargo.toml

tdrust is not on crates.io. Depend on it by path. The repository layout is:

```
tdrust/               <- workspace root (Cargo.toml with [workspace])
  tdrust/             <- the framework crate
  tdrust-tl/          <- code generator (build dependency; ignore)
  sample-app/         <- a complete example bot + user client
  libtdjson/libtdjson.so   <- a prebuilt Linux TDLib library
```

**Option A: new crate inside the workspace (recommended).** Create
`tdrust/my-bot/`, and **add `"my-bot"` to `members` in the root
`Cargo.toml`**. Otherwise cargo errors with "current package believes it's in
a workspace when it's not".

```toml
# root Cargo.toml
[workspace]
resolver = "2"
members = ["tdrust", "tdrust-tl", "sample-app", "my-bot"]
```

```toml
# my-bot/Cargo.toml
[package]
name = "my-bot"
version = "0.1.0"
edition = "2021"
rust-version = "1.85"
publish = false

[dependencies]
tdrust = { path = "../tdrust" }
tokio = { version = "1.38", features = ["macros", "rt-multi-thread", "signal", "time", "sync"] }
log = "0.4"          # optional: tdrust logs warnings through `log`
env_logger = "0.11"  # optional: prints those logs
dotenvy = "0.15"     # optional: load settings from a .env file
serde_json = "1"     # only if you use `send_json`
```

**Option B: crate outside the workspace:**
`tdrust = { path = "/abs/path/to/tdrust/tdrust" }`.

Minimum Rust version: 1.85. Cargo features of `tdrust`: `console` (default;
hidden password input in the terminal login) and `link` (link `tdjson` at
build time instead of loading it at run time; needs `TDJSON_LIB_DIR`). Bots
need neither.

### 3.2 Runtime requirements

| Need | How |
| --- | --- |
| TDLib shared library | Found in this order: env `TDJSON_PATH` (full path to the file), then next to the executable, then the system library path. Error `Error::Library` explains what was tried. |
| API credentials | `api_id` (i32) and `api_hash` (32 hex chars) from https://my.telegram.org. Pass them to the builder or set env `TELEGRAM_API_ID` / `TELEGRAM_API_HASH`. **Required even for bots.** |
| Bot token | From @BotFather, e.g. `123456:ABC-DEF...`. By convention read from env `TELEGRAM_BOT_TOKEN`. |
| Session directory | `database_directory(...)` on the builder, default `./tdlib`. One directory per account. Two running processes must not share one. |

This repository builds and runs under **WSL (Ubuntu)**, with
`CARGO_TARGET_DIR=~/tdrust-target`. TDLib's databases need a native Linux file
system, so put the session directory under `~`, not `/mnt/...`.

Suggested `.env` for the bot (load it with `dotenvy::dotenv().ok();` at the
start of `main`, before any threads start):

```ini
TELEGRAM_API_ID=12345678
TELEGRAM_API_HASH=0123456789abcdef0123456789abcdef
TELEGRAM_BOT_TOKEN=123456789:AAAbbbCCC...
TDJSON_PATH=/home/me/tdjson/libtdjson.so
```

Note that `.env` files do not expand `~`. Use absolute paths there.

To load TDLib from an explicit path in code instead of via `TDJSON_PATH`,
run this before building any client:

```rust
tdrust::Runtime::with_backend(tdrust::Tdjson::load_from("/opt/tdlib/libtdjson.so")?)
    .set_global()
    .expect("runtime already initialized");
```

---

## 4. Complete bot template

This is the recommended skeleton. It has a command router, spawned handlers,
callback queries and graceful Ctrl+C shutdown.

```rust
use std::sync::Arc;

use tdrust::prelude::*;
use tdrust::types::{CallbackQueryPayload, FormattedText, UpdateNewCallbackQuery};

/// Shared, read-mostly state handed to every handler.
struct App {
    client: Client,
    bot_username: String,
}

#[tokio::main]
async fn main() -> tdrust::Result<()> {
    let _ = dotenvy::dotenv(); // optional
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init(); // optional

    let token = std::env::var("TELEGRAM_BOT_TOKEN")
        .map_err(|_| tdrust::Error::Config("set TELEGRAM_BOT_TOKEN to a token from @BotFather".into()))?;

    // api_id / api_hash come from TELEGRAM_API_ID / TELEGRAM_API_HASH.
    let (client, mut updates) = Client::builder()
        .database_directory("tdlib-bot")
        .device_model("my bot")
        .build()?;
    client.authorize(BotAuthenticator::new(token)).await?;

    let me = client.get_me().await?;
    let bot_username = me.usernames.map(|u| u.editable_username).unwrap_or_default();
    log::info!("running as @{bot_username}");

    let app = Arc::new(App { client: client.clone(), bot_username });

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        tokio::select! {
            update = updates.next() => match update {
                Some(update) => route(&app, update),
                None => break, // client closed
            },
            _ = &mut ctrl_c => {
                log::info!("stopping");
                client.shutdown().await;
                break;
            }
        }
    }
    Ok(())
}

/// Runs synchronously inside the update loop: only pick updates and spawn.
fn route(app: &Arc<App>, update: Update) {
    match update {
        Update::NewMessage(u) if !u.message.is_outgoing => {
            let app = app.clone();
            tokio::spawn(async move {
                let (chat_id, message_id) = (u.message.chat_id, u.message.id);
                if let Err(err) = on_message(&app, u.message).await {
                    log::warn!("message {message_id} in chat {chat_id}: {err}");
                }
            });
        }
        Update::NewCallbackQuery(q) => {
            let app = app.clone();
            tokio::spawn(async move {
                if let Err(err) = on_callback(&app, q).await {
                    log::warn!("callback query: {err}");
                }
            });
        }
        _ => {}
    }
}

async fn on_message(app: &App, message: Message) -> tdrust::Result<()> {
    let Some(text) = message.text() else { return Ok(()) }; // not a text message
    match parse_command(text, &app.bot_username) {
        Some(("start", _)) | Some(("help", _)) => {
            app.client.reply_text(&message, FormattedText::markdown("*Hi\\!* Send /echo _text_\\.")?).await?;
        }
        Some(("echo", args)) if !args.is_empty() => {
            app.client.reply_text(&message, args).await?;
        }
        Some((other, _)) => {
            app.client.reply_text(&message, format!("Unknown command /{other}")).await?;
        }
        None => {
            app.client.send_text(message.chat_id, text).await?; // plain echo
        }
    }
    Ok(())
}

async fn on_callback(app: &App, q: UpdateNewCallbackQuery) -> tdrust::Result<()> {
    if let CallbackQueryPayload::Data(payload) = &q.payload {
        let data = String::from_utf8_lossy(&payload.data);
        app.client.answer_callback_query(q.id, format!("You pressed {data}"), false, "", 0).await?;
    }
    Ok(())
}

/// Splits "/cmd@botname args" into ("cmd", "args"). Returns None for non-commands
/// and for commands addressed to another bot.
fn parse_command<'a>(text: &'a str, bot_username: &str) -> Option<(&'a str, &'a str)> {
    let rest = text.strip_prefix('/')?;
    let (word, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let (command, target) = word.split_once('@').unwrap_or((word, ""));
    if command.is_empty() || (!target.is_empty() && !target.eq_ignore_ascii_case(bot_username)) {
        return None;
    }
    Some((command, args.trim()))
}
```

Run it with: `cargo run -p my-bot`. The environment must provide
`TDJSON_PATH`, `TELEGRAM_API_ID`, `TELEGRAM_API_HASH` and `TELEGRAM_BOT_TOKEN`.

---

## 5. How the generated API maps from TDLib

tdrust generates Rust from TDLib's `td_api.tl`. If you know a TDLib
function or object from the official docs
(https://core.telegram.org/tdlib/docs/td__api_8h.html), these rules give
you its exact Rust form. §7 and §8 list the common ones.

| TDLib | Rust |
| --- | --- |
| function `getChatHistory` | method `client.get_chat_history(...)` and struct `tdrust::functions::GetChatHistory` |
| acronyms: `getRTMPUrl` | `get_rtmp_url` |
| object/constructor `formattedText` | struct `tdrust::types::FormattedText` |
| abstract class `MessageContent` | enum `tdrust::types::MessageContent` |
| constructor `messageText` of class `MessageContent` | variant `MessageContent::Text(MessageText)`: the variant name is the constructor name minus the class-name prefix |
| `updateNewMessage` in `Update` | `Update::NewMessage(UpdateNewMessage)` |
| `authorizationStateReady` | `AuthorizationState::Ready(AuthorizationStateReady)` |
| `inputMessageText` in `InputMessageContent` | `InputMessageContent::Text(InputMessageText)` |
| `chatListMain` in `ChatList` | `ChatList::Main(ChatListMain)` |
| `int32` | `i32` |
| `int53`, `int64` | `i64` (int64 values travel as JSON strings; handled for you) |
| `double` | `f64` |
| `string` | `String` |
| `bytes` | `Vec<u8>` (base64 on the wire; handled for you) |
| `Bool` | `bool` |
| `vector<T>` | `Vec<T>` |
| object field documented "may be null" / "pass null" | `Option<T>` |
| field `type` (keyword) | `r#type` |
| recursive field | `Box<T>` (rare; e.g. `RichText`) |

**Enum construction:** every variant struct implements `From<Struct> for
Enum`, so `ChatListMain {}.into()` gives a `ChatList`, and
`MessageSenderUser { user_id }.into()` gives a `MessageSender`. Empty
constructors are unit-like structs written with braces: `ChatActionTyping {}`,
`ButtonStyleDefault {}`, `KeyboardButtonTypeText {}`.

**Enum inspection:** `enum_value.type_name()` returns the TDLib `@type`
string, e.g. `"messagePhoto"`. This also works for `Unrecognized`.

**Method parameter conversion rules (generated methods):**

| Parameter schema type | Rust parameter type |
| --- | --- |
| `string` | `impl Into<String>` |
| required abstract (enum) type | `impl Into<Enum>`, so pass the variant struct directly |
| nullable object/enum | `Option<T>`: pass `None` or `Some(x.into())` |
| required concrete struct | `T` by value |
| numbers/bools/bytes/vectors | the plain type |

**Return types:** each method returns `tdrust::Result<ReturnType>`, where
`ReturnType` is the TDLib result. It is often `Ok` (struct `tdrust::types::Ok`,
an empty acknowledgement), which you normally discard.

**Three equivalent call styles:**

```rust
use tdrust::functions::GetChatHistory;
use serde_json::json;

// 1. Method with positional arguments (preferred).
let h = client.get_chat_history(chat_id, 0, 0, 50, false).await?;
// 2. Request struct with named fields (good when many args are defaults).
let h = client.send(GetChatHistory { chat_id, limit: 50, ..Default::default() }).await?;
// 3. Raw JSON, for functions newer than the bindings. Returns serde_json::Value.
let v = client.send_json(json!({"@type": "getChatHistory", "chat_id": chat_id, "limit": 50})).await?;
```

A request struct implements `Default` only when all its fields can be
defaulted. For example, `SendMessage` and `EditMessageText` cannot, because
`input_message_content` is a required enum.

---

## 6. Hand-written API reference

### 6.1 Crate root

```rust
pub use tdrust::{Client, ClientBuilder, Updates, Error, Result, TdError,
                 Authenticator, AuthStep, Login, Registration,
                 BotAuthenticator, ConsoleAuthenticator, Backend, Tdjson, Runtime};
pub const BUNDLED_TDLIB_VERSION: &str = "1.8.67";
pub fn execute<F: SyncFunction>(function: F) -> Result<F::Output>; // sync, no client needed
pub fn set_log_verbosity(level: i32) -> Result<()>; // TDLib stderr log: 0 fatal .. 5 verbose; default 1
pub fn tdlib_version() -> Result<String>;
pub trait Function: Serialize + Send { type Output; const TYPE: &'static str; }
pub trait SyncFunction: Function {} // functions TDLib documents as "Can be called synchronously"
```

`tdrust::prelude::*` exports: `AuthorizationState, Chat, FormattedText,
InputMessageContent, Message, MessageContent, Update, User` (from types) and
`Authenticator, BotAuthenticator, Client, ClientBuilder, ConsoleAuthenticator,
Error, Login, Result, TdError, Updates`. Note that `Result` here is
`tdrust::Result<T, E = tdrust::Error>`, so `Result<T, OtherErr>` still works.

### 6.2 `ClientBuilder` (from `Client::builder()`)

All setters take `self` and return `Self`. Only the credentials are required.

| Method | Default |
| --- | --- |
| `.api_id(i32)` | env `TELEGRAM_API_ID` |
| `.api_hash(impl Into<String>)` | env `TELEGRAM_API_HASH` |
| `.database_directory(impl Into<PathBuf>)` | `"tdlib"` |
| `.files_directory(impl Into<PathBuf>)` | same as database dir |
| `.database_encryption_key(impl Into<Vec<u8>>)` | empty = no encryption |
| `.use_test_dc(bool)` | `false` |
| `.use_file_database(bool)` / `.use_chat_info_database(bool)` / `.use_message_database(bool)` | `true` |
| `.use_secret_chats(bool)` | `false` |
| `.system_language_code(impl Into<String>)` | `"en"` |
| `.device_model(..)` / `.system_version(..)` / `.application_version(..)` | `"tdrust"` / OS / crate version |
| `.runtime(Runtime)` | `Runtime::global()` |
| `.build() -> Result<(Client, Updates)>` | fails with `Error::Config` if credentials are missing, `Error::Library` if TDLib is not found |

### 6.3 `Client` (hand-written methods)

```rust
impl Client {
    pub fn builder() -> ClientBuilder;
    pub fn id(&self) -> i32;
    pub fn runtime(&self) -> &Runtime;
    pub async fn authorize<A: Authenticator>(&self, authenticator: A) -> Result<()>; // returns when logged in
    pub async fn send<F: Function>(&self, function: F) -> Result<F::Output>;          // request struct
    pub async fn send_json(&self, request: serde_json::Value) -> Result<serde_json::Value>; // raw; needs "@type"
    pub fn authorization_state(&self) -> Option<AuthorizationState>;
    pub fn is_closed(&self) -> bool;
    pub async fn wait_closed(&self);
    pub async fn shutdown(&self);  // close + wait; keeps the session on disk
    // conveniences:
    pub async fn send_text(&self, chat_id: i64, text: impl Into<FormattedText>) -> Result<Message>;
    pub async fn reply_text(&self, to: &Message, text: impl Into<FormattedText>) -> Result<Message>; // same chat + topic, as a reply
}
```

Plus about 1,000 generated methods, one per TDLib function (§7).

### 6.4 `Updates`

```rust
impl Updates {
    pub async fn next(&mut self) -> Option<Update>; // None once the client is closed
    pub fn try_next(&mut self) -> Option<Update>;   // non-blocking
    pub fn client_id(&self) -> i32;
}
// also implements futures_core::Stream<Item = Update>
```

Dropping `Updates` discards all future updates. That is fine for scripts that
only make requests.

### 6.5 Authentication

```rust
client.authorize(BotAuthenticator::new(token)).await?;          // bot
client.authorize(ConsoleAuthenticator::new()).await?;           // user, interactive terminal prompts
client.authorize(ConsoleAuthenticator::with_phone_number("+15551234567")).await?;
```

A custom authenticator (for a web/GUI login or secrets from storage) only needs
`login`. All other methods have defaults that fail with
`Error::Authorization` if TDLib asks for that step:

```rust
use tdrust::{Authenticator, AuthStep, Login, Registration, Result, TdError};
use tdrust::types::{AuthenticationCodeInfo, AuthorizationStateWaitPassword,
                    AuthorizationStateWaitEmailCode, TermsOfService};

pub trait Authenticator: Send {
    async fn login(&mut self) -> Result<Login>;                       // required
    async fn code(&mut self, info: &AuthenticationCodeInfo) -> Result<String>;
    async fn password(&mut self, state: &AuthorizationStateWaitPassword) -> Result<String>; // 2FA; state.password_hint
    async fn registration(&mut self, terms: &TermsOfService) -> Result<Registration>;       // new account
    async fn email_address(&mut self) -> Result<String>;
    async fn email_code(&mut self, state: &AuthorizationStateWaitEmailCode) -> Result<String>;
    async fn qr_code_link(&mut self, link: &str) -> Result<()>;       // for Login::QrCode; called on each refresh
    async fn on_error(&mut self, step: AuthStep, error: &TdError) -> Result<()>; // Ok(()) = ask again; default aborts
}
pub enum Login { PhoneNumber(String), BotToken(String), QrCode }
pub struct Registration { pub first_name: String, pub last_name: String }
pub enum AuthStep { Login, Code, Password, Registration, EmailAddress, EmailCode } // #[non_exhaustive]
```

(Implement the methods with plain `async fn` in the `impl` block. The trait
declares them as `fn ... -> impl Future + Send`.)

If a session exists in the database directory, `authorize` returns
immediately without calling the authenticator. If the bot token was revoked,
delete the database directory to force a fresh login.

### 6.6 Convenience helpers on generated types

```rust
FormattedText::plain(text) -> FormattedText
FormattedText::markdown(text) -> Result<FormattedText>  // Telegram MarkdownV2; parsed locally by TDLib
FormattedText::html(text) -> Result<FormattedText>      // Telegram HTML subset
impl From<&str> for FormattedText; impl From<String> for FormattedText; // plain
InputMessageContent::text(impl Into<FormattedText>) -> InputMessageContent
Message::text(&self) -> Option<&str>          // Some only for text messages
Message::sender_user_id(&self) -> Option<i64> // None if sent on behalf of a chat/channel
MessageSender::user_id(&self) -> Option<i64>
MessageSender::chat_id(&self) -> Option<i64>
AnyEnum::type_name(&self) -> &str             // TDLib "@type"
```

`FormattedText::markdown`/`html` return `Err(Error::Td(..))` (code 400) when
the markup is invalid. For example, an unescaped `.` or `!` in MarkdownV2
is an error.

### 6.7 `Runtime`, `Tdjson`, `Backend` (rarely needed)

```rust
Runtime::with_backend(impl Backend) -> Runtime
Runtime::global() -> Result<Runtime>          // lazily loads tdjson
Runtime::set_global(self) -> Result<(), Runtime>
runtime.execute<F: SyncFunction>(f) -> Result<F::Output>
runtime.execute_json(serde_json::Value) -> Result<serde_json::Value>
Tdjson::load() -> Result<Tdjson>;  Tdjson::load_from(path) -> Result<Tdjson>
pub trait Backend: Send + Sync + 'static {
    fn create_client_id(&self) -> i32;
    fn send(&self, client_id: i32, request: &str);
    fn receive(&self, timeout: std::time::Duration) -> Option<String>;
    fn execute(&self, request: &str) -> Option<String>;
}
```

Several accounts in one process: build one client per account, each with its
own `database_directory`. They share one runtime and receiver thread.

---

## 7. Generated method signatures most bots need

Exact signatures. Types are from `tdrust::types`, and `Result` is `tdrust::Result`.
`Ok` is `tdrust::types::Ok` (an acknowledgement).

### Messages

```rust
send_message(chat_id: i64, topic_id: Option<MessageTopic>, reply_to: Option<InputMessageReplyTo>, options: Option<MessageSendOptions>, reply_markup: Option<ReplyMarkup>, input_message_content: impl Into<InputMessageContent>) -> Result<Message>
send_message_album(chat_id: i64, topic_id: Option<MessageTopic>, reply_to: Option<InputMessageReplyTo>, options: Option<MessageSendOptions>, input_message_contents: Vec<InputMessageContent>) -> Result<Messages>
forward_messages(chat_id: i64, topic_id: Option<MessageTopic>, from_chat_id: i64, message_ids: Vec<i64>, options: Option<MessageSendOptions>, send_copy: bool, remove_caption: bool) -> Result<Messages>
edit_message_text(chat_id: i64, message_id: i64, reply_markup: Option<ReplyMarkup>, input_message_content: impl Into<InputMessageContent>) -> Result<Message>
edit_message_caption(chat_id: i64, message_id: i64, reply_markup: Option<ReplyMarkup>, caption: Option<FormattedText>, show_caption_above_media: bool) -> Result<Message>
edit_message_media(chat_id: i64, message_id: i64, reply_markup: Option<ReplyMarkup>, input_message_content: impl Into<InputMessageContent>) -> Result<Message>
edit_message_reply_markup(chat_id: i64, message_id: i64, reply_markup: Option<ReplyMarkup>) -> Result<Message>
delete_messages(chat_id: i64, message_ids: Vec<i64>, revoke: bool) -> Result<Ok>   // revoke=true deletes for everyone
delete_chat_messages_by_sender(chat_id: i64, sender_id: impl Into<MessageSender>) -> Result<Ok>
get_message(chat_id: i64, message_id: i64) -> Result<Message>
get_messages(chat_id: i64, message_ids: Vec<i64>) -> Result<Messages>
get_chat_pinned_message(chat_id: i64) -> Result<Message>
pin_chat_message(chat_id: i64, message_id: i64, disable_notification: bool, only_for_self: bool) -> Result<Ok>
unpin_chat_message(chat_id: i64, message_id: i64) -> Result<Ok>
get_message_link(chat_id: i64, message_id: i64, media_timestamp: i32, checklist_task_id: i32, poll_option_id: impl Into<String>, for_album: bool, in_message_thread: bool) -> Result<MessageLink>
set_message_reactions(chat_id: i64, message_id: i64, reaction_types: Vec<ReactionType>, is_big: bool) -> Result<Ok>   // bots
add_message_reaction(chat_id: i64, message_id: i64, reaction_type: impl Into<ReactionType>, is_big: bool, update_recent_reactions: bool) -> Result<Ok> // users
send_chat_action(chat_id: i64, topic_id: Option<MessageTopic>, business_connection_id: impl Into<String>, action: Option<ChatAction>) -> Result<Ok>
stop_poll(chat_id: i64, message_id: i64, reply_markup: Option<ReplyMarkup>) -> Result<Ok>
parse_text_entities(text: impl Into<String>, parse_mode: impl Into<TextParseMode>) -> Result<FormattedText>
```

### Bot-specific

```rust
answer_callback_query(callback_query_id: i64, text: impl Into<String>, show_alert: bool, url: impl Into<String>, cache_time: i32) -> Result<Ok>
get_callback_query_message(chat_id: i64, message_id: i64, callback_query_id: i64) -> Result<Message>
answer_inline_query(inline_query_id: i64, is_personal: bool, button: Option<InlineQueryResultsButton>, results: Vec<InputInlineQueryResult>, cache_time: i32, next_offset: impl Into<String>) -> Result<Ok>
answer_web_app_query(web_app_query_id: impl Into<String>, result: impl Into<InputInlineQueryResult>) -> Result<InlineMessageId>
answer_shipping_query(shipping_query_id: i64, shipping_options: Vec<ShippingOption>, error_message: impl Into<String>) -> Result<Ok>
answer_pre_checkout_query(pre_checkout_query_id: i64, error_message: impl Into<String>) -> Result<Ok>
set_commands(scope: Option<BotCommandScope>, language_code: impl Into<String>, commands: Vec<BotCommand>) -> Result<Ok>
get_commands(scope: Option<BotCommandScope>, language_code: impl Into<String>) -> Result<BotCommands>
set_menu_button(user_id: i64, menu_button: BotMenuButton) -> Result<Ok>
set_bot_name(bot_user_id: i64, language_code: impl Into<String>, name: impl Into<String>) -> Result<Ok>
set_bot_info_description(bot_user_id: i64, language_code: impl Into<String>, description: impl Into<String>) -> Result<Ok>
```

### Chats and users

```rust
get_me() -> Result<User>
get_user(user_id: i64) -> Result<User>
get_user_full_info(user_id: i64) -> Result<UserFullInfo>
get_user_profile_photos(user_id: i64, offset: i32, limit: i32) -> Result<ChatPhotos>
get_chat(chat_id: i64) -> Result<Chat>
create_private_chat(user_id: i64, force: bool) -> Result<Chat>  // get the private chat with a user (chat_id == user_id)
search_public_chat(username: impl Into<String>) -> Result<Chat> // "@name" without '@'
get_basic_group(basic_group_id: i64) -> Result<BasicGroup>
get_supergroup(supergroup_id: i64) -> Result<Supergroup>
get_supergroup_full_info(supergroup_id: i64) -> Result<SupergroupFullInfo>
get_supergroup_members(supergroup_id: i64, filter: Option<SupergroupMembersFilter>, offset: i32, limit: i32) -> Result<ChatMembers>
get_chat_member(chat_id: i64, member_id: impl Into<MessageSender>) -> Result<ChatMember>
get_chat_administrators(chat_id: i64) -> Result<ChatAdministrators>
set_chat_member_status(chat_id: i64, member_id: impl Into<MessageSender>, status: impl Into<ChatMemberStatus>) -> Result<Ok>
ban_chat_member(chat_id: i64, member_id: impl Into<MessageSender>, banned_until_date: i32, revoke_messages: bool) -> Result<Ok> // 0 = forever
add_chat_member(chat_id: i64, user_id: i64, forward_limit: i32) -> Result<FailedToAddMembers>
process_chat_join_request(chat_id: i64, user_id: i64, approve: bool) -> Result<Ok>
create_chat_invite_link(chat_id: i64, name: impl Into<String>, expiration_date: i32, member_limit: i32, creates_join_request: bool) -> Result<ChatInviteLink>
set_chat_title(chat_id: i64, title: impl Into<String>) -> Result<Ok>
set_chat_description(chat_id: i64, description: impl Into<String>) -> Result<Ok>
set_chat_permissions(chat_id: i64, permissions: ChatPermissions) -> Result<Ok>
set_chat_photo(chat_id: i64, photo: Option<InputChatPhoto>) -> Result<Ok>
leave_chat(chat_id: i64) -> Result<Ok>
join_chat(chat_id: i64) -> Result<ChatJoinResult>
create_forum_topic(chat_id: i64, name: impl Into<String>, is_name_implicit: bool, icon: ForumTopicIcon) -> Result<ForumTopicInfo>
```

### User-account oriented (chat lists and history)

```rust
load_chats(chat_list: Option<ChatList>, limit: i32) -> Result<Ok>        // Err 404 when everything is loaded
get_chats(chat_list: Option<ChatList>, limit: i32) -> Result<Chats>      // Chats { total_count, chat_ids }
search_chats(query: impl Into<String>, type_filter: Option<SearchChatTypeFilter>, limit: i32) -> Result<Chats>
get_chat_history(chat_id: i64, from_message_id: i64, offset: i32, limit: i32, only_local: bool) -> Result<Messages>
search_chat_messages(chat_id: i64, topic_id: Option<MessageTopic>, query: impl Into<String>, sender_id: Option<MessageSender>, from_message_id: i64, offset: i32, limit: i32, filter: Option<SearchMessagesFilter>) -> Result<FoundChatMessages>
view_messages(chat_id: i64, message_ids: Vec<i64>, source: Option<MessageSource>, force_read: bool) -> Result<Ok> // mark as read
open_chat(chat_id: i64) -> Result<Ok>;  close_chat(chat_id: i64) -> Result<Ok>
```

### Files

```rust
get_file(file_id: i32) -> Result<File>
get_remote_file(remote_file_id: impl Into<String>, file_type: Option<FileType>) -> Result<File>
download_file(file_id: i32, priority: i32, offset: i64, limit: i64, synchronous: bool) -> Result<File> // priority 1..32
cancel_download_file(file_id: i32, only_if_pending: bool) -> Result<Ok>
read_file_part(file_id: i32, offset: i64, count: i64) -> Result<Data>   // Data { data: Vec<u8> }
preliminary_upload_file(file: impl Into<InputFile>, file_type: Option<FileType>, priority: i32) -> Result<File>
delete_file(file_id: i32) -> Result<Ok>
```

### Misc

```rust
get_option(name: impl Into<String>) -> Result<OptionValue>
set_option(name: impl Into<String>, value: Option<OptionValue>) -> Result<Ok>
log_out() -> Result<Ok>     // destroys the session
close() -> Result<Ok>       // prefer client.shutdown()
```

For any function not listed, apply the naming rules from §5 to the TDLib
docs. Methods marked "for bots only" in the TDLib docs fail for user
accounts, and vice versa.

---

## 8. Type reference

`[Default]` means the struct implements `Default`. Enum lines list
`Variant(StructName)`, and every enum also has `Unrecognized(serde_json::Value)`.
Only the fields bots typically use are guaranteed complete for the large
structs.

### 8.1 Updates

```text
enum Update (~190 variants). The ones bots use:
  NewMessage(UpdateNewMessage)                     { message: Message }
  MessageSendSucceeded(UpdateMessageSendSucceeded) { message: Message, old_message_id: i64 }
  MessageSendFailed(UpdateMessageSendFailed)       { message: Message, old_message_id: i64, error: types::Error{code,message} }
  MessageEdited(UpdateMessageEdited) [Default]     { chat_id, message_id: i64, edit_date: i32, reply_markup: Option<ReplyMarkup> }
  MessageContent(UpdateMessageContent)             { chat_id, message_id: i64, new_content: MessageContent }  // new content after an edit
  DeleteMessages(UpdateDeleteMessages) [Default]   { chat_id: i64, message_ids: Vec<i64>, is_permanent: bool, from_cache: bool }
  NewCallbackQuery(UpdateNewCallbackQuery)         { id: i64, sender_user_id: i64, chat_id: i64, message_id: i64, chat_instance: i64, payload: CallbackQueryPayload }
  NewInlineCallbackQuery(UpdateNewInlineCallbackQuery) { id: i64, sender_user_id: i64, inline_message_id: String, chat_instance: i64, payload: CallbackQueryPayload }
  NewInlineQuery(UpdateNewInlineQuery) [Default]   { id: i64, sender_user_id: i64, user_location: Option<Location>, chat_type: Option<ChatType>, query: String, offset: String }
  NewChosenInlineResult(UpdateNewChosenInlineResult) [Default] { sender_user_id: i64, user_location: Option<Location>, query: String, result_id: String, inline_message_id: String }
  ChatMember(UpdateChatMember)                     { chat_id, actor_user_id: i64, date: i32, invite_link: Option<ChatInviteLink>, via_join_request: bool, via_chat_folder_invite_link: bool, old_chat_member: ChatMember, new_chat_member: ChatMember }
  NewChatJoinRequest(UpdateNewChatJoinRequest) [Default] { chat_id: i64, request: ChatJoinRequest{user_id: i64, date: i32, bio: String}, user_chat_id: i64, invite_link: Option<ChatInviteLink>, query_id: i64 }
  PollAnswer(UpdatePollAnswer)                     { poll_id: i64, voter_id: MessageSender, option_ids: Vec<String>, option_positions: Vec<i32> }
  NewShippingQuery, NewPreCheckoutQuery, MessageReaction, MessageReactions, ChatBoost, NewBusinessMessage, ...
  File(UpdateFile) [Default]                       { file: File }      // download/upload progress
  AuthorizationState(UpdateAuthorizationState)     { authorization_state: AuthorizationState }
  NewChat, ChatTitle{chat_id, title}, ChatLastMessage, User{user}, Option{name, value}, ConnectionState, ... (state sync; bots usually ignore)
```

### 8.2 Messages

```text
struct Message {
  id: i64, sender_id: MessageSender, chat_id: i64, is_outgoing: bool, is_pinned: bool,
  is_channel_post: bool, date: i32 (unix), edit_date: i32,
  sending_state: Option<MessageSendingState>,       // Some while pending/failed
  forward_info: Option<MessageForwardInfo>,
  reply_to: Option<MessageReplyTo>,                 // what this message replies to
  topic_id: Option<MessageTopic>,
  via_bot_user_id: i64, author_signature: String, media_album_id: i64,
  content: MessageContent, reply_markup: Option<ReplyMarkup>, ...more fields
}  // no Default

enum MessageSender: User(MessageSenderUser{user_id: i64}), Chat(MessageSenderChat{chat_id: i64})
enum MessageReplyTo: Message(MessageReplyToMessage{chat_id: i64, message_id: i64, quote: Option<TextQuote>, origin: Option<MessageOrigin>, content: Option<MessageContent>, ...}), Story(..)
enum MessageTopic: Thread(MessageTopicThread{message_thread_id: i64}), Forum(MessageTopicForum{forum_topic_id: i32}),
                   DirectMessages(..), SavedMessages(..)

enum MessageContent (~110 variants). Common ones:
  Text(MessageText{ text: FormattedText, link_preview: Option<LinkPreview>, link_preview_options: Option<LinkPreviewOptions> })
  Photo(MessagePhoto{ photo: Photo, caption: FormattedText, has_spoiler: bool, .. })
  Video(MessageVideo{ video: Video, caption: FormattedText, .. })
  Document(MessageDocument{ document: Document, caption: FormattedText })
  Audio(MessageAudio{ audio: Audio, caption: FormattedText }), Animation(MessageAnimation),
  VoiceNote(MessageVoiceNote{ voice_note: VoiceNote, caption: FormattedText, is_listened: bool }),
  VideoNote(MessageVideoNote), Sticker(MessageSticker{sticker: Sticker, is_premium}),
  Location(MessageLocation{location: Location}), Venue(MessageVenue), Contact(MessageContact{contact: Contact}),
  Poll(MessagePoll), Dice(MessageDice),
  ChatAddMembers(MessageChatAddMembers{member_user_ids: Vec<i64>}),   // service: users added
  ChatJoinByLink(MessageChatJoinByLink{}),                            // service: sender joined via link
  ChatJoinByRequest(MessageChatJoinByRequest{}),
  ChatDeleteMember(MessageChatDeleteMember{user_id: i64}),            // service: user left/removed
  ChatUpgradeTo(MessageChatUpgradeTo{supergroup_id: i64}),
  ChatChangeTitle(..), PinMessage(..), SupergroupChatCreate(..), BasicGroupChatCreate(..),
  WebAppDataReceived(..), PaymentSuccessfulBot(..), UsersShared(..), ChatShared(..),
  Unsupported(MessageUnsupported{}), ...

struct Photo [Default]       { has_stickers: bool, minithumbnail: Option<Minithumbnail>, sizes: Vec<PhotoSize> } // sizes ascending; last = largest
struct PhotoSize [Default]   { r#type: String, photo: File, width: i32, height: i32, progressive_sizes: Vec<i32> }
struct Document [Default]    { file_name: String, mime_type: String, minithumbnail, thumbnail: Option<Thumbnail>, document: File }
struct Video [Default]       { duration, width, height: i32, file_name: String, mime_type: String, supports_streaming: bool, thumbnail: Option<Thumbnail>, video: File, .. }
struct Audio [Default]       { duration: i32, title: String, performer: String, file_name: String, mime_type: String, audio: File, .. }
struct VoiceNote [Default]   { duration: i32, waveform: Vec<u8>, mime_type: String, voice: File, .. }
struct Location [Default]    { latitude: f64, longitude: f64, horizontal_accuracy: f64 }
```

### 8.3 Text

```text
struct FormattedText [Default] { text: String, entities: Vec<TextEntity> }
struct TextEntity { offset: i32, length: i32, r#type: TextEntityType }   // offset/length in UTF-16 code units!
enum TextEntityType: Mention, Hashtag, Cashtag, BotCommand, Url, EmailAddress, PhoneNumber, BankCardNumber,
  Bold, Italic, Underline, Strikethrough, Spoiler, Code, Pre, PreCode{language: String}, BlockQuote, ExpandableBlockQuote,
  TextUrl{url: String}, MentionName{user_id: i64}, CustomEmoji{custom_emoji_id}, MediaTimestamp, DateTime
  (variant structs are named TextEntityType<Variant>, e.g. TextEntityTypeBold {})
```

### 8.4 Sending: content, options, reply target

```text
enum InputMessageContent: Text(InputMessageText), Photo(InputMessagePhoto), Video(InputMessageVideo),
  Document(InputMessageDocument), Audio(InputMessageAudio), Animation(InputMessageAnimation),
  VoiceNote(InputMessageVoiceNote), VideoNote(..), Sticker(InputMessageSticker), Location(InputMessageLocation),
  LiveLocation(..), Venue(..), Contact(..), Dice(InputMessageDice), Poll(InputMessagePoll), Invoice(..),
  Game(..), Story(..), Checklist(..), PaidMedia(..), RichMessage(..), StakeDice(..), Forwarded(InputMessageForwarded)

struct InputMessageText [Default]     { text: FormattedText, link_preview_options: Option<LinkPreviewOptions>, clear_draft: bool }
struct LinkPreviewOptions [Default]   { is_disabled: bool, url: String, force_small_media: bool, force_large_media: bool, show_above_text: bool }
struct InputMessagePhoto              { photo: InputPhoto, caption: Option<FormattedText>, show_caption_above_media: bool, self_destruct_type: Option<MessageSelfDestructType>, has_spoiler: bool }
struct InputPhoto                     { photo: InputFile, thumbnail: Option<InputThumbnail>, video: Option<InputFile>, added_sticker_file_ids: Vec<i32>, width: i32, height: i32 }
struct InputMessageDocument           { document: InputDocument, caption: Option<FormattedText> }
struct InputDocument                  { document: InputFile, thumbnail: Option<InputThumbnail>, disable_content_type_detection: bool }
struct InputMessageVideo              { video: InputVideo, caption: Option<FormattedText>, show_caption_above_media: bool, self_destruct_type: Option<MessageSelfDestructType>, has_spoiler: bool }
struct InputVideo                     { video: InputFile, thumbnail: Option<InputThumbnail>, cover: Option<InputFile>, start_timestamp: i32, added_sticker_file_ids: Vec<i32>, duration: i32, width: i32, height: i32, supports_streaming: bool }
struct InputMessageAudio              { audio: InputAudio, caption: Option<FormattedText> }
struct InputAudio                     { audio: InputFile, album_cover_thumbnail: Option<InputThumbnail>, duration: i32, title: String, performer: String }
struct InputMessageVoiceNote          { voice_note: InputVoiceNote, caption: Option<FormattedText>, self_destruct_type: Option<MessageSelfDestructType> }
struct InputVoiceNote                 { voice_note: InputFile, duration: i32, waveform: Vec<u8> }
struct InputMessageAnimation          { animation: InputAnimation, caption: Option<FormattedText>, show_caption_above_media: bool, has_spoiler: bool }
struct InputAnimation                 { animation: InputFile, thumbnail: Option<InputThumbnail>, added_sticker_file_ids: Vec<i32>, duration: i32, width: i32, height: i32 }
struct InputMessageSticker            { sticker: InputSticker, emoji: String }
struct InputSticker                   { sticker: InputFile, thumbnail: Option<InputThumbnail>, width: i32, height: i32 }
struct InputThumbnail                 { thumbnail: InputFile, width: i32, height: i32 }
struct InputMessageLocation [Default] { location: Location }
struct InputMessageDice [Default]     { emoji: String, clear_draft: bool }        // "🎲", "🎯", "🏀", ...
struct InputMessagePoll               { question: FormattedText, options: Vec<InputPollOption>, description: Option<FormattedText>, media: Option<InputPollMedia>,
                                        is_anonymous: bool, allows_multiple_answers: bool, allows_revoting: bool, members_only: bool, country_codes: Vec<String>,
                                        shuffle_options: bool, hide_results_until_closes: bool, r#type: InputPollType, open_period: i32, close_date: i32, is_closed: bool }
struct InputPollOption [Default]      { text: FormattedText, media: Option<InputPollMedia> }
enum InputPollType: Regular(InputPollTypeRegular{allow_adding_options: bool}),
                    Quiz(InputPollTypeQuiz{correct_option_ids: Vec<i32>, explanation: FormattedText, explanation_media: Option<InputPollMedia>})
struct InputMessageForwarded [Default] { from_chat_id: i64, message_id: i64, in_game_share: bool, replace_video_start_timestamp: bool, new_video_start_timestamp: i32, copy_options: Option<MessageCopyOptions> }
struct MessageCopyOptions [Default]   { send_copy: bool, replace_caption: bool, new_caption: Option<FormattedText>, new_show_caption_above_media: bool }

enum InputFile: Local(InputFileLocal{path: String}),     // file on disk (absolute path recommended)
                Remote(InputFileRemote{id: String}),     // file.remote.id of an already-uploaded file (re-send without upload)
                Id(InputFileId{id: i32}),                // TDLib file id (file.id) in this session
                Generated(InputFileGenerated{original_path, conversion, expected_size})
  // all four variant structs are [Default]

struct MessageSendOptions [Default] { disable_notification: bool, from_background: bool, protect_content: bool,
  allow_paid_broadcast: bool, paid_message_star_count: i64, scheduling_state: Option<MessageSchedulingState>,
  effect_id: i64, sending_id: i32, only_preview: bool, suggested_post_info: Option<..>, update_order_of_installed_sticker_sets: bool }
enum MessageSchedulingState: SendAtDate(MessageSchedulingStateSendAtDate{send_date: i32, repeat_period: i32}), SendWhenOnline(..), SendWhenVideoProcessed(..)

enum InputMessageReplyTo: Message(InputMessageReplyToMessage), ExternalMessage(InputMessageReplyToExternalMessage), Story(..), EphemeralMessage(..)
struct InputMessageReplyToMessage [Default]         { message_id: i64, quote: Option<InputTextQuote>, checklist_task_id: i32, poll_option_id: String }
struct InputMessageReplyToExternalMessage [Default] { chat_id: i64, message_id: i64, quote: Option<InputTextQuote>, checklist_task_id: i32, poll_option_id: String }
```

### 8.5 Keyboards and callbacks

```text
enum ReplyMarkup: InlineKeyboard(ReplyMarkupInlineKeyboard), ShowKeyboard(ReplyMarkupShowKeyboard),
                  RemoveKeyboard(ReplyMarkupRemoveKeyboard), ForceReply(ReplyMarkupForceReply)
struct ReplyMarkupInlineKeyboard [Default] { rows: Vec<Vec<InlineKeyboardButton>>, force_reply: bool }
struct InlineKeyboardButton                { text: String, icon_custom_emoji_id: i64, style: ButtonStyle, r#type: InlineKeyboardButtonType }  // no Default
enum ButtonStyle: Default(ButtonStyleDefault{}), Primary(ButtonStylePrimary{}), Danger(ButtonStyleDanger{}), Success(ButtonStyleSuccess{}), Link(ButtonStyleLink{})
enum InlineKeyboardButtonType: Callback(InlineKeyboardButtonTypeCallback{data: Vec<u8>}), Url(InlineKeyboardButtonTypeUrl{url: String}),
  WebApp(..{url}), LoginUrl(..), CallbackWithPassword(..), CallbackGame(..), SwitchInline(..), Buy(..), User(..{user_id}),
  CopyText(..{text}), Disabled(..)
struct ReplyMarkupShowKeyboard [Default]   { rows: Vec<Vec<KeyboardButton>>, is_persistent: bool, resize_keyboard: bool, one_time: bool,
                                             is_personal: bool, force_reply: bool, input_field_placeholder: String }
struct KeyboardButton                      { text: String, icon_custom_emoji_id: i64, style: ButtonStyle, r#type: KeyboardButtonType }  // no Default
enum KeyboardButtonType: Text(KeyboardButtonTypeText{}), RequestPhoneNumber(KeyboardButtonTypeRequestPhoneNumber{}),
  RequestLocation(KeyboardButtonTypeRequestLocation{}), RequestPoll(..), RequestUsers(..), RequestChat(..), RequestManagedBot(..), WebApp(..)
struct ReplyMarkupRemoveKeyboard [Default] { is_personal: bool }
struct ReplyMarkupForceReply [Default]     { is_personal: bool, input_field_placeholder: String }

enum CallbackQueryPayload: Data(CallbackQueryPayloadData{data: Vec<u8>}), DataWithPassword(..), Game(..)
```

### 8.6 Inline mode

```text
enum InputInlineQueryResult: Article, Photo, Video, Document, Audio, Animation, Sticker, VoiceNote, Location, Venue, Contact, Game
struct InputInlineQueryResultArticle { id: String, url: String, title: String, description: String, thumbnail_url: String,
  thumbnail_width: i32, thumbnail_height: i32, reply_markup: Option<ReplyMarkup>, input_message_content: InputMessageContent }  // no Default
```

### 8.7 Chats, users, members

```text
struct Chat { id: i64, r#type: ChatType, title: String, photo: Option<ChatPhotoInfo>, permissions: ChatPermissions,
  last_message: Option<Message>, unread_count: i32, has_protected_content: bool, ...more }  // no Default
enum ChatType: Private(ChatTypePrivate{user_id: i64}), BasicGroup(ChatTypeBasicGroup{basic_group_id: i64}),
               Supergroup(ChatTypeSupergroup{supergroup_id: i64, is_channel: bool}), Secret(ChatTypeSecret{secret_chat_id: i32, user_id: i64})
enum ChatList: Main(ChatListMain{}), Archive(ChatListArchive{}), Folder(ChatListFolder{chat_folder_id: i32})

struct User { id: i64, first_name: String, last_name: String, usernames: Option<Usernames>, phone_number: String,
  status: UserStatus, is_contact: bool, is_premium: bool, r#type: UserType, language_code: String, ...more }  // no Default
struct Usernames [Default] { active_usernames: Vec<String>, disabled_usernames: Vec<String>, editable_username: String, collectible_usernames: Vec<String> }
enum UserType: Regular(UserTypeRegular), Bot(UserTypeBot), Deleted(UserTypeDeleted), Unknown(UserTypeUnknown)

struct ChatMember { member_id: MessageSender, tag: String, inviter_user_id: i64, joined_chat_date: i32, status: ChatMemberStatus }
enum ChatMemberStatus:
  Creator(ChatMemberStatusCreator{is_anonymous: bool, is_member: bool}),
  Administrator(ChatMemberStatusAdministrator{can_be_edited: bool, rights: ChatAdministratorRights}),
  Member(ChatMemberStatusMember{member_until_date: i32}),
  Restricted(ChatMemberStatusRestricted{is_member: bool, restricted_until_date: i32, permissions: ChatPermissions}),
  Left(ChatMemberStatusLeft{banned_until_date: i32}),
  Banned(ChatMemberStatusBanned{banned_until_date: i32})
  // all [Default]
struct ChatPermissions [Default] { can_send_basic_messages, can_send_audios, can_send_documents, can_send_photos, can_send_videos,
  can_send_video_notes, can_send_voice_notes, can_send_polls, can_send_other_messages, can_add_link_previews,
  can_react_to_messages, can_edit_tag, can_change_info, can_invite_users, can_pin_messages, can_create_topics: bool }
struct ChatAdministratorRights [Default] { can_manage_chat, can_change_info, can_post_messages, can_edit_messages,
  can_delete_messages, can_invite_users, can_restrict_members, can_pin_messages, can_manage_topics, can_promote_members,
  can_manage_video_chats, can_post_stories, can_edit_stories, can_delete_stories, can_manage_direct_messages,
  can_manage_tags, can_send_welcome_messages, is_anonymous: bool }
struct ChatAdministrators [Default] { administrators: Vec<ChatAdministrator{user_id: i64, custom_title: String, is_owner: bool, can_be_edited: bool}> }
struct ChatInviteLink [Default] { invite_link: String, name: String, creator_user_id: i64, expiration_date: i32, member_limit: i32, ... }
```

### 8.8 Bot configuration, actions, reactions

```text
struct BotCommand [Default] { command: String, description: String, is_ephemeral: bool }   // command without '/'
enum BotCommandScope: Default(BotCommandScopeDefault{}), AllPrivateChats(..{}), AllGroupChats(BotCommandScopeAllGroupChats{}),
  AllChatAdministrators(..{}), Chat(BotCommandScopeChat{chat_id: i64}), ChatAdministrators(..{chat_id}), ChatMember(..{chat_id, user_id})
enum ChatAction: Typing(ChatActionTyping{}), UploadingPhoto(ChatActionUploadingPhoto{progress: i32}), UploadingDocument(..{progress}),
  RecordingVoiceNote(..{}), UploadingVideo(..{progress}), ChoosingSticker(..{}), ChoosingLocation(..{}), Cancel(ChatActionCancel{}), ...
enum ReactionType: Emoji(ReactionTypeEmoji{emoji: String}), CustomEmoji(ReactionTypeCustomEmoji{custom_emoji_id: i64}), Paid(..{})
```

### 8.9 Files

```text
struct File [Default]       { id: i32, size: i64, expected_size: i64, local: LocalFile, remote: RemoteFile }
struct LocalFile [Default]  { path: String, can_be_downloaded: bool, can_be_deleted: bool, is_downloading_active: bool,
                              is_downloading_completed: bool, download_offset: i64, downloaded_prefix_size: i64, downloaded_size: i64 }
struct RemoteFile [Default] { id: String, unique_id: String, is_uploading_active: bool, is_uploading_completed: bool, uploaded_size: i64 }
```

### 8.10 Misc results

```text
struct Ok [Default]        { length: i32 }  // acknowledgement; ignore it
struct Messages [Default]  { total_count: i32, messages: Vec<Message> }
struct Chats [Default]     { total_count: i32, chat_ids: Vec<i64> }
struct types::Error [Default] { code: i32, message: String }  // TDLib error object (in UpdateMessageSendFailed)
enum OptionValue: Boolean(OptionValueBoolean{value: bool}), Empty(..), Integer(OptionValueInteger{value: i64}), String(OptionValueString{value: String})
enum AuthorizationState: WaitTdlibParameters, WaitPhoneNumber, WaitPremiumPurchase, WaitEmailAddress, WaitEmailCode, WaitCode,
  WaitOtherDeviceConfirmation, WaitRegistration, WaitPassword, Ready, LoggingOut, Closing, Closed
```

`tdrust::types::Error` (TDLib's error object) is different from
`tdrust::Error` (the framework's error enum). Don't glob-import both. Refer to
them by path when you need both.

---

## 9. Recipes

The snippets assume `use tdrust::prelude::*;` and a `client: Client` in scope.
Add the listed `tdrust::types` imports.

### 9.1 Send text, Markdown, HTML, reply

```rust
use tdrust::types::FormattedText;

client.send_text(chat_id, "plain text").await?;
client.send_text(chat_id, FormattedText::markdown("*bold* _italic_ `code` [link](https://t.me)")?).await?;
client.send_text(chat_id, FormattedText::html("<b>bold</b> <i>italic</i> <a href=\"https://t.me\">link</a>")?).await?;
client.reply_text(&message, "a reply").await?;   // replies in the same chat and topic
```

MarkdownV2 requires escaping these characters with `\` outside of entities:
`_ * [ ] ( ) ~ ` > # + - = | { } . !` and `\` itself. Escape user-provided
text before putting it into Markdown:

```rust
fn escape_md(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "_*[]()~`>#+-=|{}.!\\".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
// Mention a user by id (works without a username):
let t = FormattedText::markdown(format!("Hi [{}](tg://user?id={user_id})\\!", escape_md(&name)))?;
```

In Rust string literals, a Markdown `\!` is written `"\\!"`, or use raw strings
`r"Hi\!"`.

### 9.2 Send with options: silent, protected, disable link preview, schedule, to a forum topic

```rust
use tdrust::types::{FormattedText, InputMessageText, LinkPreviewOptions, MessageSendOptions,
                    MessageSchedulingStateSendAtDate, MessageTopicForum};

let content = InputMessageText {
    text: FormattedText::plain("see https://example.com"),
    link_preview_options: Some(LinkPreviewOptions { is_disabled: true, ..Default::default() }),
    clear_draft: false,
};
let options = MessageSendOptions {
    disable_notification: true,
    protect_content: true,
    // scheduling_state: Some(MessageSchedulingStateSendAtDate { send_date: unix_ts, repeat_period: 0 }.into()),
    ..Default::default()
};
let topic = Some(MessageTopicForum { forum_topic_id }.into()); // or None
client.send_message(chat_id, topic, None, Some(options), None, content).await?;
```

### 9.3 Reply to a specific message (other than `reply_text`)

```rust
use tdrust::types::{InputMessageReplyToMessage, InputMessageContent};

let reply_to = Some(InputMessageReplyToMessage { message_id, ..Default::default() }.into());
client.send_message(chat_id, None, reply_to, None, None, InputMessageContent::text("reply")).await?;
```

### 9.4 Inline keyboard + handling the button press

```rust
use tdrust::types::{
    ButtonStyleDefault, CallbackQueryPayload, InlineKeyboardButton, InlineKeyboardButtonTypeCallback,
    InlineKeyboardButtonTypeUrl, InputMessageContent, ReplyMarkup, ReplyMarkupInlineKeyboard, UpdateNewCallbackQuery,
};

fn callback_button(text: &str, data: &str) -> InlineKeyboardButton {
    InlineKeyboardButton {
        text: text.into(),
        icon_custom_emoji_id: 0,
        style: ButtonStyleDefault {}.into(),
        r#type: InlineKeyboardButtonTypeCallback { data: data.as_bytes().to_vec() }.into(), // max 64 bytes
    }
}
fn url_button(text: &str, url: &str) -> InlineKeyboardButton {
    InlineKeyboardButton {
        text: text.into(),
        icon_custom_emoji_id: 0,
        style: ButtonStyleDefault {}.into(),
        r#type: InlineKeyboardButtonTypeUrl { url: url.into() }.into(),
    }
}
fn inline_keyboard(rows: Vec<Vec<InlineKeyboardButton>>) -> ReplyMarkup {
    ReplyMarkupInlineKeyboard { rows, ..Default::default() }.into()
}

// send
let markup = inline_keyboard(vec![
    vec![callback_button("Yes", "vote:yes"), callback_button("No", "vote:no")],
    vec![url_button("Docs", "https://core.telegram.org")],
]);
client.send_message(chat_id, None, None, None, Some(markup), InputMessageContent::text("Continue?")).await?;

// handle (from the update loop: Update::NewCallbackQuery(q))
async fn on_callback(client: &Client, q: UpdateNewCallbackQuery) -> tdrust::Result<()> {
    let CallbackQueryPayload::Data(payload) = &q.payload else { return Ok(()) };
    let data = String::from_utf8_lossy(&payload.data).into_owned();
    // ALWAYS answer, or the button spinner keeps spinning on the user's side.
    client.answer_callback_query(q.id, "", false, "", 0).await?;   // text != "" shows a toast; show_alert=true shows a dialog
    // q.message_id is the final server id; editing works directly. None removes the keyboard.
    client.edit_message_text(q.chat_id, q.message_id, None, InputMessageContent::text(format!("You chose {data}"))).await?;
    Ok(())
}
```

Presses on buttons of messages sent through inline mode arrive as
`Update::NewInlineCallbackQuery` (with `inline_message_id: String`, no
chat/message ids).

### 9.5 Reply keyboard (custom keyboard below the input) and removing it

```rust
use tdrust::types::{ButtonStyleDefault, InputMessageContent, KeyboardButton, KeyboardButtonTypeRequestPhoneNumber,
                    KeyboardButtonTypeText, ReplyMarkupRemoveKeyboard, ReplyMarkupShowKeyboard};

fn text_key(text: &str) -> KeyboardButton {
    KeyboardButton { text: text.into(), icon_custom_emoji_id: 0, style: ButtonStyleDefault {}.into(), r#type: KeyboardButtonTypeText {}.into() }
}
let keyboard = ReplyMarkupShowKeyboard {
    rows: vec![vec![text_key("Option A"), text_key("Option B")]],
    resize_keyboard: true,
    one_time: true,
    ..Default::default()
};
client.send_message(chat_id, None, None, None, Some(keyboard.into()), InputMessageContent::text("Pick one")).await?;
// Pressing a text key simply sends that text as a normal message.

let remove = ReplyMarkupRemoveKeyboard { is_personal: false };
client.send_message(chat_id, None, None, None, Some(remove.into()), InputMessageContent::text("Thanks")).await?;
```

For a phone-request key, use `r#type: KeyboardButtonTypeRequestPhoneNumber {}.into()`.
The answer arrives as `MessageContent::Contact`.

### 9.6 Send a photo, document, video, or re-send an existing file

```rust
use tdrust::types::{ChatActionUploadingPhoto, FormattedText, InputDocument, InputFileLocal, InputFileRemote,
                    InputMessageDocument, InputMessagePhoto, InputPhoto};

let photo = InputMessagePhoto {
    photo: InputPhoto {
        photo: InputFileLocal { path: "/abs/path/cat.jpg".into() }.into(),
        thumbnail: None, video: None, added_sticker_file_ids: vec![], width: 0, height: 0,
    },
    caption: Some(FormattedText::plain("A cat")),
    show_caption_above_media: false, self_destruct_type: None, has_spoiler: false,
};
client.send_message(chat_id, None, None, None, None, photo).await?;

let doc = InputMessageDocument {
    document: InputDocument {
        document: InputFileLocal { path: "/abs/path/report.pdf".into() }.into(),
        thumbnail: None, disable_content_type_detection: false,
    },
    caption: None,
};
client.send_message(chat_id, None, None, None, None, doc).await?;

// Re-send a file Telegram already has (no upload): use File.remote.id from a received message
// as the InputFile of InputPhoto.photo / InputDocument.document:
//     InputFileRemote { id: file.remote.id.clone() }.into()
```

Width, height and duration may be 0 if unknown. Show "uploading" while
you prepare a file: `client.send_chat_action(chat_id, None, "", Some(ChatActionUploadingPhoto { progress: 0 }.into())).await?;`.
Uploads are asynchronous. `send_message` returns right away, and
`Update::MessageSendSucceeded` or `Update::MessageSendFailed` follows.

### 9.7 Download a received file

```rust
use tdrust::types::MessageContent;

let file_id = match &message.content {
    MessageContent::Photo(p) => p.photo.sizes.last().map(|s| s.photo.id),      // largest size
    MessageContent::Document(d) => Some(d.document.document.id),
    MessageContent::Video(v) => Some(v.video.video.id),
    MessageContent::Audio(a) => Some(a.audio.audio.id),
    MessageContent::VoiceNote(v) => Some(v.voice_note.voice.id),
    _ => None,
};
if let Some(file_id) = file_id {
    // synchronous=true: the call returns when the download is complete.
    let file = client.download_file(file_id, 1, 0, 0, true).await?;
    let bytes = std::fs::read(&file.local.path)?;  // tdrust::Error implements From<std::io::Error>
}
```

Downloaded files are stored under the files directory (default: the database
directory). For bots, there is a Telegram-side download limit on large files.

### 9.8 Set the bot's command menu

```rust
use tdrust::types::{BotCommand, BotCommandScopeAllGroupChats};

client.set_commands(None, "", vec![
    BotCommand { command: "start".into(), description: "Start the bot".into(), ..Default::default() },
    BotCommand { command: "help".into(), description: "Show help".into(), ..Default::default() },
]).await?;
// Scoped, e.g. only in groups:
client.set_commands(Some(BotCommandScopeAllGroupChats {}.into()), "", vec![/* ... */]).await?;
```

### 9.9 Typing indicator

```rust
use tdrust::types::ChatActionTyping;
client.send_chat_action(chat_id, None, "", Some(ChatActionTyping {}.into())).await?; // lasts ~5 s or until you send
```

### 9.10 Group moderation

```rust
use tdrust::types::{ChatMemberStatus, ChatMemberStatusMember, ChatMemberStatusRestricted, ChatPermissions,
                    MessageContent, MessageSenderUser};

// Is the user an admin?
let member = client.get_chat_member(chat_id, MessageSenderUser { user_id }).await?;
let is_admin = matches!(member.status, ChatMemberStatus::Creator(_) | ChatMemberStatus::Administrator(_));

// Delete a message for everyone
client.delete_messages(chat_id, vec![message.id], true).await?;

// Ban (0 = forever; otherwise a unix timestamp)
client.ban_chat_member(chat_id, MessageSenderUser { user_id }, 0, false).await?;

// Mute (restrict all permissions) until a time
let until = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 3600) as i32;
client.set_chat_member_status(chat_id, MessageSenderUser { user_id },
    ChatMemberStatusRestricted { is_member: true, restricted_until_date: until, permissions: ChatPermissions::default() }).await?;

// Unmute / unban back to a normal member
client.set_chat_member_status(chat_id, MessageSenderUser { user_id }, ChatMemberStatusMember { member_until_date: 0 }).await?;

// Welcome new members (service message)
match &message.content {
    MessageContent::ChatAddMembers(m) => { for id in &m.member_user_ids { /* greet *id */ } }
    MessageContent::ChatJoinByLink(_) | MessageContent::ChatJoinByRequest(_) => { /* greet message.sender_user_id() */ }
    _ => {}
}

// Approve join requests: Update::NewChatJoinRequest(u)
client.process_chat_join_request(u.chat_id, u.request.user_id, true).await?;
```

The bot must be an administrator with the matching right (e.g.
`can_restrict_members`, `can_delete_messages`). Otherwise you get error 400
(e.g. `CHAT_ADMIN_REQUIRED`). Moderation methods work in supergroups. Many
of them fail in legacy basic groups.

### 9.11 Chat info: private vs. group, sender name

```rust
use tdrust::types::ChatType;

let chat = client.get_chat(message.chat_id).await?; // cached, cheap
match &chat.r#type {
    ChatType::Private(p) => { /* 1:1 chat with user p.user_id */ }
    ChatType::BasicGroup(_) => { /* small legacy group */ }
    ChatType::Supergroup(s) if s.is_channel => { /* channel */ }
    ChatType::Supergroup(_) => { /* group */ }
    _ => {}
}
if let Some(user_id) = message.sender_user_id() {
    let user = client.get_user(user_id).await?;
    let display = format!("{} {}", user.first_name, user.last_name).trim().to_owned();
    let username = user.usernames.as_ref().map(|u| u.editable_username.clone()); // may be empty
}
```

### 9.12 Edit or delete a message you just sent (final ID)

`send_message` returns the message with a **temporary** ID. After the server
confirms it, `Update::MessageSendSucceeded { message, old_message_id }`
carries the final message. Edits, deletes, pins and reactions need the final
ID. This helper works whichever of the two events arrives first:

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

enum Entry { Waiting(oneshot::Sender<Message>), Done(Message) }

/// Maps temporary message ids to the final messages.
#[derive(Clone, Default)]
pub struct SentMessages(Arc<Mutex<HashMap<i64, Entry>>>);

impl SentMessages {
    /// Call from the update loop on Update::MessageSendSucceeded.
    pub fn succeeded(&self, old_message_id: i64, message: Message) {
        let mut map = self.0.lock().unwrap();
        match map.remove(&old_message_id) {
            Some(Entry::Waiting(tx)) => { let _ = tx.send(message); }
            _ => {
                if map.len() > 1000 { map.retain(|_, e| matches!(e, Entry::Waiting(_))); } // bound memory
                map.insert(old_message_id, Entry::Done(message));
            }
        }
    }
    /// Call from the update loop on Update::MessageSendFailed.
    pub fn failed(&self, old_message_id: i64) {
        self.0.lock().unwrap().remove(&old_message_id); // drops the sender, so `confirmed` returns None
    }
    /// Waits until a sent message is confirmed; None if sending failed.
    pub async fn confirmed(&self, temporary: &Message) -> Option<Message> {
        let rx = {
            let mut map = self.0.lock().unwrap();
            if let Some(Entry::Done(m)) = map.remove(&temporary.id) { return Some(m); }
            let (tx, rx) = oneshot::channel();
            map.insert(temporary.id, Entry::Waiting(tx));
            rx
        };
        rx.await.ok()
    }
}

// update loop:
//   Update::MessageSendSucceeded(u) => sent.succeeded(u.old_message_id, u.message),
//   Update::MessageSendFailed(u)    => sent.failed(u.old_message_id),
// handler:
let temp = client.send_text(chat_id, "Working...").await?;
if let Some(msg) = sent.confirmed(&temp).await {
    client.edit_message_text(msg.chat_id, msg.id, None, InputMessageContent::text("Done!")).await?;
}
```

Messages from `Update::NewMessage` (sent by others), and `q.message_id` from
callback queries, already have final IDs.

### 9.13 Inline mode (`@yourbot query` in any chat)

Enable inline mode for the bot in @BotFather first.

```rust
use tdrust::types::{InputInlineQueryResultArticle, InputMessageContent, UpdateNewInlineQuery};

async fn on_inline(client: &Client, q: UpdateNewInlineQuery) -> tdrust::Result<()> {
    let article = InputInlineQueryResultArticle {
        id: "echo".into(),
        url: String::new(),
        title: format!("Send \"{}\"", q.query),
        description: "Echo your text".into(),
        thumbnail_url: String::new(), thumbnail_width: 0, thumbnail_height: 0,
        reply_markup: None,
        input_message_content: InputMessageContent::text(q.query.clone()),
    };
    client.answer_inline_query(q.id, false, None, vec![article.into()], 300, "").await?;
    Ok(())
}
```

### 9.14 Forward or copy messages

```rust
// forward (shows "Forwarded from")
client.forward_messages(to_chat_id, None, from_chat_id, vec![message_id], None, false, false).await?;
// copy (no attribution); remove_caption=true drops the media caption
client.forward_messages(to_chat_id, None, from_chat_id, vec![message_id], None, true, false).await?;
```

### 9.15 Reactions (bot)

```rust
use tdrust::types::ReactionTypeEmoji;
client.set_message_reactions(chat_id, message_id, vec![ReactionTypeEmoji { emoji: "👍".into() }.into()], false).await?;
```

### 9.16 Polls and dice

```rust
use tdrust::types::{FormattedText, InputMessageDice, InputMessagePoll, InputPollOption, InputPollTypeRegular};

let poll = InputMessagePoll {
    question: FormattedText::plain("Lunch?"),
    options: vec![
        InputPollOption { text: FormattedText::plain("Pizza"), ..Default::default() },
        InputPollOption { text: FormattedText::plain("Sushi"), ..Default::default() },
    ],
    description: None, media: None,
    is_anonymous: true, allows_multiple_answers: false, allows_revoting: true, members_only: false,
    country_codes: vec![], shuffle_options: false, hide_results_until_closes: false,
    r#type: InputPollTypeRegular { allow_adding_options: false }.into(),
    open_period: 0, close_date: 0, is_closed: false,
};
client.send_message(chat_id, None, None, None, None, poll).await?;
client.send_message(chat_id, None, None, None, None, InputMessageDice { emoji: "🎲".into(), clear_draft: false }).await?;
```

### 9.17 Per-chat conversation state (multi-step dialogs)

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default)]
enum Step { #[default] Idle, AskedName, AskedAge { name: String } }

#[derive(Clone, Default)]
struct Sessions(Arc<Mutex<HashMap<(i64, i64), Step>>>); // key: (chat_id, user_id)

impl Sessions {
    fn get(&self, key: (i64, i64)) -> Step { self.0.lock().unwrap().get(&key).cloned().unwrap_or_default() }
    fn set(&self, key: (i64, i64), step: Step) { self.0.lock().unwrap().insert(key, step); }
}

// in on_message:
let key = (message.chat_id, message.sender_user_id().unwrap_or(0));
match (sessions.get(key), message.text()) {
    (_, Some("/register")) => { sessions.set(key, Step::AskedName); client.reply_text(&message, "Your name?").await?; }
    (Step::AskedName, Some(name)) => {
        sessions.set(key, Step::AskedAge { name: name.to_owned() });
        client.reply_text(&message, "Your age?").await?;
    }
    (Step::AskedAge { name }, Some(age)) => {
        sessions.set(key, Step::Idle);
        client.reply_text(&message, format!("Registered {name}, {age}")).await?;
    }
    _ => {}
}
```

Never hold a `std::sync::Mutex` guard across an `.await`. Copy the data out
first, as above. Because handlers are spawned, two quick messages from one
user can be processed concurrently. If ordering per chat matters, give each
chat its own queue: a `tokio::sync::mpsc` channel per `chat_id`, consumed by
one task.

### 9.18 Retry on flood wait (rate limits)

```rust
use std::future::Future;

async fn with_retry<T, F, Fut>(mut call: F) -> tdrust::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = tdrust::Result<T>>,
{
    for _ in 0..5 {
        match call().await {
            Err(err) => {
                let wait = err.as_td().and_then(|td| td.retry_after());
                match wait {
                    Some(duration) => tokio::time::sleep(duration).await,
                    None => return Err(err),
                }
            }
            ok => return ok,
        }
    }
    call().await
}

// usage
with_retry(|| client.send_text(chat_id, "hello")).await?;
```

Telegram's limits are roughly 1 message/second per chat, 20 messages/minute
per group, and 30 messages/second overall for bots. For broadcasts, pace your
sends (e.g. `tokio::time::sleep(Duration::from_millis(50))` between chats).

### 9.19 Periodic / scheduled jobs

```rust
let job_client = client.clone();
tokio::spawn(async move {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
    loop {
        tick.tick().await;
        if job_client.is_closed() { break; }
        if let Err(err) = job_client.send_text(ADMIN_CHAT_ID, "hourly report").await {
            log::warn!("report failed: {err}");
        }
    }
});
```

### 9.20 Sending to a chat the bot hasn't seen in this session

TDLib only sends to chats it knows. For a user who has started the bot
before, call `client.create_private_chat(user_id, false).await?` (the private
chat id equals the user id) or `client.get_chat(chat_id).await?` before
sending. Error 400 `Chat not found` means TDLib doesn't know the chat. Bots
cannot message users who never started them (403).

### 9.21 User account ("userbot"): list chats and read history

```rust
use tdrust::types::ChatListMain;

client.authorize(ConsoleAuthenticator::new()).await?;
if let Err(err) = client.load_chats(Some(ChatListMain {}.into()), 50).await {
    if err.as_td().map(|td| td.code) != Some(404) { return Err(err); } // 404 = all loaded
}
let chats = client.get_chats(Some(ChatListMain {}.into()), 50).await?;
for chat_id in chats.chat_ids {
    let chat = client.get_chat(chat_id).await?;
    println!("{} {}", chat.id, chat.title);
}

// Page through history, newest first
let mut from = 0i64;
loop {
    let page = client.get_chat_history(chat_id, from, 0, 100, false).await?;
    if page.messages.is_empty() { break; }
    for m in &page.messages { /* ... */ }
    from = page.messages.last().unwrap().id;
}
```

The first `get_chat_history` call may return fewer messages than requested,
even just one. Keep paging until the result is empty.

### 9.22 Raw JSON escape hatch

```rust
use serde_json::json;
let value = client.send_json(json!({"@type": "getMe"})).await?; // Result<serde_json::Value>
```

Use this for functions newer than the bindings. TDLib `error` responses still
become `Error::Td`.

---

## 10. Errors

```rust
#[non_exhaustive]
pub enum tdrust::Error {
    Td(TdError),                 // TDLib rejected the request
    Library(String),             // tdjson could not be loaded (fix TDJSON_PATH)
    ClientClosed(i32),           // client closed / closing
    InvalidRequest(String),      // request couldn't be encoded, or sync execute unsupported
    Decode { function: &'static str, source: serde_json::Error, json: Box<serde_json::Value> }, // TDLib/bindings version mismatch
    Config(String),              // missing api_id/api_hash, etc.
    Authorization(String),       // login could not complete
    Io(std::io::Error),          // From<std::io::Error>
}
impl Error { pub fn as_td(&self) -> Option<&TdError>; }

pub struct TdError { pub code: i32, pub message: String }
impl TdError {
    pub fn retry_after(&self) -> Option<Duration>; // for 429 "retry after N" and 420 FLOOD_WAIT_N
    pub fn is_silent(&self) -> bool;               // code 406: don't show to the user
}
```

`Error` implements `std::error::Error + Display + Send + Sync`. It converts
into `anyhow::Error` and `Box<dyn Error>` if you use those. Because the enum
is `#[non_exhaustive]`, a `match` on it needs a `_` arm.

| Code | Meaning / common messages |
| --- | --- |
| 400 | Bad request: `Chat not found`, `Message not found`, `MESSAGE_NOT_MODIFIED` (edit with identical content, safe to ignore), `Can't parse entities` (bad Markdown/HTML), `CHAT_ADMIN_REQUIRED`, `USER_NOT_PARTICIPANT` |
| 401 | Unauthorized: session revoked. Delete the database directory and log in again |
| 403 | Forbidden: bot blocked by the user, kicked from the group, or not allowed to write |
| 404 | Not found (also "nothing more to load" for `load_chats`) |
| 406 | Must not be shown to the user; ignore |
| 420 / 429 | Flood wait: use `retry_after()` |
| 500 | Internal error: retry later |

Pattern:

```rust
match client.get_chat(chat_id).await {
    Ok(chat) => { /* ... */ }
    Err(err) => match err.as_td() {
        Some(td) if td.code == 400 => log::info!("no such chat: {}", td.message),
        Some(td) if td.code == 403 => log::info!("blocked or kicked"),
        _ => return Err(err),
    },
}
```

---

## 11. Gotchas

1. **Own messages arrive as `Update::NewMessage`** with `is_outgoing == true`.
   Filter them, or the bot will answer itself in a loop.
2. **Temporary IDs:** see recipe 9.12. Editing a temporary ID fails with
   `Message not found`.
3. **Missed updates on startup:** Telegram delivers messages sent while the
   bot was offline. To ignore stale messages, compare `message.date` (unix
   seconds) with the start time.
4. **Privacy mode:** by default, bots in groups only receive commands,
   replies to them, and mentions. Disable it via @BotFather `/setprivacy`
   (then re-add the bot to the group) to see all messages.
5. **Commands in groups** look like `/cmd@BotName args`. Strip the `@BotName`
   part (see `parse_command` in §4).
6. **Text entity offsets are UTF-16 code units,** not bytes or chars. Prefer
   `FormattedText::markdown`/`html` over building entities by hand. If you
   must, compute offsets with `s.encode_utf16().count()`.
7. **`message.text()` is `None` for media.** The caption of a photo, video or
   document is in `MessageContent::Photo(p) => p.caption.text`, etc.
8. **`FormattedText::markdown` / `html` use TDLib synchronously** through the
   global runtime. They are cheap and need no client, but they do need the
   TDLib library to be loadable.
9. **Callback data is `Vec<u8>`** and is limited to 64 bytes. Always answer
   callback queries.
10. **`chat.r#type`, `entity.r#type`, `user.r#type`, button `r#type`:** the
    `type` field always needs the `r#` prefix.
11. **One database directory per account and per process.** A second process
    using the same directory will fail or hang, because TDLib locks it.
12. **Dropping all `Client` clones closes the client.** Don't let the `main`
    function's client go out of scope while tasks still run.
13. **Keep the update loop fast.** Spawn handlers. Everything in `route` must
    be non-blocking.
14. **Large enums change between TDLib versions.** Always include `_ => {}`.
    Undecodable updates arrive as `Update::Unrecognized(json)` and are logged
    at `warn` level.
15. **Nullable vs empty:** `Option` fields → `None`. Non-`Option` strings,
    numbers and vectors mean "absent" when empty or zero (e.g.
    `via_bot_user_id == 0`, `usernames.editable_username == ""`).
16. **`tdrust::types::Error` ≠ `tdrust::Error`.** Don't glob-import both.
17. **Bots can't use user-only methods** such as `load_chats`/`get_chats`
    (chat lists) or `search_chats`, and user accounts can't use "for bots
    only" methods such as `answer_callback_query`, `answer_inline_query`,
    `set_commands` and `set_message_reactions`.
18. **Group chat IDs change on upgrade:** when a basic group becomes a
    supergroup, a `MessageContent::ChatUpgradeTo { supergroup_id }` message
    appears, and the chat gets a new `-100…` ID.
19. **tokio features:** `ctrl_c` needs `signal`, `sleep`/`interval` need
    `time`, and `#[tokio::main]` needs `macros` + `rt-multi-thread`.

---

## 12. Testing without Telegram

Pure logic (command parsing, state machines, formatting) should be plain
functions with ordinary unit tests. To test code that calls `Client` without
TDLib, implement `tdrust::Backend` as a fake. The fake receives JSON requests
and pushes JSON responses and updates:

```rust
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use serde_json::{json, Value};
use tdrust::{Backend, Client, Runtime};

#[derive(Clone, Default)]
struct Fake { outbox: Arc<(Mutex<VecDeque<String>>, Condvar)>, sent: Arc<Mutex<Vec<Value>>> }

impl Fake {
    fn push(&self, v: Value) { self.outbox.0.lock().unwrap().push_back(v.to_string()); self.outbox.1.notify_all(); }
    fn update(&self, client_id: i32, mut v: Value) { v["@client_id"] = client_id.into(); self.push(v); }
}

impl Backend for Fake {
    fn create_client_id(&self) -> i32 { 1 }
    fn send(&self, client_id: i32, request: &str) {
        let req: Value = serde_json::from_str(request).unwrap();
        self.sent.lock().unwrap().push(req.clone());
        let auth = |state: &str| json!({"@type": "updateAuthorizationState", "authorization_state": {"@type": state}});
        let respond = |mut v: Value| { v["@extra"] = req["@extra"].clone(); v["@client_id"] = client_id.into(); v };
        match req["@type"].as_str().unwrap() {
            // The builder sends getOption first; TDLib then asks for parameters.
            "getOption" => { self.update(client_id, auth("authorizationStateWaitTdlibParameters"));
                             self.push(respond(json!({"@type": "optionValueString", "value": "1.8.67"}))); }
            "setTdlibParameters" => { self.push(respond(json!({"@type": "ok"})));
                                      self.update(client_id, auth("authorizationStateWaitPhoneNumber")); }
            "checkAuthenticationBotToken" => { self.push(respond(json!({"@type": "ok"})));
                                               self.update(client_id, auth("authorizationStateReady")); }
            "close" => { self.push(respond(json!({"@type": "ok"})));
                         self.update(client_id, auth("authorizationStateClosed")); }
            // Answer everything else with a minimal object, as your test needs:
            _ => self.push(respond(json!({"@type": "ok"}))),
        }
    }
    fn receive(&self, timeout: Duration) -> Option<String> {
        let (lock, cv) = &*self.outbox;
        let (mut q, _) = cv.wait_timeout_while(lock.lock().unwrap(), timeout, |q| q.is_empty()).unwrap();
        q.pop_front()
    }
    fn execute(&self, _request: &str) -> Option<String> {
        Some(json!({"@type": "error", "code": 400, "message": "unsupported in fake"}).to_string())
    }
}

#[tokio::test]
async fn logs_in_with_fake() {
    let fake = Fake::default();
    let (client, _updates) = Client::builder()
        .api_id(1).api_hash("x")
        .runtime(Runtime::with_backend(fake.clone()))
        .build().unwrap();
    client.authorize(tdrust::BotAuthenticator::new("1:abc")).await.unwrap();
    assert!(fake.sent.lock().unwrap().iter().any(|r| r["@type"] == "checkAuthenticationBotToken"));
}
```

Wire rules for the fake: responses must echo the request's `@extra` field,
updates must carry `@client_id`, `int64` values are JSON strings, and
`bytes` values are base64. `FormattedText::markdown`/`html` go through the
*global* runtime (real TDLib), so a fake backend does not serve them.
Mark tests that use them `#[ignore]`, or use `FormattedText::plain`.

To inject an incoming message in a test, push an update such as
`{"@type":"updateNewMessage","message":{...full message object...}}`. The
object must include every non-optional `Message` field, so it is usually
simpler to test handlers through their pure-logic parts.

---

## 13. Pre-delivery checklist

- [ ] New crate added to the workspace `members` (or depends on tdrust by an absolute path).
- [ ] tokio features include `macros`, `rt-multi-thread`, `signal` (and `time` if sleeping).
- [ ] Settings come from env / `.env`: `TELEGRAM_API_ID`, `TELEGRAM_API_HASH`, `TELEGRAM_BOT_TOKEN`, `TDJSON_PATH`. No secrets are hard-coded.
- [ ] `database_directory` is set to a dedicated folder.
- [ ] The update loop only routes and spawns. Handlers get `client.clone()`.
- [ ] `message.is_outgoing` messages are ignored.
- [ ] Every enum `match` has a `_` arm. `r#type` is used for `type` fields.
- [ ] Callback queries are always answered.
- [ ] Markdown text is escaped (or `FormattedText::plain` is used for user input).
- [ ] Edits/deletes of just-sent messages use the final ID (9.12).
- [ ] Errors in handlers are logged, not `unwrap`ped. A failed reply must not crash the bot.
- [ ] Ctrl+C triggers `client.shutdown().await`.
- [ ] `..Default::default()` is only used on `[Default]` structs (§8).
- [ ] Group features: the bot is an admin with the needed rights, and privacy mode is considered.
