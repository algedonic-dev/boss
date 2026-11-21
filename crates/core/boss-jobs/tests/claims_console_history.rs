use boss_core::event::Event;
use boss_jobs::claims_console::{
    ClaimsHistory, HistoryBatch, InMemoryClaimsHistory, PgClaimsHistory, SequencedFact,
};
use chrono::{DateTime, Utc};
use serde_json::json;

// Every Pg history in this file runs on `PgClaimsHistory::for_fold_fixture`:
// what these tests judge is what the projection folds, and main gate
// 643e3baf at f150f78f went red here on a loaded gate drive when one of them
// spent production's 2 s pre-commit budget on the drive (backlog ebbf38c6).
// The two blocked-read tests below judge the refusal itself, so they take
// `blocked_read_history`, a budget they state, rather than production's.

/// The budget a blocked read waits out. A lock that never releases makes
/// any budget expire AT the locked statement; this one is long enough that
/// the measured 5 s stall a loaded gate drive puts in BEGIN and the statements
/// before the lock (backlog ebbf38c6) does not expire it one statement
/// early, which would name the wrong query. What production refuses at is
/// pinned in the adapter's own tests.
const BLOCKED_READ_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);
fn blocked_read_history(db: &boss_testing::TestDb) -> PgClaimsHistory {
    PgClaimsHistory::for_fixture(
        db.pool.clone(),
        BLOCKED_READ_BUDGET,
        tokio::time::Instant::now,
    )
}

#[tokio::test]
async fn a_blocked_cuts_query_names_the_pg_budget_and_query() {
    let db = boss_testing::TestDb::new().await;
    let at = fact(1, "jobs.unrelated", json!({})).recorded_at;
    PgClaimsHistory::for_fold_fixture(db.pool.clone())
        .materialize(&HistoryBatch {
            through: 1,
            through_at: at,
            facts: vec![],
        })
        .await
        .unwrap();
    let mut blocker = db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE jobs_claim_history_cuts IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let error = blocked_read_history(&db)
        .cuts(None, None, 4, at)
        .await
        .unwrap_err()
        .to_string();
    blocker.rollback().await.unwrap();
    assert!(
        error.contains("first_retained")
            && error.contains("elapsed_ms")
            && error.contains("query budget"),
        "{error}"
    );
}

/// The head read names the statement its budget ran out in, as the cuts
/// read does (backlog 81f8e468): on 2026-11-07 GET /api/claims answered
/// 503 'read deadline exhausted' for an hour with no statement named,
/// while the cuts read beside it named `first_retained` and its elapsed
/// time.
#[tokio::test]
async fn a_blocked_head_read_names_the_pg_budget_and_query() {
    let db = boss_testing::TestDb::new().await;
    let at = fact(1, "jobs.unrelated", json!({})).recorded_at;
    PgClaimsHistory::for_fold_fixture(db.pool.clone())
        .materialize(&HistoryBatch {
            through: 1,
            through_at: at,
            facts: vec![],
        })
        .await
        .unwrap();
    let mut blocker = db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE jobs_claim_history_current IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let error = blocked_read_history(&db)
        .read(None, 24, at)
        .await
        .unwrap_err()
        .to_string();
    blocker.rollback().await.unwrap();
    assert!(
        error.contains("active_query=rows_bound")
            && error.contains("elapsed_ms")
            && error.contains("query budget")
            && error.contains("split"),
        "the refusal names the statement it waited in and the ones before it: {error}"
    );
}

/// A fold-fixture history whose SOURCE read alone has production's 2 s
/// from a clock that has already lost the measured stall (gate-run
/// a5c6c1ea, 5254 ms of BEGIN): every statement of the history itself
/// still runs on the fixture. The stall is boss_events::statement_budget's,
/// spelled once (backlog 03a5dbdc).
struct SpentSource(PgClaimsHistory);
#[async_trait::async_trait]
impl ClaimsHistory for SpentSource {
    fn source_deadline(&self) -> tokio::time::Instant {
        boss_events::statement_budget::after_the_measured_stall()
            + std::time::Duration::from_secs(2)
    }
    async fn cursor(
        &self,
    ) -> Result<boss_jobs::claims_console::ProjectionCursor, boss_jobs::claims_console::ClaimsError>
    {
        self.0.cursor().await
    }
    async fn advance(
        &self,
        c: boss_jobs::claims_console::ProjectionCursor,
        b: &HistoryBatch,
        s: boss_jobs::claims_console::SourceObservation,
    ) -> Result<(), boss_jobs::claims_console::ClaimsError> {
        self.0.advance(c, b, s).await
    }
    async fn source_failed(
        &self,
        c: boss_jobs::claims_console::ProjectionCursor,
        at: DateTime<Utc>,
    ) -> Result<(), boss_jobs::claims_console::ClaimsError> {
        self.0.source_failed(c, at).await
    }
}

/// The source's audit-tail read spends the deadline its history answers,
/// so a fold fixture's tail outlasts a loaded gate drive as its statements
/// do. It took a literal 2 s of its own until backlog ebbf38c6: this fails
/// if the read stops spending the history's deadline, and the control is
/// the same advance on the fixture's own, which every fold test here runs.
#[tokio::test]
async fn the_native_source_reads_the_audit_tail_on_its_historys_deadline() {
    let db = boss_testing::TestDb::new().await;
    sqlx::query("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES($1,now(),'jobs','jobs.unrelated','{}'::jsonb)")
        .bind(uuid::Uuid::new_v4()).execute(&db.pool).await.unwrap();
    let spent = SpentSource(PgClaimsHistory::for_fold_fixture(db.pool.clone()));
    let refused =
        boss_jobs::claims_console::source::advance_native_once(&spent, &db.pool, 1024).await;
    match refused {
        Err(why) => assert!(
            why.to_string().contains("audit prefix unavailable")
                && why
                    .to_string()
                    .contains("pre-commit statement budget expired"),
            "{why}"
        ),
        Ok(caught_up) => panic!(
            "a spent source deadline must refuse the tail read, not advance (caught up: {caught_up})"
        ),
    }
    let control = PgClaimsHistory::for_fold_fixture(db.pool.clone());
    assert!(
        boss_jobs::claims_console::source::advance_native_once(&control, &db.pool, 1024)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn native_source_keeps_unknown_claim_presence_without_copying_its_body() {
    let db = boss_testing::TestDb::new().await;
    for (kind, payload) in [
        (
            "jobs.pool.published",
            pool(json!({"state":"configured","amount":"2"})),
        ),
        (
            "jobs.stock.obligated",
            json!({"claim":{"private_body":"not needed"}}),
        ),
        (
            "jobs.stock.nested",
            json!({"step":{"metadata":{"resource_claim":{"private_body":"not needed"}}}}),
        ),
        ("jobs.step.resource_future", json!({})),
        ("jobs.unrelated", json!({"unrelated":"ignored"})),
    ] {
        sqlx::query("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES($1,now(),'jobs',$2,$3)")
            .bind(uuid::Uuid::new_v4()).bind(kind).bind(payload).execute(&db.pool).await.unwrap();
    }
    let history = PgClaimsHistory::for_fold_fixture(db.pool.clone());
    assert!(
        boss_jobs::claims_console::source::advance_native_once(&history, &db.pool, 1024)
            .await
            .unwrap()
    );
    let reading = history.read(None, 24, Utc::now()).await.unwrap();
    let wire = serde_json::to_value(&reading).unwrap();
    let names = wire["projection"]["unknown_kinds"]
        .as_array()
        .expect("unknown native facts must be visible");
    assert_eq!(names.len(), 3);
    assert_eq!(names[0]["kind"], "jobs.step.resource_future");
    assert_eq!(names[1]["kind"], "jobs.stock.nested");
    assert_eq!(names[2]["kind"], "jobs.stock.obligated");
    assert_eq!(
        reading.pools[0].band.as_deref(),
        Some("unknown-resource-kind")
    );
}

// A recording decorator around the REAL Pg projection, not a fabricated
// source/result. It witnesses maintenance ordering while native audit is ahead.
struct BacklogPruneProbe {
    inner: PgClaimsHistory,
    advances: std::sync::atomic::AtomicUsize,
    pruned: std::sync::atomic::AtomicBool,
    source_head: std::sync::atomic::AtomicI64,
    notice: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<bool>>>,
}
impl BacklogPruneProbe {
    fn notice(&self, good: bool) {
        if let Some(tx) = self.notice.lock().unwrap().take() {
            let _ = tx.send(good);
        }
    }
}
#[async_trait::async_trait]
impl ClaimsHistory for BacklogPruneProbe {
    // The wrapped history's budget, not the trait's production default.
    fn source_deadline(&self) -> tokio::time::Instant {
        self.inner.source_deadline()
    }
    async fn cursor(
        &self,
    ) -> Result<boss_jobs::claims_console::ProjectionCursor, boss_jobs::claims_console::ClaimsError>
    {
        self.inner.cursor().await
    }
    async fn advance(
        &self,
        c: boss_jobs::claims_console::ProjectionCursor,
        b: &HistoryBatch,
        s: boss_jobs::claims_console::SourceObservation,
    ) -> Result<(), boss_jobs::claims_console::ClaimsError> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.advances.fetch_add(1, SeqCst) > 0 && !self.pruned.swap(false, SeqCst) {
            self.notice(false);
        }
        self.inner.advance(c, b, s).await?;
        self.source_head.store(s.audit_head, SeqCst);
        Ok(())
    }
    async fn prune(
        &self,
        c: boss_jobs::claims_console::ProjectionCursor,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<usize, boss_jobs::claims_console::ClaimsError> {
        use std::sync::atomic::Ordering::SeqCst;
        let n = self.inner.prune(c, now, limit).await?;
        self.pruned.store(true, SeqCst);
        if self.source_head.load(SeqCst) > c.through {
            self.notice(true);
        }
        Ok(n)
    }
    async fn source_failed(
        &self,
        c: boss_jobs::claims_console::ProjectionCursor,
        at: DateTime<Utc>,
    ) -> Result<(), boss_jobs::claims_console::ClaimsError> {
        self.inner.source_failed(c, at).await
    }
}

#[tokio::test]
async fn native_backlog_is_maintained_between_prefixes_not_only_after_catch_up() {
    use boss_core::audit::AuditWriter;
    let db = boss_testing::TestDb::new().await;
    let events: Vec<_> = (0..1025)
        .map(|_| Event::new("other", "console.backlog", json!({}), Utc::now()))
        .collect();
    boss_events::PgAuditWriter::new(db.pool.clone())
        .write_batch(&events)
        .await
        .unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let history = std::sync::Arc::new(BacklogPruneProbe {
        inner: PgClaimsHistory::for_fold_fixture(db.pool.clone()),
        advances: 0.into(),
        pruned: false.into(),
        source_head: 0.into(),
        notice: std::sync::Mutex::new(Some(tx)),
    });
    let (stop, cancel) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(boss_jobs::claims_console::source::run(
        history,
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        cancel,
    ));
    let maintained = tokio::time::timeout(std::time::Duration::from_secs(30), rx)
        .await
        .expect("bounded harness witness, not production timing")
        .unwrap();
    stop.send(true).unwrap();
    task.await.unwrap();
    assert!(
        maintained,
        "a second source prefix was applied before bounded retention ran on the first lagging prefix"
    );
}

#[tokio::test]
async fn native_source_catches_up_in_bounded_prefixes_and_rebuilds_the_same_claims() {
    use boss_jobs::claims_console::source::advance_native_once;
    let db = boss_testing::TestDb::new().await;
    let history = PgClaimsHistory::for_fold_fixture(db.pool.clone());
    let job = uuid::Uuid::new_v4();
    let step = uuid::Uuid::new_v4();
    for (kind, payload) in [
        ("jobs.pool.published", pool(json!({"state":"unknown"}))),
        ("jobs.job.created", json!({"id":job,"status":"open"})),
        (
            "jobs.step.updated",
            json!({"id":step,"job_id":job,"status":"active","assignee_id":"emp-human","metadata":{}}),
        ),
    ] {
        sqlx::query("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES($1,now(),'jobs',$2,$3)")
            .bind(uuid::Uuid::new_v4()).bind(kind).bind(payload).execute(&db.pool).await.unwrap();
    }
    assert!(!advance_native_once(&history, &db.pool, 1).await.unwrap());
    assert!(
        history.read(None, 24, Utc::now()).await.is_err(),
        "an unfinished source prefix is not current"
    );
    assert!(!advance_native_once(&history, &db.pool, 1).await.unwrap());
    assert!(advance_native_once(&history, &db.pool, 1).await.unwrap());
    let before = history.read(None, 24, Utc::now()).await.unwrap();
    assert_eq!(before.claims.len(), 1);
    assert_eq!(before.claims[0].step_id, step.to_string());
    assert_eq!(before.projection.sequence_distance, Some(0));
    boss_jobs::claims_console::source::rebuild_native_with(&history, &db.pool)
        .await
        .unwrap();
    let rebuilt = history.read(None, 24, Utc::now()).await.unwrap();
    assert_eq!(rebuilt.claims, before.claims);
    assert_eq!(rebuilt.pools, before.pools);
    assert_eq!(rebuilt.as_of, before.as_of);
    assert_eq!(
        rebuilt.projection.generation,
        before.projection.generation + 1
    );
    // The updater owns progress; a GET must not perform source catch-up.
    sqlx::query("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES($1,now(),'jobs','jobs.step.updated',$2)")
        .bind(uuid::Uuid::new_v4()).bind(json!({"id":step,"job_id":job,"status":"completed","metadata":{}})).execute(&db.pool).await.unwrap();
    let history = std::sync::Arc::new(history);
    let (stop, rx) = tokio::sync::watch::channel(false);
    let mut task = tokio::spawn(boss_jobs::claims_console::source::run(
        history.clone(),
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        rx,
    ));
    let observed = async {
        loop {
            if let Ok(reading) = history.read(None, 24, Utc::now()).await
                && reading.as_of > rebuilt.as_of
            {
                assert!(reading.claims.is_empty());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    tokio::select! {
        result=&mut task=>panic!("owned updater exited before observing its source: {result:?}"),
        result=tokio::time::timeout(std::time::Duration::from_secs(30),observed)=>result.expect("bounded harness observation, not a production timing oracle"),
    }
    stop.send(true).unwrap();
    task.await.unwrap();
}

/// Backlog 6a146fbf (2026-11-06): `disk:ci-workspaces` v3 was published
/// live with `station = "forge-ci"` in its `jobs.pool.published` payload,
/// and `GET /api/claims` served that pool with no station, because the
/// native source's payload selection named every pool key but `station`
/// (and `roster`, `program`, `return_verifier`, `identity_pin_unenforced`).
/// So this seeds the TREE's own pool declarations through the seed door
/// the platform seed uses, relays them into `audit_log` the way the
/// outbox does, projects, and reads: every active pool must come back as
/// exactly the row the registry stored, field for field.
#[tokio::test]
async fn every_seeded_pool_reads_back_whole_on_the_claims_console() {
    use boss_jobs::claims_console::source::advance_native_once;
    use boss_jobs::pools::{PgPools, PoolRegistry, load_pools, platform_pools_path, seed_pools};
    let db = boss_testing::TestDb::new().await;
    let pools = load_pools(platform_pools_path()).expect("the tree's pools load");
    assert!(
        pools.iter().any(|p| p.station.is_some()),
        "the tree no longer declares a pool station, so this pins nothing about it"
    );
    let registry = PgPools::new(db.pool.clone());
    let actor = boss_core::actor::ActorId::Automation("platform-workflow-seed".into());
    seed_pools(&registry, &pools, &actor, Utc::now(), false)
        .await
        .expect("seed the tree's pools");
    let bus: std::sync::Arc<dyn boss_core::port::EventBus> = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .expect("relay the published facts into audit_log");
    let history = PgClaimsHistory::for_fold_fixture(db.pool.clone());
    while !advance_native_once(&history, &db.pool, 1024).await.unwrap() {}
    let reading = history.read(None, 24, Utc::now()).await.unwrap();
    let active: Vec<_> = pools
        .iter()
        .filter(|p| p.status == boss_jobs::registry::WorkflowStatus::Active)
        .collect();
    assert!(!active.is_empty());
    for declared in active {
        let stored = registry
            .list_versions(&declared.name)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.version == declared.version)
            .expect("the seed stored the declared version");
        let read = reading
            .pools
            .iter()
            .find(|p| p.pool_key == declared.name)
            .and_then(|p| p.pool.as_ref())
            .unwrap_or_else(|| panic!("{} is missing from the claims read", declared.name));
        assert_eq!(
            serde_json::to_value(read).unwrap(),
            serde_json::to_value(&stored).unwrap(),
            "{} reads back on /api/claims other than the registry stored it",
            declared.name
        );
    }
}

// The immutable requested snapshot and the current projection's operational
// horizon are separate facts. Compare every snapshot field; pin progress too.
fn same_snapshot_with_later_progress(
    before: &boss_jobs::claims_console::ClaimsReading,
    after: &boss_jobs::claims_console::ClaimsReading,
    old_through: i64,
    new_through: i64,
) {
    assert_eq!(before.projection.through, old_through);
    assert_eq!(after.projection.through, new_through);
    assert_eq!(before.projection.generation, after.projection.generation);
    let mut expected = serde_json::to_value(before).unwrap();
    let mut actual = serde_json::to_value(after).unwrap();
    expected.as_object_mut().unwrap().remove("projection");
    actual.as_object_mut().unwrap().remove("projection");
    assert_eq!(
        actual, expected,
        "every historical snapshot field is conserved"
    );
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => {(InMemoryClaimsHistory::default(),())},
        postgres => {let db=boss_testing::TestDb::new().await;(PgClaimsHistory::for_fold_fixture(db.pool.clone()),db)},
    }
    cases {
        general_waits_are_counted_and_malformed_waits_do_not_blank_the_read,
        open_claim_station_is_bound_to_its_original_claim_transaction,
        a_per_step_claim_reads_the_station_its_own_fact_recorded,
        an_unbilled_per_step_claim_reads_its_station_and_a_reclaim_never_inherits_it,
        open_claim_packet_opening_is_not_its_activation_time,
        open_claim_legacy_packet_opening_is_explicitly_null,
        open_claim_missing_run_and_station_are_named_not_inferred,
        open_claim_context_at_an_old_cut_survives_later_changes,
        every_claim_holder_resolves_to_exactly_one_executor_kind,
        a_refused_raw_bucket_max_seeks_the_next_whole_cut_in_that_same_bucket,
        cut_samples_conserve_nonmonotonic_microseconds_and_count_thresholds,
        an_expired_head_is_not_offered_as_a_retained_cut,
        zero_width_cut_ranges_sample_the_same_answerable_endpoints,
        unknown_resource_kinds_are_counted_and_make_every_pool_attention,
        retained_cuts_are_published_with_their_times_and_every_listed_cut_is_answerable,
        both_refusal_doors_share_one_attempt_count_and_binding_ties_use_registry_keys,
        exact_reservations_and_releases_are_generation_bound_historical_vectors,
        a_changed_original_transaction_is_refused_without_mutating_visible_rows,
        unknown_capacity_is_attention_and_exact_string_capacity_survives_historical_read,
        unbilled_active_steps_are_visible_and_later_exits_do_not_rewrite_as_of,
        unobserved_history_is_unavailable_not_a_successful_zero_or_future_sequence,
        duplicate_event_identity_inside_one_batch_cannot_change_the_projection,
        an_undeclared_pool_refusal_is_attention_not_an_invisible_zero,
        a_stale_prepared_batch_cannot_skip_a_new_rebuild_prefix,
        source_lag_and_failure_never_present_a_partial_basis_as_current,
        a_reservation_without_its_active_step_is_unavailable_not_free_capacity,
        bounded_retention_reclaims_in_passes_without_losing_old_anchors_or_current_state,
        a_full_pool_must_keep_draining_and_an_unchanged_republish_cannot_hide_a_stall,
        a_known_pool_with_failed_waiting_claims_and_no_release_is_troubled,
        cumulative_materialized_bytes_are_bounded_without_truncating_valid_waiting_claims,
        an_old_consumer_reads_the_allocations_of_a_live_version_two_claim,
    }
}

// READERS GO FIRST ON THE CONSOLE TOO (backlog 30c1efe8; review bf361c75
// P2). `GET /api/claims` is read by every installed CLI: `boss orient`'s
// `claims_lines` decodes the whole answer into `ClaimsReading`, whose
// `OpenClaim.allocations` embeds `Allocation`, and main's `Allocation`
// refuses an unknown field. So while claim doors still write version 2,
// an allocation the console answers must carry exactly the three fields
// the reader before version 3 knows — not a reserved/borrowed split the
// writer has not recorded (that enrichment rides with the writer, item
// bf2310ed). The frozen `Allocation` here is main's at e11ff0d9.
mod before_version_three {
    use boss_jobs::flow_resources::Amount;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Allocation {
        pub pool: String,
        pub version: i32,
        pub amount: Amount,
    }
}

async fn an_old_consumer_reads_the_allocations_of_a_live_version_two_claim<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    let job = uuid::Uuid::from_u128(3000);
    let step = uuid::Uuid::from_u128(3001);
    let allocation = json!({"pool":"vcpu","version":1,"amount":"1"});
    let claim = json!({"version":2,"id":uuid::Uuid::from_u128(3002),"step_id":step,"job_id":job,"generation":1,"holder":"emp-human","workflow_kind":"fixture","workflow_version":1,"bill":{"vcpu":"1"},"allocations":[allocation],"observed":[{"pool":"vcpu","version":1,"amount":"1","capacity":{"state":"configured","amount":"2"},"held":"0","stock":false}],"claimed_at":"2027-01-15T08:00:00Z","resolved_duration_ms":3_600_000,"lease_until":"2027-01-15T10:00:00Z"});
    let active = json!({"id":step,"job_id":job,"status":"active","assignee_id":"emp-human","metadata":{"resource_claim":claim}});
    let batch = HistoryBatch {
        through: 4,
        through_at: fact(4, "ignored", json!({})).recorded_at,
        facts: vec![
            fact(
                1,
                "jobs.pool.published",
                pool(json!({"state":"configured","amount":"2"})),
            ),
            fact(2, "jobs.job.created", json!({"id":job,"status":"open"})),
            fact(3, "jobs.step.updated", active.clone()),
            fact(
                4,
                boss_jobs::flow_resources::RESERVED,
                json!({"job_id":job,"step_id":step,"claim":claim,"step":active}),
            ),
        ],
    };
    history.materialize(&batch).await.unwrap();
    let reading = history.read(None, 24, batch.through_at).await.unwrap();
    let wire = serde_json::to_value(&reading).unwrap();
    let answered = wire["claims"][0]["allocations"].clone();
    assert_eq!(
        answered,
        json!([allocation]),
        "a version-2 claim's allocation is answered as the token recorded it: {wire:#}"
    );
    for allocation in answered.as_array().unwrap() {
        let old: before_version_three::Allocation = serde_json::from_value(allocation.clone())
            .unwrap_or_else(|e| panic!("the reader before version 3 refuses {allocation}: {e}"));
        assert_eq!(serde_json::to_value(&old).unwrap(), *allocation);
    }
}

async fn a_refused_raw_bucket_max_seeks_the_next_whole_cut_in_that_same_bucket<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    use boss_events::original_group::{GroupReference, OriginalGroup};
    let at = fact(1, "jobs.unrelated", json!({})).recorded_at;
    let mut facts: Vec<_> = [9, 1, 7, 3, 5, 4, 8, 2, 6]
        .into_iter()
        .enumerate()
        .map(|(i, us)| {
            let mut f = fact(i as i64 + 1, "jobs.unrelated", json!({}));
            f.recorded_at = at + chrono::Duration::microseconds(us);
            f.event_at = f.recorded_at;
            f
        })
        .collect();
    let ids = vec![facts[6].event_id, facts[7].event_id];
    let group = OriginalGroup {
        reference: GroupReference {
            id: uuid::Uuid::from_u128(47771),
            schema_version: 1,
            event_count: 2,
            member_hash: OriginalGroup::roster_hash(&ids),
        },
        event_ids: ids,
        audit_sequences: vec![Some(7), Some(8)],
        complete: true,
    };
    facts[6].original_group = Some(group.clone());
    facts[7].original_group = Some(group);
    history
        .materialize(&HistoryBatch {
            through: 10,
            through_at: at + chrono::Duration::microseconds(10),
            facts,
        })
        .await
        .unwrap();
    let reading = history
        .cuts(
            Some(at + chrono::Duration::microseconds(1)),
            Some(at + chrono::Duration::microseconds(9)),
            4,
            at + chrono::Duration::microseconds(10),
        )
        .await
        .unwrap();
    assert_eq!(
        reading.cuts.iter().map(|c| c.seq).collect::<Vec<_>>(),
        vec![1, 3, 6, 8, 9],
        "rawmax7 bisects7/8; nextvalid LAST-bucketcut is3, notseq6 fromanotherbucket"
    );
    assert!(!reading.complete);
    assert!(
        history
            .read(Some(7), 24, at + chrono::Duration::microseconds(10))
            .await
            .is_err()
    );
    history
        .read(Some(8), 24, at + chrono::Duration::microseconds(10))
        .await
        .unwrap();
}

async fn cut_samples_conserve_nonmonotonic_microseconds_and_count_thresholds<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    let at = fact(1, "jobs.unrelated", json!({})).recorded_at;
    let facts = [9, 1, 7, 3, 5, 4, 8, 2, 6]
        .into_iter()
        .enumerate()
        .map(|(i, us)| {
            let mut f = fact(i as i64 + 1, "jobs.unrelated", json!({}));
            f.recorded_at = at + chrono::Duration::microseconds(us);
            f.event_at = f.recorded_at;
            f
        })
        .collect();
    history
        .materialize(&HistoryBatch {
            through: 10,
            through_at: at + chrono::Duration::microseconds(10),
            facts,
        })
        .await
        .unwrap();
    for (end, expected, complete) in [
        (3, vec![2, 4, 8], true),
        (4, vec![2, 4, 6, 8], true),
        (5, vec![2, 4, 6, 8], false),
        (8, vec![2, 6, 7, 8, 9], false),
        (9, vec![1, 6, 7, 8, 9], false),
    ] {
        let reading = history
            .cuts(
                Some(at + chrono::Duration::microseconds(1)),
                Some(at + chrono::Duration::microseconds(end)),
                4,
                at + chrono::Duration::microseconds(10),
            )
            .await
            .unwrap();
        assert_eq!(
            reading.cuts.iter().map(|c| c.seq).collect::<Vec<_>>(),
            expected,
            "end={end}"
        );
        assert_eq!(reading.complete, complete, "end={end}");
        let basis = reading.read_basis.as_ref().unwrap();
        assert_eq!(basis.range_count_exact, complete);
        assert_eq!(basis.range_count, if end < 5 { end as usize } else { 5 });
        assert_eq!(
            reading.first_retained.seq, 1,
            "global earliest sequence, not earliest timestamp or requested endpoint"
        );
    }
}

async fn an_expired_head_is_not_offered_as_a_retained_cut<H: ClaimsHistory>(history: &H, _: &str) {
    let f = fact(1, "jobs.unrelated", json!({}));
    let at = f.recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 1,
            through_at: at,
            facts: vec![f],
        })
        .await
        .unwrap();
    let now = at + chrono::Duration::hours(169);
    let reading = history.cuts(None, None, 4, now).await;
    assert!(
        reading.is_err(),
        "no retained cut: an expired head must not be offered as answerable: {reading:?}"
    );
}

async fn zero_width_cut_ranges_sample_the_same_answerable_endpoints<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    let at = fact(1, "jobs.unrelated", json!({})).recorded_at;
    let facts = (1..=9)
        .map(|seq| {
            let mut f = fact(seq, "jobs.unrelated", json!({}));
            f.recorded_at = at;
            f.event_at = at;
            f
        })
        .collect();
    history
        .materialize(&HistoryBatch {
            through: 9,
            through_at: at,
            facts,
        })
        .await
        .unwrap();
    let reading = history.cuts(Some(at), Some(at), 4, at).await.unwrap();
    assert!(!reading.complete);
    assert_eq!(reading.first_retained.seq, 1);
    assert_eq!(
        reading.cuts.iter().map(|c| c.seq).collect::<Vec<_>>(),
        vec![1, 9]
    );
    for cut in reading.cuts {
        history.read(Some(cut.seq), 24, at).await.unwrap();
    }
}

async fn general_waits_are_counted_and_malformed_waits_do_not_blank_the_read<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    let job = uuid::Uuid::from_u128(1800);
    let waits = [
        json!({"on":"packet","id":"6bc062b1-8d49-464f-abe1-6912c7d25fa3","since":"2026-10-25T20:48:20Z","why":"prerequisite must land"}),
        json!({"on":"person","id":"emp-david","since":"2026-10-25T20:48:20Z","why":"decision"}),
        json!({"resource":"vcpu","reason":"capacity-exhausted"}),
        json!({"on":"packet","id":"missing-required-evidence"}),
        json!(17),
    ];
    let mut facts = vec![fact(
        1,
        "jobs.job.created",
        json!({"id":job,"status":"open"}),
    )];
    for (i, wait) in waits.iter().enumerate() {
        facts.push(fact(
            i as i64 + 2,
            "jobs.step.updated",
            json!({
                "id":uuid::Uuid::from_u128(1801+i as u128),"job_id":job,"status":"ready",
                "metadata":{"waiting_on":wait}
            }),
        ));
    }
    let at = facts.last().unwrap().recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 6,
            through_at: at,
            facts,
        })
        .await
        .unwrap();
    let reading = history
        .read(None, 24, at)
        .await
        .expect("a legitimate general wait or malformed row must not darken the read");
    assert_eq!(
        reading.waiting_on.len(),
        1,
        "resource waits keep their existing representation"
    );
    assert_eq!(reading.waiting_on[0].pool, "vcpu");
    let wire = serde_json::to_value(&reading).unwrap();
    assert_eq!(
        wire["projection"]["other_waits"],
        json!([
            {"kind":"packet","count":1},{"kind":"person","count":1}
        ])
    );
    let attention = wire["projection"]["wait_attention"]
        .as_array()
        .expect("malformed waits must be named in the basis");
    assert_eq!(attention.len(), 2);
    for (i, row) in attention.iter().enumerate() {
        assert_eq!(row["job_id"], job.to_string());
        assert_eq!(
            row["step_id"],
            uuid::Uuid::from_u128(1804 + i as u128).to_string()
        );
        assert_eq!(row["kind"], "malformed-waiting-on");
        assert!(row["reason"].as_str().is_some_and(|s| !s.is_empty()));
    }
    let after = fact(
        7,
        "jobs.step.updated",
        json!({"id":uuid::Uuid::from_u128(1801),"job_id":job,"status":"completed","metadata":{}}),
    );
    history
        .materialize(&HistoryBatch {
            through: 7,
            through_at: after.recorded_at,
            facts: vec![after],
        })
        .await
        .unwrap();
    let old = history.read(Some(6), 24, at).await.unwrap();
    same_snapshot_with_later_progress(&reading, &old, 6, 7);
    assert_eq!(
        serde_json::to_value(old).unwrap()["projection"]["other_waits"],
        wire["projection"]["other_waits"],
        "later state cannot rewrite historical wait counts"
    );
}

async fn open_claim_station_is_bound_to_its_original_claim_transaction<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    use boss_events::original_group::{GroupReference, OriginalGroup};
    let step = json!({"id":"station-step","job_id":"station-job","status":"active","assignee_id":"agent-one","metadata":{"agent_run":"run-one"}});
    let mut state = fact(2, "jobs.step.updated", step.clone());
    let mut station = fact(3, boss_jobs::events::STATION_CLAIMED, json!({}));
    let receipt = json!({"version":2,"event_id":station.event_id,"station":{"name":"original-build"},"selected":{"job_id":"station-job","step_id":"station-step"},"claimed_step":step,"selected_at":state.event_at});
    station.payload = json!({"receipt_json":receipt.to_string()}).to_string();
    let ids = vec![state.event_id, station.event_id];
    let group = OriginalGroup {
        reference: GroupReference {
            id: uuid::Uuid::from_u128(70001),
            schema_version: 1,
            event_count: 2,
            member_hash: OriginalGroup::roster_hash(&ids),
        },
        event_ids: ids,
        audit_sequences: vec![Some(2), Some(3)],
        complete: true,
    };
    state.original_group = Some(group.clone());
    station.original_group = Some(group);
    let at = station.recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 3,
            through_at: at,
            facts: vec![
                fact(
                    1,
                    "jobs.job.created",
                    json!({"id":"station-job","status":"open"}),
                ),
                state,
                station,
            ],
        })
        .await
        .unwrap();
    let original = serde_json::to_value(history.read(Some(3), 24, at).await.unwrap()).unwrap();
    assert_eq!(original["claims"][0]["station"], "original-build");
    assert_eq!(
        original["claims"][0]["station_unavailable"],
        serde_json::Value::Null
    );
    let at = fact(5, "ignored", json!({})).recorded_at;
    history.materialize(&HistoryBatch{through:5,through_at:at,facts:vec![
        fact(4,"jobs.step.updated",json!({"id":"station-step","job_id":"station-job","status":"ready","assignee_id":"agent-one","metadata":{}})),
        fact(5,"jobs.step.updated",json!({"id":"station-step","job_id":"station-job","status":"active","assignee_id":"agent-one","metadata":{"agent_run":"run-two"}})),
    ]}).await.unwrap();
    let head = serde_json::to_value(history.read(None, 24, at).await.unwrap()).unwrap();
    assert_eq!(
        head["claims"][0]["station"],
        serde_json::Value::Null,
        "same holder reacquiring must not inherit an old station receipt"
    );
    assert_eq!(
        head["claims"][0]["station_unavailable"],
        "unmatched-claim-receipt"
    );
    let old = serde_json::to_value(history.read(Some(3), 24, at).await.unwrap()).unwrap();
    assert_eq!(old["claims"], original["claims"]);
}

/// THE PER-STEP CLAIM DOOR RECORDS ITS STATION (backlog 2682c418). Only
/// `claim-next` wrote a station receipt, so every claim made through
/// `POST /api/jobs/{id}/steps/{sid}/claim` — every robot claim `boss
/// dispatch` makes — read `not-recorded` (32 of 96 open claims on
/// 2026-11-06). The door now records the station its step is queued at
/// on the claim's own fact, and the fold reads it into the same
/// `station-claim` row a `jobs.station.claimed` receipt makes, bound to
/// the same claim transaction. A claim whose fact names no station still
/// reads `not-recorded`, by name, and a later activation by the same
/// holder does not inherit the station of an earlier one.
async fn a_per_step_claim_reads_the_station_its_own_fact_recorded<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    use boss_events::original_group::{GroupReference, OriginalGroup};
    let grouped = |mut fact: SequencedFact, group: u128| {
        let ids = vec![fact.event_id];
        fact.original_group = Some(OriginalGroup {
            reference: GroupReference {
                id: uuid::Uuid::from_u128(group),
                schema_version: 1,
                event_count: 1,
                member_hash: OriginalGroup::roster_hash(&ids),
            },
            event_ids: ids,
            audit_sequences: vec![Some(fact.seq)],
            complete: true,
        });
        fact
    };
    let claim = |id: &str, station: Option<&str>| {
        let mut body = json!({"id":id,"job_id":"door-job","status":"active",
            "assignee_id":"agent-one","metadata":{"agent_run":"run-one"},
            "_executor_kind":"robot"});
        if let Some(station) = station {
            body[boss_jobs::events::CLAIM_STATION_KEY] = json!(station);
        }
        body
    };
    let at = fact(3, "ignored", json!({})).recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 3,
            through_at: at,
            facts: vec![
                fact(
                    1,
                    "jobs.job.created",
                    json!({"id":"door-job","status":"open","opened_at":"2027-01-14T08:00:00Z"}),
                ),
                grouped(
                    fact(
                        2,
                        "jobs.step.updated",
                        claim("queued-step", Some("a.platform-admin.opus-5-5")),
                    ),
                    80001,
                ),
                grouped(
                    fact(3, "jobs.step.updated", claim("bare-step", None)),
                    80002,
                ),
            ],
        })
        .await
        .unwrap();
    let wire = serde_json::to_value(history.read(None, 24, at).await.unwrap()).unwrap();
    let claim_of = |wire: &serde_json::Value, id: &str| {
        wire["claims"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["step_id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no open claim for {id}: {wire:#}"))
    };
    let queued = claim_of(&wire, "queued-step");
    assert_eq!(queued["station"], "a.platform-admin.opus-5-5", "{queued}");
    assert_eq!(queued["station_unavailable"], serde_json::Value::Null);
    let bare = claim_of(&wire, "bare-step");
    assert_eq!(bare["station"], serde_json::Value::Null, "{bare}");
    assert_eq!(bare["station_unavailable"], "not-recorded");

    // The holder's own later write (a run edge, a metadata merge) keeps
    // the activation and so keeps its station; a release and a fresh
    // claim that names none does not inherit it.
    let at = fact(6, "ignored", json!({})).recorded_at;
    let mut later = claim("queued-step", None);
    later["metadata"]["notes"] = json!("kept");
    history
        .materialize(&HistoryBatch {
            through: 4,
            through_at: at,
            facts: vec![grouped(fact(4, "jobs.step.updated", later), 80003)],
        })
        .await
        .unwrap();
    let kept = serde_json::to_value(history.read(None, 24, at).await.unwrap()).unwrap();
    assert_eq!(
        claim_of(&kept, "queued-step")["station"],
        "a.platform-admin.opus-5-5"
    );
    history
        .materialize(&HistoryBatch {
            through: 6,
            through_at: at,
            facts: vec![
                grouped(
                    fact(
                        5,
                        "jobs.step.updated",
                        json!({"id":"queued-step","job_id":"door-job","status":"ready","assignee_id":null,"metadata":{}}),
                    ),
                    80004,
                ),
                grouped(fact(6, "jobs.step.updated", claim("queued-step", None)), 80005),
            ],
        })
        .await
        .unwrap();
    let head = serde_json::to_value(history.read(None, 24, at).await.unwrap()).unwrap();
    let again = claim_of(&head, "queued-step");
    assert_eq!(again["station"], serde_json::Value::Null, "{again}");
    // The earlier activation's receipt is still on the step, so the read
    // names it as unmatched rather than as never recorded — the same
    // answer a stale claim-next receipt gets.
    assert_eq!(again["station_unavailable"], "unmatched-claim-receipt");
    let old = serde_json::to_value(history.read(Some(3), 24, at).await.unwrap()).unwrap();
    assert_eq!(
        old["claims"], wire["claims"],
        "a later claim rewrites no cut"
    );
}

/// AN UNBILLED PER-STEP CLAIM READS ITS STATION (backlog 971a6190). The
/// case above folds claims that each carry an original transaction, so
/// the receipt bound by `activation_group` matched. A robot claim made
/// through the per-step door today carries NEITHER an activation group
/// nor a resource claim, and the match required one of the two: both
/// claims made after 2682c418 converged carried `_station` on their own
/// fact and still read `not-recorded` (measured 2026-11-06 13:47Z).
/// When neither side carries either, the receipt is bound to THIS
/// activation by sequence: only the fact that starts an activation
/// writes a receipt, and the step row keeps that fact's sequence across
/// the holder's later writes, so a receipt from an earlier activation of
/// the same step by the same holder sits at a different sequence and
/// can never attach to a later one.
async fn an_unbilled_per_step_claim_reads_its_station_and_a_reclaim_never_inherits_it<
    H: ClaimsHistory,
>(
    history: &H,
    _: &str,
) {
    use boss_events::original_group::{GroupReference, OriginalGroup};
    let station = boss_jobs::events::CLAIM_STATION_KEY;
    let claim = |id: &str, at: Option<&str>| {
        let mut body = json!({"id":id,"job_id":"unbilled-job","status":"active",
            "assignee_id":"agent-one","metadata":{"agent_run":"run-one"},
            "_executor_kind":"robot"});
        if let Some(name) = at {
            body[station] = json!(name);
        }
        body
    };
    let release = |id: &str| json!({"id":id,"job_id":"unbilled-job","status":"ready","assignee_id":null,"metadata":{}});
    let claim_of = |wire: &serde_json::Value, id: &str| {
        wire["claims"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["step_id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no open claim for {id}: {wire:#}"))
    };
    // A grouped claim beside them: the rule for a claim that carries its
    // original transaction is unchanged.
    let mut grouped = fact(
        4,
        "jobs.step.updated",
        claim("grouped-step", Some("q.platform-admin.task")),
    );
    let ids = vec![grouped.event_id];
    grouped.original_group = Some(OriginalGroup {
        reference: GroupReference {
            id: uuid::Uuid::from_u128(971_001),
            schema_version: 1,
            event_count: 1,
            member_hash: OriginalGroup::roster_hash(&ids),
        },
        event_ids: ids,
        audit_sequences: vec![Some(4)],
        complete: true,
    });
    let at = fact(4, "ignored", json!({})).recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 4,
            through_at: at,
            facts: vec![
                fact(
                    1,
                    "jobs.job.created",
                    json!({"id":"unbilled-job","status":"open","opened_at":"2027-01-14T08:00:00Z"}),
                ),
                fact(
                    2,
                    "jobs.step.updated",
                    claim("unbilled-step", Some("a.platform-admin.opus-5-5")),
                ),
                fact(3, "jobs.step.updated", claim("bare-step", None)),
                grouped,
            ],
        })
        .await
        .unwrap();
    let first = serde_json::to_value(history.read(None, 24, at).await.unwrap()).unwrap();
    let read = claim_of(&first, "unbilled-step");
    assert_eq!(
        read["resource_acquisition"],
        serde_json::Value::Null,
        "{read}"
    );
    assert_eq!(read["station"], "a.platform-admin.opus-5-5", "{read}");
    assert_eq!(read["station_unavailable"], serde_json::Value::Null);
    let read = claim_of(&first, "bare-step");
    assert_eq!(read["station"], serde_json::Value::Null, "{read}");
    assert_eq!(read["station_unavailable"], "not-recorded");
    let read = claim_of(&first, "grouped-step");
    assert_eq!(read["station"], "q.platform-admin.task", "{read}");

    // The holder's own later write keeps the activation, so keeps its
    // station.
    let mut later = claim("unbilled-step", None);
    later["metadata"]["notes"] = json!("kept");
    let at = fact(5, "ignored", json!({})).recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 5,
            through_at: at,
            facts: vec![fact(5, "jobs.step.updated", later)],
        })
        .await
        .unwrap();
    let kept = serde_json::to_value(history.read(None, 24, at).await.unwrap()).unwrap();
    assert_eq!(
        claim_of(&kept, "unbilled-step")["station"],
        "a.platform-admin.opus-5-5"
    );

    // Released, then claimed again by the SAME holder on a fact that
    // records no station: the earlier activation's receipt is still the
    // step's latest, and it must not attach.
    let at = fact(7, "ignored", json!({})).recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 7,
            through_at: at,
            facts: vec![
                fact(6, "jobs.step.updated", release("unbilled-step")),
                fact(7, "jobs.step.updated", claim("unbilled-step", None)),
            ],
        })
        .await
        .unwrap();
    let again = serde_json::to_value(history.read(None, 24, at).await.unwrap()).unwrap();
    let read = claim_of(&again, "unbilled-step");
    assert_eq!(
        read["station"],
        serde_json::Value::Null,
        "a re-claim must not inherit the earlier activation's station: {read}"
    );
    assert_eq!(read["station_unavailable"], "unmatched-claim-receipt");

    // A third activation that records its own station reads that one.
    let at = fact(9, "ignored", json!({})).recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 9,
            through_at: at,
            facts: vec![
                fact(8, "jobs.step.updated", release("unbilled-step")),
                fact(
                    9,
                    "jobs.step.updated",
                    claim("unbilled-step", Some("q.platform-admin.task")),
                ),
            ],
        })
        .await
        .unwrap();
    let third = serde_json::to_value(history.read(None, 24, at).await.unwrap()).unwrap();
    assert_eq!(
        claim_of(&third, "unbilled-step")["station"],
        "q.platform-admin.task"
    );
    let old = serde_json::to_value(history.read(Some(4), 24, at).await.unwrap()).unwrap();
    assert_eq!(
        old["claims"], first["claims"],
        "a later claim rewrites no cut"
    );
}

#[tokio::test]
async fn native_station_artifacts_respect_source_owner_without_dropping_prefix_facts() {
    use boss_core::audit::AuditWriter;
    let db = boss_testing::TestDb::new().await;
    let at = Utc::now();
    let events = vec![
        Event::new(
            "jobs",
            "jobs.job.created",
            json!({"id":"owner-job","status":"open"}),
            at,
        ),
        Event::new(
            "jobs",
            "jobs.step.updated",
            json!({"id":"owner-step","job_id":"owner-job","status":"active","assignee_id":"agent-one","metadata":{"agent_run":"owner-run"}}),
            at,
        ),
        Event::new(
            "other-owner",
            boss_jobs::events::STATION_CLAIMED,
            json!({}),
            at,
        ),
        Event::new(
            "other-owner",
            boss_jobs::events::STATION_SELECTION_EVIDENCE,
            json!({}),
            at,
        ),
    ];
    let ids: Vec<_> = events.iter().map(|event| event.id).collect();
    boss_events::PgAuditWriter::new(db.pool.clone())
        .write_batch(&events)
        .await
        .unwrap();
    let history = PgClaimsHistory::for_fold_fixture(db.pool.clone());
    boss_jobs::claims_console::source::advance_native_once(&history, &db.pool, 1024)
        .await
        .expect("foreign source kinds are not jobs receipt artifacts");
    let before = history.read(None, 24, Utc::now()).await.unwrap();
    assert_eq!(before.claims.len(), 1);
    assert_eq!(
        serde_json::to_value(&before).unwrap()["claims"][0]["run"],
        "owner-run"
    );
    let head: i64 = sqlx::query_scalar("SELECT max(id) FROM audit_log")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(history.cursor().await.unwrap().through, head);
    let retained: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs_claim_history_facts WHERE event_id=ANY($1)")
            .bind(&ids)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        retained, 4,
        "owner filtering must not remove facts from prefix identity/group conservation"
    );
    boss_jobs::claims_console::source::rebuild_native_with(&history, &db.pool)
        .await
        .unwrap();
    assert_eq!(
        history.read(None, 24, Utc::now()).await.unwrap().claims,
        before.claims
    );
}

#[tokio::test]
async fn ungrouped_legacy_station_context_does_not_invent_acquisition_provenance() {
    use boss_core::audit::AuditWriter;
    let db = boss_testing::TestDb::new().await;
    let at = Utc::now();
    let job = boss_core::job::JobId::new();
    let mut state = boss_core::job::Step::new(job, "task", "Legacy station", 0);
    state.status = boss_core::job::StepStatus::Active;
    state.assignee_id = Some("agent-one".into());
    let step = serde_json::to_value(&state).unwrap();
    let mut selected = Event::new("jobs", boss_jobs::events::STATION_CLAIMED, json!({}), at);
    use sha2::Digest;
    let station = json!({"name":"original-native"});
    let station_hash = hex::encode(sha2::Sha256::digest(boss_core::job::canonical_json_bytes(
        &station,
    )));
    selected.payload["job_id"] = json!(job);
    selected.payload["receipt_json"]=json!(json!({"version":2,"request_id":uuid::Uuid::new_v4(),"actor":"agent-one","event_id":selected.id,"station":station,"station_hash":station_hash,"selected":{"job_id":job,"step_id":state.id},"claimed_step":step,"selected_at":at}).to_string());
    let events = vec![
        Event::new(
            "jobs",
            "jobs.job.created",
            json!({"id":job,"status":"open"}),
            at,
        ),
        Event::new("jobs", "jobs.step.updated", step, at),
        selected.clone(),
    ];
    boss_events::PgAuditWriter::new(db.pool.clone())
        .write_batch(&events)
        .await
        .unwrap();
    let history = PgClaimsHistory::for_fold_fixture(db.pool.clone());
    boss_jobs::claims_console::source::advance_native_once(&history, &db.pool, 1024)
        .await
        .unwrap();
    let before = history.read(None, 24, Utc::now()).await.unwrap();
    // PgAuditWriter does not record an original producer transaction group.
    // An unbilled legacy fact therefore cannot prove which acquisition it names.
    assert_eq!(
        serde_json::to_value(&before).unwrap()["claims"][0]["station"],
        serde_json::Value::Null
    );
    assert_eq!(
        serde_json::to_value(&before).unwrap()["claims"][0]["station_unavailable"],
        "unmatched-claim-receipt"
    );
    boss_jobs::claims_console::source::rebuild_native_with(&history, &db.pool)
        .await
        .unwrap();
    assert_eq!(
        history.read(None, 24, Utc::now()).await.unwrap().claims,
        before.claims
    );
    // The historical redundant copy is itself validator input. Projection
    // selection must not hide a disagreement that the jobs rebuilder refuses.
    let mut contradictory = selected;
    contradictory.id = uuid::Uuid::new_v4();
    let mut receipt: serde_json::Value =
        serde_json::from_str(contradictory.payload["receipt_json"].as_str().unwrap()).unwrap();
    receipt["event_id"] = json!(contradictory.id);
    receipt["request_id"] = json!(uuid::Uuid::new_v4());
    contradictory.payload["receipt_json"] = json!(receipt.to_string());
    contradictory.payload["receipt"] = json!({"contradicts":"the exact original JSON"});
    boss_events::PgAuditWriter::new(db.pool.clone())
        .write(&contradictory)
        .await
        .unwrap();
    let cursor = history.cursor().await.unwrap();
    let error = boss_jobs::claims_console::source::advance_native_once(&history, &db.pool, 1024)
        .await
        .expect_err("a historical contradictory receipt must remain visible to the validator");
    assert!(error.to_string().contains("receipt disagrees"), "{error}");
    assert_eq!(history.cursor().await.unwrap(), cursor);
    assert!(
        history.read(None, 24, Utc::now()).await.is_err(),
        "a failed native receipt validation must mark the latest basis unavailable"
    );
    assert_eq!(
        history
            .read(Some(cursor.through), 24, Utc::now())
            .await
            .unwrap()
            .claims,
        before.claims,
        "a failed new prefix must preserve the explicitly requested older cut"
    );
}

#[tokio::test]
async fn native_source_preserves_open_claim_packet_and_run_provenance() {
    let db = boss_testing::TestDb::new().await;
    for (kind, payload) in [
        (
            "jobs.job.created",
            json!({"id":"native-context-job","status":"open","opened_at":"2027-01-14T08:00:00Z"}),
        ),
        (
            "jobs.step.updated",
            json!({"id":"native-context-step","job_id":"native-context-job","status":"active","assignee_id":"agent-one","metadata":{"agent_run":"native-run"}}),
        ),
    ] {
        sqlx::query("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES($1,now(),'jobs',$2,$3)").bind(uuid::Uuid::new_v4()).bind(kind).bind(payload).execute(&db.pool).await.unwrap();
    }
    let history = PgClaimsHistory::for_fold_fixture(db.pool.clone());
    boss_jobs::claims_console::source::advance_native_once(&history, &db.pool, 1024)
        .await
        .unwrap();
    let wire = serde_json::to_value(history.read(None, 24, Utc::now()).await.unwrap()).unwrap();
    assert_eq!(
        wire["claims"][0]["packet_opened_at"],
        "2027-01-14T08:00:00Z"
    );
    assert_eq!(wire["claims"][0]["run"], "native-run");
    boss_jobs::claims_console::source::rebuild_native_with(&history, &db.pool)
        .await
        .unwrap();
    let rebuilt = serde_json::to_value(history.read(None, 24, Utc::now()).await.unwrap()).unwrap();
    assert_eq!(rebuilt["claims"], wire["claims"]);
}

async fn context_fixture<H: ClaimsHistory>(history: &H, opened: bool, run: Option<&str>) {
    let mut packet = json!({"id":"context-job","status":"open"});
    if opened {
        packet["opened_at"] = json!("2027-01-14T08:00:00Z");
    }
    let mut metadata = json!({});
    if let Some(run) = run {
        metadata["agent_run"] = json!(run);
    }
    history.materialize(&HistoryBatch {
        through: 2,
        through_at: fact(2, "ignored", json!({})).recorded_at,
        facts: vec![
            fact(1, "jobs.job.created", packet),
            fact(2, "jobs.step.updated", json!({"id":"context-step","job_id":"context-job","status":"active","assignee_id":"agent-one","metadata":metadata})),
        ],
    }).await.unwrap();
}

async fn open_claim_packet_opening_is_not_its_activation_time<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    context_fixture(history, true, Some("run-one")).await;
    let at = fact(2, "ignored", json!({})).recorded_at;
    let wire = serde_json::to_value(history.read(None, 24, at).await.unwrap()).unwrap();
    assert_eq!(
        wire["claims"][0]["packet_opened_at"],
        "2027-01-14T08:00:00Z"
    );
    assert_ne!(
        wire["claims"][0]["packet_opened_at"],
        wire["claims"][0]["since"]
    );
    assert_eq!(wire["claims"][0]["run"], "run-one");
}

async fn open_claim_legacy_packet_opening_is_explicitly_null<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    context_fixture(history, false, None).await;
    let at = fact(2, "ignored", json!({})).recorded_at;
    let wire = serde_json::to_value(history.read(None, 24, at).await.unwrap()).unwrap();
    assert_eq!(
        wire["claims"][0].get("packet_opened_at"),
        Some(&serde_json::Value::Null)
    );
}

async fn open_claim_missing_run_and_station_are_named_not_inferred<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    context_fixture(history, true, None).await;
    let at = fact(2, "ignored", json!({})).recorded_at;
    let wire = serde_json::to_value(history.read(None, 24, at).await.unwrap()).unwrap();
    let claim = &wire["claims"][0];
    assert_eq!(claim.get("run"), Some(&serde_json::Value::Null));
    assert_eq!(claim["run_unavailable"], "not-recorded");
    assert_eq!(claim.get("station"), Some(&serde_json::Value::Null));
    assert_eq!(claim["station_unavailable"], "not-recorded");
}

async fn open_claim_context_at_an_old_cut_survives_later_changes<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    context_fixture(history, true, Some("run-one")).await;
    let at = fact(4, "ignored", json!({})).recorded_at;
    let before = serde_json::to_value(history.read(Some(2), 24, at).await.unwrap()).unwrap();
    assert_eq!(before["claims"][0]["run"], "run-one");
    history.materialize(&HistoryBatch {
        through: 4,
        through_at: at,
        facts: vec![
            fact(3, "jobs.job.updated", json!({"id":"context-job","status":"open","opened_at":"2027-01-14T08:00:00Z"})),
            fact(4, "jobs.step.updated", json!({"id":"context-step","job_id":"context-job","status":"active","assignee_id":"agent-two","metadata":{"agent_run":"run-two"}})),
        ],
    }).await.unwrap();
    let old = serde_json::to_value(history.read(Some(2), 24, at).await.unwrap()).unwrap();
    let head = serde_json::to_value(history.read(None, 24, at).await.unwrap()).unwrap();
    assert_eq!(old["claims"], before["claims"]);
    assert_eq!(head["claims"][0]["run"], "run-two");
    assert_eq!(
        head["claims"][0]["packet_opened_at"],
        "2027-01-14T08:00:00Z"
    );
}

/// The false-fourth-kind check over the claims history (factory taxonomy
/// M1, backlog 3ff2ef60): every holder an open claim names resolves to
/// exactly one executor kind. A claim the door recorded carries its kind
/// (`_executor_kind` on the claim's step fact) and the fold keeps it
/// across the holder's later writes; a claim recorded before the door
/// did — or by a writer that does not — is given the kind of its
/// holder's id, and says so (`derived-from-id`), so the backfill is the
/// fold itself: rebuilt from the log, never a hand row.
async fn every_claim_holder_resolves_to_exactly_one_executor_kind<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    let step = |id: &str, holder: Option<&str>, kind: Option<&str>| {
        let mut body = json!({"id":id,"job_id":"kinds-job","metadata":{},
            "status": if holder.is_some() {"active"} else {"ready"}, "assignee_id":holder});
        if let Some(kind) = kind {
            body["_executor_kind"] = json!(kind);
        }
        body
    };
    let at = fact(9, "ignored", json!({})).recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 9,
            through_at: at,
            facts: vec![
                fact(
                    1,
                    "jobs.job.created",
                    json!({"id":"kinds-job","status":"open","opened_at":"2027-01-14T08:00:00Z"}),
                ),
                fact(
                    2,
                    "jobs.step.updated",
                    step("s-person", Some("emp-032"), None),
                ),
                fact(
                    3,
                    "jobs.step.updated",
                    step("s-legacy", Some("claude:opus-5"), None),
                ),
                fact(
                    4,
                    "jobs.step.updated",
                    step("s-agent", Some("agent-claude"), Some("robot")),
                ),
                fact(
                    5,
                    "jobs.step.updated",
                    step("s-auto", Some("automation:boss-step"), None),
                ),
                // A registered row that declares automaton: the door records
                // the row's kind, which the id alone could not give.
                fact(
                    6,
                    "jobs.step.updated",
                    step("s-declared", Some("agent-gate"), Some("automaton")),
                ),
                // The holder's own later write carries no kind; the claim's stays.
                fact(7, "jobs.step.updated", {
                    let mut later = step("s-declared", Some("agent-gate"), None);
                    later["metadata"]["agent_run"] = json!("run-gate");
                    later
                }),
                // Released, then claimed by someone else: the next holder
                // does not inherit the last one's kind.
                fact(8, "jobs.step.updated", step("s-agent", None, None)),
                fact(
                    9,
                    "jobs.step.updated",
                    step("s-agent", Some("emp-040"), None),
                ),
            ],
        })
        .await
        .unwrap();
    let wire = serde_json::to_value(history.read(None, 24, at).await.unwrap()).unwrap();
    let claims: std::collections::BTreeMap<String, &serde_json::Value> = wire["claims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["step_id"].as_str().unwrap().to_string(), c))
        .collect();
    let want = [
        ("s-person", "emp-032", "person", "derived-from-id"),
        ("s-legacy", "claude:opus-5", "robot", "derived-from-id"),
        (
            "s-auto",
            "automation:boss-step",
            "automaton",
            "derived-from-id",
        ),
        ("s-declared", "agent-gate", "automaton", "recorded"),
        ("s-agent", "emp-040", "person", "derived-from-id"),
    ];
    assert_eq!(claims.len(), want.len(), "{wire:#}");
    for (step_id, holder, kind, basis) in want {
        let claim = claims[step_id];
        assert_eq!(claim["holder"], holder, "{step_id}");
        assert_eq!(claim["executor_kind"], kind, "{step_id}");
        assert_eq!(claim["executor_kind_basis"], basis, "{step_id}");
    }
    // One kind per holder across the whole reading — none with zero,
    // none with two.
    let mut kinds: std::collections::BTreeMap<&str, std::collections::BTreeSet<&str>> =
        Default::default();
    for claim in claims.values() {
        kinds
            .entry(claim["holder"].as_str().unwrap())
            .or_default()
            .extend(claim["executor_kind"].as_str());
    }
    for (holder, kinds) in kinds {
        assert_eq!(kinds.len(), 1, "{holder} resolves to {kinds:?}");
    }
}

async fn unknown_resource_kinds_are_counted_and_make_every_pool_attention<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    let job = uuid::Uuid::from_u128(1700);
    let step = uuid::Uuid::from_u128(1701);
    let claim = json!({"version":1,"id":uuid::Uuid::from_u128(1702),"step_id":step,"job_id":job,"generation":1,"holder":"emp-human","workflow_kind":"fixture","workflow_version":1,"bill":{"vcpu":"1"},"allocations":[{"pool":"vcpu","version":1,"amount":"1"}],"observed":[{"pool":"vcpu","version":1,"amount":"1","capacity":{"state":"configured","amount":"2"},"held":"0","stock":false}]});
    let active = json!({"id":step,"job_id":job,"status":"active","assignee_id":"emp-human","metadata":{"resource_claim":claim}});
    let mut ram = pool(json!({"state":"configured","amount":"2"}));
    ram["name"] = json!("ram_gb");
    let initial = HistoryBatch {
        through: 5,
        through_at: fact(5, "ignored", json!({})).recorded_at,
        facts: vec![
            fact(
                1,
                "jobs.pool.published",
                pool(json!({"state":"configured","amount":"2"})),
            ),
            fact(2, "jobs.pool.published", ram),
            fact(3, "jobs.job.created", json!({"id":job,"status":"open"})),
            fact(4, "jobs.step.updated", active.clone()),
            fact(
                5,
                boss_jobs::flow_resources::RESERVED,
                json!({"job_id":job,"step_id":step,"claim":claim,"step":active}),
            ),
        ],
    };
    history.materialize(&initial).await.unwrap();
    let before = history.read(None, 24, initial.through_at).await.unwrap();
    let batch = HistoryBatch {
        through: 8,
        through_at: fact(8, "ignored", json!({})).recorded_at,
        facts: vec![
            fact(6, "jobs.stock.obligated", json!({"claim":claim})),
            fact(7, "jobs.stock.obligated", json!({"claim":claim})),
            fact(8, "jobs.unrelated", json!({"note":"ignored"})),
        ],
    };
    history.materialize(&batch).await.unwrap();
    let reading = history.read(None, 24, batch.through_at).await.unwrap();
    let wire = serde_json::to_value(&reading).unwrap();
    assert_eq!(
        wire["projection"]["unknown_kinds"],
        json!([{"kind":"jobs.stock.obligated","first_seq":6,"count":2}]),
        "unknown resource facts cannot silently disappear"
    );
    assert_eq!(reading.pools.len(), 2);
    assert!(
        reading
            .pools
            .iter()
            .all(|p| p.state == boss_jobs::regions::RegionState::Attention
                && p.band.as_deref() == Some("unknown-resource-kind"))
    );
    assert_eq!(reading.claims, before.claims);
    history.materialize(&batch).await.unwrap();
    assert_eq!(
        history.read(None, 24, batch.through_at).await.unwrap(),
        reading,
        "duplicate immutable facts cannot count twice"
    );
    let historical = history.read(Some(5), 24, batch.through_at).await.unwrap();
    assert_eq!(
        serde_json::to_value(historical).unwrap()["projection"]["unknown_kinds"],
        json!([])
    );
}

async fn cumulative_materialized_bytes_are_bounded_without_truncating_valid_waiting_claims<
    H: ClaimsHistory,
>(
    history: &H,
    _: &str,
) {
    let job = uuid::Uuid::from_u128(1400);
    let reason = "x".repeat(3 * 1024 * 1024);
    let ready = |seq: i64| {
        fact(
            seq,
            "jobs.step.updated",
            json!({"id":uuid::Uuid::from_u128(1400+seq as u128),"job_id":job,"status":"ready","metadata":{"waiting_on":{"resource":"vcpu","reason":reason}}}),
        )
    };
    let a = ready(2);
    let b = ready(3);
    let at = b.recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 3,
            through_at: at,
            facts: vec![
                fact(1, "jobs.job.created", json!({"id":job,"status":"open"})),
                a,
                b,
            ],
        })
        .await
        .unwrap();
    assert_eq!(
        history.read(None, 24, at).await.unwrap().waiting_on.len(),
        2,
        "a valid bounded basis must remain readable"
    );
    let c = ready(4);
    history
        .materialize(&HistoryBatch {
            through: 4,
            through_at: c.recorded_at,
            facts: vec![c],
        })
        .await
        .unwrap();
    match history.read(None, 24, at).await {
        Err(boss_jobs::claims_console::ClaimsError::Unavailable(why)) => assert!(
            why.contains("byte bound"),
            "actual refusal must name bytes: {why}"
        ),
        _ => panic!("cumulative selected bytes must refuse, without truncation or a false zero"),
    }
}

async fn bounded_retention_reclaims_in_passes_without_losing_old_anchors_or_current_state<
    H: ClaimsHistory,
>(
    history: &H,
    _: &str,
) {
    let now = Utc::now();
    let old = now - chrono::Duration::days(30);
    let job = uuid::Uuid::from_u128(950);
    let step = uuid::Uuid::from_u128(951);
    let mut facts = vec![
        fact(1, "jobs.pool.published", pool(json!({"state":"unknown"}))),
        fact(2, "jobs.job.created", json!({"id":job,"status":"open"})),
        fact(
            3,
            "jobs.step.updated",
            json!({"id":step,"job_id":job,"status":"completed","metadata":{}}),
        ),
        fact(4, "jobs.job.updated", json!({"id":job,"status":"closed"})),
    ];
    for f in &mut facts {
        f.recorded_at = old + chrono::Duration::seconds(f.seq);
        f.event_at = f.recorded_at;
    }
    let mut anchor = fact(5, "unrelated", json!({}));
    anchor.recorded_at = now - chrono::Duration::days(6);
    anchor.event_at = anchor.recorded_at;
    let mut recent = pool(json!({"state":"configured","amount":"2"}));
    recent["version"] = json!(2);
    let mut last = fact(6, "jobs.pool.published", recent);
    last.recorded_at = now;
    last.event_at = now;
    facts.extend([anchor, last]);
    history
        .materialize(&HistoryBatch {
            through: 6,
            through_at: now,
            facts,
        })
        .await
        .unwrap();
    let retained = history.read(Some(5), 24, now).await.unwrap();
    let current = history.read(None, 24, now).await.unwrap();
    assert_eq!(
        retained.pools[0].state,
        boss_jobs::regions::RegionState::Attention
    );
    let cursor = history.cursor().await.unwrap();
    let mut deleted = 0;
    let mut completed = false;
    for _ in 0..100 {
        let n = history.prune(cursor, now, 1).await.unwrap();
        assert!(n <= 1, "the whole pass must respect its deletion bound");
        deleted += n;
        if n == 0 {
            completed = true;
            break;
        }
    }
    assert!(completed);
    assert!(
        deleted >= 13,
        "four facts, four cuts, three closed row versions and both closed current entities must actually be reclaimed"
    );
    assert_eq!(
        history.read(Some(5), 24, now).await.unwrap(),
        retained,
        "retained old snapshot needs the earlier pool anchor"
    );
    assert_eq!(history.read(None, 24, now).await.unwrap(), current);
    assert!(
        matches!(
            history.read(Some(1), 24, now).await,
            Err(boss_jobs::claims_console::ClaimsError::OutsideRetention(_))
        ),
        "a pruned old cut is410, not an invented empty snapshot"
    );
    assert!(history.prune(cursor, now, 0).await.is_err());
}

async fn a_full_pool_must_keep_draining_and_an_unchanged_republish_cannot_hide_a_stall<
    H: ClaimsHistory,
>(
    history: &H,
    _: &str,
) {
    use boss_jobs::regions::RegionState;
    let job = uuid::Uuid::from_u128(1000);
    let step = uuid::Uuid::from_u128(1001);
    let claim = |generation: u64| json!({"version":1,"id":uuid::Uuid::from_u128(1002+u128::from(generation)),"step_id":step,"job_id":job,"generation":generation,"holder":"emp-human","workflow_kind":"fixture","workflow_version":1,"bill":{"vcpu":"1"},"allocations":[{"pool":"vcpu","version":1,"amount":"1"}],"observed":[{"pool":"vcpu","version":1,"amount":"1","capacity":{"state":"configured","amount":"1"},"held":"0","stock":false}]});
    let first = claim(1);
    let state = json!({"id":step,"job_id":job,"status":"active","assignee_id":"emp-human","metadata":{"resource_claim":first}});
    let mut facts = vec![
        fact(
            1,
            "jobs.pool.published",
            pool(json!({"state":"configured","amount":"1"})),
        ),
        fact(2, "jobs.job.created", json!({"id":job,"status":"open"})),
        fact(3, "jobs.step.updated", state.clone()),
        fact(
            4,
            boss_jobs::flow_resources::RESERVED,
            json!({"job_id":job,"step_id":step,"claim":first,"step":state}),
        ),
    ];
    let start = facts[0].recorded_at;
    // Domain/simulation time is not the wall recorded-at basis of this lens.
    // A future event timestamp cannot keep a recorded five-day stall healthy.
    for f in &mut facts {
        f.recorded_at = start;
        f.event_at = start + chrono::Duration::days(100);
    }
    history
        .materialize(&HistoryBatch {
            through: 4,
            through_at: start,
            facts,
        })
        .await
        .unwrap();
    let fresh = history.read(None, 24, start).await.unwrap();
    assert_eq!(fresh.pools[0].state, RegionState::Full);
    assert_eq!(
        fresh.pools[0].band.as_deref(),
        Some(boss_jobs::region_states::FULL.id),
        "even healthy full must name its deciding band"
    );
    let later = start + chrono::Duration::days(5);
    let mut tick = fact(5, "unrelated", json!({}));
    tick.recorded_at = later;
    tick.event_at = later;
    history
        .materialize(&HistoryBatch {
            through: 5,
            through_at: later,
            facts: vec![tick],
        })
        .await
        .unwrap();
    assert_eq!(
        history.read(None, 24, later).await.unwrap().pools[0].state,
        RegionState::Troubled,
        "full without observed draining is not healthy forever"
    );
    let mut identical = pool(json!({"state":"configured","amount":"1"}));
    identical["version"] = json!(2);
    let mut republish = fact(6, "jobs.pool.published", identical);
    republish.recorded_at = later + chrono::Duration::seconds(1);
    republish.event_at = republish.recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 6,
            through_at: republish.recorded_at,
            facts: vec![republish],
        })
        .await
        .unwrap();
    let stalled = history.read(Some(6), 24, later).await.unwrap();
    assert_eq!(
        stalled.pools[0].state,
        RegionState::Troubled,
        "same capacity publication cannot reset a stall's clock"
    );
    let mut done = state;
    done["status"] = json!("completed");
    let mut cause = fact(7, "jobs.step.updated", done.clone());
    cause.recorded_at = later + chrono::Duration::seconds(2);
    cause.event_at = cause.recorded_at;
    let mut released = fact(
        8,
        boss_jobs::flow_resources::RELEASED,
        json!({"job_id":job,"step_id":step,"claim":first,"step":done,"reason":"completed","cause_event_id":cause.event_id}),
    );
    released.recorded_at = later + chrono::Duration::seconds(3);
    released.event_at = released.recorded_at;
    let second = claim(2);
    let active = json!({"id":step,"job_id":job,"status":"active","assignee_id":"emp-human","metadata":{"resource_claim":second}});
    let mut resumed = fact(9, "jobs.step.updated", active.clone());
    resumed.recorded_at = later + chrono::Duration::seconds(4);
    resumed.event_at = resumed.recorded_at;
    let mut reserved = fact(
        10,
        boss_jobs::flow_resources::RESERVED,
        json!({"job_id":job,"step_id":step,"claim":second,"step":active}),
    );
    reserved.recorded_at = later + chrono::Duration::seconds(5);
    reserved.event_at = reserved.recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 10,
            through_at: reserved.recorded_at,
            facts: vec![cause, released, resumed, reserved],
        })
        .await
        .unwrap();
    let moving = history.read(None, 24, later).await.unwrap();
    assert_eq!(moving.pools[0].state, RegionState::Full);
    assert_eq!(moving.pools[0].releases, 1);
    same_snapshot_with_later_progress(
        &stalled,
        &history.read(Some(6), 24, later).await.unwrap(),
        6,
        10,
    );
}

async fn a_known_pool_with_failed_waiting_claims_and_no_release_is_troubled<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    use boss_events::original_group::{GroupReference, OriginalGroup};
    let job = uuid::Uuid::from_u128(1200);
    let step = uuid::Uuid::from_u128(1201);
    let demand:boss_jobs::flow_resources::Demand=serde_json::from_value(json!({"pool":"vcpu","version":1,"amount":"2","capacity":{"state":"configured","amount":"1"},"held":"0","stock":false})).unwrap();
    let reason = boss_jobs::flow_resources::plan(&[demand]).unwrap_err();
    let ready = json!({"id":step,"job_id":job,"status":"ready","metadata":{"waiting_on":{"resource":reason.pool,"reason":reason.reason}}});
    let mut refused = fact(
        4,
        boss_jobs::flow_resources::REFUSED,
        json!({"job_id":job,"step_id":step,"reason":reason,"step":ready}),
    );
    let ids = vec![refused.event_id];
    refused.original_group = Some(OriginalGroup {
        reference: GroupReference {
            id: uuid::Uuid::from_u128(1202),
            schema_version: 1,
            event_count: 1,
            member_hash: OriginalGroup::roster_hash(&ids),
        },
        event_ids: ids,
        audit_sequences: vec![Some(4)],
        complete: true,
    });
    let at = refused.recorded_at;
    let facts = vec![
        fact(
            1,
            "jobs.pool.published",
            pool(json!({"state":"configured","amount":"1"})),
        ),
        fact(2, "jobs.job.created", json!({"id":job,"status":"open"})),
        fact(3, "jobs.step.updated", ready),
        refused,
    ];
    history
        .materialize(&HistoryBatch {
            through: 4,
            through_at: at,
            facts,
        })
        .await
        .unwrap();
    let reading = history.read(None, 24, at).await.unwrap();
    assert_eq!(reading.waiting_on.len(), 1);
    assert_eq!(reading.binding.as_deref(), Some("vcpu"));
    assert!(reading.pools[0].reserved.is_zero());
    assert_eq!(reading.pools[0].failed_reservations, 1);
    assert_eq!(
        reading.pools[0].state,
        boss_jobs::regions::RegionState::Troubled,
        "a known unsatisfied waiting reservation without any release is not clear"
    );
    assert!(reading.pools[0].band.is_some());
}

async fn a_reservation_without_its_active_step_is_unavailable_not_free_capacity<
    H: ClaimsHistory,
>(
    history: &H,
    _: &str,
) {
    let job = uuid::Uuid::from_u128(900);
    let step = uuid::Uuid::from_u128(901);
    let claim:boss_jobs::flow_resources::Claim=serde_json::from_value(json!({
        "version":1,"id":uuid::Uuid::from_u128(902),"step_id":step,"job_id":job,"generation":1,"holder":"emp-human",
        "workflow_kind":"fixture","workflow_version":1,"bill":{"vcpu":"1"},
        "allocations":[{"pool":"vcpu","version":1,"amount":"1"}],
        "observed":[{"pool":"vcpu","version":1,"amount":"1","capacity":{"state":"configured","amount":"1"},"held":"0","stock":false}]
    })).unwrap();
    let state = json!({"id":step,"job_id":job,"status":"active","assignee_id":"emp-human","metadata":{"resource_claim":claim}});
    let facts = vec![
        fact(
            1,
            "jobs.pool.published",
            pool(json!({"state":"configured","amount":"1"})),
        ),
        fact(2, "jobs.job.created", json!({"id":job,"status":"open"})),
        fact(3, "jobs.step.updated", state.clone()),
        fact(
            4,
            boss_jobs::flow_resources::RESERVED,
            json!({"job_id":job,"step_id":step,"claim":claim,"step":state}),
        ),
    ];
    let at = facts.last().unwrap().recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 4,
            through_at: at,
            facts,
        })
        .await
        .unwrap();
    assert_eq!(
        history.read(None, 24, at).await.unwrap().pools[0]
            .reserved
            .to_string(),
        "1"
    );
    let mut completed = state;
    completed["status"] = json!("completed");
    let cause = fact(5, "jobs.step.updated", completed);
    history
        .materialize(&HistoryBatch {
            through: 5,
            through_at: cause.recorded_at,
            facts: vec![cause],
        })
        .await
        .unwrap();
    assert!(
        history.read(None, 24, at).await.is_err(),
        "a missing release cannot erase an outstanding reservation into zero"
    );
    assert_eq!(
        history.read(Some(4), 24, at).await.unwrap().pools[0]
            .reserved
            .to_string(),
        "1"
    );
}

async fn source_lag_and_failure_never_present_a_partial_basis_as_current<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    use boss_jobs::claims_console::SourceObservation;
    let first = fact(1, "jobs.pool.published", pool(json!({"state":"unknown"})));
    let mut fifth = fact(5, "unrelated", json!({}));
    fifth.source = "other".into();
    let now = fifth.recorded_at;
    let source = SourceObservation {
        audit_head: 5,
        observed_at: now,
    };
    let cursor = history.cursor().await.unwrap();
    history
        .advance(
            cursor,
            &HistoryBatch {
                through: 1,
                through_at: first.recorded_at,
                facts: vec![first],
            },
            source,
        )
        .await
        .unwrap();
    let past = serde_json::to_value(history.read(Some(1), 24, now).await.unwrap()).unwrap();
    assert_eq!(past["projection"]["observed_audit_head"], 5);
    assert_eq!(past["projection"]["through"], 1);
    assert_eq!(
        past["projection"]["sequence_distance"], 4,
        "distance is not a count of missing facts; IDs may have gaps"
    );
    assert!(
        history.read(None, 24, now).await.is_err(),
        "default current cannot quietly mean the unfinished prefix"
    );
    let cursor = history.cursor().await.unwrap();
    history
        .advance(
            cursor,
            &HistoryBatch {
                through: 5,
                through_at: now,
                facts: vec![fifth],
            },
            source,
        )
        .await
        .unwrap();
    let current = serde_json::to_value(history.read(None, 24, now).await.unwrap()).unwrap();
    assert_eq!(current["as_of"], 5);
    assert_eq!(current["projection"]["sequence_distance"], 0);
    let cursor = history.cursor().await.unwrap();
    history
        .source_failed(cursor, now + chrono::Duration::seconds(1))
        .await
        .unwrap();
    assert!(
        history.read(None, 24, now).await.is_err(),
        "source unavailable is not a newly healthy current zero"
    );
    let retained = serde_json::to_value(history.read(Some(5), 24, now).await.unwrap()).unwrap();
    assert_eq!(
        retained["projection"]["source_unavailable_at"],
        serde_json::json!(now + chrono::Duration::seconds(1))
    );
}

async fn a_stale_prepared_batch_cannot_skip_a_new_rebuild_prefix<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    use boss_jobs::claims_console::SourceObservation;
    let first = fact(1, "jobs.pool.published", pool(json!({"state":"unknown"})));
    let at = first.recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 1,
            through_at: at,
            facts: vec![first.clone()],
        })
        .await
        .unwrap();
    let old = history.cursor().await.unwrap();
    let mut configured = pool(json!({"state":"configured","amount":"2"}));
    configured["version"] = json!(2);
    let second = fact(2, "jobs.pool.published", configured);
    let observed = SourceObservation {
        audit_head: 2,
        observed_at: second.recorded_at,
    };
    let stale = HistoryBatch {
        through: 2,
        through_at: second.recorded_at,
        facts: vec![second.clone()],
    };
    let fresh = history.begin_rebuild(old).await.unwrap();
    assert_eq!(fresh.generation, old.generation + 1);
    assert_eq!(fresh.through, 0);
    assert!(
        history.advance(old, &stale, observed).await.is_err(),
        "a former generation's prepared suffix cannot become the new prefix"
    );
    assert!(
        history.read(None, 24, at).await.is_err(),
        "a reset projection is unavailable until it observes its new prefix"
    );
    assert_eq!(history.cursor().await.unwrap(), fresh);
    history
        .advance(
            fresh,
            &HistoryBatch {
                through: 2,
                through_at: second.recorded_at,
                facts: vec![first, second],
            },
            observed,
        )
        .await
        .unwrap();
    let rebuilt = serde_json::to_value(history.read(Some(2), 24, at).await.unwrap()).unwrap();
    assert_eq!(rebuilt["pools"][0]["pool"]["capacity"]["amount"], "2");
    assert!(
        history.advance(fresh, &stale, observed).await.is_err(),
        "a stale cursor in the SAME generation also cannot overwrite later work"
    );
}

async fn an_undeclared_pool_refusal_is_attention_not_an_invisible_zero<H: ClaimsHistory>(
    history: &H,
    _: &str,
) {
    use boss_events::original_group::{GroupReference, OriginalGroup};
    let job = uuid::Uuid::from_u128(300);
    let step = uuid::Uuid::from_u128(301);
    let reason = json!({"pool":"robots:new","reason":"pool-version-unknown","observed":[{"pool":"robots:new","version":0,"amount":"1","capacity":{"state":"unknown"},"held":"0","stock":false}]});
    let state = json!({"id":step,"job_id":job,"status":"ready","metadata":{"waiting_on":{"resource":"robots:new","reason":"pool-version-unknown"}}});
    let mut refused = fact(
        1,
        boss_jobs::flow_resources::REFUSED,
        json!({"job_id":job,"step_id":step,"reason":reason,"step":state}),
    );
    let ids = vec![refused.event_id];
    refused.original_group = Some(OriginalGroup {
        reference: GroupReference {
            id: uuid::Uuid::from_u128(302),
            schema_version: 1,
            event_count: 1,
            member_hash: OriginalGroup::roster_hash(&ids),
        },
        event_ids: ids,
        audit_sequences: vec![Some(1)],
        complete: true,
    });
    let at = refused.recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 1,
            through_at: at,
            facts: vec![refused],
        })
        .await
        .unwrap();
    let reading = serde_json::to_value(history.read(None, 24, at).await.unwrap()).unwrap();
    assert_eq!(
        reading["pools"].as_array().unwrap().len(),
        1,
        "missing declaration cannot erase the failed demand"
    );
    assert_eq!(reading["pools"][0]["pool_key"], "robots:new");
    assert_eq!(
        reading["pools"][0]["pool"],
        serde_json::Value::Null,
        "no synthetic registry version or measured capacity"
    );
    assert_eq!(reading["pools"][0]["state"], "attention");
    assert_eq!(reading["pools"][0]["failed_reservations"], 1);
    assert_eq!(reading["binding"], "robots:new");
}

async fn duplicate_event_identity_inside_one_batch_cannot_change_the_projection<
    H: ClaimsHistory,
>(
    history: &H,
    _: &str,
) {
    let first = fact(1, "jobs.pool.published", pool(json!({"state":"unknown"})));
    let at = first.recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 1,
            through_at: at,
            facts: vec![first],
        })
        .await
        .unwrap();
    let before = history.read(None, 24, at).await.unwrap();
    let mut changed = pool(json!({"state":"configured","amount":"2"}));
    changed["version"] = json!(2);
    let one = fact(2, "jobs.pool.published", changed.clone());
    changed["version"] = json!(3);
    let mut duplicate = fact(3, "jobs.pool.published", changed);
    duplicate.event_id = one.event_id;
    assert!(
        history
            .materialize(&HistoryBatch {
                through: 3,
                through_at: duplicate.recorded_at,
                facts: vec![one, duplicate]
            })
            .await
            .is_err(),
        "one immutable event cannot acquire a second sequence within a batch"
    );
    assert_eq!(history.read(None, 24, at).await.unwrap(), before);
}

fn fact(seq: i64, kind: &str, payload: serde_json::Value) -> SequencedFact {
    let at = DateTime::<Utc>::from_timestamp(1_800_000_000 + seq, 0).unwrap();
    let event = Event::new("jobs", kind, payload, at);
    SequencedFact {
        seq,
        recorded_at: at,
        event_id: event.id,
        event_at: event.timestamp,
        source: event.source,
        kind: event.kind,
        payload: serde_json::to_string(&event.payload).unwrap(),
        original_group: None,
    }
}
fn pool(capacity: serde_json::Value) -> serde_json::Value {
    json!({"name":"vcpu","version":1,"status":"active","unit":"vCPU","kind":"flow","capacity":capacity,"created_at":"2027-01-15T08:00:00Z"})
}

async fn both_refusal_doors_share_one_attempt_count_and_binding_ties_use_registry_keys<
    H: ClaimsHistory,
>(
    history: &H,
    _: &str,
) {
    use boss_events::original_group::{GroupReference, OriginalGroup};
    let job = uuid::Uuid::from_u128(200);
    let step = uuid::Uuid::from_u128(201);
    let mut ram = pool(json!({"state":"configured","amount":"1"}));
    ram["name"] = json!("ram_gb");
    let mut facts = vec![
        fact(
            1,
            "jobs.pool.published",
            pool(json!({"state":"configured","amount":"1"})),
        ),
        fact(2, "jobs.pool.published", ram),
        fact(3, "jobs.job.created", json!({"id":job,"status":"open"})),
    ];
    for (seq, pool) in [(4, "vcpu"), (6, "ram_gb")] {
        let reason = json!({"pool":pool,"reason":"capacity-exhausted","observed":[{"pool":pool,"version":1,"amount":"1","capacity":{"state":"configured","amount":"1"},"held":"1","stock":false}]});
        let state = json!({"id":step,"job_id":job,"status":"ready","metadata":{"waiting_on":{"resource":pool,"reason":"capacity-exhausted"}}});
        let mut normal = fact(
            seq,
            boss_jobs::flow_resources::REFUSED,
            json!({"job_id":job,"step_id":step,"reason":reason,"step":state}),
        );
        let receipt =
            json!({"outcome":"resource-refused","refused_step":state,"refusal":reason}).to_string();
        let mut station = fact(
            seq + 1,
            boss_jobs::events::STATION_RESOURCE_REFUSED,
            json!({"receipt_json":receipt}),
        );
        let ids = vec![normal.event_id, station.event_id];
        let group = OriginalGroup {
            reference: GroupReference {
                id: uuid::Uuid::from_u128(seq as u128),
                schema_version: 1,
                event_count: 2,
                member_hash: OriginalGroup::roster_hash(&ids),
            },
            event_ids: ids,
            audit_sequences: vec![Some(seq), Some(seq + 1)],
            complete: true,
        };
        normal.original_group = Some(group.clone());
        station.original_group = Some(group);
        facts.extend([normal, station]);
    }
    let at = facts.last().unwrap().recorded_at;
    let batch = HistoryBatch {
        through: 7,
        through_at: at,
        facts,
    };
    history.materialize(&batch).await.unwrap();
    let reading = history.read(None, 24, at).await.unwrap();
    assert_eq!(
        reading
            .pools
            .iter()
            .map(|p| (p.pool_key.as_str(), p.failed_reservations))
            .collect::<Vec<_>>(),
        vec![("ram_gb", 1), ("vcpu", 1)]
    );
    assert_eq!(reading.binding.as_deref(), Some("ram_gb"));
    history.materialize(&batch).await.unwrap();
    assert_eq!(
        history.read(None, 24, at).await.unwrap(),
        reading,
        "replay cannot count the same immutable attempt twice"
    );
    assert!(
        history.read(Some(4), 24, at).await.is_err(),
        "the two records of one original transaction cannot be split"
    );
}

async fn exact_reservations_and_releases_are_generation_bound_historical_vectors<
    H: ClaimsHistory,
>(
    history: &H,
    _: &str,
) {
    let job = uuid::Uuid::from_u128(100);
    let step = uuid::Uuid::from_u128(101);
    let claim:boss_jobs::flow_resources::Claim=serde_json::from_value(json!({
        "version":1,"id":uuid::Uuid::from_u128(102),"step_id":step,"job_id":job,"generation":1,"holder":"emp-human",
        "workflow_kind":"fixture","workflow_version":1,"bill":{"vcpu":"0.1234567890123456789012345678"},
        "allocations":[{"pool":"vcpu","version":1,"amount":"0.1234567890123456789012345678"}],
        "observed":[{"pool":"vcpu","version":1,"amount":"0.1234567890123456789012345678","capacity":{"state":"configured","amount":"1"},"held":"0","stock":false}]
    })).unwrap();
    claim.validate().unwrap();
    let state = json!({"id":step,"job_id":job,"status":"active","assignee_id":"emp-human","metadata":{"resource_claim":claim}});
    let facts = vec![
        fact(
            1,
            "jobs.pool.published",
            pool(json!({"state":"configured","amount":"1"})),
        ),
        fact(2, "jobs.job.created", json!({"id":job,"status":"open"})),
        fact(3, "jobs.step.updated", state.clone()),
        fact(
            4,
            boss_jobs::flow_resources::RESERVED,
            json!({"job_id":job,"step_id":step,"claim":claim,"step":state}),
        ),
    ];
    let at = facts.last().unwrap().recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 4,
            through_at: at,
            facts,
        })
        .await
        .unwrap();
    let before = history.read(Some(4), 24, at).await.unwrap();
    assert_eq!(before.claims[0].resource_acquisition, Some(claim.clone()));
    assert_eq!(
        before.pools[0].reserved.to_string(),
        "0.1234567890123456789012345678"
    );
    let mut completed = state;
    completed["status"] = json!("completed");
    let cause = fact(5, "jobs.step.updated", completed.clone());
    let release = fact(
        6,
        boss_jobs::flow_resources::RELEASED,
        json!({"job_id":job,"step_id":step,"claim":claim,"step":completed,"reason":"completed","cause_event_id":cause.event_id}),
    );
    history
        .materialize(&HistoryBatch {
            through: 6,
            through_at: release.recorded_at,
            facts: vec![cause, release],
        })
        .await
        .unwrap();
    let after = history.read(None, 24, at).await.unwrap();
    assert!(after.claims.is_empty());
    assert!(after.pools[0].reserved.is_zero());
    assert_eq!(after.pools[0].releases, 1);
    same_snapshot_with_later_progress(&before, &history.read(Some(4), 24, at).await.unwrap(), 4, 6);
}

async fn a_changed_original_transaction_is_refused_without_mutating_visible_rows<
    H: ClaimsHistory,
>(
    history: &H,
    _: &str,
) {
    use boss_events::original_group::{GroupReference, OriginalGroup};
    fn grouped(mut fact: SequencedFact, id: uuid::Uuid) -> SequencedFact {
        let ids = vec![fact.event_id];
        fact.original_group = Some(OriginalGroup {
            reference: GroupReference {
                id,
                schema_version: 1,
                event_count: 1,
                member_hash: OriginalGroup::roster_hash(&ids),
            },
            event_ids: ids,
            audit_sequences: vec![Some(fact.seq)],
            complete: true,
        });
        fact
    }
    let group = uuid::Uuid::from_u128(50);
    let first = grouped(
        fact(1, "jobs.pool.published", pool(json!({"state":"unknown"}))),
        group,
    );
    let at = first.recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 1,
            through_at: at,
            facts: vec![first],
        })
        .await
        .unwrap();
    let before = history.read(None, 24, at).await.unwrap();
    let mut changed = pool(json!({"state":"configured","amount":"9"}));
    changed["version"] = json!(2);
    let second = grouped(fact(2, "jobs.pool.published", changed), group);
    assert!(
        history
            .materialize(&HistoryBatch {
                through: 2,
                through_at: second.recorded_at,
                facts: vec![second]
            })
            .await
            .is_err()
    );
    assert_eq!(
        history.read(None, 24, at).await.unwrap(),
        before,
        "a refused batch must conserve rows and horizon together"
    );
}

async fn unknown_capacity_is_attention_and_exact_string_capacity_survives_historical_read<
    H: ClaimsHistory,
>(
    history: &H,
    _: &str,
) {
    let first = fact(1, "jobs.pool.published", pool(json!({"state":"unknown"})));
    let at = first.recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 1,
            through_at: at,
            facts: vec![first],
        })
        .await
        .unwrap();
    let reading = history.read(Some(1), 24, at).await.unwrap();
    assert_eq!(reading.as_of, 1);
    assert_eq!(
        reading.pools.len(),
        1,
        "configured pools must not disappear from an empty factory"
    );
    assert_eq!(
        serde_json::to_value(&reading.pools[0]).unwrap()["state"],
        "attention"
    );
    assert_eq!(
        serde_json::to_value(&reading.pools[0]).unwrap()["pool"]["capacity"]["state"],
        "unknown"
    );
    let mut exact = pool(json!({"state":"configured","amount":"0.1234567890123456789012345678"}));
    exact["version"] = json!(2);
    let second = fact(2, "jobs.pool.published", exact);
    history
        .materialize(&HistoryBatch {
            through: 2,
            through_at: second.recorded_at,
            facts: vec![second],
        })
        .await
        .unwrap();
    let current = history.read(None, 24, at).await.unwrap();
    assert_eq!(
        serde_json::to_value(&current.pools[0]).unwrap()["pool"]["capacity"]["amount"],
        "0.1234567890123456789012345678"
    );
    same_snapshot_with_later_progress(
        &reading,
        &history.read(Some(1), 24, at).await.unwrap(),
        1,
        2,
    );
}

async fn unbilled_active_steps_are_visible_and_later_exits_do_not_rewrite_as_of<
    H: ClaimsHistory,
>(
    history: &H,
    _: &str,
) {
    let job = uuid::Uuid::from_u128(1);
    let step = uuid::Uuid::from_u128(2);
    let create = fact(1, "jobs.job.created", json!({"id":job,"status":"open"}));
    let active = fact(
        2,
        "jobs.step.updated",
        json!({"id":step,"job_id":job,"status":"active","assignee_id":"emp-human","metadata":{}}),
    );
    let at = active.recorded_at;
    history
        .materialize(&HistoryBatch {
            through: 2,
            through_at: at,
            facts: vec![create, active],
        })
        .await
        .unwrap();
    let before = history.read(Some(2), 24, at).await.unwrap();
    let wire = serde_json::to_value(&before).unwrap();
    assert_eq!(
        wire["claims"].as_array().unwrap().len(),
        1,
        "zero-demand work is still a held step"
    );
    assert_eq!(wire["claims"][0]["holder"], "emp-human");
    assert_eq!(wire["claims"][0]["allocations"], json!([]));
    assert_eq!(
        wire["claims"][0]["resource_acquisition"],
        serde_json::Value::Null
    );
    assert_eq!(
        wire["claims"][0]["lease_remaining"],
        serde_json::Value::Null
    );
    assert_eq!(
        wire["claims"][0]["lease_unavailable"],
        "leases arrive with car 2b"
    );
    let completed = fact(
        3,
        "jobs.step.updated",
        json!({"id":step,"job_id":job,"status":"completed","assignee_id":"emp-human","metadata":{}}),
    );
    history
        .materialize(&HistoryBatch {
            through: 3,
            through_at: completed.recorded_at,
            facts: vec![completed],
        })
        .await
        .unwrap();
    assert!(history.read(None, 24, at).await.unwrap().claims.is_empty());
    same_snapshot_with_later_progress(&before, &history.read(Some(2), 24, at).await.unwrap(), 2, 3);
}

async fn unobserved_history_is_unavailable_not_a_successful_zero_or_future_sequence<
    H: ClaimsHistory,
>(
    history: &H,
    _: &str,
) {
    let at = DateTime::<Utc>::from_timestamp(1_800_000_000, 0).unwrap();
    assert!(history.read(None, 24, at).await.is_err());
    history
        .materialize(&HistoryBatch {
            through: 1,
            through_at: at,
            facts: vec![],
        })
        .await
        .unwrap();
    let measured = history.read(None, 24, at).await.unwrap();
    assert_eq!(measured.as_of, 1);
    assert!(measured.claims.is_empty());
    assert!(measured.binding.is_none());
    assert!(
        history.read(Some(2), 24, at).await.is_err(),
        "not-yet-materialized is not a guessed future zero"
    );
}

/// Car 3c (item c46a4e99): the review screen scrubs only cuts the server can
/// answer. The index names the head and the first retained cut, lists every
/// answerable cut in a range when they fit and one per equal time slice
/// when they do not, and every cut it lists is one `read(Some(seq))`
/// answers — recorded, after the pruned horizon, inside retention, and not
/// bisecting an original transaction.
async fn retained_cuts_are_published_with_their_times_and_every_listed_cut_is_answerable<
    H: ClaimsHistory,
>(
    history: &H,
    _: &str,
) {
    use boss_events::original_group::{GroupReference, OriginalGroup};
    use boss_jobs::claims_console::{ClaimsError, Cut};
    let at = |seq: i64| fact(seq, "ignored", json!({})).recorded_at;
    // Three batches: facts 1..=3, 4..=5, 6..=9. Facts 7 and 8 are one original
    // transaction, so cut 7 bisects it and is not answerable; cut 8 is whole.
    let mut facts: Vec<SequencedFact> = (1..=9)
        .map(|seq| fact(seq, "jobs.unrelated", json!({"seq": seq})))
        .collect();
    let ids = vec![facts[6].event_id, facts[7].event_id];
    let group = OriginalGroup {
        reference: GroupReference {
            id: uuid::Uuid::from_u128(4700),
            schema_version: 1,
            event_count: 2,
            member_hash: OriginalGroup::roster_hash(&ids),
        },
        event_ids: ids,
        audit_sequences: vec![Some(7), Some(8)],
        complete: true,
    };
    facts[6].original_group = Some(group.clone());
    facts[7].original_group = Some(group);
    for (lo, through) in [(1, 3), (4, 5), (6, 9)] {
        let slice: Vec<_> = facts
            .iter()
            .filter(|f| (lo..=through).contains(&f.seq))
            .cloned()
            .collect();
        history
            .materialize(&HistoryBatch {
                through,
                through_at: at(through),
                facts: slice,
            })
            .await
            .unwrap();
    }
    let now = at(9) + chrono::Duration::hours(1);
    let index = history.cuts(None, None, 1024, now).await.unwrap();
    assert_eq!(index.head, Cut { seq: 9, at: at(9) });
    assert_eq!(index.first_retained, Cut { seq: 1, at: at(1) });
    assert_eq!(index.pruned_through, 0);
    assert_eq!(index.retention_hours, 168);
    assert_eq!(index.retained_from, now - chrono::Duration::hours(168));
    assert_eq!((index.from, index.to), (index.retained_from, now));
    assert!(index.complete);
    assert_eq!(
        index.cuts.iter().map(|c| c.seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5, 6, 8, 9],
        "cut 7 bisects an original transaction and is not published"
    );
    for cut in &index.cuts {
        let reading = history
            .read(Some(cut.seq), 24, now)
            .await
            .unwrap_or_else(|e| panic!("published cut {} must be answerable: {e}", cut.seq));
        assert_eq!((reading.as_of, reading.as_of_at), (cut.seq, cut.at));
    }
    assert!(
        matches!(
            history.read(Some(7), 24, now).await,
            Err(ClaimsError::Invalid(_))
        ),
        "the bisecting sequence the index left out is refused by the read"
    );

    // Retention passes cuts 1..=5: the index starts at the first retained cut
    // and the read refuses what was pruned.
    let later = at(5) + chrono::Duration::hours(168) + chrono::Duration::seconds(1);
    let cursor = history.cursor().await.unwrap();
    while history.prune(cursor, later, 1024).await.unwrap() == 1024 {}
    let index = history.cuts(None, None, 1024, later).await.unwrap();
    assert_eq!(index.pruned_through, 5);
    assert_eq!(index.first_retained, Cut { seq: 6, at: at(6) });
    assert_eq!(
        index.cuts.iter().map(|c| c.seq).collect::<Vec<_>>(),
        vec![6, 8, 9]
    );
    assert!(matches!(
        history.read(Some(5), 24, later).await,
        Err(ClaimsError::OutsideRetention(_))
    ));

    // Forty more facts in one batch. Over [at(6), at(49)] four slices publish
    // the last cut of each plus the first in range: a bounded sample whose
    // every member is answerable, the same list from both adapters.
    let many: Vec<_> = (10..=49)
        .map(|seq| fact(seq, "jobs.unrelated", json!({"seq": seq})))
        .collect();
    history
        .materialize(&HistoryBatch {
            through: 49,
            through_at: at(49),
            facts: many,
        })
        .await
        .unwrap();
    let now = at(49) + chrono::Duration::hours(1);
    let sampled = history
        .cuts(Some(at(6)), Some(at(49)), 4, now)
        .await
        .unwrap();
    assert!(!sampled.complete);
    assert_eq!(
        sampled.cuts.iter().map(|c| c.seq).collect::<Vec<_>>(),
        vec![6, 16, 27, 38, 49],
        "{:?}",
        sampled.cuts
    );
    assert_eq!((sampled.from, sampled.to), (at(6), at(49)));
    for cut in &sampled.cuts {
        history
            .read(Some(cut.seq), 24, now)
            .await
            .unwrap_or_else(|e| panic!("sampled cut {} must be answerable: {e}", cut.seq));
    }
    let narrow = history
        .cuts(Some(at(20)), Some(at(30)), 1024, now)
        .await
        .unwrap();
    assert!(narrow.complete);
    assert_eq!(
        narrow.cuts.iter().map(|c| c.seq).collect::<Vec<_>>(),
        (20..=30).collect::<Vec<_>>()
    );
    // A `from` before retention is clamped to the floor and the answer says
    // so; an inverted range and a bucket count outside the bound are refused.
    let clamped = history
        .cuts(Some(at(1) - chrono::Duration::days(400)), None, 1024, now)
        .await
        .unwrap();
    assert_eq!(clamped.from, clamped.retained_from);
    assert!(matches!(
        history.cuts(Some(at(30)), Some(at(20)), 8, now).await,
        Err(ClaimsError::Invalid(_))
    ));
    assert!(matches!(
        history.cuts(None, None, 0, now).await,
        Err(ClaimsError::Invalid(_))
    ));
    assert!(matches!(
        history.cuts(None, None, 1025, now).await,
        Err(ClaimsError::Invalid(_))
    ));
}
