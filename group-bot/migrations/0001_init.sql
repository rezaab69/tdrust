-- Groups the bot manages. Scalar settings live in one JSONB document per
-- group (see src/model.rs `Settings`); list-like data has its own tables.
CREATE TABLE groups (
    chat_id     BIGINT PRIMARY KEY,
    title       TEXT        NOT NULL DEFAULT '',
    settings    JSONB       NOT NULL DEFAULT '{}'::jsonb,
    active      BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Users seen by the bot, for @username resolution and display names.
CREATE TABLE users (
    user_id     BIGINT PRIMARY KEY,
    first_name  TEXT        NOT NULL DEFAULT '',
    last_name   TEXT        NOT NULL DEFAULT '',
    username    TEXT,
    is_bot      BOOLEAN     NOT NULL DEFAULT FALSE,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX users_username_idx ON users (lower(username));

-- Bot-managed ranks: 1 exempt, 2 vip, 3 mod, 4 owner.
CREATE TABLE ranks (
    chat_id     BIGINT      NOT NULL REFERENCES groups ON DELETE CASCADE,
    user_id     BIGINT      NOT NULL,
    rank        SMALLINT    NOT NULL CHECK (rank BETWEEN 1 AND 4),
    expires_at  TIMESTAMPTZ,
    granted_by  BIGINT      NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chat_id, user_id)
);
CREATE INDEX ranks_expiry_idx ON ranks (expires_at) WHERE expires_at IS NOT NULL;

-- Active bans and mutes applied through the bot.
CREATE TABLE sanctions (
    chat_id     BIGINT      NOT NULL REFERENCES groups ON DELETE CASCADE,
    user_id     BIGINT      NOT NULL,
    kind        TEXT        NOT NULL CHECK (kind IN ('ban', 'mute')),
    until       TIMESTAMPTZ,
    actor       BIGINT      NOT NULL DEFAULT 0,
    reason      TEXT        NOT NULL DEFAULT '',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chat_id, user_id, kind)
);
CREATE INDEX sanctions_until_idx ON sanctions (until) WHERE until IS NOT NULL;

CREATE TABLE warns (
    chat_id     BIGINT      NOT NULL REFERENCES groups ON DELETE CASCADE,
    user_id     BIGINT      NOT NULL,
    count       INTEGER     NOT NULL DEFAULT 0,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chat_id, user_id)
);

-- Filtered words; action is one of delete, warn, mute, ban.
CREATE TABLE filters (
    chat_id     BIGINT      NOT NULL REFERENCES groups ON DELETE CASCADE,
    word        TEXT        NOT NULL,
    action      TEXT        NOT NULL DEFAULT 'delete',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chat_id, word)
);

-- Auto answers: a keyword with up to 20 responses, one picked at random.
CREATE TABLE answers (
    id          BIGSERIAL PRIMARY KEY,
    chat_id     BIGINT      NOT NULL REFERENCES groups ON DELETE CASCADE,
    keyword     TEXT        NOT NULL,
    audience    TEXT        NOT NULL DEFAULT 'all',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (chat_id, keyword)
);
CREATE TABLE answer_responses (
    id          BIGSERIAL PRIMARY KEY,
    answer_id   BIGINT      NOT NULL REFERENCES answers ON DELETE CASCADE,
    content     JSONB       NOT NULL
);

-- Joins and invitations, for force-add, the "new members" scope and stats.
CREATE TABLE members (
    chat_id       BIGINT      NOT NULL REFERENCES groups ON DELETE CASCADE,
    user_id       BIGINT      NOT NULL,
    joined_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    invited_by    BIGINT,
    added_count   INTEGER     NOT NULL DEFAULT 0,
    total_added   INTEGER     NOT NULL DEFAULT 0,
    force_done_at TIMESTAMPTZ,
    PRIMARY KEY (chat_id, user_id)
);

-- Messages per user per local day (days since 1970-01-01 in the group's time zone).
CREATE TABLE message_stats (
    chat_id     BIGINT      NOT NULL REFERENCES groups ON DELETE CASCADE,
    user_id     BIGINT      NOT NULL,
    day         INTEGER     NOT NULL,
    messages    INTEGER     NOT NULL DEFAULT 0,
    PRIMARY KEY (chat_id, day, user_id)
);

CREATE TABLE nicknames (
    chat_id     BIGINT      NOT NULL REFERENCES groups ON DELETE CASCADE,
    user_id     BIGINT      NOT NULL,
    nickname    TEXT        NOT NULL,
    PRIMARY KEY (chat_id, user_id)
);

-- "Asl" profile cards; chat_id 0 holds the global card.
CREATE TABLE profiles (
    chat_id     BIGINT      NOT NULL,
    user_id     BIGINT      NOT NULL,
    content     TEXT        NOT NULL,
    PRIMARY KEY (chat_id, user_id)
);

-- Per-group overrides of any bot text (HTML templates).
CREATE TABLE custom_texts (
    chat_id     BIGINT      NOT NULL REFERENCES groups ON DELETE CASCADE,
    key         TEXT        NOT NULL,
    lang        TEXT        NOT NULL,
    content     TEXT        NOT NULL,
    PRIMARY KEY (chat_id, key, lang)
);

CREATE TABLE timed_pins (
    chat_id     BIGINT      NOT NULL REFERENCES groups ON DELETE CASCADE,
    message_id  BIGINT      NOT NULL,
    unpin_at    TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (chat_id, message_id)
);

-- Every moderation and settings action, for the log channel and audits.
CREATE TABLE audit_log (
    id          BIGSERIAL PRIMARY KEY,
    chat_id     BIGINT      NOT NULL,
    actor       BIGINT      NOT NULL,
    target      BIGINT,
    action      TEXT        NOT NULL,
    detail      TEXT        NOT NULL DEFAULT '',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX audit_log_chat_idx ON audit_log (chat_id, created_at DESC);
