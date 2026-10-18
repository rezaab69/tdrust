# tdrust

An async, fully typed Rust framework for [TDLib](https://core.telegram.org/tdlib),
Telegram's official client library. Build user clients, bots and automation
tools with the whole Telegram API available as ordinary Rust types and `async`
methods.

```rust
use tdrust::prelude::*;

#[tokio::main]
async fn main() -> tdrust::Result<()> {
    let (client, mut updates) = Client::builder().build()?;
    client.authorize(ConsoleAuthenticator::new()).await?;

    let me = client.get_me().await?;
    println!("Logged in as {}", me.first_name);

    while let Some(update) = updates.next().await {
        if let Update::NewMessage(update) = update {
            if update.message.text() == Some("ping") {
                client.reply_text(&update.message, "pong").await?;
            }
        }
    }
    Ok(())
}
```

## Features

- **The complete API, typed.** All ~2,200 TDLib objects and ~1,000 functions
  are generated from TDLib's own schema, with its documentation attached.
  Every function is an async method on `Client` (`client.send_message(...)`,
  `client.get_chat_history(...)`), and a request struct in `tdrust::functions`
  for when you prefer named fields and `..Default::default()`.
- **Idiomatic types.** Abstract types are enums with short variant names
  (`Update::NewMessage`, `MessageContent::Text`); optional fields are
  `Option`; 64-bit ids are `i64`, and binary data is `Vec<u8>`. The string and
  base64 encoding TDLib uses on the wire is handled for you.
- **Forward compatible.** Objects newer than the bindings arrive as
  `Unrecognized(serde_json::Value)` instead of breaking your program, and
  `send_json` can call any function by name.
- **One-call login.** `client.authorize(...)` handles TDLib's whole
  authorization state machine: phone number, code, 2FA password,
  registration, email, and QR code login. Use the built-in console or bot
  authenticators, or implement `Authenticator` for your own UI.
- **Many clients, one thread.** A single receiver thread routes responses and
  updates for any number of accounts. Requests work on any async executor.
- **No build-time TDLib dependency.** `tdjson` is loaded at run time, and a
  clear error explains where it was looked for. Link it at build time instead
  with the `link` feature.
- **Testable.** The `Backend` trait lets you substitute a scripted fake for
  TDLib in your own tests, which is how tdrust tests itself.

## Getting started

> **Just want to see it run?** Follow the
> [sample app guide](sample-app/README.md): it walks through every step, from
> getting TDLib to chatting with your own bot.

### 1. Get TDLib

tdrust needs the `tdjson` shared library (`libtdjson.so`, `libtdjson.dylib`,
or `tdjson.dll`). Build it with the
[official instructions](https://tdlib.github.io/td/build.html). The bindings
are generated for TDLib **1.8.67**; see [Versions](#versions) for others.

tdrust looks for the library, in order:

1. at the path in the `TDJSON_PATH` environment variable;
2. next to your executable;
3. on the system library path (`PATH`, `LD_LIBRARY_PATH`, `DYLD_LIBRARY_PATH`).

To load it from somewhere else, install a runtime before creating clients:

```rust
tdrust::Runtime::with_backend(tdrust::Tdjson::load_from("/opt/tdlib/libtdjson.so")?)
    .set_global()
    .expect("runtime already initialized");
```

### 2. Get API credentials

Create an application at <https://my.telegram.org> to get an `api_id` and
`api_hash`. Pass them to the builder, or set `TELEGRAM_API_ID` and
`TELEGRAM_API_HASH` in the environment.

### 3. Add the dependency

```toml
[dependencies]
tdrust = { path = "path/to/tdrust/tdrust" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

tdrust works with any async runtime; tokio is only used in the examples.

## Guide

### Creating a client

```rust
let (client, updates) = tdrust::Client::builder()
    .api_id(12345)
    .api_hash("0123456789abcdef0123456789abcdef")
    .database_directory("sessions/alice") // one directory per account
    .system_language_code("de")
    .build()?;
```

`build` returns the client and its update stream. `Client` is cheap to clone
and can be used from many tasks at once. When the last clone is dropped, the
TDLib instance is closed; call `client.shutdown().await` to close it and wait
until its databases have been flushed.

### Logging in

```rust
// Interactive, in the terminal (asks for the phone number, code, password, ...):
client.authorize(tdrust::ConsoleAuthenticator::new()).await?;

// As a bot:
client.authorize(tdrust::BotAuthenticator::new(token)).await?;
```

The session is stored in the database directory, so the next run logs in
without asking. For a GUI or web login, implement `Authenticator`. Only
`login` is required, and every step receives the details TDLib provides:

```rust
use tdrust::{Authenticator, AuthStep, Login, Result, TdError};
use tdrust::types::AuthenticationCodeInfo;

struct WebLogin { /* channels to your UI */ }

impl Authenticator for WebLogin {
    async fn login(&mut self) -> Result<Login> {
        Ok(Login::PhoneNumber(ask_ui_for_phone().await))
    }
    async fn code(&mut self, info: &AuthenticationCodeInfo) -> Result<String> {
        Ok(ask_ui_for_code(&info.phone_number).await)
    }
    async fn on_error(&mut self, _step: AuthStep, error: &TdError) -> Result<()> {
        show_error(&error.message).await;
        Ok(()) // ask again
    }
}
```

### Calling the API

Every TDLib function has three equivalent forms:

```rust
use tdrust::functions::GetChatHistory;
use serde_json::json;

// 1. A method with positional arguments.
let history = client.get_chat_history(chat_id, 0, 0, 50, false).await?;

// 2. A request struct: named fields and defaults.
let history = client.send(GetChatHistory { chat_id, limit: 50, ..Default::default() }).await?;

// 3. Raw JSON, for functions newer than the bindings.
let history = client.send_json(json!({"@type": "getChatHistory", "chat_id": chat_id, "limit": 50})).await?;
```

Abstract parameters accept any of their variants through `Into`, so you
rarely spell out the enum:

```rust
use tdrust::types::{ChatListArchive, FormattedText, InputMessageContent};

client.get_chats(Some(ChatListArchive {}.into()), 100).await?;
client.send_text(chat_id, FormattedText::markdown("*Bold* and _italic_")?).await?;
client.send_message(chat_id, None, None, None, None, InputMessageContent::text("hi")).await?;
```

Functions TDLib can run without a client are available synchronously:

```rust
let entities = tdrust::execute(tdrust::functions::GetTextEntities { text: "t.me/telegram".into() })?;
println!("TDLib {}", tdrust::tdlib_version()?);
tdrust::set_log_verbosity(2)?;
```

### Handling updates

```rust
use tdrust::types::{MessageContent, Update};

while let Some(update) = updates.next().await {
    match update {
        Update::NewMessage(update) => match &update.message.content {
            MessageContent::Text(text) => println!("text: {}", text.text.text),
            MessageContent::Photo(_) => println!("a photo"),
            other => println!("{}", other.type_name()),
        },
        Update::ChatTitle(update) => println!("chat {} renamed to {}", update.chat_id, update.title),
        _ => {}
    }
}
```

Updates are buffered until read, so either read them continuously or drop the
stream to discard them. `Updates` also implements `futures_core::Stream` for
use with stream combinators. Handle slow work in spawned tasks so the update
loop keeps up; see [`examples/echo_bot.rs`](tdrust/examples/echo_bot.rs).

### Errors

All fallible operations return `tdrust::Result<T>`. TDLib errors are
`Error::Td(TdError { code, message })`, and flood waits tell you how long to
back off:

```rust
match client.get_chat(chat_id).await {
    Ok(chat) => println!("{}", chat.title),
    Err(err) => match err.as_td() {
        Some(td) if td.code == 400 => println!("no such chat: {}", td.message),
        Some(td) if td.retry_after().is_some() => tokio::time::sleep(td.retry_after().unwrap()).await,
        _ => return Err(err),
    },
}
```

Other variants cover a missing library (`Library`), a closed client
(`ClientClosed`), bad configuration (`Config`), login problems
(`Authorization`), and responses that do not match the bindings (`Decode`,
which includes the raw JSON).

### Several accounts

Create one client per account, each with its own database directory. All
clients share the same runtime and receiver thread:

```rust
let (alice, alice_updates) = Client::builder().database_directory("sessions/alice").build()?;
let (bob, bob_updates) = Client::builder().database_directory("sessions/bob").build()?;
```

### Testing your code

Implement `tdrust::Backend` to stand in for TDLib, and pass it with
`ClientBuilder::runtime(Runtime::with_backend(fake))`. See
[`tdrust/tests/common/mod.rs`](tdrust/tests/common/mod.rs) for a scripted fake
that answers requests and emits updates.

## Reference

### Environment variables

| Variable | Used for |
| --- | --- |
| `TELEGRAM_API_ID`, `TELEGRAM_API_HASH` | API credentials when not set on the builder |
| `TDJSON_PATH` | Path of the `tdjson` library file to load |
| `TDRUST_TL_SCHEMA` | Build time: `td_api.tl` to generate the bindings from |
| `TDJSON_LIB_DIR` | Build time, with the `link` feature: directory containing `tdjson` |

### Cargo features

| Feature | Default | Effect |
| --- | --- | --- |
| `console` | yes | Hidden password input for `ConsoleAuthenticator` |
| `link` | no | Link `tdjson` at build time instead of loading it at run time |

### Versions

The bindings are generated at build time from
[`tdrust/tl/td_api.tl`](tdrust/tl/td_api.tl), the schema of TDLib 1.8.67
(`tdrust::BUNDLED_TDLIB_VERSION`). With a different TDLib version, new objects
arrive as `Unrecognized` and changed ones may fail to decode. To generate
matching bindings, point `TDRUST_TL_SCHEMA` at the `td/generate/scheme/td_api.tl`
of the version you use, or replace the bundled file.

### Examples

| Example | Shows |
| --- | --- |
| [`offline`](tdrust/examples/offline.rs) | Checking that TDLib loads; synchronous calls; raw JSON |
| [`login`](tdrust/examples/login.rs) | Interactive login and listing chats |
| [`echo_bot`](tdrust/examples/echo_bot.rs) | A bot with an update loop, Markdown, and graceful shutdown |

```sh
cargo run --example offline
TELEGRAM_BOT_TOKEN=... cargo run --example echo_bot
```

## Project layout

| Path | Contents |
| --- | --- |
| [`tdrust/`](tdrust) | The framework: runtime, client, login, conveniences, and generated API |
| [`tdrust-tl/`](tdrust-tl) | The `td_api.tl` parser and Rust code generator used by the build script |
| [`sample-app/`](sample-app) | A complete sample application (command bot and user client) with [step-by-step run instructions](sample-app/README.md) |

## Development

```sh
cargo test                     # unit, integration (against a fake TDLib) and doc tests
cargo clippy --all-targets --all-features
TDJSON_PATH=/path/to/libtdjson.so cargo test --test live -- --ignored   # against real TDLib
```

## License

MIT. TDLib itself is distributed under the Boost Software License.
