-- 20260929231035-a-credential-row-names-the-stamping-clients-not-attach.sql
-- — a credentials row that says the machine token is read through
-- `machine_token::attach` says what the platform does now instead
-- (backlog 2ee29275, item S4 of the blocking-senders review).
--
-- MEASURED 2026-09-29 through GET /api/credentials/boss-machine-token:
-- the live row's third consumer reads "every service writer via
-- boss_core::machine_token::attach (reads BOSS_MACHINE_TOKEN at process
-- start)". Both halves stopped being true in design 6805c764 car 2: the
-- env var was deleted as a source (the `current` slot of the mounted
-- directory is the one source), and `attach`, the read-once door, is
-- deleted by the blocking-senders slice with its last caller. The text
-- came from 202609031700-credentials-are-registry-rows.sql:151 and was
-- carried into the instance's own declaration; that migration is
-- applied history and is not edited (migrations-append-only.sh).
--
-- WHY A MIGRATION. The credentials registry has a declaration door
-- (POST /api/credentials/batch, insert-if-absent: it keeps a row that is
-- already there) and a rotation door, and no door that edits a row's
-- text — so a declaration cannot correct it, and there is no
-- `credential.updated` fact to record the edit with. This UPDATE is the
-- correction the packet asked for; the missing edit door is the
-- follow-up, and until it exists this change is visible here and not in
-- the audit log.
--
-- WHAT IT TOUCHES. Only a consumer entry whose `location` names
-- `machine_token::attach` — the platform's own deleted function, not
-- any one instance's fact — on any row that carries one: its `kind` and
-- `location` replaced in place (same position) by what the platform
-- does now, and any other key it carries kept. Every other
-- entry, and every row without that text, is left exactly as it is, so
-- a fresh database (which holds no credential row, 20260918063829) and
-- a second run are both no-ops.

UPDATE credentials c
   SET consumers = (
       SELECT jsonb_agg(
                  CASE
                      WHEN strpos(e ->> 'location', 'machine_token::attach') > 0
                      -- `||` keeps every other key the entry carried and
                      -- replaces only these two (review of 54d9a23a, LOW-3).
                      THEN e || jsonb_build_object(
                               'kind', 'secret-mount',
                               'location',
                               'every service writer via boss_core::machine_token::Client and '
                               'BlockingClient, and the gateway''s MachineClient: the `current` '
                               'slot of BOSS_MACHINE_TOKEN_DIR (default /etc/boss/machine-token), '
                               're-read every 5 s and stamped per request, only on loopback and '
                               'the hosts and .namespace suffixes BOSS_MACHINE_TOKEN_HOSTS lists '
                               '(an instance lists its own namespace; a host, its record)')
                      ELSE e
                  END
                  ORDER BY n)
         FROM jsonb_array_elements(c.consumers) WITH ORDINALITY AS t(e, n))
 WHERE strpos(c.consumers::text, 'machine_token::attach') > 0;
