-- 20260930234959-the-personal-github-token-is-retired.sql — the
-- credentials row for the personal GitHub token says it is retired
-- (backlog d2b7c947, car 2).
--
-- David, 2026-09-30: "we have made a fundamental change to our posture
-- and now are working professionally out of the algedonic-dev Github
-- org via the Github App." The row `dauld-github-token` declared a
-- fine-grained personal access token of David's own GitHub account,
-- minted by hand (202609081230, applied history, not edited): the public
-- mirror's pull request was pushed to a public fork in that account and
-- opened as it. MEASURED 2026-09-30 through GET /api/credentials/
-- dauld-github-token: the live row declares one consumer, the publish
-- verb, scopes unverified, rotation never recorded. Since car 1 of this
-- item (83cd517f) the verb opens its PR from a branch of the mirror
-- itself as the GitHub App, with an installation token the broker mints
-- per request; since this car the off-site push declares no target for
-- the fork and the recovery kit no longer takes the token. Nothing in the
-- estate reads it.
--
-- WHY A MIGRATION, AND WHY AN UPDATE. The registry's posture
-- (202609031700): "Rows are never deleted while the credential exists
-- anywhere. A revoked credential keeps its row (notes say revoked)." The
-- token still exists until David revokes it in GitHub — it lives in his
-- personal account, where no machine of the estate can act — so the row
-- stays and says so. The registry has no door that edits a row's text
-- (the insert-if-absent declaration keeps a row as it is), so this is the
-- edit, as 20260929231035 was; it is visible here and not in the audit
-- log, which is that same follow-up.
--
-- WHAT IT TOUCHES. The one row, by id: no consumer (nothing reads it),
-- and a note that begins RETIRED and says what is left to do and whose it
-- is, with the note it replaces kept after it. A fresh database holds no
-- such row (20260918063829 removed the seeded ones), and a second run
-- matches nothing, so both are no-ops.

UPDATE credentials
   SET consumers = '[]'::jsonb,
       notes = 'RETIRED 2026-09-30 (backlog d2b7c947; David: all GitHub work runs through '
               'the algedonic-dev organisation, as the GitHub App). Nothing in the estate '
               'reads this token: the publish verb opens its pull request as the App '
               '(car 83cd517f), the off-site push names no target for the fork, and the '
               'recovery kit no longer takes it. Left to David, in his personal GitHub '
               'account: revoke the token, delete the public fork dauld/boss-mirror, and '
               'delete the token file on the forge host. The row stays, as a revoked '
               'credential''s does (202609031700), until the forge-side audit finds '
               'nothing that matches it. Before retirement: ' || notes
 WHERE id = 'dauld-github-token'
   AND notes NOT LIKE 'RETIRED 2026-09-30%';
