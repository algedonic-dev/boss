-- 20260928012747-the-gateway-audit-role-opens-no-session.sql — the
-- role 111 created can no longer log in, carries no password, and
-- holds nothing in this database.
--
-- Origin: backlog 7ec7113b (history scan finding F1, 2026-09-27) and
-- d49b4355. 111-gateway-audit-events.sql created `boss_gateway_audit`
-- as a LOGIN role whose password was its own name, and the bare-metal
-- drop-in that was its only user (infra/gateway/audit-events.conf,
-- deleted by this car) carried the same value in a URL. Every database
-- that ran 111 therefore had a login anyone reading the tree could use
-- to stage events on the outbox. 111 cannot be edited — applied
-- migrations are hashed (migrations-append-only.sh) — so this file
-- takes the credential away instead.
--
-- Nothing connects as the role any more: the gateway stages its auth
-- events through the service database URL every binary in the container
-- already reads (boss_gateway::audit::AUDIT_SINK_URL_VAR), because the
-- least privilege the role promised was never real there — the launcher
-- hands the gateway the whole container environment, the service URL
-- included.
--
-- ALTER, not DROP. Roles are cluster-global, and Postgres refuses to
-- drop one while ANY database on the server still holds a grant to it:
-- boss-testing's parallel scratch databases each do, and so may a live
-- server's other databases. The revokes below clear THIS database; the
-- DROP ROLE is the operator's act on the live server, with the exact
-- SQL recorded on backlog 7ec7113b. Every statement here is idempotent,
-- and the guard makes it a no-op where 111's role was already dropped.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'boss_gateway_audit') THEN
        ALTER ROLE boss_gateway_audit NOLOGIN PASSWORD NULL;
        REVOKE ALL ON event_outbox FROM boss_gateway_audit;
        REVOKE ALL ON SEQUENCE event_outbox_id_seq FROM boss_gateway_audit;
        REVOKE ALL ON audit_log_ref_checks FROM boss_gateway_audit;
    END IF;
END $$;
