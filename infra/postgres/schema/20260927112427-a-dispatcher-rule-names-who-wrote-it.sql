-- 20260927112427-a-dispatcher-rule-names-who-wrote-it.sql — a
-- `dispatcher_rules` row carries the SIGNED caller of each authoring
-- act: who drafted it, who made it live, who took it out of service.
--
-- Origin: backlog 847af5c7, 2026-09-27. The three rule writes on
-- boss-dispatcher (`POST /api/dispatcher/rules`, `.../{name}/publish`,
-- `.../{name}/retire`) took no caller and asked no policy, so any
-- writable session through the gateway and any pod through the
-- ClusterIP machine door could change what the dispatcher enforces —
-- and the row it left said nothing about who. The doors now require an
-- identity and a `dispatcher-rule` grant (platform-admin by default),
-- and each act stamps the caller the request was SIGNED as
-- (`x-boss-user`), never a name from the body.
--
-- NULL means no signed caller wrote the column through a door: the boot
-- seed (rules::seed, which lands the authored directory directly), a
-- migration, or a row older than this file. `retired_by` is also the
-- publisher when a publish superseded the row, because that publish is
-- the act that retired it.
--
-- Not part of the content the runtime compares: `rules_fingerprint`
-- hashes what the dispatcher ENFORCES, and who wrote a rule does not
-- change what it does.

ALTER TABLE dispatcher_rules ADD COLUMN IF NOT EXISTS created_by TEXT;
ALTER TABLE dispatcher_rules ADD COLUMN IF NOT EXISTS published_by TEXT;
ALTER TABLE dispatcher_rules ADD COLUMN IF NOT EXISTS retired_by TEXT;
