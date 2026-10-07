//! Manual replay refuses incomplete evidence before clearing any projection.

use std::collections::{BTreeMap, BTreeSet};

use boss_events::replay::ReplayEvent;
use serde::Deserialize;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::types::{ManualSection, ManualSectionVersion};

#[derive(Deserialize)]
pub(crate) struct ManualFact {
    pub section: ManualSection,
    pub version: ManualSectionVersion,
    pub history_id: i64,
}

fn decode(event: &ReplayEvent) -> Result<ManualFact, String> {
    serde_json::from_value(event.payload.clone()).map_err(|e| {
        format!(
            "manual event {} has malformed evidence: {e}",
            event.audit_id
        )
    })
}

/// Validate chronology and identity from the log, then coverage of existing rows.
/// Values may drift and be repaired; an identity absent from the log must survive.
pub(crate) async fn validate_manual(
    conn: &mut PgConnection,
    events: &[ReplayEvent],
) -> Result<(), String> {
    let mut sections: BTreeMap<Uuid, ManualSection> = BTreeMap::new();
    let mut slugs: BTreeMap<String, Uuid> = BTreeMap::new();
    let mut history = BTreeSet::new();
    let mut history_ids = BTreeSet::new();
    for event in events
        .iter()
        .filter(|e| e.kind.starts_with("content.manual."))
    {
        let fact = decode(event)?;
        let s = &fact.section;
        let v = &fact.version;
        let refuse = |why| format!("manual event {}: {why}", event.audit_id);
        if event
            .payload
            .get("_actor")
            .and_then(serde_json::Value::as_str)
            != Some(v.edited_by.as_str())
        {
            return Err(refuse("stored editor does not match the recorded actor"));
        }
        // Postgres stores microseconds; the event envelope retains nanoseconds.
        if v.edited_at.timestamp_micros() != event.ts.timestamp_micros() {
            return Err(refuse(
                "stored edit time does not match the recorded event time",
            ));
        }
        if fact.history_id <= 0 || !history_ids.insert(fact.history_id) {
            return Err(refuse("invalid or repeated history identity"));
        }
        if s.id != v.section_id
            || s.current_version != v.version
            || v.version < 1
            || s.title != v.title
            || s.body != v.body
            || s.audience != v.audience
            || s.updated_at != v.edited_at
            || s.slug.trim().is_empty()
            || s.title.trim().is_empty()
        {
            return Err(refuse("section/version identity or state mismatch"));
        }
        match event.kind.as_str() {
            crate::events::SECTION_CREATED => {
                if v.version != 1
                    || sections.contains_key(&s.id)
                    || slugs.contains_key(&s.slug)
                    || s.created_at != s.updated_at
                {
                    return Err(refuse("creation must introduce one section at version 1"));
                }
                if s.parent_slug
                    .as_ref()
                    .is_some_and(|p| !slugs.contains_key(p))
                {
                    return Err(refuse("parent is not established before its child"));
                }
            }
            crate::events::SECTION_UPDATED => {
                let prior = sections
                    .get(&s.id)
                    .ok_or_else(|| refuse("version gap: update has no creation evidence"))?;
                if prior.current_version.checked_add(1) != Some(v.version)
                    || prior.slug != s.slug
                    || prior.parent_slug != s.parent_slug
                    || prior.created_at != s.created_at
                {
                    return Err(refuse("version gap or immutable section identity changed"));
                }
            }
            _ => return Err(refuse("unsupported manual event kind")),
        }
        history.insert((fact.history_id, v.section_id, v.version));
        slugs.insert(s.slug.clone(), s.id);
        sections.insert(s.id, fact.section);
    }
    let live_sections: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM manual_sections")
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| e.to_string())?;
    for id in live_sections {
        if !sections.contains_key(&id) {
            return Err(format!(
                "manual legacy coverage absent for section {id}; refusing rebuild before wipe"
            ));
        }
    }
    let live_history: Vec<(i64, Uuid, i32)> =
        sqlx::query_as("SELECT id, section_id, version FROM manual_section_history")
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| e.to_string())?;
    for identity in live_history {
        if !history.contains(&identity) {
            return Err(format!(
                "manual legacy coverage absent for history {identity:?}; refusing rebuild before wipe"
            ));
        }
    }
    Ok(())
}

pub(crate) async fn apply_manual(
    conn: &mut PgConnection,
    event: &ReplayEvent,
) -> Result<(), String> {
    let fact = decode(event)?;
    let s = &fact.section;
    let v = &fact.version;
    sqlx::query(
        "INSERT INTO manual_sections (id, slug, parent_slug, title, body, sort_order, audience, current_version, published, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT (id) DO UPDATE SET \
         title=EXCLUDED.title, body=EXCLUDED.body, sort_order=EXCLUDED.sort_order, audience=EXCLUDED.audience, \
         current_version=EXCLUDED.current_version, published=EXCLUDED.published, updated_at=EXCLUDED.updated_at",
    ).bind(s.id).bind(&s.slug).bind(&s.parent_slug).bind(&s.title).bind(&s.body)
        .bind(s.sort_order).bind(&s.audience.0).bind(s.current_version).bind(s.published)
        .bind(s.created_at).bind(s.updated_at).execute(&mut *conn).await.map_err(|e| e.to_string())?;
    sqlx::query(
        "INSERT INTO manual_section_history (id, section_id, version, title, body, audience, edited_by, edited_at, reason) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    ).bind(fact.history_id).bind(v.section_id).bind(v.version).bind(&v.title).bind(&v.body)
        .bind(&v.audience.0).bind(&v.edited_by).bind(v.edited_at).bind(&v.reason)
        .execute(&mut *conn).await.map_err(|e| e.to_string())?;
    // Explicit replay identities must not collide with the next ordinary write.
    // Never move the sequence backwards, including on repeated rebuilds.
    sqlx::query("SELECT setval(pg_get_serial_sequence('manual_section_history', 'id'), GREATEST(pg_sequence_last_value(pg_get_serial_sequence('manual_section_history', 'id')::regclass), $1), true)")
        .bind(fact.history_id).execute(&mut *conn).await.map_err(|e| e.to_string())?;
    Ok(())
}
