-- Bot-wide administration by the bot's operators (see src/admin/): sudo
-- users, global bans, bot-wide settings, background jobs, private chat
-- users for broadcasts, and error monitoring.

-- Users the creators (BOT_SUDO_IDS) made sudo: full rank in every group and
-- a limited operator panel.
CREATE TABLE sudo_users (
    user_id     BIGINT PRIMARY KEY,
    added_by    BIGINT      NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Users banned from every group the bots serve.
CREATE TABLE global_bans (
    user_id     BIGINT PRIMARY KEY,
    reason      TEXT        NOT NULL DEFAULT '',
    actor       BIGINT      NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One JSONB document of bot-wide settings (src/admin/global.rs `GlobalSettings`).
CREATE TABLE global_settings (
    id          SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    settings    JSONB       NOT NULL DEFAULT '{}'::jsonb,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO global_settings (id) VALUES (1);

-- Blocked groups are left by every bot, now and whenever it is added again.
ALTER TABLE groups ADD COLUMN blocked BOOLEAN NOT NULL DEFAULT FALSE;
-- Member count, refreshed when the bot joins and by the group sync job.
ALTER TABLE groups ADD COLUMN members INTEGER NOT NULL DEFAULT 0;

ALTER TABLE users ADD COLUMN created_at TIMESTAMPTZ NOT NULL DEFAULT now();

-- What each bot process reports at start, for the health screen.
ALTER TABLE bots ADD COLUMN booted_at TIMESTAMPTZ NOT NULL DEFAULT now();
ALTER TABLE bots ADD COLUMN version TEXT NOT NULL DEFAULT '';
ALTER TABLE bots ADD COLUMN host TEXT NOT NULL DEFAULT '';

-- People who opened a private chat with a bot: broadcast recipients.
CREATE TABLE pv_users (
    user_id     BIGINT      NOT NULL,
    bot_id      BIGINT      NOT NULL,
    started_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    seen_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    blocked     BOOLEAN     NOT NULL DEFAULT FALSE,
    PRIMARY KEY (user_id, bot_id)
);
CREATE INDEX pv_users_bot_idx ON pv_users (bot_id) WHERE NOT blocked;

-- Background jobs. `bot_id` NULL: any bot may run it; otherwise only that bot.
-- `origin_bot` is the bot the job was created on; it reports the outcome.
CREATE TABLE jobs (
    id          BIGSERIAL PRIMARY KEY,
    kind        TEXT        NOT NULL,
    payload     JSONB       NOT NULL DEFAULT '{}'::jsonb,
    bot_id      BIGINT,
    origin_bot  BIGINT      NOT NULL DEFAULT 0,
    status      TEXT        NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'running', 'done', 'failed', 'cancelled')),
    cancel      BOOLEAN     NOT NULL DEFAULT FALSE,
    progress    INTEGER     NOT NULL DEFAULT 0,
    total       INTEGER     NOT NULL DEFAULT 0,
    result      TEXT        NOT NULL DEFAULT '',
    created_by  BIGINT      NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at  TIMESTAMPTZ,
    finished_at TIMESTAMPTZ
);
CREATE INDEX jobs_pending_idx ON jobs (id) WHERE status = 'pending';

-- Errors logged by any bot, grouped by a fingerprint of their text.
CREATE TABLE error_events (
    id          BIGSERIAL UNIQUE,
    fingerprint TEXT PRIMARY KEY,
    level       TEXT        NOT NULL,
    target      TEXT        NOT NULL DEFAULT '',
    message     TEXT        NOT NULL,
    bot_id      BIGINT      NOT NULL DEFAULT 0,
    count       BIGINT      NOT NULL DEFAULT 1,
    first_seen  TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen   TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved    BOOLEAN     NOT NULL DEFAULT FALSE
);
CREATE INDEX error_events_open_idx ON error_events (last_seen DESC) WHERE NOT resolved;
