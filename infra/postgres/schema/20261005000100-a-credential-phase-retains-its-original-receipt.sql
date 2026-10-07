-- Owner receipts outlive delivered-outbox pruning. Credential row locking
-- serializes first observations and their conflicts across every phase.
CREATE TABLE IF NOT EXISTS credential_phase_receipts (
    credential_id TEXT NOT NULL REFERENCES credentials(id),
    phase TEXT NOT NULL CHECK (phase IN ('minted','installed','verified','revoked')),
    observation_id TEXT NOT NULL CHECK (length(observation_id) BETWEEN 1 AND 128),
    receipt_json TEXT NOT NULL,
    PRIMARY KEY (credential_id, phase, observation_id)
);
