-- =========================================================================
-- 26-messages.sql — Messages — internal messaging + system signals.
-- =========================================================================


-- -----------------------------------------------------------------------------
-- Messages (internal messaging + system signals)
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS messages (
    id              TEXT PRIMARY KEY,
    sender_id       TEXT NOT NULL,
    recipient_id    TEXT NOT NULL,
    subject         TEXT NOT NULL,
    body            TEXT NOT NULL,
    entity_type     TEXT,
    entity_id       TEXT,
    -- SPA-resolvable path for the entity. When populated, the
    -- inbox renders this directly instead of dispatching on
    -- `entity_type` to a hand-rolled route mapper. NULL means the
    -- message has no linked entity (plain DMs).
    entity_path     TEXT,
    -- Routing kind; no DB CHECK. The Class registry validates values
    -- at the messages API boundary under (subject_kind='message',
    -- member_attribute='kind').
    kind            TEXT NOT NULL,
    sent_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    read_at         TIMESTAMPTZ,
    reply_to        TEXT REFERENCES messages(id) ON DELETE SET NULL
);


CREATE INDEX IF NOT EXISTS messages_recipient ON messages(recipient_id, sent_at DESC);

CREATE INDEX IF NOT EXISTS messages_unread ON messages(recipient_id) WHERE read_at IS NULL;

CREATE INDEX IF NOT EXISTS messages_thread ON messages(reply_to) WHERE reply_to IS NOT NULL;

