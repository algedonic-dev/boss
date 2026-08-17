-- 143-step-assurance.sql — a step records the weakest stamp it will
-- accept.
--
-- A SignOffStamp already carried who attested and, via shape_hash,
-- exactly what they attested. It never carried how hard the attestation
-- was to produce, so "David clicked approve" and "David logged in this
-- morning and something clicked approve" were indistinguishable facts.
--
-- David, 2026-08-16: "Passkey authorization as actor-auth feature for
-- job packets is broadly useful. Let's make sure we design and build it
-- that way." So assurance is a property of a stamp and a requirement of
-- a step, not a feature of one workflow — elevation, a payment release,
-- a deploy sign-off and an incident's closure all want it.
--
-- NULL means "whatever the StepType's floor says", which is `session`
-- unless a kind raises it. Every existing row is NULL and therefore
-- unchanged; the gate is inert until a protocol asks for more.
--
-- The stamp's OWN assurance needs no column: sign_offs is jsonb and the
-- field rides inside it, defaulting to `session` for every stamp
-- written before it existed — which is exactly what those stamps were.
ALTER TABLE steps ADD COLUMN IF NOT EXISTS assurance_required TEXT;

COMMENT ON COLUMN steps.assurance_required IS
  'Weakest SignOffStamp assurance this step accepts (session|presence). NULL = the StepType floor.';
