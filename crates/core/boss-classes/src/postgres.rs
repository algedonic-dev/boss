//! Postgres adapter for [`ClassRepository`]. Queries the single
//! `classes` table.

use async_trait::async_trait;
use boss_core::primitives::{Class, ClassRef};
use boss_core::publisher::EventStamp;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::port::{
    Backfill, BirthPlan, CLASS_DECLARED, CLASS_RETIRED, CLASS_UPDATED, ClassError, ClassFact,
    ClassRepository, NamedBirth, apply_change, backfill_birth, backfill_edit,
    backfilled_declared_event, backfilled_edit_events, birth_plan, changed_outside_the_doors,
    class_change, declared_event, facts_in_replay_order, observed_birth, observed_declared_event,
    retired_event, undeclared_birth, updated_event,
};

pub struct PgClasses {
    pool: PgPool,
}

impl PgClasses {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Row shape mirroring the `classes` table. Kept private so
/// `boss-core::Class` doesn't need a `sqlx::FromRow` derive (which
/// would force `sqlx` into `boss-core`'s dep graph).
#[derive(sqlx::FromRow)]
struct ClassRow {
    subject_kind: String,
    code: String,
    display_name: String,
    parent_code: Option<String>,
    member_attribute: Option<String>,
    metadata: Value,
    sort_order: i32,
    retired_at: Option<DateTime<Utc>>,
}

impl From<ClassRow> for Class {
    fn from(r: ClassRow) -> Self {
        Self {
            subject_kind: r.subject_kind,
            code: r.code,
            display_name: r.display_name,
            parent_code: r.parent_code,
            member_attribute: r.member_attribute,
            metadata: r.metadata,
            sort_order: r.sort_order,
            retired_at: r.retired_at,
        }
    }
}

/// The row a backfill locks, with the two stamps that say whether it
/// changed after its insert (review of car ce5ce2de, HIGH-1).
#[derive(sqlx::FromRow)]
struct LockedRow {
    #[sqlx(flatten)]
    row: ClassRow,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const SELECT_COLUMNS: &str = "subject_kind, code, display_name, parent_code, member_attribute, \
     metadata, sort_order, retired_at";

#[async_trait]
impl ClassRepository for PgClasses {
    async fn list_for_subject_kind(&self, subject_kind: &str) -> Result<Vec<Class>, ClassError> {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM classes \
             WHERE subject_kind = $1 AND retired_at IS NULL \
             ORDER BY sort_order ASC, code ASC"
        );
        let rows: Vec<ClassRow> = sqlx::query_as(&sql)
            .bind(subject_kind)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn get(&self, class_ref: &ClassRef) -> Result<Option<Class>, ClassError> {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM classes \
             WHERE subject_kind = $1 AND code = $2"
        );
        let row: Option<ClassRow> = sqlx::query_as(&sql)
            .bind(&class_ref.subject_kind)
            .bind(&class_ref.code)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(row.map(Into::into))
    }

    async fn exists_active(&self, class_ref: &ClassRef) -> Result<bool, ClassError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM classes \
             WHERE subject_kind = $1 AND code = $2 AND retired_at IS NULL)",
        )
        .bind(&class_ref.subject_kind)
        .bind(&class_ref.code)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(exists)
    }

    async fn update(&self, class: &Class, stamp: &EventStamp) -> Result<bool, ClassError> {
        // One transaction for the read, the UPDATE and its fact
        // (backlog 10dabe13): the row is locked while the before image
        // is read, so the `class.updated` names exactly what this edit
        // replaced, and the row and the fact commit or roll back
        // together. Until 2026-09-27 this was a bare UPDATE on the
        // pool that left nothing in the log. The lock also holds the
        // Class's log still while the birth is decided (backlog 6c2aa86c).
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        let key = ClassRef::new(&class.subject_kind, &class.code);
        let Some(locked) = lock_with_its_log(&mut tx, &key).await? else {
            return Ok(false);
        };
        let held = &locked.live;
        // A body identical to the held row changes no state, so it
        // writes nothing and records nothing (idempotence). The row
        // written is the held row with the logged change applied — the
        // one function a replay of this fact runs (backlog 3c6d0186),
        // so the write and its rebuild cannot disagree.
        let change = class_change(held, class)?;
        let Some(event) = updated_event(stamp, held, &change)? else {
            return Ok(true);
        };
        stage_the_birth_of_an_undeclared_class(&mut tx, &locked, stamp).await?;
        let class = &apply_change(held, &change)?;
        // The composite key is deliberately absent from the SET list:
        // a code is an identity other rows point at, so renaming it in
        // place would orphan them silently. `retired_at` is likewise
        // untouched — retiring a Class is its own action, not a side
        // effect of editing a label.
        sqlx::query(
            "UPDATE classes SET \
             display_name = $3, parent_code = $4, member_attribute = $5, \
             metadata = $6, sort_order = $7, updated_at = now() \
             WHERE subject_kind = $1 AND code = $2",
        )
        .bind(&class.subject_kind)
        .bind(&class.code)
        .bind(&class.display_name)
        .bind(&class.parent_code)
        .bind(&class.member_attribute)
        .bind(&class.metadata)
        .bind(class.sort_order)
        .execute(&mut *tx)
        .await
        .map_err(|e| ClassError::Storage(e.to_string()))?;
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(ClassError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(true)
    }

    async fn retire(&self, class_ref: &ClassRef, stamp: &EventStamp) -> Result<bool, ClassError> {
        // Stamp only when not already stamped — when a Class was
        // withdrawn is a fact, and a repeat call must not move it.
        // RETURNING hands back the row as stamped, so the
        // `class.retired` staged beside it in the same transaction
        // carries the exact `retired_at` the row holds (backlog
        // 10dabe13). The row is locked with its log first, so an
        // undeclared Class's birth is decided and staged before the
        // stamp, in the same transaction (backlog 6c2aa86c).
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        let Some(locked) = lock_with_its_log(&mut tx, class_ref).await? else {
            // No such row: the caller's 404.
            return Ok(false);
        };
        if locked.live.retired_at.is_some() {
            // Already retired: idempotent success, no fact — nothing moved.
            return Ok(true);
        }
        stage_the_birth_of_an_undeclared_class(&mut tx, &locked, stamp).await?;
        let sql = format!(
            "UPDATE classes SET retired_at = now(), updated_at = now() \
             WHERE subject_kind = $1 AND code = $2 AND retired_at IS NULL \
             RETURNING {SELECT_COLUMNS}"
        );
        let row: ClassRow = sqlx::query_as(&sql)
            .bind(&class_ref.subject_kind)
            .bind(&class_ref.code)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        boss_events::outbox::record_event_in_tx(&mut tx, &retired_event(stamp, &row.into()))
            .await
            .map_err(ClassError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(true)
    }

    async fn batch_upsert(&self, rows: &[Class], stamp: &EventStamp) -> Result<u64, ClassError> {
        // One `ON CONFLICT DO NOTHING` per row inside a single
        // transaction, mirroring the idempotent semantics of the seed
        // `classes.sql`. `created_at` / `updated_at` default in the
        // table; `retired_at` is intentionally not seeded (rows arrive
        // active). Row-at-a-time keeps the bind logic trivial — the
        // registry is tiny (≤ a few hundred rows) so a multi-row VALUES
        // batch would buy nothing — and is what lets `rows_affected`
        // say, per row, whether THIS one was inserted: only those
        // stage a `class.declared` on the outbox, in the same
        // transaction, so the fact and the row commit or roll back
        // together (backlog d9409039).
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        let mut inserted: u64 = 0;
        for r in rows {
            let result = sqlx::query(
                "INSERT INTO classes \
                 (subject_kind, code, display_name, parent_code, member_attribute, metadata, sort_order) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (subject_kind, code) DO NOTHING",
            )
            .bind(&r.subject_kind)
            .bind(&r.code)
            .bind(&r.display_name)
            .bind(&r.parent_code)
            .bind(&r.member_attribute)
            .bind(&r.metadata)
            .bind(r.sort_order)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                // The classes.subject_kind FK (SQLSTATE 23503):
                // surface WHICH kind was unregistered instead of a
                // generic storage 500.
                if let sqlx::Error::Database(ref dbe) = e
                    && dbe.code().as_deref() == Some("23503")
                {
                    return ClassError::UnregisteredKind(r.subject_kind.clone());
                }
                ClassError::Storage(e.to_string())
            })?;
            if result.rows_affected() == 1 {
                boss_events::outbox::record_event_in_tx(&mut tx, &declared_event(stamp, r)?)
                    .await
                    .map_err(ClassError::Storage)?;
                inserted += 1;
            }
        }
        tx.commit()
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(inserted)
    }

    async fn backfill_declared(
        &self,
        class_ref: &ClassRef,
        stamp: &EventStamp,
    ) -> Result<Backfill, ClassError> {
        // Locked, then its facts read ([`lock_with_its_log`]), so a race
        // of backfills records one birth.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        let Some(locked) = lock_with_its_log(&mut tx, class_ref).await? else {
            return Ok(Backfill::NotFound);
        };
        // Review of car ce5ce2de, HIGH-1, and backlog 6c2aa86c: a fact
        // un-applied recovers only what the log saw, so a row written
        // outside the doors would be declared with that write as its
        // birth — a false declare every fresh database replays as drift,
        // for good (facts are immutable). The row's stamps say whether
        // one happened after the insert and the newest fact
        // ([`changed_outside_the_doors`]); HIGH-1 asked only when the log
        // was empty, so the first door fact switched the check off. (A
        // seeded row a migration re-stamped is refused too — a safe
        // refusal, since a migration seed needs no backfill.)
        let Some(born) = stage_the_birth_of_an_undeclared_class(&mut tx, &locked, stamp).await?
        else {
            // Declared already: nothing to do, once the log is shown to
            // replay to the row (review of car ce5ce2de, LOW-3).
            backfill_birth(&locked.live, &locked.log)?;
            return Ok(Backfill::AlreadyDeclared);
        };
        tx.commit()
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(Backfill::Recorded(born))
    }

    async fn backfill_edited(
        &self,
        class_ref: &ClassRef,
        named: &NamedBirth,
        stamp: &EventStamp,
    ) -> Result<Backfill, ClassError> {
        // The bodiless backfill's lock and log read, so a race records
        // one pair: whoever waited reads a declared Class.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        let Some(Locked {
            live,
            created_at,
            updated_at,
            log,
        }) = lock_with_its_log(&mut tx, class_ref).await?
        else {
            return Ok(Backfill::NotFound);
        };
        // The one thing outside the log that says an edit happened
        // (backlog 93f361af): an insert writes both stamps with one
        // NOW(), and each fact-less door before 10dabe13 moved
        // `updated_at`. A row with no fact and unmoved stamps was never
        // edited, so an edit claimed for it would be invented.
        if log.is_empty() && updated_at == created_at {
            return Err(ClassError::Drift(format!(
                "({}, {}): the row has not changed since its insert at {created_at}, so no \
                 edit was made outside the doors — its live body is its birth, and the \
                 backfill without a body records it",
                live.subject_kind, live.code
            )));
        }
        let Some(change) = backfill_edit(&live, &log, named)? else {
            return Ok(Backfill::AlreadyDeclared);
        };
        let born = Class {
            retired_at: None,
            ..named.born.clone()
        };
        // The stamps read under the lock are the only record of when the
        // row was born and when it was edited, and the next door PUT moves
        // `updated_at` — so they go into the facts now (review of car
        // 8778f12f, finding 2).
        let events = backfilled_edit_events(
            stamp,
            &born,
            &change,
            &named.source,
            Some(created_at),
            Some(updated_at),
        )?;
        for event in events {
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(ClassError::Storage)?;
        }
        tx.commit()
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(Backfill::RecordedWithEdit { born, change })
    }

    async fn backfill_observed(
        &self,
        class_ref: &ClassRef,
        source: &str,
        stamp: &EventStamp,
    ) -> Result<Backfill, ClassError> {
        // The bodiless backfill's lock and log read, so a race records
        // one birth; the stamps are asked only for the drift the fact
        // carries, never to refuse — the operator has already been told.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        let Some(Locked {
            live,
            created_at,
            updated_at,
            log,
        }) = lock_with_its_log(&mut tx, class_ref).await?
        else {
            return Ok(Backfill::NotFound);
        };
        let Some(born) = observed_birth(&live, &log)? else {
            return Ok(Backfill::AlreadyDeclared);
        };
        let drift = changed_outside_the_doors(created_at, updated_at, &log);
        let event = observed_declared_event(
            stamp,
            &born,
            source,
            Some(created_at),
            Some(updated_at),
            drift,
        )?;
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(ClassError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(Backfill::Observed(born))
    }

    async fn birth_plans(&self, subject_kind: Option<&str>) -> Result<Vec<BirthPlan>, ClassError> {
        // Two reads, no lock: a dry run answers for the moment it read.
        let sql = format!(
            "SELECT {SELECT_COLUMNS}, created_at, updated_at FROM classes \
             WHERE ($1::text IS NULL OR subject_kind = $1) ORDER BY subject_kind, code"
        );
        let rows: Vec<LockedRow> = sqlx::query_as(&sql)
            .bind(subject_kind)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        let facts: Vec<(String, String, String, Value)> = sqlx::query_as(
            "SELECT subject_kind, code, kind, payload FROM ( \
               SELECT 0 AS src, id, payload->>'subject_kind' AS subject_kind, \
                      payload->>'code' AS code, kind, payload FROM audit_log \
                WHERE kind = ANY($1) AND ($2::text IS NULL OR payload->>'subject_kind' = $2) \
               UNION ALL \
               SELECT 1, o.id, o.payload->>'subject_kind', o.payload->>'code', o.kind, \
                      o.payload FROM event_outbox o \
                WHERE o.kind = ANY($1) AND ($2::text IS NULL OR o.payload->>'subject_kind' = $2) \
                  AND NOT EXISTS (SELECT 1 FROM audit_log a WHERE a.event_id = o.event_id) \
             ) f ORDER BY src, id",
        )
        .bind([CLASS_DECLARED, CLASS_UPDATED, CLASS_RETIRED].map(String::from))
        .bind(subject_kind)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ClassError::Storage(e.to_string()))?;
        let mut by_key: std::collections::HashMap<(String, String), Vec<(String, Value)>> =
            std::collections::HashMap::new();
        for (kind_of, code, kind, payload) in facts {
            by_key
                .entry((kind_of, code))
                .or_default()
                .push((kind, payload));
        }
        rows.into_iter()
            .map(|r| {
                let live = Class::from(r.row);
                let logged = by_key
                    .get(&(live.subject_kind.clone(), live.code.clone()))
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let log = facts_in_replay_order(logged.iter().map(|(k, p)| (k.as_str(), p)))?;
                Ok(birth_plan(&live, &log, Some((r.created_at, r.updated_at))))
            })
            .collect()
    }
}

/// Stage the backfilled `class.declared` of a Class the log does not
/// declare, inside `tx`, and answer the birth staged — `None` when the
/// log already declares it. What the bodiless backfill does, and what
/// the edit and retire doors do before their own fact (backlog 6c2aa86c),
/// so all three ask the row's stamps the same question
/// ([`changed_outside_the_doors`]) and refuse the same way.
async fn stage_the_birth_of_an_undeclared_class(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    locked: &Locked,
    stamp: &EventStamp,
) -> Result<Option<Class>, ClassError> {
    let outside = changed_outside_the_doors(locked.created_at, locked.updated_at, &locked.log);
    let Some(born) = undeclared_birth(&locked.live, &locked.log, outside)? else {
        return Ok(None);
    };
    boss_events::outbox::record_event_in_tx(tx, &backfilled_declared_event(stamp, &born)?)
        .await
        .map_err(ClassError::Storage)?;
    Ok(Some(born))
}

/// A Class row locked for a write, its two stamps, and its facts in
/// replay order.
struct Locked {
    live: Class,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    log: Vec<ClassFact>,
}

/// Lock one Class row and read its facts — the log's AND the outbox's
/// not yet relayed — inside `tx`. The row is locked before its facts are
/// read, and every door that stages a Class fact takes it through here,
/// so no fact can land between the read and the stage: a concurrent door
/// either committed first (its fact is read) or waits for this one. A retire staged a
/// second ago is a fact a birth must un-apply, and a backfill staged a
/// second ago is a declare, so a quick repeat records nothing. `None`
/// for no row.
async fn lock_with_its_log(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    class_ref: &ClassRef,
) -> Result<Option<Locked>, ClassError> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS}, created_at, updated_at FROM classes \
         WHERE subject_kind = $1 AND code = $2 FOR UPDATE"
    );
    let Some(locked) = sqlx::query_as::<_, LockedRow>(&sql)
        .bind(&class_ref.subject_kind)
        .bind(&class_ref.code)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| ClassError::Storage(e.to_string()))?
    else {
        return Ok(None);
    };
    let facts: Vec<(String, Value)> = sqlx::query_as(
        "SELECT kind, payload FROM ( \
           SELECT 0 AS src, id, kind, payload FROM audit_log \
            WHERE kind = ANY($1) AND payload->>'subject_kind' = $2 AND payload->>'code' = $3 \
           UNION ALL \
           SELECT 1, o.id, o.kind, o.payload FROM event_outbox o \
            WHERE o.kind = ANY($1) AND o.payload->>'subject_kind' = $2 \
              AND o.payload->>'code' = $3 \
              AND NOT EXISTS (SELECT 1 FROM audit_log a WHERE a.event_id = o.event_id) \
         ) f ORDER BY src, id",
    )
    .bind([CLASS_DECLARED, CLASS_UPDATED, CLASS_RETIRED].map(String::from))
    .bind(&class_ref.subject_kind)
    .bind(&class_ref.code)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| ClassError::Storage(e.to_string()))?;
    Ok(Some(Locked {
        live: Class::from(locked.row),
        created_at: locked.created_at,
        updated_at: locked.updated_at,
        log: facts_in_replay_order(facts.iter().map(|(k, p)| (k.as_str(), p)))?,
    }))
}
