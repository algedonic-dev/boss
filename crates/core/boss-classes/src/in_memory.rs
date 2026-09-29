//! In-memory `ClassRepository` impl. Used by tests and for service
//! startup before the postgres feature is wired in.

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::primitives::{Class, ClassRef};
use boss_core::publisher::EventStamp;
use std::sync::RwLock;

use crate::port::{
    Backfill, BirthPlan, ClassError, ClassFact, ClassRepository, NamedBirth, apply_change,
    backfill_birth, backfill_edit, backfilled_declared_event, backfilled_edit_events, birth_plan,
    class_change, declared_event, facts_in_replay_order, observed_birth, observed_declared_event,
    retired_event, undeclared_birth, updated_event,
};

/// Trivial in-memory store. Holds a snapshot of `Class` rows; lookups
/// are linear scans because the registry is tiny (≤ 100 rows in
/// account) and tests don't need indexing.
#[derive(Debug, Default)]
pub struct InMemoryClasses {
    rows: RwLock<Vec<Class>>,
    events: RwLock<Vec<Event>>,
}

impl InMemoryClasses {
    pub fn new(rows: Vec<Class>) -> Self {
        Self {
            rows: RwLock::new(rows),
            events: RwLock::new(Vec::new()),
        }
    }

    /// Every event recorded through this adapter, in order — what a
    /// Pg deployment would find on the outbox.
    pub fn recorded_events(&self) -> Vec<Event> {
        self.events.read().expect("rwlock poisoned").clone()
    }
}

#[async_trait]
impl ClassRepository for InMemoryClasses {
    async fn list_for_subject_kind(&self, subject_kind: &str) -> Result<Vec<Class>, ClassError> {
        let rows = self.rows.read().expect("rwlock poisoned");
        let mut out: Vec<Class> = rows
            .iter()
            .filter(|c| c.subject_kind == subject_kind && c.retired_at.is_none())
            .cloned()
            .collect();
        out.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.code.cmp(&b.code)));
        Ok(out)
    }

    async fn get(&self, class_ref: &ClassRef) -> Result<Option<Class>, ClassError> {
        let rows = self.rows.read().expect("rwlock poisoned");
        Ok(rows
            .iter()
            .find(|c| c.subject_kind == class_ref.subject_kind && c.code == class_ref.code)
            .cloned())
    }

    async fn exists_active(&self, class_ref: &ClassRef) -> Result<bool, ClassError> {
        let rows = self.rows.read().expect("rwlock poisoned");
        Ok(rows.iter().any(|c| {
            c.subject_kind == class_ref.subject_kind
                && c.code == class_ref.code
                && c.retired_at.is_none()
        }))
    }

    async fn update(&self, class: &Class, stamp: &EventStamp) -> Result<bool, ClassError> {
        let mut rows = self.rows.write().expect("rwlock poisoned");
        let mut events = self.events.write().expect("rwlock poisoned");
        match rows
            .iter_mut()
            .find(|c| c.subject_kind == class.subject_kind && c.code == class.code)
        {
            Some(existing) => {
                // The Pg adapter's shape: no change, no write, no fact;
                // and the row written is the held row with the logged
                // change applied — the function a replay runs (backlog
                // 3c6d0186), which keeps the key and the retirement.
                let change = class_change(existing, class)?;
                if let Some(event) = updated_event(stamp, existing, &change)? {
                    // An undeclared Class is declared first (backlog
                    // 6c2aa86c). Every fallible step runs before anything
                    // is pushed, so a refusal writes nothing.
                    let born = birth_before_a_door_fact(existing, &events, stamp)?;
                    let edited = apply_change(existing, &change)?;
                    events.extend(born);
                    events.push(event);
                    *existing = edited;
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn retire(&self, class_ref: &ClassRef, stamp: &EventStamp) -> Result<bool, ClassError> {
        let mut rows = self.rows.write().expect("rwlock poisoned");
        let mut events = self.events.write().expect("rwlock poisoned");
        match rows
            .iter_mut()
            .find(|c| c.subject_kind == class_ref.subject_kind && c.code == class_ref.code)
        {
            Some(existing) => {
                // Keep the original stamp on a repeat call — when it
                // was withdrawn is a fact, not a counter — and record
                // the fact only for the call that set it, after the
                // birth of an undeclared Class (backlog 6c2aa86c).
                if existing.retired_at.is_none() {
                    let born = birth_before_a_door_fact(existing, &events, stamp)?;
                    let retired = Class {
                        retired_at: Some(chrono::Utc::now()),
                        ..existing.clone()
                    };
                    events.extend(born);
                    events.push(retired_event(stamp, &retired));
                    *existing = retired;
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn batch_upsert(
        &self,
        incoming: &[Class],
        stamp: &EventStamp,
    ) -> Result<u64, ClassError> {
        // Mirror the Postgres `ON CONFLICT (subject_kind, code) DO
        // NOTHING`: a row whose composite key already exists is left
        // untouched; only genuinely-new rows are appended, and only
        // they record a `class.declared`. Returns the count actually
        // inserted.
        let mut rows = self.rows.write().expect("rwlock poisoned");
        let mut events = self.events.write().expect("rwlock poisoned");
        let mut inserted: u64 = 0;
        for r in incoming {
            let exists = rows
                .iter()
                .any(|c| c.subject_kind == r.subject_kind && c.code == r.code);
            if !exists {
                rows.push(r.clone());
                events.push(declared_event(stamp, r)?);
                inserted += 1;
            }
        }
        Ok(inserted)
    }

    async fn backfill_declared(
        &self,
        class_ref: &ClassRef,
        stamp: &EventStamp,
    ) -> Result<Backfill, ClassError> {
        // The Pg adapter's shape, over this adapter's own record: the
        // Class's facts are the events recorded here for its key.
        let rows = self.rows.read().expect("rwlock poisoned");
        let mut events = self.events.write().expect("rwlock poisoned");
        let Some(live) = rows
            .iter()
            .find(|c| c.subject_kind == class_ref.subject_kind && c.code == class_ref.code)
        else {
            return Ok(Backfill::NotFound);
        };
        let log = facts_of(&events, class_ref)?;
        match backfill_birth(live, &log)? {
            None => Ok(Backfill::AlreadyDeclared),
            Some(born) => {
                events.push(backfilled_declared_event(stamp, &born)?);
                Ok(Backfill::Recorded(born))
            }
        }
    }

    async fn backfill_edited(
        &self,
        class_ref: &ClassRef,
        named: &NamedBirth,
        stamp: &EventStamp,
    ) -> Result<Backfill, ClassError> {
        // The Pg adapter's shape minus its stamps: this adapter holds no
        // `created_at` / `updated_at`, so the proof is the whole check and
        // the facts say `born_at` / `edited_at` are unknown (null).
        let rows = self.rows.read().expect("rwlock poisoned");
        let mut events = self.events.write().expect("rwlock poisoned");
        let Some(live) = rows
            .iter()
            .find(|c| c.subject_kind == class_ref.subject_kind && c.code == class_ref.code)
        else {
            return Ok(Backfill::NotFound);
        };
        let log = facts_of(&events, class_ref)?;
        let Some(change) = backfill_edit(live, &log, named)? else {
            return Ok(Backfill::AlreadyDeclared);
        };
        let born = Class {
            retired_at: None,
            ..named.born.clone()
        };
        events.extend(backfilled_edit_events(
            stamp,
            &born,
            &change,
            &named.source,
            None,
            None,
        )?);
        Ok(Backfill::RecordedWithEdit { born, change })
    }

    async fn backfill_observed(
        &self,
        class_ref: &ClassRef,
        source: &str,
        stamp: &EventStamp,
    ) -> Result<Backfill, ClassError> {
        // The Pg adapter's shape minus its stamps: no `created_at` /
        // `updated_at` here, so the fact carries them as null, and no
        // drift a stamp could show.
        let rows = self.rows.read().expect("rwlock poisoned");
        let mut events = self.events.write().expect("rwlock poisoned");
        let Some(live) = rows
            .iter()
            .find(|c| c.subject_kind == class_ref.subject_kind && c.code == class_ref.code)
        else {
            return Ok(Backfill::NotFound);
        };
        let log = facts_of(&events, class_ref)?;
        let Some(born) = observed_birth(live, &log)? else {
            return Ok(Backfill::AlreadyDeclared);
        };
        events.push(observed_declared_event(
            stamp, &born, source, None, None, None,
        )?);
        Ok(Backfill::Observed(born))
    }

    async fn birth_plans(&self, subject_kind: Option<&str>) -> Result<Vec<BirthPlan>, ClassError> {
        let rows = self.rows.read().expect("rwlock poisoned");
        let events = self.events.read().expect("rwlock poisoned");
        let mut held: Vec<&Class> = rows
            .iter()
            .filter(|c| subject_kind.is_none_or(|k| c.subject_kind == k))
            .collect();
        held.sort_by(|a, b| (&a.subject_kind, &a.code).cmp(&(&b.subject_kind, &b.code)));
        held.into_iter()
            .map(|c| {
                let log = facts_of(&events, &ClassRef::new(&c.subject_kind, &c.code))?;
                Ok(birth_plan(c, &log, None))
            })
            .collect()
    }
}

/// One Class's facts, in replay order: the events recorded here for its
/// key — this adapter's own log.
fn facts_of(events: &[Event], class_ref: &ClassRef) -> Result<Vec<ClassFact>, ClassError> {
    let mine = events.iter().filter(|e| {
        ClassFact::from_logged(&e.kind, &e.payload).is_ok_and(|(key, _)| key == *class_ref)
    });
    facts_in_replay_order(mine.map(|e| (e.kind.as_str(), &e.payload)))
}

/// The backfilled `class.declared` a door stages before its own fact on
/// a Class this log does not declare ([`undeclared_birth`], backlog
/// 6c2aa86c), or `None` when it declares it. This double has no stamps
/// and no path that writes a row outside its doors — a seeded row is a
/// migration's — so nothing here can show a change outside them; the Pg
/// adapter asks its row's stamps.
fn birth_before_a_door_fact(
    held: &Class,
    events: &[Event],
    stamp: &EventStamp,
) -> Result<Option<Event>, ClassError> {
    let log = facts_of(events, &ClassRef::new(&held.subject_kind, &held.code))?;
    undeclared_birth(held, &log, None)?
        .map(|born| backfilled_declared_event(stamp, &born))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    fn employee(code: &str, sort: i32, retired: bool) -> Class {
        Class {
            subject_kind: "employee".into(),
            code: code.into(),
            display_name: code.to_uppercase(),
            parent_code: None,
            member_attribute: Some("role".into()),
            metadata: json!({}),
            sort_order: sort,
            retired_at: if retired {
                Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())
            } else {
                None
            },
        }
    }

    #[tokio::test]
    async fn list_returns_only_active_sorted() {
        let repo = InMemoryClasses::new(vec![
            employee("ceo", 10, false),
            employee("retired-role", 5, true),
            employee("service-tech", 31, false),
            employee("sales-rep", 22, false),
        ]);

        let out = repo.list_for_subject_kind("employee").await.unwrap();
        let codes: Vec<&str> = out.iter().map(|c| c.code.as_str()).collect();
        assert_eq!(codes, vec!["ceo", "sales-rep", "service-tech"]);
    }

    #[tokio::test]
    async fn list_filters_by_subject_kind() {
        let repo = InMemoryClasses::new(vec![
            employee("ceo", 10, false),
            Class {
                subject_kind: "account".into(),
                code: "account".into(),
                display_name: "Account".into(),
                parent_code: None,
                member_attribute: Some("account_type".into()),
                metadata: json!({}),
                sort_order: 0,
                retired_at: None,
            },
        ]);

        let employee_only = repo.list_for_subject_kind("employee").await.unwrap();
        assert_eq!(employee_only.len(), 1);
        assert_eq!(employee_only[0].code, "ceo");

        let account_only = repo.list_for_subject_kind("account").await.unwrap();
        assert_eq!(account_only.len(), 1);
        assert_eq!(account_only[0].code, "account");
    }

    #[tokio::test]
    async fn get_returns_retired_rows() {
        // Audit / history surfaces need to resolve old codes even
        // after retirement.
        let repo = InMemoryClasses::new(vec![employee("retired-role", 5, true)]);
        let r = repo
            .get(&ClassRef::new("employee", "retired-role"))
            .await
            .unwrap();
        assert!(r.is_some());
        assert!(r.unwrap().retired_at.is_some());
    }

    /// Backlog 6c2aa86c: a seeded row (a migration's, or one the batch
    /// door inserted before it staged facts) has no birth in the log. Its
    /// first door fact records the birth first — the row as it stood
    /// before that fact, marked as a backfill — then the fact itself, so
    /// no door fact lands on an undeclared Class. A restatement records
    /// nothing, and a Class the log declares gets no second birth.
    #[tokio::test]
    async fn a_door_fact_on_a_seeded_row_records_its_birth_first() {
        let stamp = EventStamp::new(
            "classes",
            boss_core::actor::ActorId::Automation("tenant-seed".into()),
        );
        let seeded = employee("clerk", 3, false);
        let repo = InMemoryClasses::new(vec![seeded.clone(), employee("cook", 4, false)]);

        assert!(repo.update(&seeded, &stamp).await.unwrap());
        assert!(repo.recorded_events().is_empty(), "a restatement: no fact");

        let mut edited = seeded.clone();
        edited.display_name = "Clerk of works".into();
        assert!(repo.update(&edited, &stamp).await.unwrap());
        assert!(
            repo.retire(&ClassRef::new("employee", "cook"), &stamp)
                .await
                .unwrap()
        );
        let mut again = edited.clone();
        again.sort_order = 9;
        assert!(repo.update(&again, &stamp).await.unwrap());

        let events = repo.recorded_events();
        let seen: Vec<(&str, &str)> = events
            .iter()
            .map(|e| (e.kind.as_str(), e.payload["code"].as_str().unwrap()))
            .collect();
        assert_eq!(
            seen,
            vec![
                (crate::port::CLASS_DECLARED, "clerk"),
                (crate::port::CLASS_UPDATED, "clerk"),
                (crate::port::CLASS_DECLARED, "cook"),
                (crate::port::CLASS_RETIRED, "cook"),
                (crate::port::CLASS_UPDATED, "clerk"),
            ],
            "{events:?}"
        );
        assert!(crate::port::is_backfill(&events[0].payload));
        assert_eq!(events[0].payload["display_name"], json!("CLERK"));
        assert_eq!(events[2].payload["retired_at"], serde_json::Value::Null);
        for key in [
            ClassRef::new("employee", "clerk"),
            ClassRef::new("employee", "cook"),
        ] {
            assert_eq!(
                repo.backfill_declared(&key, &stamp).await.unwrap(),
                Backfill::AlreadyDeclared,
                "{key:?}: the log declares it and replays to the row"
            );
        }
    }

    #[tokio::test]
    async fn exists_active_excludes_retired() {
        let repo = InMemoryClasses::new(vec![
            employee("ceo", 10, false),
            employee("retired-role", 5, true),
        ]);
        assert!(
            repo.exists_active(&ClassRef::new("employee", "ceo"))
                .await
                .unwrap()
        );
        assert!(
            !repo
                .exists_active(&ClassRef::new("employee", "retired-role"))
                .await
                .unwrap(),
            "retired classes are not considered active"
        );
        assert!(
            !repo
                .exists_active(&ClassRef::new("employee", "no-such-code"))
                .await
                .unwrap()
        );
    }
}
