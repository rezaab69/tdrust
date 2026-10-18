# group-bot

A multilingual Telegram group management bot built on [tdrust](../README.md),
with PostgreSQL as the primary store and Redis for caching and real-time
state. It implements the features of [`bot_guide.md`](../bot_guide.md)
except those that need external services (see [Not included](#not-included)).

## Quick start

```sh
# 1. Storage
cd group-bot && docker compose up -d        # PostgreSQL 17 + Redis 7

# 2. Settings
cp .env.example .env && nano .env           # API id/hash, bot token, TDJSON_PATH

# 3. Run (from the repository root; under WSL use a Linux target dir)
export CARGO_TARGET_DIR=~/tdrust-target
cargo run -p group-bot --release
```

Then, in Telegram:

1. In @BotFather, **disable privacy mode** (`/setprivacy` → Disable) so the bot sees every message.
2. Add the bot to a group and make it an **administrator** with the rights to
   delete messages, ban users, pin messages, invite users and change info.
3. Send `راهنما` or `Help` in the group.

The database schema is created automatically on start (`migrations/`).

For a production server, follow [DEPLOY-VPS.md](DEPLOY-VPS.md): every step
for running the bot (or several bots) on an Ubuntu VPS as systemd services.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `TELEGRAM_API_ID`, `TELEGRAM_API_HASH` | — | From https://my.telegram.org (required) |
| `TELEGRAM_BOT_TOKEN` | — | From @BotFather (required) |
| `TDJSON_PATH` | search | Path of the TDLib library |
| `DATABASE_URL` | — | PostgreSQL URL (required) |
| `DATABASE_MAX_CONNECTIONS` | `10` | Pool size |
| `REDIS_URL` | `redis://127.0.0.1:6379` | Redis URL |
| `REDIS_PREFIX` | `gb:` | Key prefix, so several bots can share a Redis |
| `BOT_SUDO_IDS` | — | The bot's creators (comma-separated user ids): full control everywhere and the [operator panel](#bot-management) |
| `DEFAULT_LANG` | `fa` | Language of new groups (the operator panel can override it) |
| `DATA_DIR` | `group-bot-data` | Backups and broadcast media; bots on one machine should share it |
| `LOCALES_DIR` | — | Directory of extra `<code>.json` translations |
| `TDLIB_DATA_DIR` | `tdlib-group-bot` | TDLib session directory |
| `RUST_LOG` | `info` | Log level |

## Architecture

```
Telegram ⇄ TDLib ⇄ tdrust ─ update loop ─▶ handler tasks
                                   │
          ┌────────────────────────┼─────────────────────────┐
          ▼                        ▼                         ▼
   commands (fa/en)        guard (enforcement)        members (join/leave,
   exec/{settings,         locks, flood, limit,        captcha, welcome,
   moderation,fun}         filters, duplicates,        anti-tabchi)
          │                force add/join               │
          └──────────────┬─────────┴──────────────┬─────┘
                         ▼                        ▼
             PostgreSQL (source of truth)   Redis (cache + real time)
             groups.settings JSONB,         settings/ranks/admins/texts cache,
             ranks, sanctions, warns,       flood & limit counters, dedupe,
             filters, answers, members,     throttles, captcha deadlines,
             message_stats, audit_log …     recent message ids, stats buffer,
                                            scheduler leader lease
```

| Module | Responsibility |
| --- | --- |
| `main.rs` | Startup, update routing, graceful shutdown (Ctrl+C / SIGTERM) |
| `commands.rs` | Bilingual command table and parser (longest phrase wins, `/cmd@bot` supported) |
| `exec/` | Command handlers: `settings`, `moderation`, `fun` |
| `guard.rs` | Every check on members' messages, edits and pins; auto answers |
| `members.rs` | Joins, leaves, bot lock, force-add counting, captcha, welcome/goodbye |
| `panels.rs` | Inline settings, lists and user panels |
| `scheduler.rs` | Timed jobs every 20 s: stats flush, rank/pin/captcha expiry, auto lock/clean/stats |
| `admin/` | The operators' panel: bot-wide settings, flags, maintenance, global bans, jobs, backups, metrics, logs |
| `db.rs` / `cache.rs` | PostgreSQL repository (`db/admin.rs`: bot-wide queries) / Redis layer |
| `i18n.rs` + `locales/` | Translations and per-group text overrides |
| `content.rs` | Message types, formatted text ⇄ HTML, stored content ⇄ messages |

Design notes:

- **Consistency:** settings changes run in a PostgreSQL transaction with
  `SELECT … FOR UPDATE`, so concurrent changes are never lost, even with
  several bot instances. Redis is write-through and treated as disposable:
  every cache failure falls back to PostgreSQL.
- **Hot path:** a member's message costs a few Redis operations; message
  statistics are buffered in Redis and flushed to PostgreSQL by the scheduler.
- **Several bots:** any number of bots can share the database and Redis; see
  [Running several bots](#running-several-bots).
- **Safety:** users of equal or higher rank are never targeted; the bot never
  acts on itself; callback data stays under Telegram's 64-byte limit.

## Running several bots

Telegram limits how fast one bot may act, so busy deployments spread their
groups over several bots. All bots share one PostgreSQL and one Redis, so
settings, ranks, filters, statistics and texts are the same whichever bot
serves a group.

**How groups are shared.** Every bot in a group is active. Each of them
receives every message, edit and member change, and the first to claim it in
Redis (an atomic `SET NX`, kept for 25 hours) handles it; the others skip it.
So nothing is answered, warned, welcomed or deleted twice, and the work is
spread over the bots in the group. A bot that is not an administrator of the
group leaves the work to those that are.

- **Commands for one bot:** `/cmd@thatbot` is handled by that bot only. Panel
  buttons are always handled by the bot that sent the panel.
- **Bots never act on each other:** they ignore each other's messages, are
  never kicked by the bot lock, and cannot be targeted by commands.
- **Scheduled jobs** (auto lock and clean, temporary ranks, timed pins) run
  once per group, on its *owner*: the first bot that saw it. Every bot writes a
  heartbeat every 20 seconds; if the owner stops (crash, shutdown, removed
  token), another bot in the group takes over its jobs within about 90 seconds.
  Captcha timeouts run on the bot that asked the captcha.
- **Moving a group:** add the new bot, then remove the old one; the old bot
  releases the group and the new one takes it at once.
- All bots must share `REDIS_PREFIX`, or they cannot see each other's claims.
- In basic (non-super) groups, message ids differ between bots, so messages
  are matched by sender, time and text; edits and pins there are checked by
  the owner only.
- If Redis is down, only the owner handles a group's updates.
- `ربات ها` / `Bots` (owners) lists every bot, whether it is running, how many
  groups it owns, and which one owns the current group.

**Adding a bot** (in WSL, as root):

```sh
# 1. Create the bot in @BotFather and disable its privacy mode (/setprivacy).
# 2. Add it:
/mnt/d/ClaudeCode/tdrust/group-bot/deploy/add-bot.sh bot2    # asks for the token
```

Each bot runs as the systemd service `group-bot@<name>` with its own token and
TDLib session (`~/group-bot/bots/<name>.env` and `~/group-bot/bots/<name>/`);
everything else comes from `group-bot/.env`. The first bot is
`group-bot@main`, whose token stays in `group-bot/.env`.

```sh
systemctl list-units 'group-bot@*'         # all bots
journalctl -u group-bot@bot2 -f            # one bot's log
systemctl restart 'group-bot@*'            # after installing a new build
systemctl disable --now group-bot@bot2     # remove a bot (its groups fail over)
```

Note: media stored for welcome messages and auto answers is referenced by
Telegram file id; if another bot takes over a group, such media may need to be
set again (the text is still sent).

## Bot management

The people in `BOT_SUDO_IDS` are the bot's **creators**. Send `/admin` (or
`مدیریت`, or `/start`) to the bot in a private chat to open the operator
panel. Creators can add **sudo users**: they get sudo rank in every group and
see the panel, and can manage users and global bans, but cannot change the
bot itself (sections marked 👑 below). Everything is shared by all bots of a
fleet: settings and bans made on one bot apply to all within 20 seconds.

| Section | What it does |
| --- | --- |
| 📊 Statistics | Groups (active, blocked, new), users (known, new, active, private chats), messages today/week/total, bans, mutes, warnings, global bans, filters, top groups of the week, actions of the last 24 hours |
| 👤 Users | New and most active users, search; each user's **global profile**: groups with messages and ranks, warnings, sanctions, private chats with the bots, global ban status; ban globally, make sudo 👑, clear warnings everywhere, remove all ranks 👑 |
| ⛔ Global bans | Banned users are deleted and banned (or, by setting, only deleted) in every group the bots serve; existing groups are swept by background jobs, others as soon as the user appears or joins |
| 👥 Groups | Active, inactive and blocked groups; each group's card (members, serving bot, activity, lists); invite link, sync, send a message 👑, reset settings 👑, leave 👑, block/unblock 👑 (blocked groups are left by every bot, now and whenever re-added) |
| 📣 Broadcast 👑 | Any message (formatted text or media) to all groups, all private chats or both, optionally pinned; each bot sends to its own groups at a configurable pace, as cancellable jobs |
| 🛡 Sudo users 👑 | List, add and remove sudo users |
| 🚩 Feature flags 👑 | Switch features off in every group at once: new groups, fun commands, captcha, welcome, force add/join, auto answers, filters, anti-flood, anti-tabchi, statistics, scheduled jobs, log channels, private filter panel |
| ⚙️ Global settings 👑 | Language of new groups, global ban mode, join/leave notices, minimum group size, broadcast pace, alerts (errors, bot down, error spikes) and where they go, automatic backups, history retention, log level |
| 🛠 Maintenance 👑 | Pause commands (protection keeps running) or everything, for a while or until turned off, with a custom notice; operators are never paused |
| ⏱ Jobs | Background jobs of all bots with progress; cancel and retry 👑; start pruning, group sync or a backup 👑 |
| 🐞 Errors | Errors of all bots grouped by kind (numbers ignored), counts, first/last seen; resolve; alerts for new and returning errors |
| 📜 Logs | The latest log lines of this process by level; download as a file |
| 📈 Monitoring | Updates, messages, commands, buttons, deletions, punishments, failed sends, warnings and errors over the last hour/day/week as charts, average handling time, per-bot numbers, top commands |
| 🩺 Health | PostgreSQL, Redis and TDLib latency, memory, uptime, connection pool, scheduler, job runner, every bot's heartbeat, version and host |
| 🗄 Database | Migrations (applied, pending, changed), database size and connections, largest tables, Redis facts; `ANALYZE` 👑 |
| 💾 Backups 👑 | Config or full backups as downloadable JSON-lines files, automatic backups with retention, restore from an uploaded file (after a safety backup) |
| 📝 Operator log | The latest actions of the operators |

Commands in the private chat (Persian forms work too; `cancel` / `لغو` stops a question):

| Command | |
| --- | --- |
| `gban <user> [reason]`, `ungban <user>` | Global ban; `<user>` is an id, @username, mention, or a reply to a forwarded message |
| `user <id \| @username \| name>` | A user's profile, or search |
| `group <chat id>` | A group's card |
| `addsudo <user>`, `remsudo <user>` 👑 | Sudo users |
| `broadcast` 👑 | Asks for the message, then shows the broadcast screen |
| `sendto <chat id>` 👑 | Asks for a message for one group, sent by the bot serving it |
| `leave <chat id>` 👑 | Every bot leaves the group |
| `backup [full]`, `restore` 👑 | Make a backup (sent to you), or restore an uploaded one |
| `maintenance on\|full\|off [2 h]`, `maintenance msg <text>` 👑 | Maintenance mode |
| `alertchat <chat id \| here \| off>` 👑 | Where alerts go (default: the creators' private chats) |
| `botstats`, `health`, `users`, `groups`, `gbanlist`, `jobs`, `errors`, `logs`, `monitor`, `dbstatus`, `auditlog`, `flags`, `globalsettings`, `sudolist`, `backups` | Open a panel section |

In groups, sudo users can also reply to a member with `gban` / `بن سراسری`
(or `ungban` / `حذف بن سراسری`).

Operators only get alerts from a bot they have started a private chat with,
unless an alert chat is set.

## Languages and texts

- Every text the bot sends comes from `locales/<lang>.json`. Persian (`fa`) and
  English (`en`) are built in; add languages by dropping `xx.json` into
  `LOCALES_DIR` (missing keys fall back to English).
- Each group chooses its language: `زبان en` / `Lang fa`. Commands work in both
  languages in every group.
- Any text can be customized per group: `SetText <key> <text>` (or reply to a
  formatted message), `RemText <key>`; `TextKeys` lists the keys.
- Formatting is preserved everywhere admins provide text (welcome, goodbye,
  rules, auto answers, force add/join texts, custom texts): bold, italic,
  underline, strikethrough, spoilers, code, links, mentions, quotes and
  custom emoji are stored as Telegram HTML.

## Feature map (bot_guide.md)

| § | Section | Status |
| --- | --- | --- |
| 1 | Basic and media locks (31 + 3 emoji modes) | ✅ |
| 2 | Media lock (permanent/timed/temporary), group lock (permanent/timed/daily auto), limit, flood with 4 punishments, character limits, strict mode per type | ✅ |
| 3 | Anti-tabchi: tabchi, advertiser, link-in-bio; ban/mute; on join or first message | ✅ |
| 4 | Ranks: creator, owner, mod (+temporary), VIP (+temporary), exempt, Telegram admins; lists and clean-up | ✅ |
| 5 | Ban, Ban+, mute (permanent/timed), kick, warnings with limit and action, user panel | ✅ |
| 6 | Filters with delete/warn/ban/mute, continuous filter mode, private (PV) filter panel | ✅ |
| 7 | Delete last N, scheduled auto clean (1–5 times a day), all list clean-ups incl. bots, fake and deleted accounts, blocked/restricted lists | ✅ |
| 8 | Force add: count, expiry, text with variables, all/new members, history reset | ✅ |
| 9 | Force join: channel (public, or private with its link), text, status | ✅ |
| 10 | Welcome/goodbye with media, auto-delete and all variables (English and Persian names) | ✅ |
| 11 | Auto answers: up to 20 random responses (text or media), audience, cooldown | ✅ set up in the group by reply or `keyword \| answer`, not a PV wizard |
| 12 | Stats: summary, top N, inviters, staff, all-time, weekly, per user, scheduled | ✅ |
| 13 | Captcha, anti-betrayal, duplicates, reports (group/PV), deletion reasons, log channel, time zone | ✅ |
| 14 | Fonts, time (Jalali), echo, jokes, id/info, join date, profile cards (asl), tag, nicknames, profile photos, rules, pin (timed), link, group photo | ✅ |
| 15 | Settings panel, lists panel, user panel | ✅ |

### Not included

Skipped because they need external services or media tooling: news, Hafez
fortune, poems, prayer times, translation, currency rates, weather,
dictionary, and the sticker section (pack management and sticker/photo/GIF/video
conversions). The "helper bot" for deleting history in large groups is not
needed: bulk deletion uses the message ids the bot records itself.

Known limits: a bot only sees messages sent while it is in the group, so
`Del N` and auto clean cover the last 2,000 messages the bot has seen. Joins
are detected from service messages, so keep join messages enabled (the
`TgService` lock can still delete them after processing).

## Development

```sh
cargo test -p group-bot                                   # unit tests
DATABASE_URL=postgres://bot:bot@localhost:5432/groupbot \
REDIS_URL=redis://127.0.0.1:6379 \
cargo test -p group-bot -- --ignored                      # against real PostgreSQL and Redis
```

The unit tests also check that both locales have the same keys and
placeholders, that every key used in the code exists, and that every built-in
text is valid Telegram HTML. The `--ignored` tests change data (the admin test
even restores a table), so point them at a scratch database.
