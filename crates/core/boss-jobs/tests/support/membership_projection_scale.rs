// Explicit, isolated synthetic COPY/plan and actual HTTP-consumer measurement.
// No production export, production timing inference or background throughput claim.
use boss_events::history_snapshot::{MetadataKeys, OriginalEntry, read_on, read_paged_on};
use boss_jobs::{membership_projection::{MembershipProjectionRepository, ProjectionIdentity, StoredProjection}, membership_projection_pg::PgMembershipProjection};
use std::{collections::{BTreeMap, BTreeSet}, sync::Arc, time::Instant};
use serde_json::Value;

fn translate(value: &mut Value, shift: chrono::Duration) {
    match value {
        Value::String(text) => {
            if let Ok(at) = DateTime::parse_from_rfc3339(text) {
                *text = (at.with_timezone(&Utc) + shift).to_rfc3339();
            } else if let Ok(date) = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d") {
                *text = (date + chrono::Duration::days(shift.num_days())).to_string();
            }
        }
        Value::Array(values) => values.iter_mut().for_each(|v| translate(v, shift)),
        Value::Object(values) => values.values_mut().for_each(|v| translate(v, shift)),
        _ => {}
    }
}

fn copy_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n").replace('\r', "\\r")
}

fn digest(rows: &[OriginalEntry]) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for entry in rows {
        hash.update(serde_json::to_vec(&json!([entry.row.seq,entry.row.event_id,entry.row.timestamp,entry.row.created_at,entry.row.source,entry.row.kind,entry.row.payload,entry.original_group])).unwrap());
    }
    hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "explicit production-sized synthetic COPY/EXPLAIN/owned HTTP consumer measurement"]
async fn synthetic_copy_and_owned_http_consumer_at_history_scale() {
    use boss_jobs::{JobsRepository, InMemoryJobs, InMemoryStations, InMemoryWorkflows};
    use boss_jobs::membership_projection_service::MembershipProjectionService;
    use boss_policy_client::{FakePolicyClient, Action, Resource, Scope};
    use boss_core::{port::EventBus, publisher::DomainPublisher};
    use boss_testing::{RecordingEventBus, TestDb};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    const PREFIX: i64 = 1_000_000;
    const LIMIT: i64 = 250_000;
    let actor = "arbitrary+measurement@caller.invalid";
    let (groups, _, _) = population_groups(86_410, 86_420, 70_000);
    let mut input = cut(groups, 5, 172_805, actor);
    let shift = Utc::now() - input.until;
    input.since += shift;
    input.until += shift;
    input.audit_cutoff += PREFIX;
    let mut clock = serde_json::to_value(&input.clock).unwrap();
    translate(&mut clock, shift);
    input.clock = serde_json::from_value(clock).unwrap();
    let mut packet_ids = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for group in &mut input.groups {
        for seq in &mut group.original.as_mut().unwrap().audit_sequences { *seq = seq.map(|seq| seq + PREFIX); }
        for row in &mut group.rows {
            row.seq += PREFIX;
            row.created_at = row.created_at.map(|at| at + shift);
            row.event.timestamp += shift;
            translate(&mut row.event.payload, shift);
            if matches!(row.event.kind.as_str(), boss_jobs::events::JOB_CREATED | boss_jobs::events::JOB_UPDATED) {
                packet_ids.insert(row.event.payload["id"].as_str().unwrap().to_owned());
                row.event.payload["metadata"]["scale_padding"] = json!("synthetic-unused-padding".repeat(90));
            }
            if let Some(metadata) = row.event.payload.get("metadata").and_then(Value::as_object) {
                keys.extend(metadata.keys().filter(|key| key.as_str() != "scale_padding").cloned());
            }
        }
    }
    let db = TestDb::new().await;
    let mut conn = db.pool.acquire().await.unwrap();
    sqlx::raw_sql("CREATE SCHEMA projection_scale; CREATE TABLE projection_scale.audit_log (LIKE public.audit_log INCLUDING DEFAULTS INCLUDING INDEXES); ALTER TABLE projection_scale.audit_log ALTER COLUMN prev_hash DROP NOT NULL, ALTER COLUMN row_hash DROP NOT NULL; SET search_path=projection_scale,public")
        .execute(&mut *conn).await.unwrap();
    let indexes: Vec<String> = sqlx::query_scalar("SELECT indexdef FROM pg_indexes WHERE schemaname='projection_scale' ORDER BY indexname").fetch_all(&mut *conn).await.unwrap();
    println!("SYNTHETIC_INDEXES {indexes:?}");
    let started = Instant::now();
    let mut copy = conn.copy_in_raw("COPY projection_scale.audit_log(id,event_id,timestamp,created_at,source,kind,payload,original_group) FROM STDIN").await.unwrap();
    let mut buffer = String::new();
    let prefix_time = (input.since - chrono::Duration::days(1)).to_rfc3339();
    for id in 1..=PREFIX {
        buffer.push_str(&format!("{id}\t{}\t{prefix_time}\t{prefix_time}\tsynthetic\tfixture.unrelated\t{{\"synthetic_prefix\":\"{id:016x}\"}}\t\\N\n", Uuid::from_u128(id as u128)));
        if id % 1000 == 0 { copy.send(buffer.as_bytes()).await.unwrap(); buffer.clear(); }
    }
    let mut raw_bytes = 0_usize;
    for group in &input.groups {
        let reference = serde_json::to_string(&group.original.as_ref().unwrap().reference).unwrap();
        for row in &group.rows {
            let payload = serde_json::to_string(&row.event.payload).unwrap();
            raw_bytes += payload.len();
            buffer.push_str(&format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",row.seq,row.event.id,row.event.timestamp.to_rfc3339(),row.created_at.unwrap().to_rfc3339(),copy_text(&row.event.source),row.event.kind,copy_text(&payload),copy_text(&reference)));
            if buffer.len() >= 1024*1024 { copy.send(buffer.as_bytes()).await.unwrap(); buffer.clear(); }
        }
    }
    if !buffer.is_empty() { copy.send(buffer.as_bytes()).await.unwrap(); }
    copy.finish().await.unwrap();
    sqlx::query("ANALYZE projection_scale.audit_log").execute(&mut *conn).await.unwrap();
    let source_rows: usize = input.groups.iter().map(|group| group.rows.len()).sum();
    println!("SYNTHETIC_COPY profile={} unrelated_prefix={PREFIX} stations=19 packets=1000 steps=5000 update_groups=70000 source_rows={source_rows} raw_source_payload_bytes={raw_bytes} seconds={} no_production_export=true no_claim_manifest_density_model=true",if cfg!(debug_assertions){"debug"}else{"optimized"},started.elapsed().as_secs_f64());
    assert!(raw_bytes > 100*1024*1024, "production-sized raw payload fixture");
    let packets = [boss_jobs::events::JOB_CREATED,boss_jobs::events::JOB_UPDATED,boss_jobs::events::STEP_CREATED,boss_jobs::events::STEP_UPDATED];
    let globals = [boss_jobs::events::STATION_PUBLISHED,boss_jobs::events::WORKFLOW_PUBLISHED];
    let keys: Vec<_> = keys.into_iter().collect();
    let metadata = MetadataKeys { kinds: &packets, keys: &keys, siblings: Some(&["job_id","step","id"]) };
    let scoped: Vec<_> = packet_ids.iter().take(3).cloned().collect();
    for (name, ids) in [("full-scope",None),("three-packet-scope",Some(scoped.as_slice()))] {
        let mut reference = None;
        for paged in [false,true,true,false] {
            sqlx::raw_sql("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL statement_timeout='20s'").execute(&mut *conn).await.unwrap();
            let started = Instant::now();
            let (rows, bytes) = if paged { read_paged_on(&mut conn,input.audit_cutoff,PREFIX,ids,&packets,&globals,metadata,LIMIT,128*1024*1024,512).await } else { read_on(&mut conn,input.audit_cutoff,PREFIX,ids,&packets,&globals,metadata,LIMIT,128*1024*1024).await }.unwrap();
            let elapsed = started.elapsed().as_secs_f64();
            let signature = (rows.len(),bytes,digest(&rows));
            println!("SOURCE_STREAM {name} paged={paged} seconds={elapsed} signature={signature:?}");
            if let Some(reference) = &reference { assert_eq!(&signature,reference); } else { reference = Some(signature); }
            sqlx::query("ROLLBACK").execute(&mut *conn).await.unwrap();
        }
    }
    let page: Vec<_> = (PREFIX+1..=PREFIX+512).collect();
    let source = include_str!("../../../boss-events/src/history_snapshot.rs");
    let identity_sql = "SELECT id FROM audit_log WHERE id>$1 AND id<=$2 ORDER BY id LIMIT $3";
    assert!(source.contains(identity_sql));
    let identity_plan: Value = sqlx::query_scalar(&format!("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) {identity_sql}")).bind(PREFIX).bind(input.audit_cutoff).bind(512_i64).fetch_one(&mut *conn).await.unwrap();
    let relevant = source.split("SELECT a.id,a.original_group->>'id' FROM audit_log a").nth(1).unwrap().split("\"#)").next().unwrap();
    let relevant_sql = format!("SELECT a.id,a.original_group->>'id' FROM audit_log a{relevant}");
    let relevant_plan: Value = sqlx::query_scalar(&format!("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) {relevant_sql}")).bind(&page).bind(Some(&scoped)).bind(packets.as_slice()).bind(input.audit_cutoff).bind(PREFIX).fetch_one(&mut *conn).await.unwrap();
    println!("ACTUAL_SOURCE_IDENTITY_PLAN {identity_plan}");
    println!("ACTUAL_SOURCE_RELEVANCE_PLAN {relevant_plan}");
    let started = Instant::now();
    let mut projection = MembershipProjection::rebuild(&input).unwrap();
    println!("SYNTHETIC_REBUILD seconds={} checkpoint_bytes={} facts={}",started.elapsed().as_secs_f64(),serde_json::to_vec(&projection).unwrap().len(),projection.facts().len());
    let identity = ProjectionIdentity::new(Some(actor.into()),JobScope::All);
    let store = Arc::new(PgMembershipProjection::new(db.pool.clone()));
    let started = Instant::now();
    let revision = store.commit(&identity,None,&projection).await.unwrap().unwrap();
    println!("DURABLE_INITIAL_COMMIT seconds={}",started.elapsed().as_secs_f64());
    let mut previous = StoredProjection { revision, checkpoint: projection.clone() };
    let jobs = Arc::new(InMemoryJobs::new());
    let mut latest_jobs = BTreeMap::new();
    let mut latest_steps = BTreeMap::new();
    let mut workflows = BTreeMap::new();
    for row in input.groups.iter().flat_map(|group| &group.rows) {
        match row.event.kind.as_str() {
            boss_jobs::events::JOB_CREATED | boss_jobs::events::JOB_UPDATED => { let job: Job = serde_json::from_value(row.event.payload.clone()).unwrap(); latest_jobs.insert(job.id.to_string(),job); }
            boss_jobs::events::STEP_CREATED | boss_jobs::events::STEP_UPDATED => { let step: Step = serde_json::from_value(row.event.payload.clone()).unwrap(); latest_steps.insert(step.id.to_string(),step); }
            boss_jobs::events::WORKFLOW_PUBLISHED => { let spec: boss_jobs::registry::WorkflowSpec = serde_json::from_value(row.event.payload.clone()).unwrap(); workflows.insert(spec.kind.clone(),spec); }
            _ => {}
        }
    }
    for job in latest_jobs.values() { jobs.create_job_at(job,input.until,&[]).await.unwrap(); }
    for step in latest_steps.values() { jobs.add_step_at(step,input.until,&[]).await.unwrap(); }
    let registry = Arc::new(InMemoryStations::new());
    for station in population() { registry.seed(station).unwrap(); }
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    for spec in workflows.into_values() { kinds.seed(spec).unwrap(); }
    let service = Arc::new(MembershipProjectionService::new(jobs.clone(),store.clone(), Arc::new(boss_clock_client::WallClockClient)));
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = boss_jobs::http::JobsApiState {
        membership_projection: Some(service), stations: Some(registry), kind_registry: Some(kinds),
        ..boss_jobs::http::JobsApiState::minimal(jobs.clone(),bus,DomainPublisher::new(bus_dyn,"synthetic"),Arc::new(FakePolicyClient::builder().allow("operator",Action::Read,Resource::job(),Scope::All).build()),Arc::new(boss_clock_client::WallClockClient))
    };
    let app = boss_jobs::http::router(state);
    let user = boss_policy_client::User { id: actor.into(),role:"operator".into(),access_tier:boss_policy_client::types::AccessTier::Operator,territory_account_ids:vec![],direct_report_ids:vec![],department:Some("it".into()) };
    for round in 0..4 {
        let until: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut *conn).await.unwrap();
        let mut clock = input.clock.clone();
        clock.known.last_mut().unwrap().until = until;
        let delta = ReplayInput { groups:vec![],since:projection.until(),until,audit_cutoff:input.audit_cutoff,actor:Some(actor.into()),scope:JobScope::All,currently_authorized:None,clock,registry_prefix_complete:true,packet_prefix_complete:true,source_unavailable:vec![],initial:InitialState::default() };
        projection.advance(&delta).unwrap();
        let revision = store.commit(&identity,Some(&previous),&projection).await.unwrap().unwrap();
        previous = StoredProjection { revision,checkpoint:projection.clone() };
        let mut paired_map_cut = None;
        let mut paired_flow_cut = None;
        for path in ["/api/yard/flow?window=24h","/api/yard/regions?window=24h","/api/yard/borders?window=24h","/api/yard/flow?window=24h"] {
            let started = Instant::now();
            let response = app.clone().oneshot(axum::http::Request::builder().uri(path).header("x-boss-user",serde_json::to_string(&user).unwrap()).body(axum::body::Body::empty()).unwrap()).await.unwrap();
            let status = response.status();
            let body: Value = serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
            let elapsed = started.elapsed().as_secs_f64();
            println!("OWNED_HTTP_WARM round={round} path={path} seconds={elapsed} status={status} body={body}");
            assert!(status.is_success(),"{body}");
            if path.contains("/flow?") {
                assert_eq!(body["state"],"ready");
                assert!(body.get("why").is_some_and(Value::is_null));
                assert_eq!(DateTime::parse_from_rfc3339(body["as_of"].as_str().unwrap()).unwrap().with_timezone(&Utc),until);
                assert!(elapsed < 2.0,"warm compact flow exceeded two seconds");
            } else {
                assert!(body.get("map_refresh_error").is_some_and(Value::is_null));
                let map_cut = body["map_as_of"].as_str().unwrap().to_owned();
                let flow_cut = body["flow_as_of"].as_str().unwrap().to_owned();
                if let Some(expected) = &paired_map_cut { assert_eq!(&map_cut,expected); }
                if let Some(expected) = &paired_flow_cut { assert_eq!(&flow_cut,expected); }
                paired_map_cut = Some(map_cut);
                paired_flow_cut = Some(flow_cut);
                assert!(elapsed < 1.0,"warm paired map exceeded one second");
            }
        }
    }
    assert_eq!(jobs.membership_reads(),0,"the actual warm HTTP consumer never asks original history");
    let projection_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM membership_projection_rows").fetch_one(&db.pool).await.unwrap();
    let stored_key: String = sqlx::query_scalar("SELECT identity FROM membership_projection_generations WHERE header->'binding'->>'actor'=$1").bind(actor).fetch_one(&db.pool).await.unwrap();
    let compact_source = include_str!("../../src/membership_projection_pg.rs");
    let prefix = "SELECT bucket,row_key,payload FROM membership_projection_rows WHERE identity=$1 AND";
    let compact_sql = format!("{prefix}{}",compact_source.split(prefix).nth(1).unwrap().split('"').next().unwrap());
    let compact_plan: Value = sqlx::query_scalar(&format!("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) {compact_sql}")).bind(stored_key).bind(projection.until()-chrono::Duration::hours(24)).fetch_one(&db.pool).await.unwrap();
    println!("DURABLE_ROW_COUNT {projection_rows} COMPACT_WINDOW_PLAN {compact_plan}");
    println!("MEASUREMENT_LIMITS: entirely synthetic copied-index source; no production cardinality/cause; no claim-manifest density model; actual HTTP router/service/Pg generation reads with populated InMemory current-job ports and FakePolicy; updater intentionally not running during fixed-generation viewer measurements; no deployed or whole-updater latency assertion; no hard RSS bound from serialized bytes.");
    for line in std::fs::read_to_string("/proc/self/status").unwrap().lines().filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:")) { println!("MEASUREMENT_PROCESS_ONLY {line}"); }
}
