-- Several bots share this database. Each group is served by exactly one bot
-- (its owner); other bots in the same group stand by and take over when the
-- owner stops sending heartbeats.

CREATE TABLE bots (
    bot_id       BIGINT PRIMARY KEY,
    username     TEXT        NOT NULL DEFAULT '',
    started_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The bot that serves the group; NULL until a bot claims it.
ALTER TABLE groups ADD COLUMN bot_id BIGINT;
CREATE INDEX groups_bot_idx ON groups (bot_id);

-- Which bots are members of which groups, for fail-over.
CREATE TABLE group_bots (
    chat_id  BIGINT      NOT NULL REFERENCES groups ON DELETE CASCADE,
    bot_id   BIGINT      NOT NULL,
    seen_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chat_id, bot_id)
);
