-- 20260927232421-an-elevated-session-is-an-event.sql — the platform
-- owner's operator-tier elevation joins the event-kinds registry
-- (backlog 3c92c5b8, decided by David 2026-09-27).
--
-- Every gateway session was minted at the user tier and nothing raised
-- it, so the IT Credentials and Agents registries answered 403 to the
-- platform owner's own browser. The gateway now raises the owner's
-- session to the operator tier after a verified passkey assertion
-- (`POST /api/auth/passkey/elevate/finish`) — the one change of trust a
-- live session can undergo, so it is on the record:
--
--   * `auth.session.elevated` — email, employee_id, method (passkey),
--     access_tier (operator), elevated_at (when the assertion was
--     verified) and expires_at (the session's own expiry; the elevation
--     never extends it), both RFC 3339 UTC, and which key: its
--     credential_label and credential_registered_at. Only an
--     operator-tier key elevates, and the event is RECORDED before the
--     grant — a grant whose event cannot be recorded is refused.
--
-- Never a credential id or any assertion material. Declared in the same
-- car that first emits it: an emitted-but-undeclared kind is the defect
-- class the audit integrity check exists to catch. Same posture as the
-- other gateway auth kinds: no ref-check rules.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('auth.session.elevated', 'gateway', 'The platform owner''s session was raised to the operator tier by a verified assertion from an operator-tier passkey (names the email and employee, when it was verified, when it ends with the session, and the key''s label and registration time; never credential material)', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
