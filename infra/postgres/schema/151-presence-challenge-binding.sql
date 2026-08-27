-- 151-presence-challenge-binding.sql — wake the dormant WebAuthn
-- tables and teach the challenge row what a presence assertion binds.
--
-- 10-people.sql created webauthn_credentials and webauthn_challenges
-- on 2026-08-10 as intent; no code has ever read them. The presence
-- ceremony (design: docs/design/presence.md, Q1-Q3 resolved
-- 2026-08-16; BOSS-native call accepted on packet 7218c3f1) makes
-- them live, and needs the challenge row to carry the binding that
-- makes a stamp mean something:
--
--   challenge = sha256(shape_hash || nonce)
--
-- The shape hash is deterministic on purpose (binding), so the nonce
-- is what restores single-use (replay). Recording both beside the
-- challenge means verification never has to reconstruct what was
-- signed from parts held by different services: the row IS the claim
-- "this challenge was minted for this step at this content".
--
-- `used_at` makes consumption explicit and atomic (UPDATE ... WHERE
-- used_at IS NULL RETURNING) instead of DELETE, so a consumed
-- challenge leaves evidence. The flow CHECK gains 'presence' —
-- register/authenticate cover enrolment and login; a step-bound
-- assertion is neither.

ALTER TABLE webauthn_challenges
  ADD COLUMN IF NOT EXISTS step_id    TEXT,
  ADD COLUMN IF NOT EXISTS shape_hash TEXT,
  ADD COLUMN IF NOT EXISTS nonce      TEXT,
  ADD COLUMN IF NOT EXISTS used_at    TIMESTAMPTZ;

ALTER TABLE webauthn_challenges
  DROP CONSTRAINT IF EXISTS webauthn_challenges_flow_check;
ALTER TABLE webauthn_challenges
  ADD CONSTRAINT webauthn_challenges_flow_check
  CHECK (flow IN ('register', 'authenticate', 'presence'));

COMMENT ON COLUMN webauthn_challenges.step_id IS
  'presence flow only: the step this challenge was minted for';
COMMENT ON COLUMN webauthn_challenges.shape_hash IS
  'presence flow only: step_shape_hash at mint time — the content the assertion approves';
COMMENT ON COLUMN webauthn_challenges.nonce IS
  'presence flow only: server nonce folded into the challenge; recorded on the stamp for single-use';
COMMENT ON COLUMN webauthn_challenges.used_at IS
  'set exactly once at consumption; a consumed challenge is evidence, not garbage';
