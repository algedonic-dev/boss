-- Disposable membership derivations, rebuilt from original audit facts.
-- The generation header and all changed rows commit in one transaction.
CREATE TABLE membership_projection_generations (
    identity text PRIMARY KEY,
    revision bigint NOT NULL CHECK (revision >= 0),
    header jsonb NOT NULL,
    -- Typed 24/48-hour readings from the same canonical checkpoint fold.
    -- Packet state, original crossing facts and arbitrary-window reads remain.
    canonical_windows jsonb NOT NULL,
    committed_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TABLE membership_projection_rows (
    identity text NOT NULL REFERENCES membership_projection_generations(identity) ON DELETE CASCADE,
    bucket text NOT NULL CHECK (bucket IN ('packet','fact','seen','span')),
    row_key text NOT NULL,
    payload jsonb NOT NULL,
    happened_at timestamptz,
    ended_at timestamptz,
    PRIMARY KEY(identity,bucket,row_key)
);
CREATE INDEX membership_projection_facts_window ON membership_projection_rows(identity,happened_at) WHERE bucket='fact';
CREATE INDEX membership_projection_spans_window ON membership_projection_rows(identity,ended_at) WHERE bucket='span';
