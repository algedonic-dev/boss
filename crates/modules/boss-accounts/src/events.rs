//! Domain event subjects for account operations.
//!
//! Wire format renamed during the boss-accounts carve-out:
//! `people.account.*` → `accounts.account.*`. Audit-log
//! consumers + NATS subscribers parse these subjects loosely
//! (substring or prefix match) so the rename doesn't break
//! existing dashboards. Strict subscribers should listen on
//! `accounts.>` going forward.
//!
//! Per `docs/design/projection-rebuilders.md`:
//! - `created` / `updated` carry the full `AccountWithContacts`
//!   payload (account row + every contact) and drive rebuild of
//!   both `accounts` and `account_contacts` projections.
//! - `deleted` carries `{id, deleted_at}`.

pub const ACCOUNT_CREATED: &str = "accounts.account.created";
pub const ACCOUNT_UPDATED: &str = "accounts.account.updated";
pub const ACCOUNT_DELETED: &str = "accounts.account.deleted";

/// Account team member assigned (or re-assigned). Payload mirrors
/// `AccountTeamAssignmentEvent` — `{id, account_id, employee_id,
/// role, assigned_on, notes}`. Emitted by every write path that
/// touches `account_team_members`: the dedicated POST handler,
/// the bulk-assign endpoint, and the `mirror_territory_rep` helper
/// in `accounts.rs` that keeps `accounts.territory_rep_id` in sync
/// with the `territory-rep` row.
///
/// `rebuild_accounts` consumes this event by UPSERTing into
/// `account_team_members` so a `boss-rebuild-all` cycle reproduces
/// the team roster from `audit_log` alone.
pub const ACCOUNT_TEAM_ASSIGNED: &str = "accounts.account.team-assigned";

/// Account team member removed. Payload `{account_id, role,
/// employee_id (was), unassigned_at}`. Rebuild deletes the matching
/// `(account_id, role)` row.
pub const ACCOUNT_TEAM_UNASSIGNED: &str = "accounts.account.team-unassigned";

/// Account note posted (free-form note, call, meeting, email, OR
/// auto-posted interaction note from a team-change). Payload mirrors
/// `AccountNotePostedEvent` — `{id, account_id, actor_id, kind,
/// body, occurred_at}`. Rebuild repopulates `account_notes`, so the
/// interaction log survives a `boss-rebuild-all` cycle.
pub const ACCOUNT_NOTE_POSTED: &str = "accounts.account.note-posted";

/// Account note soft-deleted. Payload `{note_id, deleted_by,
/// deleted_at}`. Rebuild marks the matching row as deleted; the
/// row stays in the projection (soft-delete by design).
pub const ACCOUNT_NOTE_DELETED: &str = "accounts.account.note-deleted";

/// Inbound support case opened. Payload mirrors the full
/// `SupportCase` row so the rebuilder can reproduce it byte-for-byte
/// from `audit_log`. POST /api/people/support-cases.
pub const SUPPORT_CASE_OPENED: &str = "accounts.support-case.opened";

/// Support case updated — status / assignee / resolution / CSAT.
/// Payload `SupportCaseUpdateEvent` carries the case `id` plus
/// every field that may have changed (None means "unchanged").
/// Rebuild applies the same COALESCE-style update.
pub const SUPPORT_CASE_UPDATED: &str = "accounts.support-case.updated";
