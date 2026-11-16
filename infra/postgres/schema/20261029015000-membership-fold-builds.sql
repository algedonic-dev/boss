-- Disposable progress only. A row here is never a served generation.
CREATE TABLE IF NOT EXISTS membership_fold_builds (
    identity text PRIMARY KEY,
    revision bigint NOT NULL CHECK (revision > 0),
    generation_revision bigint NOT NULL CHECK (generation_revision >= 0),
    generation_digest text,
    digest text NOT NULL,
    byte_count bigint NOT NULL CHECK (byte_count > 0 AND byte_count <= 134217728)
);
CREATE TABLE IF NOT EXISTS membership_fold_build_chunks (
    identity text NOT NULL REFERENCES membership_fold_builds(identity) ON DELETE CASCADE,
    chunk integer NOT NULL CHECK (chunk >= 0 AND chunk < 128),
    payload bytea NOT NULL CHECK (octet_length(payload) > 0 AND octet_length(payload) <= 1048576),
    PRIMARY KEY (identity, chunk)
);
