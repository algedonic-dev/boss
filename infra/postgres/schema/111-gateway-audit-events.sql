-- 111-gateway-audit-events.sql — the gateway joins the log
-- (gateway-audit-events.md, Q1+Q2 resolved 2026-08-11).
--
-- Q2: the three auth kinds enter the event-kinds registry as the
-- registry's first `gateway` source. `auth.login.denied` carries a
-- closed reason enum in its payload and deliberately NO subject
-- reference — no employee matched, and a reference row is exactly
-- what the ref-check trigger would (rightly) refuse. No ref-check
-- rules are declared for these kinds for the same reason.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('auth.login.denied',    'gateway', 'An authentication decision went against the caller (reason: bad_credentials | no_employee_record | idp_denied); asserts no employee', NULL),
  ('auth.login.succeeded', 'gateway', 'A boss_session was minted for an authenticated employee (method: password | oidc | passkey | guest)', NULL),
  ('auth.session.guest',   'gateway', 'The unauthenticated read-only guest capability was exercised (constant identity, no PII)', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;

-- Q1: the INSERT-only staging role the gateway's pool connects as
-- (BOSS_GATEWAY_AUDIT_DB_URL). The internet-facing edge gets the
-- least privilege that can stage an event: INSERT on the outbox +
-- what the outbox's own ref-check trigger reads, nothing else — no
-- UPDATE/DELETE anywhere, no audit_log access (the relay is the
-- log's single writer).
--
-- Roles are cluster-global; the guard keeps this re-runnable and
-- safe under boss-testing's parallel per-test databases. The
-- password matches the OSS demo posture (boss/boss everywhere);
-- a real tenant rotates it at deploy time.
DO $$
BEGIN
    CREATE ROLE boss_gateway_audit LOGIN PASSWORD 'boss_gateway_audit';
EXCEPTION WHEN duplicate_object THEN
    NULL;  -- already exists (another database's apply, or a re-run)
END $$;

GRANT INSERT ON event_outbox TO boss_gateway_audit;
-- BIGSERIAL id — INSERT consumes the sequence.
GRANT USAGE ON SEQUENCE event_outbox_id_seq TO boss_gateway_audit;
-- The BEFORE INSERT trigger runs with invoker rights and reads the
-- rule table (today: zero rules for auth.* kinds; the grant keeps
-- the trigger runnable if that ever changes).
GRANT SELECT ON audit_log_ref_checks TO boss_gateway_audit;
