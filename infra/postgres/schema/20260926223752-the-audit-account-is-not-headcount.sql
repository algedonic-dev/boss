-- 20260926223752-the-audit-account-is-not-headcount.sql — the
-- `audit-readonly` role Class says its holders are accounts, not people.
--
-- WHY (backlog 6a123f1f; page audit 0c0265a3 GAP 12, 2026-09-23). The
-- /ux/people header read "2 active employees" on the system of record:
-- emp-david and emp-audit, the System Audit Account the operator
-- baseline ships in every deployment (infra/operator-baseline/
-- operator_hires.toml). A headcount no reader can reproduce as people.
-- MEASURED 2026-09-26 through boss-api on the system of record: GET
-- /api/people answers exactly those two rows, both status active, and
-- the (employee, role) Classes carry no key that tells an account from
-- a person — `is_system_role` sits on BOTH `audit-readonly` and
-- `platform-admin`, and the founder wears `platform-admin`, so it cannot
-- be the headcount rule.
--
-- THE KEY. `counts_in_headcount: false` on a role Class means whoever
-- wears the role is not counted as an employee (apps/web/src/people/
-- roster-counts.ts `headcount`); absent means counted. It is data, not
-- an id list, so a tenant marks any other account-only role the same
-- way. Documented in docs/design/class-registry.md.
--
-- WHY A MIGRATION. 01-registries.sql inserted the row and migrations are
-- append-only history, so only a later migration changes it; the Class
-- registry has no platform bundle (migrations-declare-schema-only.sh
-- names none for `classes`), and an UPDATE is not an insert. 20260924172233
-- is the precedent. `||` merges the key into whatever metadata the
-- instance holds, so a tenant's own edits to the row survive.

UPDATE classes
   SET metadata = metadata || '{"counts_in_headcount": false}'::jsonb,
       updated_at = NOW()
 WHERE subject_kind = 'employee'
   AND member_attribute = 'role'
   AND code = 'audit-readonly';
