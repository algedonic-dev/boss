//! The version floor a registry's `publish_declared` enforces — ONE
//! rule for the step-plugin, station, delivery-policy and cadence
//! registries and both adapters of each. The cadence registry kept its
//! own copy, read under the one-statement lock the paragraph below
//! measures as short, until backlog 4541d511 moved it here
//! (`pg_an_older_publish_waiting_on_a_newer_one_is_refused` in its
//! adapters-agree suite).
//!
//! WHY (backlog 1cd85e94, and df793bd7 for the station and
//! delivery-policy twins, 2026-09-29). A bundle write publishes at the
//! version its file DECLARES and retires the live row of the name. The
//! step-plugin, station and delivery-policy adapters refused only the
//! exact (name, version), so a declared version BELOW the live one
//! retired it and went live in its place. The bundle seed classifies
//! first and writes only a version above the live row — but that read
//! is taken before the write, outside its transaction: two converges
//! carrying different bundles overlap, the older classifies `v4` over a
//! live `v3`, the newer lands `v5`, and the older's write retired `v5`,
//! silently downgrading what every step born next renders through. The
//! port now enforces its own floor, inside the write: a declared
//! version at or below the newest the lineage holds, ANY status, is a
//! conflict — the rule the cadence registry already had ("a publish is
//! a version bump").
//!
//! WHY A LOCK AND THEN A READ, and not the cadence adapter's
//! `MAX(version)` over rows locked `FOR UPDATE` in one statement. That
//! statement waits on a row the other writer holds, then re-reads that
//! row alone: a row the other writer INSERTED is not in the snapshot
//! the waiting statement began with. So the older of two overlapping
//! writes, let through after the newer committed `v5`, read `3`, passed
//! its floor, and retired `v5` — measured while this was built
//! (`pg_an_older_seed_waiting_on_a_newer_one_is_refused`). Under READ
//! COMMITTED each statement takes a fresh snapshot, so the floor is
//! read in a statement that STARTS after the lineage's lock is held,
//! and that lock is a transaction-scoped advisory lock keyed on the
//! (table, name) pair — which also serialises the first two writes of a
//! name no row exists for yet, where there is no row to lock.

/// The newest version a lineage holds, any status, or 0 for a name
/// never held — so a first declared version must be at least 1.
pub fn newest(lineage: impl Iterator<Item = i32>) -> i32 {
    lineage.max().unwrap_or(0)
}

/// The refusal a declared version at or below the newest answers —
/// worded once, so every registry's conflict reads the same over
/// either adapter.
pub fn not_above(name: &str, version: i32, newest: i32) -> String {
    format!(
        "{name}@{version} is not above the newest version of its lineage (v{newest}); \
         a publish is a version bump — declare v{}",
        newest + 1
    )
}

/// The refusal a publish signed over one live version answers when the
/// lineage's newest, read under the write's own lock, is another —
/// review R1 of car 4019ba69 (2026-11-08): a signed approval is a diff
/// against a NAMED version, and a client's read before its write cannot
/// hold that version still. Worded once, for both adapters.
pub fn moved(name: &str, over: i32, newest: i32) -> String {
    format!(
        "{name}: this publish was signed over live v{over}, and the lineage's newest version \
         is v{newest}; what was approved is a diff against v{over}, not against what is live \
         now, so nothing was written — file the change again over v{newest}"
    )
}

/// Serialise every declared write of `name` in `table` behind this
/// transaction, then read the newest version its lineage holds — in
/// that order, and as two statements (module doc). `table` and
/// `name_column` are the adapter's own compile-time identifiers, never
/// caller input.
#[cfg(feature = "postgres")]
pub async fn lock_and_read_newest(
    tx: &mut sqlx::PgConnection,
    table: &'static str,
    name_column: &'static str,
    name: &str,
) -> Result<i32, sqlx::Error> {
    lock_and_read_newest_with_budget(tx, table, name_column, name, None).await
}

#[cfg(feature = "postgres")]
pub(crate) async fn lock_and_read_newest_with_budget(
    tx: &mut sqlx::PgConnection,
    table: &'static str,
    name_column: &'static str,
    name: &str,
    budget: Option<boss_events::statement_budget::StatementBudget>,
) -> Result<i32, sqlx::Error> {
    boss_events::statement_budget::refresh(tx, budget)
        .await
        .map_err(sqlx::Error::Protocol)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1), hashtext($2))")
        .bind(table)
        .bind(name)
        .execute(&mut *tx)
        .await?;
    boss_events::statement_budget::refresh(tx, budget)
        .await
        .map_err(sqlx::Error::Protocol)?;
    let max: Option<i32> = sqlx::query_scalar(&format!(
        "SELECT MAX(version) FROM {table} WHERE {name_column} = $1"
    ))
    .bind(name)
    .fetch_one(&mut *tx)
    .await?;
    Ok(newest(max.into_iter()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_never_held_has_newest_zero() {
        assert_eq!(newest(std::iter::empty()), 0);
        assert_eq!(newest([3, 5, 4].into_iter()), 5);
    }

    #[test]
    fn a_moved_baseline_names_both_versions_and_the_one_to_refile_over() {
        let text = moved("robots:door", 1, 2);
        assert!(text.contains("signed over live v1"), "{text}");
        assert!(text.contains("newest version is v2"), "{text}");
        assert!(text.ends_with("file the change again over v2"), "{text}");
    }

    #[test]
    fn the_refusal_names_the_version_to_declare() {
        assert_eq!(
            not_above("sign-off", 4, 5),
            "sign-off@4 is not above the newest version of its lineage (v5); \
             a publish is a version bump — declare v6"
        );
    }
}
