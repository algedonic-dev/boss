-- 20260928170011-only-the-promotion-writes-an-operator-passkey.sql — the
-- one-writer rule of design 2cb6256f D5, held by the table itself
-- (backlog 2a228d0c; finding F4 of the adversarial review of car
-- 1d9970d1, and findings 3 and 7 of this car's own review, 2026-09-28).
--
-- An operator-tier row in webauthn_credentials is what lets the gateway
-- raise the platform owner's session to operator. boss-people's promote
-- endpoint (src/passkey_promotion.rs) is the only code meant to write
-- one, and it reads an approved passkey-promotion packet before it does.
-- Its own transaction carries the transaction-local setting named in
-- the function below, set to the packet id it spends. Without that
-- mark, this trigger refuses:
--   * a row becoming operator — an INSERT at operator, or an UPDATE from
--     another tier;
--   * a REKEY of an operator row — its public_key, credential_id or
--     employee_id changed while it stays operator. Swapping the key
--     material under an existing operator row plants an elevating key
--     exactly as a flip does (finding 3: confirmed on a scratch database
--     against the first version of this trigger, which watched only the
--     tier column).
-- Still allowed without the mark: an operator key's sign count and last
-- use moving (every assertion writes them), its label changing, and any
-- key dropping to user — that is not a grant. DELETE is the demotion (D8)
-- and is untouched.
--
-- WHAT THIS IS, PLAINLY: a guard against APPLICATION paths — an
-- enrolment handler, a careless UPDATE, a handler nobody has written
-- yet. It is NOT a security boundary against anyone who can run SQL on
-- this database: a session that can SET LOCAL the mark, or a superuser
-- who can disable the trigger, is past it. The boundary for those is who
-- holds database credentials. The tree spells the mark in two places,
-- the const in passkey_promotion.rs and this trigger, and a tree-wide
-- test there refuses a third.
--
-- promoted_by_packet records which packet a promotion spent, so a replay
-- under the same packet is recognised as harmless and one under another
-- packet is refused naming the first (finding 7).
ALTER TABLE webauthn_credentials ADD COLUMN IF NOT EXISTS promoted_by_packet TEXT;

CREATE OR REPLACE FUNCTION webauthn_credentials_operator_only_by_promotion()
RETURNS trigger AS $$
DECLARE
    grants_or_rekeys BOOLEAN := FALSE;
BEGIN
    IF NEW.access_tier = 'operator' THEN
        IF TG_OP = 'INSERT' THEN
            grants_or_rekeys := TRUE;
        ELSIF OLD.access_tier IS DISTINCT FROM 'operator'
           OR NEW.public_key IS DISTINCT FROM OLD.public_key
           OR NEW.credential_id IS DISTINCT FROM OLD.credential_id
           OR NEW.employee_id IS DISTINCT FROM OLD.employee_id THEN
            grants_or_rekeys := TRUE;
        END IF;
    END IF;
    IF grants_or_rekeys
       AND coalesce(current_setting('boss.passkey_promotion', true), '') = '' THEN
        RAISE EXCEPTION 'a passkey reaches the operator tier, or is rekeyed while at it, only through the gateway''s passkey-promotion ceremony (POST /api/people/{id}/webauthn-credentials/{credential_id}/promote); % refused', TG_OP;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS webauthn_credentials_operator_only_by_promotion_trg ON webauthn_credentials;

CREATE TRIGGER webauthn_credentials_operator_only_by_promotion_trg
    BEFORE INSERT OR UPDATE ON webauthn_credentials
    FOR EACH ROW EXECUTE FUNCTION webauthn_credentials_operator_only_by_promotion();
