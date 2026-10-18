# Running group-bot on an Ubuntu VPS

A complete, step-by-step setup: PostgreSQL, Redis, TDLib, building the bot,
running one or more bots as hardened systemd services, updating and backups.

Commands run **on the VPS as root** unless marked **(on your PC)**. Replace
`SERVER_IP` with your server's address and `DB_PASSWORD` with the database
password you create in step 6.

| What | Where on the VPS |
| --- | --- |
| Source code | `/opt/group-bot/src` |
| Bot program | `/opt/group-bot/group-bot` |
| Shared settings | `/opt/group-bot/.env` |
| One bot's token and session | `/opt/group-bot/bots/<name>.env`, `/opt/group-bot/bots/<name>/` |
| TDLib library | `/opt/tdlib/libtdjson.so` |
| Services | `group-bot@<name>` (systemd), `postgresql`, `redis-server` |

---

## 0. Before you start

- A VPS with **Ubuntu 22.04 or 24.04, x86_64** (`uname -m` shows `x86_64`),
  at least **1 GB RAM** (2 GB recommended) and 10 GB of disk. Each running
  bot uses roughly 100–200 MB of RAM.
- SSH access as root (or a user with `sudo`).
- Your `TELEGRAM_API_ID` and `TELEGRAM_API_HASH` (https://my.telegram.org) and
  the token of every bot (@BotFather).
- **Stop the bot on your PC first.** A bot token must run in one place only;
  running the same bot in WSL and on the VPS at the same time makes both
  misbehave. In WSL:

  ```sh
  systemctl disable --now 'group-bot@*'
  ```

## 1. Update the server

```sh
ssh root@SERVER_IP
apt update && apt upgrade -y
timedatectl set-timezone Asia/Tehran      # optional; the bot has its own per-group time zone
```

## 2. Firewall

The bot only makes outgoing connections; no ports need to be opened except SSH.

```sh
ufw allow OpenSSH
ufw --force enable
```

## 3. Swap (if the server has less than 4 GB of RAM)

Compiling needs about 2–3 GB of memory. Swap prevents the build from being killed.

```sh
fallocate -l 4G /swapfile && chmod 600 /swapfile && mkswap /swapfile && swapon /swapfile
echo '/swapfile none swap sw 0 0' >> /etc/fstab
free -h
```

## 4. Install the system packages

```sh
apt install -y build-essential pkg-config curl unzip rsync ca-certificates python3 postgresql redis-server
```

## 5. Create the service user and folders

The bot runs as an unprivileged user that can only write its own session files.

```sh
useradd --system --home /opt/group-bot --shell /usr/sbin/nologin groupbot
mkdir -p /opt/group-bot/bots /opt/group-bot/src /opt/tdlib
chown groupbot:groupbot /opt/group-bot/bots
```

## 6. PostgreSQL

Create the database and a login with a random password. PostgreSQL listens on
localhost only, which is what you want.

```sh
DB_PASSWORD=$(openssl rand -hex 24)
echo "Database password: $DB_PASSWORD"     # write it down; step 11 needs it
sudo -u postgres psql -c "CREATE ROLE bot LOGIN PASSWORD '$DB_PASSWORD';"
sudo -u postgres createdb -O bot groupbot
psql "postgres://bot:$DB_PASSWORD@localhost:5432/groupbot" -c 'SELECT 1'   # must print 1
```

The bot creates and upgrades its tables by itself when it starts.

## 7. Redis

Turn on persistence so counters and deadlines survive a restart, and check
that it answers. By default it listens on localhost only.

```sh
sed -i 's/^appendonly no/appendonly yes/' /etc/redis/redis.conf
systemctl enable redis-server
systemctl restart redis-server
redis-cli ping                             # PONG
```

## 8. TDLib

The bot needs TDLib's `libtdjson.so`. The quickest source is the official
prebuilt package on PyPI (TDLib 1.8.67, the version the bot is built for).
It is self-contained and works on any recent Ubuntu.

```sh
cd /tmp
URL=$(curl -s https://pypi.org/pypi/tdjson/1.8.67/json | python3 -c \
  'import sys,json; print(next(u["url"] for u in json.load(sys.stdin)["urls"] if "manylinux" in u["filename"] and "x86_64" in u["filename"]))')
curl -L -o tdjson.whl "$URL"
unzip -o -j tdjson.whl 'tdjson.libs/libtdjson-*' -d /opt/tdlib
mv /opt/tdlib/libtdjson-*.so* /opt/tdlib/libtdjson.so
chmod 644 /opt/tdlib/libtdjson.so
ldd /opt/tdlib/libtdjson.so | grep 'not found' || echo "TDLib OK"
```

> Do not copy the `libtdjson.so` from your PC. That build needs extra
> libraries (libc++, libunwind) that a fresh server does not have.
>
> On an ARM server (`uname -m` shows `aarch64`), build TDLib from source with
> the official instructions (https://tdlib.github.io/td/build.html) and copy
> the resulting `libtdjson.so` to `/opt/tdlib/`.

## 9. Rust

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
source ~/.cargo/env
cargo --version
```

## 10. Copy the source code and build

**(on your PC, in the WSL/Ubuntu terminal)** upload the project. The command
leaves out build output, secrets and the local TDLib copy:

```sh
rsync -az --delete --exclude target --exclude '.env' --exclude libtdjson --exclude '.git' \
  /mnt/d/ClaudeCode/tdrust/ root@SERVER_IP:/opt/group-bot/src/
```

**(on the VPS)** build and install the program. The first build takes 10–20
minutes.

```sh
cd /opt/group-bot/src
cargo build -p group-bot --release
install -m 755 target/release/group-bot /opt/group-bot/group-bot
```

> Building on the VPS is the safe choice. A binary built in WSL (Ubuntu 24.04)
> only runs on Ubuntu 24.04 or newer.

## 11. Shared settings

```sh
nano /opt/group-bot/.env
```

Paste and fill in. The bot token does not go here; each bot gets its own in step 12.

```ini
TELEGRAM_API_ID=12345678
TELEGRAM_API_HASH=0123456789abcdef0123456789abcdef
TDJSON_PATH=/opt/tdlib/libtdjson.so

DATABASE_URL=postgres://bot:DB_PASSWORD@localhost:5432/groupbot
DATABASE_MAX_CONNECTIONS=10
REDIS_URL=redis://127.0.0.1:6379
REDIS_PREFIX=gb:

# Your own Telegram user id(s), comma separated: full control in every group,
# and the operator panel (send /admin to the bot in a private chat)
BOT_SUDO_IDS=
DEFAULT_LANG=fa
# In-bot backups and broadcast media, shared by all bots (inside the writable folder)
DATA_DIR=/opt/group-bot/bots/data
RUST_LOG=info
```

Protect the file, since it contains secrets:

```sh
chown root:groupbot /opt/group-bot/.env
chmod 640 /opt/group-bot/.env
```

## 12. Start the bots

Each bot is a separate service, `group-bot@<name>`. The helper script asks for
the token without showing it on screen. It then writes
`/opt/group-bot/bots/<name>.env`, installs the service and starts it:

```sh
cd /opt/group-bot/src/group-bot/deploy
chmod +x add-bot.sh
export GROUP_BOT_HOME=/opt/group-bot GROUP_BOT_USER=groupbot GROUP_BOT_UNIT=$PWD/vps/group-bot@.service
./add-bot.sh main          # your first bot
./add-bot.sh bot2          # every additional bot (optional)
```

Each run should end with `active` and a log line like
`running as @YourBot (123456789)`.

All bots share the same database and Redis. Every bot in a group is active,
and each message is handled by exactly one of them. One bot per group runs its
scheduled jobs; another takes those over within about 90 seconds if it stops.
See [Running several bots](README.md#running-several-bots).

## 13. Telegram setup (for every bot)

1. In @BotFather, send `/setprivacy`, choose the bot and select **Disable**, so it sees all messages.
2. Add the bot to its group(s) and make it an **administrator** with the rights to
   delete messages, ban users, pin messages, invite users and change info.
3. In the group, send `پنل` (or `Panel`) for the control panel, and `ربات ها`
   (`Bots`) to see which bot serves the group.

## 14. Check that everything runs

```sh
systemctl list-units 'group-bot@*'         # every bot: active (running)
journalctl -u group-bot@main -f            # live log of one bot (Ctrl+C to leave)
systemctl status postgresql redis-server
```

The services start automatically when the server reboots.

## 15. Moving your existing data (optional)

To keep the settings, ranks, filters and statistics from your PC, copy the
database **before** starting the bots on the VPS (step 12), while the VPS
database is still empty.

**(on your PC, in WSL)**

```sh
DATABASE_URL=$(grep -E '^DATABASE_URL=' /mnt/d/ClaudeCode/tdrust/group-bot/.env | cut -d= -f2- | tr -d '"\r')
pg_dump -Fc "$DATABASE_URL" -f groupbot.dump
scp groupbot.dump root@SERVER_IP:/tmp/
```

**(on the VPS)**

```sh
pg_restore --no-owner --role=bot -d "postgres://bot:DB_PASSWORD@localhost:5432/groupbot" /tmp/groupbot.dump
rm /tmp/groupbot.dump
```

Groups are then claimed by whichever VPS bot sees them first.

## 16. Updating to a new version

**(on your PC)** upload the changed code with the same command as in step 10:

```sh
rsync -az --delete --exclude target --exclude '.env' --exclude libtdjson --exclude '.git' \
  /mnt/d/ClaudeCode/tdrust/ root@SERVER_IP:/opt/group-bot/src/
```

**(on the VPS)** rebuild, install and restart all bots:

```sh
cd /opt/group-bot/src && source ~/.cargo/env
cargo build -p group-bot --release
install -m 755 target/release/group-bot /opt/group-bot/group-bot
systemctl restart 'group-bot@*'
```

Database changes are applied automatically when the bots start.

## 17. Daily backups

```sh
mkdir -p /var/backups/group-bot
cat > /etc/cron.daily/group-bot-backup <<'EOF'
#!/bin/sh
# Daily database dump of group-bot, kept for 14 days.
sudo -u postgres pg_dump -Fc groupbot > /var/backups/group-bot/groupbot-$(date +%F).dump
find /var/backups/group-bot -name '*.dump' -mtime +14 -delete
EOF
chmod 755 /etc/cron.daily/group-bot-backup
/etc/cron.daily/group-bot-backup && ls -lh /var/backups/group-bot
```

Copy the dumps off the server now and then, for example with `scp` or `rsync`.
To restore one, stop the bots and run:

```sh
systemctl stop 'group-bot@*'
sudo -u postgres pg_restore --clean --if-exists -d groupbot /var/backups/group-bot/groupbot-YYYY-MM-DD.dump
systemctl start 'group-bot@*'
```

The TDLib session folders need no backup, because a bot logs in again with its token.

The bot can also back itself up: in the operator panel (`/admin` in a private
chat) open 💾 Backups to download a backup file or restore one, and set
automatic backups under ⚙️ Global settings. Those files are written to
`DATA_DIR/backups`. They complement the `pg_dump` above: `pg_dump` is still
the one to use for disaster recovery of the whole server.

## 18. Everyday commands

| Task | Command |
| --- | --- |
| List bots | `systemctl list-units 'group-bot@*'` |
| Live log | `journalctl -u group-bot@main -f` |
| Restart one bot | `systemctl restart group-bot@main` |
| Restart all bots | `systemctl restart 'group-bot@*'` |
| Stop a bot for good | `systemctl disable --now group-bot@bot2` (its groups fail over to a standby bot) |
| Change a bot's token | edit `/opt/group-bot/bots/<name>.env`, `rm -rf /opt/group-bot/bots/<name>/tdlib`, restart it |
| Change shared settings | edit `/opt/group-bot/.env`, then `systemctl restart 'group-bot@*'` |

## 19. Troubleshooting

| Symptom (in `journalctl -u group-bot@<name>`) | Fix |
| --- | --- |
| `cannot load TDLib` | Check `TDJSON_PATH` in `.env` and run `ldd /opt/tdlib/libtdjson.so` (step 8). |
| `cannot connect to PostgreSQL` / `password authentication failed` | Check `DATABASE_URL` and the password from step 6; `systemctl status postgresql`. |
| `cannot connect to Redis` | `systemctl status redis-server`; `redis-cli ping`. |
| `TELEGRAM_BOT_TOKEN is not set` | `/opt/group-bot/bots/<name>.env` is missing or empty; re-run `add-bot.sh <name>`. |
| Token rejected (`401`, `ACCESS_TOKEN_INVALID`) | Get the current token from @BotFather, update the bot's `.env`, delete its `tdlib` folder, restart. |
| `Permission denied` under `/opt/group-bot/bots` | `chown -R groupbot:groupbot /opt/group-bot/bots` |
| Build stops with `Killed` | Not enough memory: add swap (step 3). |
| Bot is silent in a group | Privacy mode is still on (step 13), the bot is not an admin, or another bot serves that group (`ربات ها` shows which). |
| Every message is answered twice | The same token runs in two places (for example still in WSL); stop one. |
