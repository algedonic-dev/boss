-- 20260928163014-a-passkey-promotion-is-an-event.sql — promoting a
-- stored passkey to the operator tier joins the event-kinds registry
-- (design 2cb6256f D6, David 2026-09-28; backlog 2a228d0c).
--
-- An operator-tier passkey is what raises the platform owner's session
-- to operator, so the one write of that tier —
-- `POST /api/people/{id}/webauthn-credentials/{credential_id}/promote`,
-- accepted from `automation:gateway` alone at the end of a
-- `passkey-promotion` ceremony — records the fact in the flip's own
-- transaction:
--
--   * `auth.passkey.promoted` — employee_id, credential_label,
--     credential_id (the WebAuthn id, base64url: public, the key's name
--     to the authenticator), registered_at, packet_id (the
--     passkey-promotion packet it spent) and vouched_by (the label of
--     the break-glass hardware key that vouched).
--
-- Never key material. Emitted by boss-people (source `people`) under
-- the gateway's auth prefix, so a key's enrolment, promotion and every
-- elevation read under one name. Declared in the same car that first
-- emits it: an emitted-but-undeclared kind is the defect class the
-- audit integrity check exists to catch. No ref-check rules, like the
-- other auth kinds.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('auth.passkey.promoted', 'people', 'A stored passkey was promoted to the operator tier by the gateway at the end of a passkey-promotion ceremony (names the employee, the key''s label, credential id and registration time, the packet it spent and the break-glass key that vouched; never key material)', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
