# tdrust sample app

A small Telegram application built with [tdrust](https://github.com/rezaab69/tdrust).
It has three modes:

| Mode | What it does |
| --- | --- |
| `check` | Verifies that TDLib loads and your settings are complete. Does not connect to Telegram. |
| `bot` | Runs a bot that answers commands (`/help`, `/id`, `/whoami`, `/echo`, `/md`, `/stats`) and echoes other text. |
| `user` | Logs in with your own Telegram account, lists your recent chats, and prints new messages as they arrive. |

This guide takes you from a fresh Linux machine to a running bot. Every command
is meant to be copied into a terminal, one block at a time.

## Before you begin

You need:

- **Linux on x86_64 (64-bit Intel/AMD)**, ideally Ubuntu 22.04 or newer, or
  Debian 12 or newer. The commands below use `apt`. On another distribution,
  install the equivalent packages (a C compiler, `make`, `pkg-config`, `curl`,
  `unzip`, `git`, `python3` with `pip`). Check your architecture with
  `uname -m`; it must print `x86_64`.
- **A regular user with `sudo`**. Do not run the steps as root unless you use
  the quick install below.
- **Internet access** and about **3 GB of free disk space** for the tools and
  the build.
- **A Telegram account**, to create the bot and to get your API credentials.

> **On Windows?** Install Ubuntu in WSL with `wsl --install -d Ubuntu` in
> PowerShell, open the Ubuntu terminal, and follow this guide there.

### Quick install (optional)

On a fresh Ubuntu/Debian server you can let the project's install script do
steps 1 to 4 for you. It must run as root and puts everything in `/opt`:

```bash
curl -fsSL https://raw.githubusercontent.com/rezaab69/tdrust/main/install.sh -o install.sh
sudo bash install.sh
```

When it finishes, run `source ~/.cargo/env`, then continue with
[Step 5](#step-5-enter-your-settings). Use these two values in later steps:
the project is in `/opt/tdrust` (so the app is in
`/opt/tdrust/examples/sample-app`) and TDLib is at `/opt/tdlib/libtdjson.so`.
The script can be run again to update an existing install.

If you prefer to do it by hand, or want to see what is installed, continue
with step 1.

## Step 1: Install system packages

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config curl unzip ca-certificates git python3 python3-pip
```

`build-essential` provides the C compiler and linker that Rust needs.

## Step 2: Install Rust

Check whether Rust is already installed:

```bash
source ~/.cargo/env 2>/dev/null; cargo --version
```

If this prints `cargo 1.85` or newer, skip to step 3. If you see
`command not found` (or an older version), install Rust with rustup:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env
cargo --version
```

`source ~/.cargo/env` makes `cargo` available in the current terminal. New
terminals do this automatically.

## Step 3: Download the project

```bash
git clone https://github.com/rezaab69/tdrust.git ~/tdrust
cd ~/tdrust/examples/sample-app
```

The sample app lives in `examples/sample-app` inside the repository. It is a
standalone Rust project that uses the `tdrust` library from the folder above it,
so you can build it from this directory with a plain `cargo` command.

## Step 4: Get the TDLib library

The app needs `libtdjson`, TDLib's shared library. The quickest way to get a
prebuilt copy for Linux is the `tdjson` package on PyPI, which contains exactly
TDLib 1.8.67, the version tdrust's bindings are generated for:

```bash
mkdir -p ~/tdlib/wheel
cd ~/tdlib
python3 -m pip download "tdjson==1.8.67" --no-deps -d wheel
unzip -q wheel/*.whl 'tdjson.libs/*' -d wheel/x
cp wheel/x/tdjson.libs/* ~/tdlib/
ln -sfn "$(ls ~/tdlib/libtdjson-*.so* | head -n 1)" ~/tdlib/libtdjson.so
```

Check that the library and everything it depends on are present:

```bash
ldd ~/tdlib/libtdjson.so | grep "not found" || echo "TDLib is ready"
```

You should see `TDLib is ready`. The path you need in step 5 is
`~/tdlib/libtdjson.so`.

> Alternatively, build TDLib yourself with the
> [official instructions](https://tdlib.github.io/td/build.html) and use the
> path of the `libtdjson.so` it produces. Do this on ARM machines (`aarch64`),
> for which no prebuilt copy is used here.

Return to the app directory:

```bash
cd ~/tdrust/examples/sample-app
```

## Step 5: Enter your settings

You need:

- **API id and hash**: log in at <https://my.telegram.org>, open
  *API development tools*, and create an application (any name works).
- **Bot token** (for `bot` mode only): in Telegram, message
  [@BotFather](https://t.me/BotFather), send `/newbot`, and follow the prompts.
  For an existing bot, send `/token` to see its token.

Create the settings file from the template and open it:

```bash
cd ~/tdrust/examples/sample-app
cp .env.example .env
chmod 600 .env
nano .env
```

Fill in your values. `TDJSON_PATH` is the path from step 4:

```ini
TELEGRAM_API_ID=12345678
TELEGRAM_API_HASH=0123456789abcdef0123456789abcdef
TELEGRAM_BOT_TOKEN=123456789:AAAbbbCCCdddEEEfffGGGhhhIIIjjjKKKll
TDJSON_PATH=~/tdlib/libtdjson.so
```

Save with `Ctrl+O`, `Enter`, then exit with `Ctrl+X`. (If you used the quick
install, the app is in `/opt/tdrust/examples/sample-app` and `TDJSON_PATH` is
`/opt/tdlib/libtdjson.so`; use `sudo nano .env` if the folder is not yours.)

`.env` is excluded from git, and `chmod 600` makes it readable only by you.
Keep it private: the token and hash give full control of your bot and
application.

> **Prefer not to edit a file?** Skip the `nano` command. When you run `check`,
> `bot` or `user` in a terminal, the app asks for any missing or invalid API
> id, API hash or bot token (`check` skips the token; the hash and token are
> typed hidden), then offers to save them to `.env` so you are asked only once.
> It does not ask for `TDJSON_PATH`; set that in `.env` or the environment.
> Without a terminal, for example under systemd, it does not ask and reports
> what is missing instead.

## Step 6: Check the setup

```bash
cd ~/tdrust/examples/sample-app
cargo run -- check
```

The first run downloads and compiles the dependencies and about 2,000
generated TDLib types, so it takes **several minutes** (longer on a small
server). Later runs take seconds. Expected output:

```text
Checking the sample app setup...

  [ OK ] TDLib: version 1.8.67
  [ OK ] TELEGRAM_API_ID: 12345678
  [ OK ] TELEGRAM_API_HASH: set
  [ OK ] TELEGRAM_BOT_TOKEN: set
  [info] bot session directory: /home/you/.tdrust-sample/bot
  [info] user session directory: /home/you/.tdrust-sample/user

Everything is in place. Run `cargo run -- bot` or `-- user`.
```

If a line says `FAIL`, fix that setting in `.env` and run the check again.
See [Troubleshooting](#troubleshooting).

## Step 7: Run the bot

```bash
cargo run -- bot
```

After a few seconds you see:

```text
[... INFO ] Logging in as a bot (session: /home/you/.tdrust-sample/bot)
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

## Step 8: Log in with your own account (optional)

```bash
cargo run -- user
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

## Step 9: Keep the bot running (optional)

`cargo run -- bot` stops when you close the terminal. Two ways to keep it alive:

**Quick: tmux.** Good for trying things out.

```bash
sudo apt-get install -y tmux
tmux new -s bot
cd ~/tdrust/examples/sample-app && cargo run --release -- bot
```

Detach with `Ctrl+B`, then `D`. Come back later with `tmux attach -t bot`.

**Permanent: a systemd service.** It starts at boot and restarts after a crash.
First build the release binary and make sure `.env` is complete (the service
cannot ask questions):

```bash
cd ~/tdrust/examples/sample-app
cargo build --release
cargo run --release -- check
```

Then create the service file, replacing `YOUR_USER` with your Linux user name
(run `whoami` to see it) in both places:

```bash
sudo nano /etc/systemd/system/tdrust-bot.service
```

```ini
[Unit]
Description=tdrust sample bot
After=network-online.target
Wants=network-online.target

[Service]
User=YOUR_USER
ExecStart=/home/YOUR_USER/tdrust/examples/sample-app/target/release/sample-app bot
Restart=on-failure
RestartSec=5
# The bot shuts down cleanly on Ctrl+C (SIGINT), not on systemd's default SIGTERM.
KillSignal=SIGINT

[Install]
WantedBy=multi-user.target
```

Start it and enable it at boot:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now tdrust-bot
sudo systemctl status tdrust-bot      # is it running?
journalctl -u tdrust-bot -f           # follow the logs (Ctrl+C to leave)
```

The binary reads the `.env` file from the project folder, so keep the
`~/tdrust` checkout where it is.

## Everyday commands

Run these in `~/tdrust/examples/sample-app`:

```bash
cargo run -- check       # verify the setup
cargo run -- bot         # run the bot
cargo run -- user        # run with your account
RUST_LOG=debug cargo run -- bot   # more detailed logs
cargo test               # unit tests
```

To update to the latest version from GitHub:

```bash
cd ~/tdrust && git pull
cd examples/sample-app && cargo build --release
sudo systemctl restart tdrust-bot     # only if you use the service
```

To reset a login, stop the app and delete its saved session:
`rm -rf ~/.tdrust-sample/bot` (or `/user`).

## Troubleshooting

| Problem | Fix |
| --- | --- |
| `cargo: command not found` | Run `source ~/.cargo/env`, or install Rust (step 2). |
| `linker 'cc' not found` or a similar build error | The C toolchain is missing. Run `sudo apt-get install -y build-essential`. |
| The build ends with `Killed` | The server ran out of memory while compiling. Add swap space or build on a larger machine. |
| First build seems stuck | It compiles about 2,000 generated TDLib types; allow several minutes. |
| `cannot load TDLib` / `[FAIL] TDLib` | `TDJSON_PATH` is wrong. Run `ls ~/tdlib/` and make sure `.env` points at `~/tdlib/libtdjson.so`. If `ldd` in step 4 reported `not found`, repeat that step. |
| `TELEGRAM_API_ID is not set` | `.env` is missing or not in `examples/sample-app/`. Repeat step 5. |
| `API_ID_INVALID` | The API id or hash is wrong. Copy both again from <https://my.telegram.org>. |
| `ACCESS_TOKEN_INVALID` or other token errors | The bot token is wrong or was revoked. Get the current one from @BotFather with `/token`, put it in `.env`, and delete the old session: `rm -rf ~/.tdrust-sample/bot`. |
| The bot does not answer in a group | By default bots only see commands in groups. Disable privacy mode in @BotFather (`/setprivacy`) to let it see all messages. |
| `Too Many Requests: retry after N` | Telegram's rate limit. Wait N seconds. |
| The systemd service fails right away | Run `journalctl -u tdrust-bot -n 50`. The usual causes are an incomplete `.env` (run `cargo run --release -- check` first) or a wrong `YOUR_USER` or path in the service file. |

## How the code is organized

| File | Contents |
| --- | --- |
| [`src/main.rs`](src/main.rs) | Command-line modes, the `check` report, and error hints |
| [`src/config.rs`](src/config.rs) | Reading `.env`, the session directory, and the bot token |
| [`src/bot.rs`](src/bot.rs) | The bot: update loop, command handling, `/help` text |
| [`src/user.rs`](src/user.rs) | User mode: interactive login, chat list, live message feed |

To add a bot command, add a branch to the `match parse_command(text)` in
`reply_to` in `src/bot.rs` and a line to `HELP`. Every TDLib function is
available as a method on `client`. To read the tdrust documentation, run
`cargo doc --open` in `~/tdrust/tdrust`.
