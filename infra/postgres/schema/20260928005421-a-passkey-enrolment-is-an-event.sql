-- 20260928005421-a-passkey-enrolment-is-an-event.sql — a self-service
-- passkey enrolment joins the event-kinds registry (backlog 3c92c5b8,
-- the adversarial review of car 0bde9b99, finding H1).
--
-- `POST /api/auth/passkey/register/finish` lets any session with an
-- employee id enrol a passkey, with no step-up, and it left no trace.
-- The key it stores is `user` tier (webauthn_credentials.access_tier):
-- it signs presence stamps and never elevates a session, which takes an
-- operator-tier key reached by a separate recorded act. The enrolment
-- itself is now on the record:
--
--   * `auth.passkey.enrolled` — email, employee_id, label, and the
--     access_tier the key was stored at.
--
-- Never a credential id or any key material. Declared in the same car
-- that first emits it: an emitted-but-undeclared kind is the defect
-- class the audit integrity check exists to catch. Same posture as the
-- other gateway auth kinds: no ref-check rules.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('auth.passkey.enrolled', 'gateway', 'A passkey was enrolled through the self-service ceremony (names the email, employee, label and the access_tier it was stored at — user for every self-service key; never credential material)', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
