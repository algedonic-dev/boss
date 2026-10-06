-- 20261001070511-a-customer-email-is-one-key-everywhere.sql — the
-- customers unique index holds the same email key the id mint and both
-- adapters hold (backlog e1b08aaa, 2026-10-01).
--
-- WHAT WAS WRONG. Three rules for one fact. The id mint trimmed and
-- folded full Unicode case; this index (30-customers.sql) was
-- lower(email) — no trim, and case folded by the DATABASE'S LOCALE; the
-- in-memory double folded full Unicode case. So explicit-id creates of
-- ' pat@x' and 'pat@x' landed as two rows for one person, and whether two
-- non-ASCII-cased addresses were one depended on how the cluster was
-- initialised.
--
-- THE ONE RULE is boss_customers::types::email_key: trim ASCII whitespace
-- (space, tab, line feed, form feed, carriage return), lower ASCII
-- letters, keep every other byte. The expression below is
-- types::SQL_EMAIL_KEY verbatim, and an_email_is_one_key_everywhere_pg.rs
-- holds this file to that constant and the constant to the Rust function
-- on a real database.
--
-- CONSERVATION. Rows the old index allowed may be one address under the
-- new key. This migration does not choose between them — merging or
-- dropping a customer is a decision about a person, and it belongs on the
-- record as events, not in a schema change. It REFUSES, naming every
-- clashing group by id (ids carry no PII; the emails are not printed),
-- and the transaction leaves the old index and every row as they were.
-- Resolve the named rows through the customers door, then re-run.
DO $$
DECLARE
    clashes TEXT;
BEGIN
    SELECT string_agg(ids, '; ' ORDER BY ids COLLATE "C") INTO clashes
      FROM (SELECT string_agg(id, ', ' ORDER BY id COLLATE "C") AS ids
              FROM customers
             WHERE email IS NOT NULL
             GROUP BY translate(btrim(email, E' \t\n\f\r'), 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz')
            HAVING count(*) > 1) AS groups;
    IF clashes IS NOT NULL THEN
        RAISE EXCEPTION 'customers_email: these customers are one address under the email key and must be resolved before the index can hold it (backlog e1b08aaa): %', clashes;
    END IF;
END
$$;

DROP INDEX IF EXISTS customers_email;
CREATE UNIQUE INDEX customers_email
    ON customers ((translate(btrim(email, E' \t\n\f\r'), 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz')))
    WHERE email IS NOT NULL;
