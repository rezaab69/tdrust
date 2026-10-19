# tdrust sample app

A small Telegram application built with tdrust. It has three modes:

| Mode | What it does |
| --- | --- |
| `check` | Verifies that TDLib loads and your settings are complete. Does not connect to Telegram. |
| `bot` | Runs a bot that answers commands (`/help`, `/id`, `/whoami`, `/echo`, `/md`, `/stats`) and echoes other text. |
| `user` | Logs in with your own Telegram account, lists your recent chats, and prints new messages as they arrive. |

These instructions are for **Windows with WSL (Ubuntu)**, the setup this app
was tested on. On Linux or macOS, skip step 1 and run the same commands in a
normal terminal.

## Step 1: Open an Ubuntu terminal

In PowerShell or the Start menu, run:

```powershell
wsl -d Ubuntu
```

All following commands run in this Ubuntu terminal. Your Windows drive `D:` is
available as `/mnt/d`.

## Step 2: Check that Rust is installed

```bash
source ~/.cargo/env
cargo --version
```

You should see something like `cargo 1.98.1`. If you see
`command not found`, install Rust:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env
sudo apt-get update && sudo apt-get install -y build-essential   # C linker, if missing
```

## Step 3: Get the TDLib library

The app needs `libtdjson`, TDLib's shared library. The quickest way to get a
prebuilt copy for Linux is the `tdjson` package on PyPI, which contains exactly
TDLib 1.8.67, the version tdrust's bindings are generated for:

```bash
mkdir -p ~/tdjson && cd ~/tdjson
python3 -m pip download "tdjson==1.8.67" --no-deps -d .
python3 -c "import zipfile, glob; zipfile.ZipFile(glob.glob('*.whl')[0]).extractall('x')"
ls ~/tdjson/x/tdjson.libs/
```

The last command prints the library's file name, for example
`libtdjson-41165018.so.1.8.67`. You need its full path in step 4:
`~/tdjson/x/tdjson.libs/libtdjson-41165018.so.1.8.67`.

> Alternatively, build TDLib yourself with the
> [official instructions](https://tdlib.github.io/td/build.html) and use the
> path of the `libtdjson.so` it produces.

## Step 4: Enter your settings

You need:

- **API id and hash**: log in at <https://my.telegram.org>, open
  *API development tools*, and create an application (any name works).
- **Bot token** (for `bot` mode only): in Telegram, message
  [@BotFather](https://t.me/BotFather), send `/newbot`, and follow the prompts.
  For an existing bot, send `/token` to see its token.

> **Prefer not to edit a file?** You can skip the rest of this step. When you
> run `bot` or `user` in a terminal, the app asks for any missing or invalid
> API id, API hash or bot token (the hash and token are typed hidden), then
> offers to save them to `.env` so you are asked only once. It does not ask
> for `TDJSON_PATH`; set that in `.env` or the environment. Without a
> terminal, for example under systemd, it does not ask and reports what is
> missing instead.

Create the settings file from the template and open it:

```bash
cd /mnt/d/ClaudeCode/tdrust/sample-app
cp .env.example .env
nano .env
```

Fill in your values. `TDJSON_PATH` is the path from step 3:

```ini
TELEGRAM_API_ID=12345678
TELEGRAM_API_HASH=0123456789abcdef0123456789abcdef
TELEGRAM_BOT_TOKEN=123456789:AAAbbbCCCdddEEEfffGGGhhhIIIjjjKKKll
TDJSON_PATH=~/tdjson/x/tdjson.libs/libtdjson-41165018.so.1.8.67
```

Save with `Ctrl+O`, `Enter`, then exit with `Ctrl+X`. (You can also edit
`D:\ClaudeCode\tdrust\sample-app\.env` in VS Code on Windows.)

`.env` is excluded from git. Keep it private: the token and hash give full
control of your bot and application.

## Step 5: Check the setup

```bash
cd /mnt/d/ClaudeCode/tdrust
export CARGO_TARGET_DIR=~/tdrust-target
cargo run -p sample-app -- check
```

`CARGO_TARGET_DIR` puts build files on the fast Linux file system; the first
build takes a few minutes, later ones seconds. To set it permanently, run
`echo 'export CARGO_TARGET_DIR=~/tdrust-target' >> ~/.bashrc`.

Expected output:

```text
Checking the sample app setup...

  [ OK ] TDLib: version 1.8.67
  [ OK ] TELEGRAM_API_ID: 12345678
  [ OK ] TELEGRAM_API_HASH: set
  [ OK ] TELEGRAM_BOT_TOKEN: set
  [info] bot session directory: /root/.tdrust-sample/bot
  [info] user session directory: /root/.tdrust-sample/user

Everything is in place. Run `cargo run -p sample-app -- bot` or `-- user`.
```

If a line says `FAIL`, fix that setting in `.env` and run the check again.

## Step 6: Run the bot

```bash
cargo run -p sample-app -- bot
```

After a few seconds you see:

```text
[... INFO ] Logging in as a bot (session: /root/.tdrust-sample/bot)
[... INFO ] Running as @YourBot. Open https://t.me/YourBot and send /start. Press Ctrl+C to stop.
```

Open the link in Telegram and try:

| Send | The bot answers with |
| --- | --- |
| `/start` or `/help` | The list of commands, formatted |
| `/id` | The chat id and your user id |
| `/whoami` | Your name, username, language and Premium status |
| `/echo hello` | `hello` |
| `/md *bold* _italic_` | The text rendered as bold and italic |
| `/stats` | Uptime and the number of messages handled |
| any other text | The same text |
| a photo, sticker, ... | A note that it only reads text |

Each incoming message is logged in the terminal. Press **Ctrl+C** to stop the
bot; it closes its session cleanly. The next start logs in from the saved
session.

## Step 7: Log in with your own account (optional)

```bash
cargo run -p sample-app -- user
```

The app asks for:

1. **Phone number** in international format, for example `+15551234567`.
2. **Login code**: Telegram sends it to your other Telegram apps (or by SMS).
3. **2FA password**, if you set one. It is not shown while you type.

It then lists your 10 most recent chats and prints new incoming messages:

```text
Logged in as Jane Doe (user id 123456789).

Your 10 most recent chats:
        777000  Telegram
    -100123...  Rust Community
...
Waiting for new messages. Press Ctrl+C to stop.

[Rust Community] Has anyone tried tdrust?
```

The app appears in *Settings > Devices* of your Telegram account as
"tdrust sample app"; you can end the session there at any time. Only read
messages this way; automated sending from personal accounts can get them
limited by Telegram.

## Everyday commands

```bash
cd /mnt/d/ClaudeCode/tdrust && export CARGO_TARGET_DIR=~/tdrust-target
cargo run -p sample-app -- check       # verify the setup
cargo run -p sample-app -- bot         # run the bot
cargo run -p sample-app -- user        # run with your account
RUST_LOG=debug cargo run -p sample-app -- bot   # more detailed logs
cargo test -p sample-app               # unit tests
```

For a faster, standalone binary:

```bash
cargo build -p sample-app --release
~/tdrust-target/release/sample-app bot
```

The binary still reads `sample-app/.env` from the project directory.

## Troubleshooting

| Problem | Fix |
| --- | --- |
| `cannot load TDLib` / `[FAIL] TDLib` | `TDJSON_PATH` is wrong. Run `ls ~/tdjson/x/tdjson.libs/` and copy the exact file name into `.env`. |
| `TELEGRAM_API_ID is not set` | `.env` is missing or not in `sample-app/`. Repeat step 4. |
| `API_ID_INVALID` | The API id or hash is wrong. Copy both again from <https://my.telegram.org>. |
| `ACCESS_TOKEN_INVALID` or other token errors | The bot token is wrong or was revoked. Get the current one from @BotFather with `/token`, put it in `.env`, and delete the old session: `rm -rf ~/.tdrust-sample/bot`. |
| The bot does not answer in a group | By default bots only see commands in groups. Disable privacy mode in @BotFather (`/setprivacy`) to let it see all messages. |
| `Too Many Requests: retry after N` | Telegram's rate limit. Wait N seconds. |
| Log out, or switch to another account/bot | Stop the app and delete its session: `rm -rf ~/.tdrust-sample/user` (or `/bot`). |
| `cargo: command not found` | Run `source ~/.cargo/env`, or install Rust (step 2). |
| First build seems stuck | It compiles about 2,000 generated TDLib types; allow a few minutes. |

## How the code is organized

| File | Contents |
| --- | --- |
| [`src/main.rs`](src/main.rs) | Command-line modes, the `check` report, and error hints |
| [`src/config.rs`](src/config.rs) | Reading `.env`, the session directory, and the bot token |
| [`src/bot.rs`](src/bot.rs) | The bot: update loop, command handling, `/help` text |
| [`src/user.rs`](src/user.rs) | User mode: interactive login, chat list, live message feed |

To add a bot command, add a branch to the `match parse_command(text)` in
`reply_to` in `src/bot.rs` and a line to `HELP`. Every TDLib function is
available as a method on `client`; see the tdrust documentation with
`cargo doc -p tdrust --open`.
