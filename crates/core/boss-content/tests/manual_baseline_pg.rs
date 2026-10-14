//! Legacy rows are observed now; their old stored history is never invented.
use boss_testing::TestDb;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

async fn rows(pool: &sqlx::PgPool) -> Value {
    let sections: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(s) FROM manual_sections s ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap();
    let history: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(h) FROM manual_section_history h ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap();
    let seq: (i64, bool) =
        sqlx::query_as("SELECT last_value,is_called FROM manual_section_history_id_seq")
            .fetch_one(pool)
            .await
            .unwrap();
    json!({"sections":sections,"history":history,"sequence":{"last_value":seq.0,"is_called":seq.1}})
}

async fn legacy(pool: &sqlx::PgPool) {
    let id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO manual_sections (id,slug,title,body,published,audience,created_at,updated_at) VALUES ($1,'legacy','Stored title','Stored body',false,'{\"roles\":[\"private\"]}','2010-01-01','2010-01-01')").bind(id).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO manual_section_history (id,section_id,version,title,body,audience,edited_by,edited_at,reason) VALUES (42,$1,1,'Stored title','Stored body','{\"roles\":[\"private\"]}','emp-historical','2010-01-01','Stored historical claim')").bind(id).execute(pool).await.unwrap();
    sqlx::query("SELECT setval(pg_get_serial_sequence('manual_section_history','id'),50,true)")
        .execute(pool)
        .await
        .unwrap();
}

async fn drain(pool: &sqlx::PgPool) {
    let bus: std::sync::Arc<dyn boss_core::port::EventBus> = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(pool, &bus, 100)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_observed_baseline_conserves_legacy_raw_rows_and_sequence_twice() {
    let db = TestDb::new().await;
    legacy(&db.pool).await;
    let before = rows(&db.pool).await;
    let hash = hex::encode(Sha256::digest(serde_json::to_vec(&before).unwrap()));
    let mut inventory = before.clone();
    inventory["section_count"] = json!(1);
    inventory["history_count"] = json!(1);
    inventory["sha256"] = json!(hash);
    let stamp = boss_core::publisher::EventStamp::new("content", "emp-observer".parse().unwrap());
    let mut event=stamp.event("content.manual.baseline_observed",json!({"admission_id":uuid::Uuid::new_v4(),"observed_at":stamp.timestamp,"observed_by":"emp-observer","inventory":inventory,"log_cutoff":0}));
    event.payload["event_id"] = json!(event.id);
    event.payload["event_source"] = json!(event.source);
    let mut tx = db.pool.begin().await.unwrap();
    boss_events::outbox::record_event_in_tx(&mut tx, &event)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    drain(&db.pool).await;
    for _ in 0..2 {
        boss_content::rebuild_content(&db.pool).await.unwrap();
        assert_eq!(
            rows(&db.pool).await,
            before,
            "baseline must conserve raw fields and consumed sequence gaps"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn admission_is_atomic_compare_and_swap_and_retry_preserves_observation() {
    use boss_content::manual_baseline::{BaselineAdmission, ManualBaselineRepository};
    let db = TestDb::new().await;
    legacy(&db.pool).await;
    let repo = boss_content::PgContent::new(db.pool.clone());
    let inventory = repo.manual_inventory().await.unwrap();
    assert_eq!(inventory.section_count, 1);
    assert_eq!(inventory.history[0]["id"], 42);
    let stamp = boss_core::publisher::EventStamp::new("content", "emp-observer".parse().unwrap());
    let mut admission = BaselineAdmission {
        admission_id: uuid::Uuid::new_v4(),
        expected_sha256: "wrong".into(),
    };
    assert!(
        repo.observe_manual_baseline(&admission, &stamp)
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    admission.expected_sha256 = inventory.sha256;
    let before = rows(&db.pool).await;
    let observed = repo
        .observe_manual_baseline(&admission, &stamp)
        .await
        .unwrap();
    assert_eq!(rows(&db.pool).await, before);
    let later = boss_core::publisher::EventStamp::new("content", "emp-observer".parse().unwrap());
    assert_eq!(
        repo.observe_manual_baseline(&admission, &later)
            .await
            .unwrap(),
        observed
    );
    let other = boss_core::publisher::EventStamp::new("content", "emp-other".parse().unwrap());
    assert!(
        repo.observe_manual_baseline(&admission, &other)
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox WHERE kind='content.manual.baseline_observed'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    drain(&db.pool).await;
    assert_eq!(
        repo.observe_manual_baseline(&admission, &later)
            .await
            .unwrap(),
        observed
    );
    boss_content::rebuild_content(&db.pool).await.unwrap();
    assert_eq!(rows(&db.pool).await, before);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_admission_records_one_fact_and_changed_inventory_refuses() {
    use boss_content::manual_baseline::{BaselineAdmission, ManualBaselineRepository};
    let db = TestDb::new().await;
    legacy(&db.pool).await;
    let repo = boss_content::PgContent::new(db.pool.clone());
    let inventory = repo.manual_inventory().await.unwrap();
    sqlx::query("UPDATE manual_sections SET published=true")
        .execute(&db.pool)
        .await
        .unwrap();
    let admission = BaselineAdmission {
        admission_id: uuid::Uuid::new_v4(),
        expected_sha256: inventory.sha256,
    };
    let stamp = boss_core::publisher::EventStamp::new("content", "emp-observer".parse().unwrap());
    assert!(
        repo.observe_manual_baseline(&admission, &stamp)
            .await
            .is_err()
    );
    let admission = BaselineAdmission {
        expected_sha256: repo.manual_inventory().await.unwrap().sha256,
        ..admission
    };
    let (a, b) = tokio::join!(
        repo.observe_manual_baseline(&admission, &stamp),
        repo.observe_manual_baseline(&admission, &stamp)
    );
    assert_eq!(a.unwrap(), b.unwrap());
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox WHERE kind='content.manual.baseline_observed'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn incomplete_raw_history_cannot_be_observed_or_wiped() {
    use boss_content::manual_baseline::ManualBaselineRepository;
    let db = TestDb::new().await;
    legacy(&db.pool).await;
    sqlx::query("DELETE FROM manual_section_history")
        .execute(&db.pool)
        .await
        .unwrap();
    let before = rows(&db.pool).await;
    let repo = boss_content::PgContent::new(db.pool.clone());
    assert!(repo.manual_inventory().await.is_err());
    assert!(boss_content::rebuild_content(&db.pool).await.is_err());
    assert_eq!(rows(&db.pool).await, before);
}

#[tokio::test(flavor = "multi_thread")]
async fn later_fact_delivered_before_baseline_replays_after_observed_cutoff() {
    use boss_content::{
        ContentRepository,
        manual_baseline::{BaselineAdmission, ManualBaselineRepository},
    };
    let db = TestDb::new().await;
    legacy(&db.pool).await;
    let repo = boss_content::PgContent::new(db.pool.clone());
    let admission = BaselineAdmission {
        admission_id: uuid::Uuid::new_v4(),
        expected_sha256: repo.manual_inventory().await.unwrap().sha256,
    };
    let stamp = boss_core::publisher::EventStamp::new("content", "emp-observer".parse().unwrap());
    repo.observe_manual_baseline(&admission, &stamp)
        .await
        .unwrap();
    let patch =
        serde_json::from_value(json!({"body":"Genuine later body","reason":"Real later edit"}))
            .unwrap();
    let later = boss_core::publisher::EventStamp::new("content", "emp-later".parse().unwrap());
    repo.update_section_at("legacy", patch, &later)
        .await
        .unwrap();
    // Reproduce two relay transactions committing the later fact first.
    sqlx::query("INSERT INTO audit_log (event_id,timestamp,source,kind,payload) SELECT event_id,timestamp,source,kind,payload FROM event_outbox WHERE kind='content.manual.section_updated'").execute(&db.pool).await.unwrap();
    drain(&db.pool).await;
    let before = rows(&db.pool).await;
    for _ in 0..2 {
        let report = boss_content::rebuild_content(&db.pool).await.unwrap();
        assert_eq!(report.events_processed, 2);
        assert_eq!(rows(&db.pool).await, before);
    }
}

#[tokio::test]
async fn source_registry_probe_reports_existing_manual_vocabulary_coverage() {
    let db = TestDb::new().await;
    for kind in [
        boss_content::events::SECTION_CREATED,
        boss_content::events::SECTION_UPDATED,
        boss_content::events::BASELINE_OBSERVED,
        "content.bulletin.created",
    ] {
        let covered:bool=sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM event_kinds k WHERE $1=k.kind_pattern OR (k.kind_pattern LIKE '%.*' AND $1 LIKE replace(k.kind_pattern,'.*','.%')))")
            .bind(kind).fetch_one(&db.pool).await.unwrap();
        println!("source-event-registry {kind} covered={covered}");
        assert!(
            covered,
            "c58245f6: emitted manual kind {kind} has no exact or family source declaration"
        );
        let exact: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM event_kinds WHERE kind_pattern=$1 AND source='content' AND suffix_domain IS NULL)")
            .bind(kind).fetch_one(&db.pool).await.unwrap();
        assert!(exact, "c58245f6: {kind} needs its exact source declaration");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn prior_genuine_facts_are_counted_once_as_checkpoint_covered() {
    use boss_content::{
        ContentRepository,
        manual_baseline::{BaselineAdmission, ManualBaselineRepository},
    };
    let db = TestDb::new().await;
    legacy(&db.pool).await;
    let repo = boss_content::PgContent::new(db.pool.clone());
    let stamp = boss_core::publisher::EventStamp::new("content", "emp-author".parse().unwrap());
    let draft=serde_json::from_value(json!({"slug":"real","title":"Real","body":"Real body","sort_order":2,"audience":{},"published":false})).unwrap();
    repo.create_section_at(draft, &stamp).await.unwrap();
    drain(&db.pool).await;
    let admission = BaselineAdmission {
        admission_id: uuid::Uuid::new_v4(),
        expected_sha256: repo.manual_inventory().await.unwrap().sha256,
    };
    let observer =
        boss_core::publisher::EventStamp::new("content", "emp-observer".parse().unwrap());
    repo.observe_manual_baseline(&admission, &observer)
        .await
        .unwrap();
    drain(&db.pool).await;
    let before = rows(&db.pool).await;
    for _ in 0..2 {
        let report = boss_content::rebuild_content(&db.pool).await.unwrap();
        assert_eq!(report.events_processed, 2);
        assert_eq!(report.manual_events_covered, 1);
        assert_eq!(rows(&db.pool).await, before);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn outbox_failure_rolls_back_admission_and_preserves_raw_inventory() {
    use boss_content::manual_baseline::{BaselineAdmission, ManualBaselineRepository};
    let db = TestDb::new().await;
    legacy(&db.pool).await;
    let repo = boss_content::PgContent::new(db.pool.clone());
    let admission = BaselineAdmission {
        admission_id: uuid::Uuid::new_v4(),
        expected_sha256: repo.manual_inventory().await.unwrap().sha256,
    };
    let before = rows(&db.pool).await;
    sqlx::query("ALTER TABLE event_outbox ADD CONSTRAINT fixture_refusal CHECK (kind <> 'content.manual.baseline_observed')").execute(&db.pool).await.unwrap();
    let stamp = boss_core::publisher::EventStamp::new("content", "emp-observer".parse().unwrap());
    assert!(
        repo.observe_manual_baseline(&admission, &stamp)
            .await
            .is_err()
    );
    assert_eq!(rows(&db.pool).await, before);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn a_baseline_cannot_hide_a_malformed_previously_recorded_fact() {
    use boss_content::manual_baseline::{BaselineAdmission, ManualBaselineRepository};
    let db = TestDb::new().await;
    legacy(&db.pool).await;
    let repo = boss_content::PgContent::new(db.pool.clone());
    let inventory = repo.manual_inventory().await.unwrap();
    let mut section = inventory.sections[0].clone();
    section["title"] = json!("Forged state");
    let mut version = inventory.history[0].clone();
    version.as_object_mut().unwrap().remove("id");
    sqlx::query("INSERT INTO audit_log (event_id,timestamp,source,kind,payload) VALUES ($1,'2010-01-01','content','content.manual.section_created',$2)")
        .bind(uuid::Uuid::new_v4()).bind(json!({"section":section,"version":version,"history_id":42,"_actor":"emp-historical"})).execute(&db.pool).await.unwrap();
    let admission = BaselineAdmission {
        admission_id: uuid::Uuid::new_v4(),
        expected_sha256: inventory.sha256,
    };
    let stamp = boss_core::publisher::EventStamp::new("content", "emp-observer".parse().unwrap());
    repo.observe_manual_baseline(&admission, &stamp)
        .await
        .unwrap();
    drain(&db.pool).await;
    let before = rows(&db.pool).await;
    assert!(boss_content::rebuild_content(&db.pool).await.is_err());
    assert_eq!(rows(&db.pool).await, before);
}

#[tokio::test(flavor = "multi_thread")]
async fn inventory_waits_for_concurrent_rows_to_commit_as_one_complete_state() {
    use boss_content::manual_baseline::ManualBaselineRepository;
    let db = TestDb::new().await;
    legacy(&db.pool).await;
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE manual_sections, manual_section_history IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE manual_sections SET title='Committed together'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE manual_section_history SET title='Committed together'")
        .execute(&mut *tx)
        .await
        .unwrap();
    let repo = boss_content::PgContent::new(db.pool.clone());
    let mut reader = Box::pin(repo.manual_inventory());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut reader)
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
    let inventory = reader.await.unwrap();
    assert_eq!(inventory.sections[0]["title"], "Committed together");
    assert_eq!(inventory.history[0]["title"], "Committed together");
    inventory.validate().unwrap();
}

#[tokio::test]
async fn admission_key_aliasing_an_audit_id_uses_an_independent_stamped_fact() {
    use boss_content::manual_baseline::{BaselineAdmission, ManualBaselineRepository};
    let db = TestDb::new().await;
    legacy(&db.pool).await;
    let repo = boss_content::PgContent::new(db.pool.clone());
    let admission = BaselineAdmission {
        admission_id: uuid::Uuid::new_v4(),
        expected_sha256: repo.manual_inventory().await.unwrap().sha256,
    };
    sqlx::query("INSERT INTO audit_log (event_id,timestamp,source,kind,payload) VALUES ($1,now(),'fixture','test.unrelated',$2)")
        .bind(admission.admission_id).bind(json!({"original":"must stay one fact"}))
        .execute(&db.pool).await.unwrap();
    let stamp = boss_core::publisher::EventStamp::new("content", "emp-observer".parse().unwrap());
    let result = repo.observe_manual_baseline(&admission, &stamp).await;
    let accepted = result.expect("the retry key is independent of an unrelated fact UUID");
    assert_ne!(accepted.event_id, admission.admission_id);
    assert_eq!(accepted.event_source, "content");
    assert_eq!(
        repo.observe_manual_baseline(&admission, &stamp)
            .await
            .unwrap(),
        accepted
    );
    let bus = boss_testing::RecordingEventBus::new();
    let relay = boss_events::outbox::drain_outbox_once(
        &db.pool,
        &(bus.clone() as std::sync::Arc<dyn boss_core::port::EventBus>),
        100,
    )
    .await;
    let recorded: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE event_id=$1 AND kind='content.manual.baseline_observed'")
        .bind(accepted.event_id).fetch_one(&db.pool).await.unwrap();
    let existing: Value = sqlx::query_scalar("SELECT payload FROM audit_log WHERE event_id=$1")
        .bind(admission.admission_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let outbox: Vec<(i64, String, bool, bool)> = sqlx::query_as("SELECT id,kind,delivered_at IS NOT NULL,dead_lettered_at IS NOT NULL FROM event_outbox ORDER BY id")
        .fetch_all(&db.pool).await.unwrap();
    let originals: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE event_id=$1 AND kind='test.unrelated'",
    )
    .bind(admission.admission_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    println!(
        "accepted={} relay={:?} baseline_audit_rows={} original_audit_rows={} original={} outbox={:?}",
        accepted.admission_id, relay, recorded, originals, existing, outbox
    );
    assert!(relay.is_ok());
    println!(
        "bus events={:?}",
        bus.events()
            .iter()
            .map(|e| (&e.kind, e.id))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        recorded, 1,
        "the separately stamped fact must be audited exactly once"
    );
    assert_eq!(originals, 1);
    assert_eq!(existing, json!({"original":"must stay one fact"}));
    let published = bus.events();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].id, accepted.event_id);
    assert_eq!(published[0].payload["event_id"], json!(accepted.event_id));
    assert_eq!(
        repo.observe_manual_baseline(&admission, &stamp)
            .await
            .unwrap(),
        accepted
    );
    // The stable witness survives pruning of the delivered staging copy.
    sqlx::query("DELETE FROM event_outbox WHERE event_id=$1")
        .bind(accepted.event_id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        repo.observe_manual_baseline(&admission, &stamp)
            .await
            .unwrap(),
        accepted
    );
    let replay_before = rows(&db.pool).await;
    boss_content::rebuild_content(&db.pool).await.unwrap();
    assert_eq!(rows(&db.pool).await, replay_before);
}

#[tokio::test]
async fn admission_key_aliasing_a_staged_fact_preserves_both_facts_and_retries() {
    use boss_content::manual_baseline::{BaselineAdmission, ManualBaselineRepository};
    let db = TestDb::new().await;
    legacy(&db.pool).await;
    let repo = boss_content::PgContent::new(db.pool.clone());
    let admission = BaselineAdmission {
        admission_id: uuid::Uuid::new_v4(),
        expected_sha256: repo.manual_inventory().await.unwrap().sha256,
    };
    let fixture = boss_core::publisher::EventStamp::new("fixture", "emp-fixture".parse().unwrap());
    let mut original = fixture.event(
        "test.unrelated",
        json!({"original":"staging identity retained"}),
    );
    original.id = admission.admission_id;
    let mut tx = db.pool.begin().await.unwrap();
    boss_events::outbox::record_event_in_tx(&mut tx, &original)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let bus = boss_testing::RecordingEventBus::new();
    let port = bus.clone() as std::sync::Arc<dyn boss_core::port::EventBus>;
    boss_events::outbox::drain_outbox_once(&db.pool, &port, 100)
        .await
        .unwrap();
    let stamp = boss_core::publisher::EventStamp::new("content", "emp-observer".parse().unwrap());
    let accepted = repo
        .observe_manual_baseline(&admission, &stamp)
        .await
        .unwrap();
    assert_ne!(accepted.event_id, admission.admission_id);
    let relay = boss_events::outbox::drain_outbox_once(&db.pool, &port, 100)
        .await
        .unwrap();
    assert_eq!(relay.delivered, 1);
    assert_eq!(relay.audit_inserted, 1);
    assert_eq!(
        repo.observe_manual_baseline(&admission, &stamp)
            .await
            .unwrap(),
        accepted
    );
    let wrong_hash = BaselineAdmission {
        expected_sha256: "forged".into(),
        ..admission.clone()
    };
    assert!(
        repo.observe_manual_baseline(&wrong_hash, &stamp)
            .await
            .is_err()
    );
    let forged_actor =
        boss_core::publisher::EventStamp::new("content", "emp-forged".parse().unwrap());
    assert!(
        repo.observe_manual_baseline(&admission, &forged_actor)
            .await
            .is_err()
    );
    let facts: Vec<(uuid::Uuid, String, Value)> =
        sqlx::query_as("SELECT event_id,kind,payload FROM audit_log ORDER BY id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(facts.len(), 2);
    assert_eq!(
        facts[0],
        (original.id, original.kind.clone(), original.payload.clone())
    );
    assert_eq!(facts[1].0, accepted.event_id);
    assert_eq!(facts[1].2["admission_id"], json!(admission.admission_id));
    let published = bus.events();
    assert_eq!(published.len(), 2);
    assert_ne!(published[0].id, published[1].id);
    let before = rows(&db.pool).await;
    for _ in 0..2 {
        boss_content::rebuild_content(&db.pool).await.unwrap();
        assert_eq!(rows(&db.pool).await, before);
    }
}

#[tokio::test]
async fn retries_refuse_conflicting_full_durable_copies_of_the_witness() {
    use boss_content::manual_baseline::{BaselineAdmission, ManualBaselineRepository};
    let db = TestDb::new().await;
    legacy(&db.pool).await;
    let repo = boss_content::PgContent::new(db.pool.clone());
    let admission = BaselineAdmission {
        admission_id: uuid::Uuid::new_v4(),
        expected_sha256: repo.manual_inventory().await.unwrap().sha256,
    };
    let stamp = boss_core::publisher::EventStamp::new("content", "emp-observer".parse().unwrap());
    let accepted = repo
        .observe_manual_baseline(&admission, &stamp)
        .await
        .unwrap();
    drain(&db.pool).await;
    sqlx::query("UPDATE event_outbox SET payload=payload || '{\"unexpected_evidence\":\"forged\"}'::jsonb WHERE event_id=$1").bind(accepted.event_id).execute(&db.pool).await.unwrap();
    assert!(
        repo.observe_manual_baseline(&admission, &stamp)
            .await
            .is_err(),
        "typed fields alone cannot excuse a different full fact"
    );
    let before = rows(&db.pool).await;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(rows(&db.pool).await, before);
}

#[tokio::test]
async fn retry_rejects_an_unrelated_audit_row_at_its_stamped_witness_id() {
    use boss_content::manual_baseline::{BaselineAdmission, ManualBaselineRepository};
    let db = TestDb::new().await;
    legacy(&db.pool).await;
    let repo = boss_content::PgContent::new(db.pool.clone());
    let admission = BaselineAdmission {
        admission_id: uuid::Uuid::new_v4(),
        expected_sha256: repo.manual_inventory().await.unwrap().sha256,
    };
    let stamp = boss_core::publisher::EventStamp::new("content", "emp-observer".parse().unwrap());
    let accepted = repo
        .observe_manual_baseline(&admission, &stamp)
        .await
        .unwrap();
    sqlx::query("INSERT INTO audit_log (event_id,timestamp,source,kind,payload) VALUES ($1,now(),'fixture','test.unrelated',$2)").bind(accepted.event_id).bind(json!({"conflict":"must not be mistaken for the baseline"})).execute(&db.pool).await.unwrap();
    assert!(
        repo.observe_manual_baseline(&admission, &stamp)
            .await
            .is_err()
    );
    let pending: bool =
        sqlx::query_scalar("SELECT delivered_at IS NULL FROM event_outbox WHERE event_id=$1")
            .bind(accepted.event_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(pending);
}

#[tokio::test]
async fn replay_refuses_a_forged_identity_or_source_witness_before_wipe() {
    use boss_content::manual_baseline::{BaselineAdmission, ManualBaselineRepository};
    for field in ["event_id", "event_source"] {
        let db = TestDb::new().await;
        legacy(&db.pool).await;
        let repo = boss_content::PgContent::new(db.pool.clone());
        let admission = BaselineAdmission {
            admission_id: uuid::Uuid::new_v4(),
            expected_sha256: repo.manual_inventory().await.unwrap().sha256,
        };
        let stamp =
            boss_core::publisher::EventStamp::new("content", "emp-observer".parse().unwrap());
        let accepted = repo
            .observe_manual_baseline(&admission, &stamp)
            .await
            .unwrap();
        let forged = if field == "event_id" {
            json!(admission.admission_id)
        } else {
            json!("forged-source")
        };
        sqlx::query(
            "UPDATE event_outbox SET payload=jsonb_set(payload,ARRAY[$2],$3) WHERE event_id=$1",
        )
        .bind(accepted.event_id)
        .bind(field)
        .bind(forged)
        .execute(&db.pool)
        .await
        .unwrap();
        assert!(
            repo.observe_manual_baseline(&admission, &stamp)
                .await
                .is_err()
        );
        drain(&db.pool).await;
        let before = rows(&db.pool).await;
        assert!(boss_content::rebuild_content(&db.pool).await.is_err());
        assert_eq!(rows(&db.pool).await, before);
    }
}

#[tokio::test]
async fn seventeen_hidden_legacy_sections_keep_raw_hierarchy_history_and_sequence() {
    use boss_content::manual_baseline::{BaselineAdmission, ManualBaselineRepository};
    let db = TestDb::new().await;
    for number in 0..17 {
        let id = uuid::Uuid::new_v4();
        let slug = format!("legacy-{number}");
        let parent = (number > 0).then(|| format!("legacy-{}", number - 1));
        sqlx::query("INSERT INTO manual_sections (id,slug,parent_slug,title,body,sort_order,published,audience,created_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,'{\"roles\":[\"private\"]}','2010-01-01','2010-01-01')")
            .bind(id).bind(&slug).bind(parent).bind(format!("Historical {number}")).bind(format!("Stored claim {number}")).bind(number).bind(number%2==0).execute(&db.pool).await.unwrap();
        sqlx::query("INSERT INTO manual_section_history (id,section_id,version,title,body,audience,edited_by,edited_at,reason) VALUES ($1,$2,1,$3,$4,'{\"roles\":[\"private\"]}','emp-historical','2010-01-01',$5)")
            .bind(42_i64+i64::from(number)*3).bind(id).bind(format!("Historical {number}")).bind(format!("Stored claim {number}")).bind(format!("Stored reason {number}")).execute(&db.pool).await.unwrap();
    }
    sqlx::query("SELECT setval(pg_get_serial_sequence('manual_section_history','id'),1001,true)")
        .execute(&db.pool)
        .await
        .unwrap();
    let before = rows(&db.pool).await;
    let repo = boss_content::PgContent::new(db.pool.clone());
    let inventory = repo.manual_inventory().await.unwrap();
    assert_eq!(inventory.section_count, 17);
    assert_eq!(inventory.history_count, 17);
    let admission = BaselineAdmission {
        admission_id: uuid::Uuid::new_v4(),
        expected_sha256: inventory.sha256,
    };
    let stamp = boss_core::publisher::EventStamp::new("content", "emp-observer".parse().unwrap());
    let baseline = repo
        .observe_manual_baseline(&admission, &stamp)
        .await
        .unwrap();
    assert_ne!(baseline.event_id, admission.admission_id);
    drain(&db.pool).await;
    for _ in 0..2 {
        boss_content::rebuild_content(&db.pool).await.unwrap();
        assert_eq!(rows(&db.pool).await, before);
    }
    let kinds: Vec<String> =
        sqlx::query_scalar("SELECT kind FROM audit_log WHERE kind LIKE 'content.manual.%'")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(kinds, vec![boss_content::events::BASELINE_OBSERVED]);
}
