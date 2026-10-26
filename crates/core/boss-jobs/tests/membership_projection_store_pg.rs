#![cfg(feature = "postgres")]
//! Durable generation commits are one atomic compare-and-swap, including
//! the checkpoint and its original source watermark (c7870939).
use boss_clock_client::{
    ClockMode,
    mapping_history::{ClockMappingFact, ClockTimeline},
};
use boss_jobs::{
    membership_projection::{MembershipProjectionRepository, ProjectionIdentity},
    membership_projection_pg::PgMembershipProjection,
    membership_replay::{InitialState, ReplayInput, projection::MembershipProjection},
    port::JobScope,
};
use boss_testing::TestDb;
use chrono::{DateTime, TimeZone, Utc};

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + seconds, 0).unwrap()
}

fn input(until: i64, cutoff: i64) -> ReplayInput {
    let clock = ClockMappingFact {
        schema_version: 1,
        revision: "original-clock".into(),
        previous_revision: None,
        epoch: Some("original-epoch".into()),
        mode: Some(ClockMode::Wall),
        params: None,
        captured_at: at(-100),
        audit_created_at: at(-99),
        seq: 1,
        complete_group: true,
    };
    ReplayInput {
        groups: vec![],
        since: at(0),
        until: at(until),
        audit_cutoff: cutoff,
        actor: Some("actual+caller@fixture.invalid".into()),
        scope: JobScope::All,
        currently_authorized: None,
        clock: ClockTimeline::build(&[clock], Some("original-clock"), at(-100), at(until)),
        registry_prefix_complete: true,
        packet_prefix_complete: true,
        source_unavailable: vec![],
        initial: InitialState::default(),
    }
}

#[tokio::test]
async fn only_the_expected_generation_can_atomically_advance_its_checkpoint() {
    let db = TestDb::new().await;
    let store = PgMembershipProjection::new(db.pool.clone());
    let identity =
        ProjectionIdentity::new(Some("actual+caller@fixture.invalid".into()), JobScope::All);
    assert!(store.load(&identity).await.unwrap().is_none());
    let first = MembershipProjection::rebuild(&input(20, 1)).unwrap();
    assert_eq!(
        store.commit(&identity, None, &first).await.unwrap(),
        Some(1)
    );
    let original = store.load(&identity).await.unwrap().unwrap();
    assert_eq!(original.revision, 1);
    assert_eq!(
        serde_json::to_value(&original.checkpoint).unwrap(),
        serde_json::to_value(&first).unwrap()
    );

    let second = MembershipProjection::rebuild(&input(40, 2)).unwrap();
    assert_eq!(
        store
            .commit(&identity, Some(&original), &second)
            .await
            .unwrap(),
        Some(2)
    );
    assert_eq!(
        store
            .commit(&identity, Some(&original), &first)
            .await
            .unwrap(),
        None
    );
    assert_eq!(store.commit(&identity, None, &first).await.unwrap(), None);
    let observed = store.load(&identity).await.unwrap().unwrap();
    assert_eq!(observed.revision, 2);
    assert_eq!(
        serde_json::to_value(&observed.checkpoint).unwrap(),
        serde_json::to_value(&second).unwrap()
    );

    let other = ProjectionIdentity::new(
        Some("different-spelling@fixture.invalid".into()),
        JobScope::All,
    );
    assert!(
        store.load(&other).await.unwrap().is_none(),
        "scope alone never shares caller-bound facts"
    );
}

#[tokio::test]
async fn an_expected_revision_cannot_move_its_native_watermark_backwards() {
    let db = TestDb::new().await;
    let store = PgMembershipProjection::new(db.pool.clone());
    let identity =
        ProjectionIdentity::new(Some("actual+caller@fixture.invalid".into()), JobScope::All);
    let first = MembershipProjection::rebuild(&input(40, 2)).unwrap();
    store.commit(&identity, None, &first).await.unwrap();
    let original = store.load(&identity).await.unwrap().unwrap();
    let backwards = MembershipProjection::rebuild(&input(20, 1)).unwrap();
    assert!(
        store
            .commit(&identity, Some(&original), &backwards)
            .await
            .is_err(),
        "revision equality does not authorize a backwards original cut"
    );
    assert_eq!(
        serde_json::to_value(store.load(&identity).await.unwrap().unwrap().checkpoint).unwrap(),
        serde_json::to_value(first).unwrap()
    );
}

#[tokio::test]
async fn a_previous_checkpoint_from_another_identity_cannot_prepare_a_row_delta() {
    let db = TestDb::new().await;
    let store = PgMembershipProjection::new(db.pool.clone());
    let alice =
        ProjectionIdentity::new(Some("actual+caller@fixture.invalid".into()), JobScope::All);
    let bob = ProjectionIdentity::new(Some("bob".into()), JobScope::All);
    let first = MembershipProjection::rebuild(&input(20, 1)).unwrap();
    store.commit(&alice, None, &first).await.unwrap();
    let previous = store.load(&alice).await.unwrap().unwrap();
    let mut other = input(20, 1);
    other.actor = Some("bob".into());
    let other = MembershipProjection::rebuild(&other).unwrap();
    store.commit(&bob, None, &other).await.unwrap();
    assert!(
        store.commit(&bob, Some(&previous), &other).await.is_err(),
        "same numeric revision is not the same checkpoint identity"
    );
}

#[tokio::test]
async fn retrying_the_same_prepared_generation_does_not_invent_another_revision() {
    let db = TestDb::new().await;
    let store = PgMembershipProjection::new(db.pool.clone());
    let identity =
        ProjectionIdentity::new(Some("actual+caller@fixture.invalid".into()), JobScope::All);
    let first = MembershipProjection::rebuild(&input(20, 1)).unwrap();
    store.commit(&identity, None, &first).await.unwrap();
    let original = store.load(&identity).await.unwrap().unwrap();
    assert_eq!(
        store
            .commit(&identity, Some(&original), &first)
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(store.load(&identity).await.unwrap().unwrap().revision, 1);
}

#[tokio::test]
async fn viewer_reads_compact_facts_without_loading_packet_payloads_and_checks_current_census() {
    use boss_jobs::membership_replay::projection::ProjectionBinding;
    let db = TestDb::new().await;
    let store = PgMembershipProjection::new(db.pool.clone());
    let original = input(20, 1);
    let identity = ProjectionIdentity::new(original.actor.clone(), JobScope::All);
    let first = MembershipProjection::rebuild(&original).unwrap();
    store.commit(&identity, None, &first).await.unwrap();
    sqlx::query("INSERT INTO membership_projection_rows(identity,bucket,row_key,payload) SELECT identity,'packet','invalid-packet','{\"not\":\"a typed packet\"}'::jsonb FROM membership_projection_generations")
        .execute(&db.pool).await.unwrap();
    assert!(
        store.load(&identity).await.is_err(),
        "control: a whole checkpoint load parses packet state"
    );
    let binding = ProjectionBinding::from_input(&original);
    let compact = store
        .read(&identity, at(0), &binding)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(compact.revision, 1);
    assert_eq!(
        serde_json::to_value(compact.reading).unwrap(),
        serde_json::to_value(boss_jobs::membership_replay::replay(&original)).unwrap()
    );
    let changed = ProjectionBinding {
        currently_authorized: Some(Default::default()),
        ..binding
    };
    assert!(
        store.read(&identity, at(0), &changed).await.is_err(),
        "a cached generation cannot bypass the current authorization census"
    );
}

#[tokio::test]
async fn caller_churn_keeps_only_the_sixteen_latest_committed_generations() {
    let db = TestDb::new().await;
    let store = PgMembershipProjection::new(db.pool.clone());
    let mut identities = vec![];
    for index in 0..17 {
        let mut original = input(20, 1);
        original.actor = Some(format!("actual-caller-{index}@fixture.invalid"));
        let identity = ProjectionIdentity::new(original.actor.clone(), JobScope::All);
        let checkpoint = MembershipProjection::rebuild(&original).unwrap();
        store.commit(&identity, None, &checkpoint).await.unwrap();
        identities.push(identity);
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM membership_projection_generations")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        count, 16,
        "caller churn must not accumulate unbounded durable derived state"
    );
    assert!(store.load(&identities[0]).await.unwrap().is_none());
    assert!(store.load(&identities[16]).await.unwrap().is_some());
}

#[tokio::test]
async fn a_failed_header_write_rolls_back_every_preceding_derived_row() {
    use boss_core::event::Event;
    use boss_events::original_group::{GroupReference, OriginalGroup};
    use boss_jobs::{membership_replay::ReplayGroup, move_diagnosis::HistoricalEvent};
    let db = TestDb::new().await;
    let store = PgMembershipProjection::new(db.pool.clone());
    let identity =
        ProjectionIdentity::new(Some("actual+caller@fixture.invalid".into()), JobScope::All);
    let first = MembershipProjection::rebuild(&input(20, 1)).unwrap();
    store.commit(&identity, None, &first).await.unwrap();
    let previous = store.load(&identity).await.unwrap().unwrap();
    let event = Event::new(
        "projection-fixture",
        "fixture.original",
        serde_json::json!({}),
        at(25),
    );
    let ids = vec![event.id];
    let mut changed = input(40, 2);
    changed.groups.push(ReplayGroup {
        original: Some(OriginalGroup {
            reference: GroupReference {
                id: uuid::Uuid::new_v4(),
                schema_version: 1,
                event_count: 1,
                member_hash: OriginalGroup::roster_hash(&ids),
            },
            event_ids: ids,
            audit_sequences: vec![Some(2)],
            complete: true,
        }),
        rows: vec![HistoricalEvent {
            seq: 2,
            created_at: Some(at(25)),
            event,
        }],
    });
    let next = MembershipProjection::rebuild(&changed).unwrap();
    sqlx::raw_sql("CREATE FUNCTION refuse_generation_header() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture header write refused'; END $$; CREATE TRIGGER refuse_generation_header BEFORE UPDATE ON membership_projection_generations FOR EACH ROW EXECUTE FUNCTION refuse_generation_header();")
        .execute(&db.pool).await.unwrap();
    let why = store
        .commit(&identity, Some(&previous), &next)
        .await
        .unwrap_err();
    assert!(why.to_string().contains("fixture header write refused"));
    let observed = store.load(&identity).await.unwrap().unwrap();
    assert_eq!(observed.revision, 1);
    assert_eq!(
        serde_json::to_value(observed.checkpoint).unwrap(),
        serde_json::to_value(first).unwrap()
    );
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM membership_projection_rows")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        rows, 0,
        "failed generation cannot leave new witness/fact rows behind"
    );
}

// One contract, run through both adapters; the separate Pg trigger test above
// additionally proves transaction rollback at the physical persistence edge.
boss_testing::adapters_agree! {
    adapters {
        in_memory => { (boss_jobs::membership_projection::InMemoryMembershipProjection::new(), ()) },
        postgres => {
            let db = TestDb::new().await;
            (PgMembershipProjection::new(db.pool.clone()), db)
        },
    }
    cases {
        the_port_preserves_original_nonempty_facts_and_bound_reads,
        the_port_reads_both_canonical_windows_from_one_generation,
        the_port_has_the_same_cas_retry_and_refusal_contract,
        the_port_retains_sixteen_committed_identities,
        the_port_refuses_cancelled_or_expired_publication_without_installing_it,
        the_port_rejects_an_evicted_incarnation_with_older_cuts,
        the_port_rejects_an_evicted_delta_basis_even_with_nondecreasing_cuts,
    }
}

async fn evicted_incarnation<R: MembershipProjectionRepository>(
    store: &R,
    adapter: &str,
    nondecreasing: bool,
    legacy_pool: Option<&sqlx::PgPool>,
) {
    let original = input(20, 1);
    let identity = ProjectionIdentity::new(original.actor.clone(), original.scope.clone());
    store
        .commit(
            &identity,
            None,
            &MembershipProjection::rebuild(&original).unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    let obsolete = store.load(&identity).await.unwrap().unwrap();
    for index in 0..16 {
        let mut source = original.clone();
        source.actor = Some(format!("evict-{index}"));
        let other = ProjectionIdentity::new(source.actor.clone(), source.scope.clone());
        store
            .commit(
                &other,
                None,
                &MembershipProjection::rebuild(&source).unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
    }
    assert!(
        store.load(&identity).await.unwrap().is_none(),
        "{adapter}: actually evicted"
    );
    let source = nonempty_input();
    let recreated = MembershipProjection::rebuild(&source).unwrap();
    assert!(
        !recreated.facts().is_empty(),
        "new incarnation carries newer-only rows"
    );
    store
        .commit(&identity, None, &recreated)
        .await
        .unwrap()
        .unwrap();
    let current = store.load(&identity).await.unwrap().unwrap();
    if let Some(pool) = legacy_pool {
        sqlx::query("UPDATE membership_projection_generations SET basis_digest=NULL")
            .execute(pool)
            .await
            .unwrap();
    }
    assert_eq!(
        current.revision, obsolete.revision,
        "control: numeric revision was reused"
    );
    let before = serde_json::to_vec(&current.checkpoint).unwrap();
    let binding = recreated.binding();
    let windows = store
        .read_windows(&identity, 24, source.until, binding)
        .await
        .unwrap()
        .unwrap();
    let before_windows = serde_json::to_value((&windows.this, &windows.both)).unwrap();
    let candidates = if nondecreasing {
        vec![(172_800, 100), (172_801, 101)]
    } else {
        vec![(30, 2)]
    };
    for (until, cutoff) in candidates {
        let stale = MembershipProjection::rebuild(&input(until, cutoff)).unwrap();
        assert_eq!(
            store
                .commit(&identity, Some(&obsolete), &stale)
                .await
                .unwrap(),
            None,
            "{adapter}: stale incarnation must lose even if its candidate cut is newer"
        );
        let retained = store.load(&identity).await.unwrap().unwrap();
        assert_eq!(retained.revision, current.revision, "{adapter}");
        assert_eq!(
            serde_json::to_vec(&retained.checkpoint).unwrap(),
            before,
            "{adapter}: exact header and every row retained"
        );
        let windows = store
            .read_windows(&identity, 24, source.until, binding)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value((&windows.this, &windows.both)).unwrap(),
            before_windows,
            "{adapter}: canonical windows retained"
        );
    }
    let mut later = source;
    later.until += chrono::Duration::seconds(1);
    later.audit_cutoff += 1;
    later.clock.known.last_mut().unwrap().until = later.until;
    let next = MembershipProjection::rebuild(&later).unwrap();
    assert_eq!(
        store
            .commit(&identity, Some(&current), &next)
            .await
            .unwrap(),
        Some(2),
        "{adapter}: fresh basis advances"
    );
    assert_eq!(
        serde_json::to_vec(&store.load(&identity).await.unwrap().unwrap().checkpoint).unwrap(),
        serde_json::to_vec(&next).unwrap()
    );
}

async fn the_port_rejects_an_evicted_incarnation_with_older_cuts<
    R: MembershipProjectionRepository,
>(
    store: &R,
    adapter: &str,
) {
    evicted_incarnation(store, adapter, false, None).await;
}

async fn the_port_rejects_an_evicted_delta_basis_even_with_nondecreasing_cuts<
    R: MembershipProjectionRepository,
>(
    store: &R,
    adapter: &str,
) {
    evicted_incarnation(store, adapter, true, None).await;
}

#[tokio::test]
async fn a_legacy_generation_without_a_digest_requires_its_actual_entire_basis() {
    let db = TestDb::new().await;
    evicted_incarnation(
        &PgMembershipProjection::new(db.pool.clone()),
        "legacy postgres",
        true,
        Some(&db.pool),
    )
    .await;
    let missing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM membership_projection_generations WHERE basis_digest IS NULL",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        missing, 15,
        "fresh valid publication upgrades exactly its own legacy basis"
    );
}

#[tokio::test]
async fn an_older_writer_cannot_leave_a_valid_digest_for_a_changed_basis() {
    let db = TestDb::new().await;
    let store = PgMembershipProjection::new(db.pool.clone());
    let source = nonempty_input();
    let identity = ProjectionIdentity::new(source.actor.clone(), source.scope.clone());
    let first = MembershipProjection::rebuild(&source).unwrap();
    store.commit(&identity, None, &first).await.unwrap();
    for operation in ["insert", "update", "delete", "header"] {
        let held = store.load(&identity).await.unwrap().unwrap();
        if operation == "header" {
            sqlx::query("UPDATE membership_projection_generations SET header=jsonb_set(header,'{since}',to_jsonb(to_char((header->>'since')::timestamptz-interval '1 second','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')))")
                .execute(&db.pool).await.unwrap();
        } else if operation == "insert" {
            let source_id = uuid::Uuid::new_v4().to_string();
            sqlx::query("INSERT INTO membership_projection_rows(identity,bucket,row_key,payload) SELECT identity,'seen',$1,to_jsonb($1::text) FROM membership_projection_generations")
                .bind(source_id).execute(&db.pool).await.unwrap();
        } else if operation == "update" {
            let source_id = uuid::Uuid::new_v4().to_string();
            sqlx::query("UPDATE membership_projection_rows SET row_key=$1,payload=to_jsonb($1::text) WHERE (identity,bucket,row_key) IN (SELECT identity,bucket,row_key FROM membership_projection_rows WHERE bucket='seen' ORDER BY row_key LIMIT 1)")
                .bind(source_id).execute(&db.pool).await.unwrap();
        } else {
            sqlx::query("DELETE FROM membership_projection_rows WHERE (identity,bucket,row_key) IN (SELECT identity,bucket,row_key FROM membership_projection_rows WHERE bucket='seen' ORDER BY row_key LIMIT 1)")
                .execute(&db.pool).await.unwrap();
        }
        let actual = store.load(&identity).await.unwrap().unwrap();
        assert_eq!(actual.checkpoint.until(), held.checkpoint.until());
        assert_eq!(
            actual.checkpoint.audit_cutoff(),
            held.checkpoint.audit_cutoff()
        );
        assert_eq!(actual.revision, held.revision);
        assert_ne!(
            serde_json::to_value(&actual.checkpoint).unwrap(),
            serde_json::to_value(&held.checkpoint).unwrap()
        );
        assert_eq!(
            store
                .commit(&identity, Some(&held), &held.checkpoint)
                .await
                .unwrap(),
            None,
            "older writer changed basis without changing revision or supplying a digest"
        );
        assert_eq!(
            serde_json::to_value(&store.load(&identity).await.unwrap().unwrap().checkpoint)
                .unwrap(),
            serde_json::to_value(&actual.checkpoint).unwrap()
        );
        assert_eq!(
            store
                .commit(&identity, Some(&actual), &actual.checkpoint)
                .await
                .unwrap(),
            Some(actual.revision),
            "fresh exact legacy retry stays idempotent and restores its witness"
        );
        let digest: Option<String> =
            sqlx::query_scalar("SELECT basis_digest FROM membership_projection_generations")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(digest.unwrap().len(), 64);
    }
}

#[tokio::test]
async fn row_basis_invalidation_is_statement_bounded_and_identity_scoped() {
    let db = TestDb::new().await;
    let store = PgMembershipProjection::new(db.pool.clone());
    for actor in ["target", "unrelated"] {
        let mut source = input(20, 1);
        source.actor = Some(actor.into());
        store
            .commit(
                &ProjectionIdentity::new(source.actor.clone(), source.scope.clone()),
                None,
                &MembershipProjection::rebuild(&source).unwrap(),
            )
            .await
            .unwrap();
    }
    let target: String = sqlx::query_scalar("SELECT identity FROM membership_projection_generations WHERE header->'binding'->>'actor'='target'")
        .fetch_one(&db.pool).await.unwrap();
    sqlx::raw_sql("CREATE SEQUENCE invalidation_writes; CREATE FUNCTION count_basis_invalidation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF OLD.basis_digest IS NOT NULL AND NEW.basis_digest IS NULL THEN PERFORM nextval('invalidation_writes'); END IF; RETURN NEW; END $$; CREATE TRIGGER count_basis_invalidation BEFORE UPDATE ON membership_projection_generations FOR EACH ROW EXECUTE FUNCTION count_basis_invalidation();")
        .execute(&db.pool).await.unwrap();
    for offset in [0, 512] {
        sqlx::query("INSERT INTO membership_projection_rows(identity,bucket,row_key,payload) SELECT $1,'seen',md5(n::text)::uuid::text,to_jsonb(md5(n::text)::uuid::text) FROM generate_series($2::int,$2::int+511) n")
            .bind(&target).bind(offset).execute(&db.pool).await.unwrap();
    }
    sqlx::query("UPDATE membership_projection_rows SET payload=payload WHERE identity=$1")
        .bind(&target)
        .execute(&db.pool)
        .await
        .unwrap();
    let writes: i64 = sqlx::query_scalar("SELECT last_value FROM invalidation_writes")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        writes, 1,
        "1024 inserted rows and later statements invalidate the header only once until restoration"
    );
    let unrelated: bool = sqlx::query_scalar(
        "SELECT basis_digest IS NOT NULL FROM membership_projection_generations WHERE identity<>$1",
    )
    .bind(&target)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(unrelated);
    let identity = ProjectionIdentity::new(Some("target".into()), JobScope::All);
    let current = store.load(&identity).await.unwrap().unwrap();
    assert_eq!(
        store
            .commit(&identity, Some(&current), &current.checkpoint)
            .await
            .unwrap(),
        Some(1)
    );
    let restored: bool = sqlx::query_scalar(
        "SELECT basis_digest IS NOT NULL FROM membership_projection_generations WHERE identity=$1",
    )
    .bind(&target)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(restored);
    // A later refused publication must roll back its transient invalidation too.
    sqlx::raw_sql("CREATE FUNCTION refuse_restoration() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.revision>OLD.revision THEN RAISE EXCEPTION 'fixture final publication refused'; END IF; RETURN NEW; END $$; CREATE TRIGGER refuse_restoration BEFORE UPDATE ON membership_projection_generations FOR EACH ROW EXECUTE FUNCTION refuse_restoration();")
        .execute(&db.pool).await.unwrap();
    let mut source = input(40, 2);
    source.actor = Some("target".into());
    let next = MembershipProjection::rebuild(&source).unwrap();
    assert!(
        store
            .commit(&identity, Some(&current), &next)
            .await
            .unwrap_err()
            .to_string()
            .contains("fixture final publication refused")
    );
    let after = store.load(&identity).await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_vec(&after.checkpoint).unwrap(),
        serde_json::to_vec(&current.checkpoint).unwrap()
    );
    let restored: bool = sqlx::query_scalar(
        "SELECT basis_digest IS NOT NULL FROM membership_projection_generations WHERE identity=$1",
    )
    .bind(&target)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        restored,
        "rollback restores the original digest with every original row"
    );
}

fn nonempty_input() -> ReplayInput {
    use boss_core::{
        event::Event,
        job::{Job, JobStatus, Priority, Step, StepStatus, Subject},
    };
    use boss_events::original_group::{GroupReference, OriginalGroup};
    use boss_jobs::{membership_replay::ReplayGroup, move_diagnosis::HistoricalEvent};
    use serde_json::json;
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/station-predicate-population-20261016.json"
    ))
    .unwrap();
    let station = fixture["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "q.platform-admin.task")
        .unwrap()
        .clone();
    let mut job = Job::new(
        "suite-projection",
        Subject::new("custom", "original"),
        "Original",
        "owner",
        Priority::Standard,
        at(0).date_naive(),
    );
    job.status = JobStatus::Open;
    let mut step = Step::new(job.id, "task", "Original task", 0);
    step.status = StepStatus::Ready;
    step.metadata["authority_role"] = json!("platform-admin");
    let mut workflow = boss_jobs::registry::WorkflowSpec::platform_seed(
        "suite-projection",
        "Original",
        "suite",
        vec!["custom".into()],
        vec![],
    );
    workflow.created_at = at(0);
    let mut seq = 1;
    let mut group = |when, events: Vec<(&str, serde_json::Value)>| {
        let rows: Vec<_> = events
            .into_iter()
            .map(|(kind, payload)| {
                seq += 1;
                HistoricalEvent {
                    seq,
                    created_at: Some(at(when)),
                    event: Event::new("jobs", kind, payload, at(when)),
                }
            })
            .collect();
        let ids: Vec<_> = rows.iter().map(|r| r.event.id).collect();
        ReplayGroup {
            original: Some(OriginalGroup {
                reference: GroupReference {
                    id: uuid::Uuid::new_v4(),
                    schema_version: 1,
                    event_count: ids.len() as i64,
                    member_hash: OriginalGroup::roster_hash(&ids),
                },
                event_ids: ids,
                audit_sequences: rows.iter().map(|r| Some(r.seq)).collect(),
                complete: true,
            }),
            rows,
        }
    };
    let mut groups = vec![
        group(
            1,
            vec![
                (boss_jobs::events::WORKFLOW_PUBLISHED, json!(workflow)),
                (boss_jobs::events::STATION_PUBLISHED, station),
            ],
        ),
        group(
            100_010,
            vec![
                (boss_jobs::events::JOB_CREATED, json!(job)),
                (boss_jobs::events::STEP_CREATED, json!(step)),
            ],
        ),
    ];
    job.status = JobStatus::Closed;
    groups.push(group(
        100_020,
        vec![(boss_jobs::events::JOB_UPDATED, json!(job))],
    ));
    job.status = JobStatus::Open;
    groups.push(group(
        100_030,
        vec![(boss_jobs::events::JOB_UPDATED, json!(job))],
    ));
    ReplayInput {
        groups,
        ..input(172_800, 100)
    }
}

async fn the_port_preserves_original_nonempty_facts_and_bound_reads<
    R: MembershipProjectionRepository,
>(
    store: &R,
    adapter: &str,
) {
    use boss_jobs::membership_replay::projection::ProjectionBinding;
    let source = nonempty_input();
    let next = MembershipProjection::rebuild(&source).unwrap();
    assert!(
        next.facts().len() >= 3,
        "{adapter}: this case must exercise actual crossing rows"
    );
    let identity = ProjectionIdentity::new(source.actor.clone(), source.scope.clone());
    assert_eq!(
        store.commit(&identity, None, &next).await.unwrap(),
        Some(1),
        "{adapter}"
    );
    let loaded = store.load(&identity).await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&loaded.checkpoint).unwrap(),
        serde_json::to_value(&next).unwrap(),
        "{adapter}"
    );
    let binding = ProjectionBinding::from_input(&source);
    let reading = store
        .read(&identity, at(5), &binding)
        .await
        .unwrap()
        .unwrap();
    let mut oracle_input = source.clone();
    oracle_input.since = at(5);
    let oracle = boss_jobs::membership_replay::replay(&oracle_input);
    assert_eq!(
        serde_json::to_value(&reading.reading).unwrap(),
        serde_json::to_value(&oracle).unwrap(),
        "{adapter}"
    );
    let flow = &reading.reading.stations[0];
    assert_eq!(
        (flow.arrived, flow.departed),
        (Some(2), Some(1)),
        "{adapter}"
    );
    let mut revoked = binding;
    revoked.currently_authorized = Some(Default::default());
    assert!(
        store.read(&identity, at(5), &revoked).await.is_err(),
        "{adapter}"
    );
    let stranger = ProjectionIdentity::new(Some("another literal caller".into()), JobScope::All);
    assert!(store.load(&stranger).await.unwrap().is_none(), "{adapter}");
}

async fn the_port_has_the_same_cas_retry_and_refusal_contract<R: MembershipProjectionRepository>(
    store: &R,
    adapter: &str,
) {
    let first = MembershipProjection::rebuild(&input(20, 1)).unwrap();
    let identity = ProjectionIdentity::new(input(20, 1).actor, JobScope::All);
    assert_eq!(
        store.commit(&identity, None, &first).await.unwrap(),
        Some(1),
        "{adapter}"
    );
    let original = store.load(&identity).await.unwrap().unwrap();
    let second = MembershipProjection::rebuild(&input(40, 2)).unwrap();
    assert_eq!(
        store
            .commit(&identity, Some(&original), &second)
            .await
            .unwrap(),
        Some(2),
        "{adapter}"
    );
    assert_eq!(
        store
            .commit(&identity, Some(&original), &second)
            .await
            .unwrap(),
        None,
        "{adapter}: stale prepared retry cannot publish twice"
    );
    assert_eq!(
        store
            .commit(&identity, Some(&original), &first)
            .await
            .unwrap(),
        None,
        "{adapter}: stale writer cannot rewind"
    );
    let latest = store.load(&identity).await.unwrap().unwrap();
    assert_eq!(
        store
            .commit(&identity, Some(&latest), &second)
            .await
            .unwrap(),
        Some(2),
        "{adapter}: current exact retry is idempotent"
    );
    let backwards = MembershipProjection::rebuild(&input(30, 1)).unwrap();
    assert!(
        store
            .commit(&identity, Some(&latest), &backwards)
            .await
            .is_err(),
        "{adapter}"
    );
    assert_eq!(
        store.load(&identity).await.unwrap().unwrap().revision,
        2,
        "{adapter}"
    );
}

async fn the_port_retains_sixteen_committed_identities<R: MembershipProjectionRepository>(
    store: &R,
    adapter: &str,
) {
    let mut identities = Vec::new();
    for index in 0..17 {
        let mut source = input(20, 1);
        source.actor = Some(format!("literal retained caller {index}"));
        let identity = ProjectionIdentity::new(source.actor.clone(), JobScope::All);
        let next = MembershipProjection::rebuild(&source).unwrap();
        assert_eq!(
            store.commit(&identity, None, &next).await.unwrap(),
            Some(1),
            "{adapter}"
        );
        identities.push(identity);
    }
    assert!(
        store.load(&identities[0]).await.unwrap().is_none(),
        "{adapter}"
    );
    for identity in &identities[1..] {
        assert!(store.load(identity).await.unwrap().is_some(), "{adapter}");
    }
}

async fn the_port_reads_both_canonical_windows_from_one_generation<
    R: MembershipProjectionRepository,
>(
    store: &R,
    adapter: &str,
) {
    use boss_jobs::membership_replay::projection::ProjectionBinding;
    let source = nonempty_input();
    let identity = ProjectionIdentity::new(source.actor.clone(), source.scope.clone());
    let checkpoint = MembershipProjection::rebuild(&source).unwrap();
    assert_eq!(
        store.commit(&identity, None, &checkpoint).await.unwrap(),
        Some(1),
        "{adapter}"
    );
    let binding = ProjectionBinding::from_input(&source);
    let pair = store
        .read_windows(&identity, 24, at(172_801), &binding)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pair.revision, 1, "{adapter}");
    assert_eq!(pair.until, source.until, "{adapter}");
    let mut narrow = source.clone();
    narrow.since = source.until - chrono::Duration::hours(24);
    assert_eq!(
        serde_json::to_value(pair.this).unwrap(),
        serde_json::to_value(boss_jobs::membership_replay::replay(&narrow)).unwrap(),
        "{adapter}"
    );
    assert_eq!(
        serde_json::to_value(pair.both.unwrap()).unwrap(),
        serde_json::to_value(boss_jobs::membership_replay::replay(&source)).unwrap(),
        "{adapter}"
    );
    let wide = store
        .read_windows(&identity, 48, at(172_801), &binding)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wide.until, source.until, "{adapter}");
    assert!(
        wide.both.is_err(),
        "{adapter}: comparison96h is outside retained evidence"
    );
    assert_eq!(
        serde_json::to_value(wide.this).unwrap(),
        serde_json::to_value(boss_jobs::membership_replay::replay(&source)).unwrap(),
        "{adapter}"
    );
}

#[tokio::test]
async fn canonical_views_do_not_load_raw_rows_and_corruption_fails_closed() {
    use boss_jobs::membership_replay::projection::ProjectionBinding;
    let db = TestDb::new().await;
    let store = PgMembershipProjection::new(db.pool.clone());
    let source = nonempty_input();
    let identity = ProjectionIdentity::new(source.actor.clone(), source.scope.clone());
    let binding = ProjectionBinding::from_input(&source);
    store
        .commit(
            &identity,
            None,
            &MembershipProjection::rebuild(&source).unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    let canonical: serde_json::Value =
        sqlx::query_scalar("SELECT canonical_windows FROM membership_projection_generations")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    sqlx::query(
        "UPDATE membership_projection_rows SET payload='null' WHERE bucket IN ('packet','fact')",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    assert!(
        store
            .read(
                &identity,
                source.until - chrono::Duration::hours(24),
                &binding
            )
            .await
            .is_err(),
        "the poisoned original-row control must be observed"
    );
    let compact = store
        .read_windows(&identity, 24, source.until, &binding)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(compact.this.stations[0].arrived, Some(2));
    let original_header: serde_json::Value =
        sqlx::query_scalar("SELECT header FROM membership_projection_generations")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    sqlx::query(
        "UPDATE membership_projection_generations SET header=jsonb_set(header,'{schema}','999')",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    assert!(
        store
            .read_windows(&identity, 24, source.until, &binding)
            .await
            .is_err(),
        "an unsupported checkpoint schema must never be a healthy canonical generation"
    );
    sqlx::query("UPDATE membership_projection_generations SET header=$1")
        .bind(original_header)
        .execute(&db.pool)
        .await
        .unwrap();
    for malformed in [serde_json::Value::Null, serde_json::json!({})] {
        sqlx::query("UPDATE membership_projection_generations SET canonical_windows=$1")
            .bind(malformed)
            .execute(&db.pool)
            .await
            .unwrap();
        assert!(
            store
                .read_windows(&identity, 24, source.until, &binding)
                .await
                .is_err()
        );
    }
    let mut wrong_cut = canonical;
    wrong_cut["hours24"]["Ok"]["until"] =
        serde_json::json!(source.until + chrono::Duration::seconds(1));
    sqlx::query("UPDATE membership_projection_generations SET canonical_windows=$1")
        .bind(wrong_cut)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        store
            .read_windows(&identity, 24, source.until, &binding)
            .await
            .unwrap_err()
            .to_string()
            .contains("original generation cut")
    );
}

#[tokio::test]
async fn a_refused_publication_keeps_both_canonical_windows_and_original_facts() {
    use boss_jobs::membership_replay::projection::ProjectionBinding;
    let db = TestDb::new().await;
    let store = PgMembershipProjection::new(db.pool.clone());
    let source = nonempty_input();
    let identity = ProjectionIdentity::new(source.actor.clone(), source.scope.clone());
    let first = MembershipProjection::rebuild(&source).unwrap();
    store
        .commit(&identity, None, &first)
        .await
        .unwrap()
        .unwrap();
    let prior = store.load(&identity).await.unwrap().unwrap();
    let before: serde_json::Value =
        sqlx::query_scalar("SELECT canonical_windows FROM membership_projection_generations")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let mut next_source = source.clone();
    next_source.since += chrono::Duration::seconds(20);
    next_source.until += chrono::Duration::seconds(20);
    next_source.audit_cutoff += 1;
    next_source.clock.known.last_mut().unwrap().until = next_source.until;
    let next = MembershipProjection::rebuild(&next_source).unwrap();
    sqlx::raw_sql("CREATE FUNCTION refuse_canonical_publication() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture canonical publication refused'; END $$; CREATE TRIGGER refuse_canonical_publication BEFORE UPDATE ON membership_projection_generations FOR EACH ROW EXECUTE FUNCTION refuse_canonical_publication();").execute(&db.pool).await.unwrap();
    assert!(
        store
            .commit(&identity, Some(&prior), &next)
            .await
            .unwrap_err()
            .to_string()
            .contains("fixture canonical publication refused")
    );
    let loaded = store.load(&identity).await.unwrap().unwrap();
    assert_eq!(loaded.revision, 1);
    assert_eq!(
        serde_json::to_value(loaded.checkpoint).unwrap(),
        serde_json::to_value(first).unwrap()
    );
    let after: serde_json::Value =
        sqlx::query_scalar("SELECT canonical_windows FROM membership_projection_generations")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(after, before);
    let pair = store
        .read_windows(
            &identity,
            24,
            source.until,
            &ProjectionBinding::from_input(&source),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pair.until, source.until);
    assert_eq!(pair.this.audit_cutoff, source.audit_cutoff);
    assert_eq!(pair.both.unwrap().audit_cutoff, source.audit_cutoff);
}

// Chunking changes transport only: all statements still publish one generation.
fn publication_chunk_input(extra: usize) -> ReplayInput {
    use boss_core::event::Event;
    use boss_events::original_group::{GroupReference, OriginalGroup};
    use boss_jobs::{membership_replay::ReplayGroup, move_diagnosis::HistoricalEvent};
    let mut source = nonempty_input();
    let original_job = source
        .groups
        .iter()
        .flat_map(|g| &g.rows)
        .rfind(|r| r.event.kind == boss_jobs::events::JOB_UPDATED)
        .unwrap()
        .event
        .payload
        .clone();
    for index in 0..extra {
        let seq = 101 + index as i64;
        let when = at(100_100 + index as i64);
        let mut payload = original_job.clone();
        payload["metadata"] = serde_json::json!({"publication_witness": index});
        let event = Event::new("jobs", boss_jobs::events::JOB_UPDATED, payload, when);
        let ids = vec![event.id];
        source.groups.push(ReplayGroup {
            original: Some(OriginalGroup {
                reference: GroupReference {
                    id: uuid::Uuid::new_v4(),
                    schema_version: 1,
                    event_count: 1,
                    member_hash: OriginalGroup::roster_hash(&ids),
                },
                event_ids: ids,
                audit_sequences: vec![Some(seq)],
                complete: true,
            }),
            rows: vec![HistoricalEvent {
                seq,
                created_at: Some(when),
                event,
            }],
        });
    }
    source.audit_cutoff = 101 + extra as i64;
    source
}

async fn publication_physical_bytes(db: &TestDb) -> Vec<u8> {
    let value: serde_json::Value = sqlx::query_scalar("SELECT jsonb_build_object('generations',COALESCE((SELECT jsonb_agg(to_jsonb(g) ORDER BY identity) FROM membership_projection_generations g),'[]'::jsonb),'rows',COALESCE((SELECT jsonb_agg(to_jsonb(r) ORDER BY identity,bucket,row_key) FROM membership_projection_rows r),'[]'::jsonb))")
        .fetch_one(&db.pool).await.unwrap();
    serde_json::to_vec(&value).unwrap()
}

async fn publication_statement_witness(db: &TestDb, operation: &str, refuse_second: bool) {
    assert!(matches!(operation, "INSERT" | "DELETE"));
    let refusal = if refuse_second {
        "IF n=2 THEN RAISE EXCEPTION 'fixture second publication chunk refused'; END IF;"
    } else {
        ""
    };
    let sql = format!(
        "CREATE SEQUENCE publication_statement_calls; CREATE FUNCTION witness_publication_statement() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE n bigint; BEGIN n := nextval('publication_statement_calls'); {refusal} RETURN NULL; END $$; CREATE TRIGGER witness_publication_statement AFTER {operation} ON membership_projection_rows FOR EACH STATEMENT EXECUTE FUNCTION witness_publication_statement();"
    );
    sqlx::raw_sql(&sql).execute(&db.pool).await.unwrap();
}

#[tokio::test]
async fn multiple_publication_chunks_upsert_and_delete_without_changing_the_original_fold() {
    use boss_jobs::membership_projection_pg::PUBLICATION_BATCH_ROWS;
    let db = TestDb::new().await;
    let store = PgMembershipProjection::new(db.pool.clone());
    let source = publication_chunk_input(PUBLICATION_BATCH_ROWS * 3);
    let identity = ProjectionIdentity::new(source.actor.clone(), source.scope.clone());
    let next = MembershipProjection::rebuild(&source).unwrap();
    publication_statement_witness(&db, "INSERT", false).await;
    assert_eq!(store.commit(&identity, None, &next).await.unwrap(), Some(1));
    let calls: i64 = sqlx::query_scalar("SELECT last_value FROM publication_statement_calls")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(
        calls >= 2,
        "actual database INSERT statements must be chunked: {calls}"
    );
    let previous = store.load(&identity).await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_vec(&previous.checkpoint).unwrap(),
        serde_json::to_vec(&next).unwrap()
    );
    sqlx::raw_sql("DROP TRIGGER witness_publication_statement ON membership_projection_rows; DROP FUNCTION witness_publication_statement(); DROP SEQUENCE publication_statement_calls;").execute(&db.pool).await.unwrap();
    publication_statement_witness(&db, "DELETE", false).await;
    let mut reduced = nonempty_input();
    reduced.audit_cutoff = source.audit_cutoff + 1;
    let reduced = MembershipProjection::rebuild(&reduced).unwrap();
    assert_eq!(
        store
            .commit(&identity, Some(&previous), &reduced)
            .await
            .unwrap(),
        Some(2)
    );
    let calls: i64 = sqlx::query_scalar("SELECT last_value FROM publication_statement_calls")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(
        calls >= 2,
        "actual database DELETE statements must be chunked: {calls}"
    );
    let observed = store.load(&identity).await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_vec(&observed.checkpoint).unwrap(),
        serde_json::to_vec(&reduced).unwrap()
    );
    // The same expected previous generation cannot publish twice.
    assert_eq!(
        store
            .commit(&identity, Some(&previous), &reduced)
            .await
            .unwrap(),
        None
    );
}

async fn a_second_chunk_failure_preserves_the_entire_previous_generation(operation: &str) {
    use boss_jobs::membership_projection_pg::PUBLICATION_BATCH_ROWS;
    let db = TestDb::new().await;
    let store = PgMembershipProjection::new(db.pool.clone());
    let large = publication_chunk_input(PUBLICATION_BATCH_ROWS * 3);
    let mut small = nonempty_input();
    let identity = ProjectionIdentity::new(large.actor.clone(), large.scope.clone());
    let (first, next) = if operation == "INSERT" {
        (
            MembershipProjection::rebuild(&small).unwrap(),
            MembershipProjection::rebuild(&large).unwrap(),
        )
    } else {
        small.audit_cutoff = large.audit_cutoff + 1;
        (
            MembershipProjection::rebuild(&large).unwrap(),
            MembershipProjection::rebuild(&small).unwrap(),
        )
    };
    store
        .commit(&identity, None, &first)
        .await
        .unwrap()
        .unwrap();
    let previous = store.load(&identity).await.unwrap().unwrap();
    let physical_before = publication_physical_bytes(&db).await;
    publication_statement_witness(&db, operation, true).await;
    let error = store
        .commit(&identity, Some(&previous), &next)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("fixture second publication chunk refused"),
        "{error}"
    );
    let calls: i64 = sqlx::query_scalar("SELECT last_value FROM publication_statement_calls")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(calls, 2, "the failure must follow an actual first chunk");
    assert_eq!(
        publication_physical_bytes(&db).await,
        physical_before,
        "header, revision, canonical pair, row payloads and timestamps must all roll back"
    );
    let observed = store.load(&identity).await.unwrap().unwrap();
    assert_eq!(observed.revision, previous.revision);
    assert_eq!(
        serde_json::to_vec(&observed.checkpoint).unwrap(),
        serde_json::to_vec(&first).unwrap()
    );
}

#[tokio::test]
async fn a_second_insert_chunk_failure_rolls_back_the_whole_generation() {
    a_second_chunk_failure_preserves_the_entire_previous_generation("INSERT").await;
}

#[tokio::test]
async fn a_second_delete_chunk_failure_rolls_back_the_whole_generation() {
    a_second_chunk_failure_preserves_the_entire_previous_generation("DELETE").await;
}

async fn a_budget_refusal_inside_publication_keeps_the_previous_generation(
    cancel_during_sql: bool,
) {
    use boss_jobs::{
        membership_projection_pg::PUBLICATION_BATCH_ROWS, membership_replay::ReplayBudget,
    };
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    };
    let db = TestDb::new().await;
    let store = Arc::new(PgMembershipProjection::new(db.pool.clone()));
    let source = publication_chunk_input(PUBLICATION_BATCH_ROWS * 3);
    let identity = ProjectionIdentity::new(source.actor.clone(), source.scope.clone());
    let first = MembershipProjection::rebuild(&nonempty_input()).unwrap();
    let next = MembershipProjection::rebuild(&source).unwrap();
    store
        .commit(&identity, None, &first)
        .await
        .unwrap()
        .unwrap();
    let previous = store.load(&identity).await.unwrap().unwrap();
    let physical_before = publication_physical_bytes(&db).await;
    // Sequence increments survive rollback and witness that the actual second
    // SQL statement has begun, rather than guessing from an elapsed sleep.
    let mut blocker = db.pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(83726041)")
        .execute(&mut *blocker)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE SEQUENCE publication_statement_calls; CREATE FUNCTION slow_publication_statement() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE n bigint; BEGIN n := nextval('publication_statement_calls'); IF n=2 THEN PERFORM pg_advisory_xact_lock(83726041); END IF; RETURN NULL; END $$; CREATE TRIGGER slow_publication_statement AFTER INSERT ON membership_projection_rows FOR EACH STATEMENT EXECUTE FUNCTION slow_publication_statement();")
        .execute(&db.pool).await.unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(if cancel_during_sql { 10 } else { 2 });
    let budget = ReplayBudget::new(deadline, cancelled.clone(), usize::MAX);
    let owned_store = store.clone();
    let owned_identity = identity.clone();
    let task = tokio::spawn(async move {
        owned_store
            .commit_with_budget(&owned_identity, Some(&previous), &next, &budget)
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (calls, called): (i64, bool) =
                sqlx::query_as("SELECT last_value,is_called FROM publication_statement_calls")
                    .fetch_one(&db.pool)
                    .await
                    .unwrap();
            if called && calls >= 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual second publication statement must start");
    if cancel_during_sql {
        cancelled.store(true, Ordering::SeqCst);
        assert!(
            !task.is_finished(),
            "cancellation cannot claim completion while actual SQL is owned"
        );
        sqlx::query("SELECT pg_advisory_unlock(83726041)")
            .execute(&mut *blocker)
            .await
            .unwrap();
    }
    let error = tokio::time::timeout(Duration::from_secs(6), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    if !cancel_during_sql {
        sqlx::query("SELECT pg_advisory_unlock(83726041)")
            .execute(&mut *blocker)
            .await
            .unwrap();
    }
    let why = error.to_string();
    if cancel_during_sql {
        assert!(why.contains("cancel"), "{why}");
    } else {
        assert!(
            why.contains("deadline") || why.contains("statement timeout"),
            "{why}"
        );
    }
    // Once the port returns, rollback must be terminal and pool reads usable.
    assert_eq!(publication_physical_bytes(&db).await, physical_before);
    let observed = store.load(&identity).await.unwrap().unwrap();
    assert_eq!(observed.revision, 1);
    assert_eq!(
        serde_json::to_vec(&observed.checkpoint).unwrap(),
        serde_json::to_vec(&first).unwrap()
    );
}

#[tokio::test]
async fn cancellation_during_a_real_second_chunk_waits_for_sql_and_rolls_back() {
    a_budget_refusal_inside_publication_keeps_the_previous_generation(true).await;
}

#[tokio::test]
async fn deadline_during_a_real_second_chunk_rolls_back_before_publication() {
    a_budget_refusal_inside_publication_keeps_the_previous_generation(false).await;
}

#[tokio::test]
async fn cap_eviction_chunks_the_oldest_rows_and_rolls_back_a_mid_delete_refusal() {
    use boss_jobs::membership_projection_pg::PUBLICATION_BATCH_ROWS;
    let db = TestDb::new().await;
    let store = PgMembershipProjection::new(db.pool.clone());
    let large_source = publication_chunk_input(PUBLICATION_BATCH_ROWS * 3);
    let oldest = ProjectionIdentity::new(large_source.actor.clone(), large_source.scope.clone());
    let original = MembershipProjection::rebuild(&large_source).unwrap();
    store
        .commit(&oldest, None, &original)
        .await
        .unwrap()
        .unwrap();
    let oldest_key: String =
        sqlx::query_scalar("SELECT identity FROM membership_projection_generations")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    for index in 0..15 {
        let mut source = input(20, 1);
        source.actor = Some(format!("eviction-reader-{index}"));
        let identity = ProjectionIdentity::new(source.actor.clone(), source.scope.clone());
        store
            .commit(
                &identity,
                None,
                &MembershipProjection::rebuild(&source).unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
    }
    let before = publication_physical_bytes(&db).await;
    sqlx::raw_sql("CREATE TABLE eviction_target(identity text PRIMARY KEY); CREATE SEQUENCE eviction_delete_calls; CREATE FUNCTION witness_eviction_delete() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE n bigint; BEGIN IF EXISTS(SELECT 1 FROM deleted_rows r JOIN eviction_target t USING(identity)) THEN n:=nextval('eviction_delete_calls'); IF NOT EXISTS(SELECT 1 FROM membership_projection_generations g JOIN eviction_target t USING(identity)) THEN RAISE EXCEPTION 'fixture eviction header removed before its rows'; END IF; IF n=2 THEN RAISE EXCEPTION 'fixture second eviction chunk refused'; END IF; END IF; RETURN NULL; END $$; CREATE TRIGGER witness_eviction_delete AFTER DELETE ON membership_projection_rows REFERENCING OLD TABLE AS deleted_rows FOR EACH STATEMENT EXECUTE FUNCTION witness_eviction_delete();")
        .execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO eviction_target VALUES($1)")
        .bind(&oldest_key)
        .execute(&db.pool)
        .await
        .unwrap();
    let mut source = input(30, 2);
    source.actor = Some("eviction-reader-17".into());
    let candidate = ProjectionIdentity::new(source.actor.clone(), source.scope.clone());
    let checkpoint = MembershipProjection::rebuild(&source).unwrap();
    let error = store
        .commit(&candidate, None, &checkpoint)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("fixture second eviction chunk refused"),
        "{error}"
    );
    let calls: i64 = sqlx::query_scalar("SELECT last_value FROM eviction_delete_calls")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        calls, 2,
        "a real second nonempty oldest-row deletion must fail"
    );
    assert_eq!(
        publication_physical_bytes(&db).await,
        before,
        "all16 generations and original row/canonical/timestamp bytes must roll back"
    );
    assert!(store.load(&candidate).await.unwrap().is_none());
    let retained = store.load(&oldest).await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_vec(&retained.checkpoint).unwrap(),
        serde_json::to_vec(&original).unwrap()
    );
    // Keep the ordering witness, remove only its deliberate second-call fault.
    sqlx::raw_sql("CREATE OR REPLACE FUNCTION witness_eviction_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF EXISTS(SELECT 1 FROM deleted_rows r JOIN eviction_target t USING(identity)) THEN PERFORM nextval('eviction_delete_calls'); IF NOT EXISTS(SELECT 1 FROM membership_projection_generations g JOIN eviction_target t USING(identity)) THEN RAISE EXCEPTION 'fixture eviction header removed before its rows'; END IF; END IF; RETURN NULL; END $$; ALTER SEQUENCE eviction_delete_calls RESTART WITH 1;")
        .execute(&db.pool).await.unwrap();
    assert_eq!(
        store.commit(&candidate, None, &checkpoint).await.unwrap(),
        Some(1)
    );
    let calls: i64 = sqlx::query_scalar("SELECT last_value FROM eviction_delete_calls")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(
        calls > 1,
        "the oldest nonempty generation must use multiple deletion chunks: {calls}"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM membership_projection_generations")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 16);
    assert!(
        store.load(&oldest).await.unwrap().is_none(),
        "the exact first committed identity is evicted"
    );
    let orphan_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM membership_projection_rows WHERE identity=$1")
            .bind(&oldest_key)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(orphan_rows, 0);
    let observed = store.load(&candidate).await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_vec(&observed.checkpoint).unwrap(),
        serde_json::to_vec(&checkpoint).unwrap()
    );
    // A stale bootstrap CAS cannot publish a duplicate generation.
    assert_eq!(
        store.commit(&candidate, None, &checkpoint).await.unwrap(),
        None
    );
}

async fn the_port_refuses_cancelled_or_expired_publication_without_installing_it<
    R: MembershipProjectionRepository,
>(
    store: &R,
    adapter: &str,
) {
    use boss_jobs::membership_replay::ReplayBudget;
    use std::sync::{Arc, atomic::AtomicBool};
    let source = nonempty_input();
    let identity = ProjectionIdentity::new(source.actor.clone(), source.scope.clone());
    let first = MembershipProjection::rebuild(&source).unwrap();
    store
        .commit(&identity, None, &first)
        .await
        .unwrap()
        .unwrap();
    let previous = store.load(&identity).await.unwrap().unwrap();
    let mut later = source.clone();
    later.until += chrono::Duration::seconds(1);
    later.audit_cutoff += 1;
    later.clock.known.last_mut().unwrap().until = later.until;
    let next = MembershipProjection::rebuild(&later).unwrap();
    for cancelled in [true, false] {
        let deadline = if cancelled {
            std::time::Instant::now() + std::time::Duration::from_secs(90)
        } else {
            std::time::Instant::now() - std::time::Duration::from_secs(1)
        };
        let budget = ReplayBudget::new(deadline, Arc::new(AtomicBool::new(cancelled)), usize::MAX);
        let why = store
            .commit_with_budget(&identity, Some(&previous), &next, &budget)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            why.contains(if cancelled { "cancelled" } else { "deadline" }),
            "{adapter}: {why}"
        );
        let observed = store.load(&identity).await.unwrap().unwrap();
        assert_eq!(observed.revision, previous.revision, "{adapter}");
        assert_eq!(
            serde_json::to_vec(&observed.checkpoint).unwrap(),
            serde_json::to_vec(&first).unwrap(),
            "{adapter}"
        );
    }
    // A later cancellation cannot turn a received publication acknowledgement
    // into a claim that the old generation was restored.
    let cancelled = Arc::new(AtomicBool::new(false));
    let budget = ReplayBudget::new(
        std::time::Instant::now() + std::time::Duration::from_secs(90),
        cancelled.clone(),
        usize::MAX,
    );
    let acknowledged = store
        .commit_with_budget(&identity, Some(&previous), &next, &budget)
        .await
        .unwrap();
    cancelled.store(true, std::sync::atomic::Ordering::Release);
    assert_eq!(acknowledged, Some(2), "{adapter}");
    let observed = store.load(&identity).await.unwrap().unwrap();
    assert_eq!(observed.revision, 2, "{adapter}");
    assert_eq!(
        serde_json::to_vec(&observed.checkpoint).unwrap(),
        serde_json::to_vec(&next).unwrap(),
        "{adapter}"
    );
}
