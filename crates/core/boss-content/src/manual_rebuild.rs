//! Manual replay refuses incomplete evidence before clearing any projection.

use std::collections::{BTreeMap, BTreeSet};

use boss_events::replay::ReplayEvent;
use serde::Deserialize;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::manual_baseline::ManualBaseline;
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
pub(crate) async fn prepare_manual(
    conn: &mut PgConnection,
    events: &[ReplayEvent],
) -> Result<Vec<ReplayEvent>, String> {
    let mut sections: BTreeMap<Uuid, ManualSection> = BTreeMap::new();
    let mut slugs: BTreeMap<String, Uuid> = BTreeMap::new();
    let mut history = BTreeSet::new();
    let mut history_ids = BTreeSet::new();
    let mut plan: Vec<ReplayEvent> = Vec::new();
    let baselines: Vec<_> = events
        .iter()
        .filter(|e| e.kind == crate::events::BASELINE_OBSERVED)
        .collect();
    if baselines.len() > 1 {
        return Err("manual log has more than one observed baseline".into());
    }
    let mut cutoff = None;
    if let Some(event) = baselines.first() {
        let baseline: ManualBaseline = serde_json::from_value(event.payload.clone())
            .map_err(|e| format!("manual baseline: {e}"))?;
        let source: String =
            sqlx::query_scalar("SELECT source FROM audit_log WHERE id=$1 AND event_id=$2")
                .bind(event.audit_id)
                .bind(event.event_id)
                .fetch_one(&mut *conn)
                .await
                .map_err(|e| e.to_string())?;
        baseline.validate_fact(event.event_id, &source, event.ts, &event.payload)?;
        if baseline.log_cutoff >= event.audit_id {
            return Err("manual baseline observation provenance disagrees with its event".into());
        }
        for raw in &baseline.inventory.sections {
            let section: ManualSection =
                serde_json::from_value(raw.clone()).map_err(|e| e.to_string())?;
            slugs.insert(section.slug.clone(), section.id);
            sections.insert(section.id, section);
        }
        for raw in &baseline.inventory.history {
            let id = raw["id"]
                .as_i64()
                .ok_or("manual baseline history id absent")?;
            let version: ManualSectionVersion =
                serde_json::from_value(raw.clone()).map_err(|e| e.to_string())?;
            history.insert((id, version.section_id, version.version));
            history_ids.insert(id);
        }
        // Earlier real facts remain facts, and must agree with the stored
        // history observed by this baseline. No missing creation is invented.
        for prior in events
            .iter()
            .filter(|e| e.kind.starts_with("content.manual.") && e.audit_id <= baseline.log_cutoff)
        {
            if !matches!(
                prior.kind.as_str(),
                crate::events::SECTION_CREATED | crate::events::SECTION_UPDATED
            ) {
                return Err("unsupported manual fact before observed baseline".into());
            }
            let fact = decode(prior)?;
            let stored = baseline
                .inventory
                .history
                .iter()
                .find(|h| h["id"].as_i64() == Some(fact.history_id))
                .ok_or("manual baseline omits previously recorded history")?;
            let version: ManualSectionVersion =
                serde_json::from_value(stored.clone()).map_err(|e| e.to_string())?;
            let observed = sections
                .get(&fact.section.id)
                .ok_or("manual baseline omits prior section")?;
            if version != fact.version
                || fact.section.id != fact.version.section_id
                || fact.section.current_version != fact.version.version
                || fact.section.title != fact.version.title
                || fact.section.body != fact.version.body
                || fact.section.audience != fact.version.audience
                || fact.section.updated_at != fact.version.edited_at
                || (prior.kind == crate::events::SECTION_CREATED && fact.version.version != 1)
                || prior.payload["_actor"].as_str() != Some(fact.version.edited_by.as_str())
                || prior.ts.timestamp_micros() != fact.version.edited_at.timestamp_micros()
                || observed.slug != fact.section.slug
                || observed.parent_slug != fact.section.parent_slug
                || observed.created_at != fact.section.created_at
            {
                return Err("manual baseline disagrees with previously recorded fact".into());
            }
        }
        cutoff = Some(baseline.log_cutoff);
        plan.push((*event).clone());
    }
    for event in events.iter().filter(|e| {
        e.kind.starts_with("content.manual.")
            && e.kind != crate::events::BASELINE_OBSERVED
            && cutoff.is_none_or(|cut| e.audit_id > cut)
    }) {
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
    plan.extend(
        events
            .iter()
            .filter(|e| e.kind != crate::events::BASELINE_OBSERVED)
            .cloned(),
    );
    Ok(plan)
}

pub(crate) async fn covered_by_baseline(
    conn: &mut PgConnection,
    event: &ReplayEvent,
) -> Result<bool, String> {
    let cutoff: Option<i64> = sqlx::query_scalar(
        "SELECT (payload->>'log_cutoff')::bigint FROM audit_log WHERE kind = $1",
    )
    .bind(crate::events::BASELINE_OBSERVED)
    .fetch_optional(conn)
    .await
    .map_err(|e| e.to_string())?;
    Ok(cutoff.is_some_and(|cut| event.audit_id <= cut))
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
