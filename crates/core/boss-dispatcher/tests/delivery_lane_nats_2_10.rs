//! Explicit real-server rehearsal: BOSS_TEST_NATS_2_10 names a pinned 2.10 binary.
//! No estate connection or credential is read; each fixture owns its server.
use boss_dispatcher::rules::{authoring, delivery, registry};
use futures::StreamExt;
use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};
struct Server {
    child: Child,
    _dir: tempfile::TempDir,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        for channel in ["stdout", "stderr"] {
            match std::fs::read_to_string(self._dir.path().join(channel)) {
                Ok(text) => eprintln!(
                    "NATS2.10 SERVER {channel} BEGIN\n{text}NATS2.10 SERVER {channel} END"
                ),
                Err(error) => eprintln!("NATS2.10 {channel} evidence unread: {error}"),
            }
        }
    }
}
async fn fixture() -> (Server, boss_testing::TestDb, async_nats::jetstream::Context) {
    let binary =
        std::env::var("BOSS_TEST_NATS_2_10").expect("explicit pinned NATS2.10 binary required");
    let version = Command::new(&binary).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains("v2.10."));
    let dir = tempfile::tempdir().unwrap();
    let child = Command::new(binary)
        .args(["-js", "-a", "127.0.0.1", "-p", "-1", "-sd"])
        .arg(dir.path())
        .arg("--ports_file_dir")
        .arg(dir.path())
        .stdout(Stdio::from(
            std::fs::File::create(dir.path().join("stdout")).unwrap(),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(dir.path().join("stderr")).unwrap(),
        ))
        .spawn()
        .unwrap();
    let mut server = Server { child, _dir: dir };
    let ports = server
        ._dir
        .path()
        .join(format!("nats-server_{}.ports", server.child.id()));
    let client = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "owned NATS server exited"
            );
            if let Ok(bytes) = std::fs::read(&ports) {
                assert!(
                    bytes.len() <= 4096,
                    "owned server ports record exceeded bound"
                );
                if let Ok(record) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    let urls = record["nats"].as_array().expect("native client listeners");
                    assert_eq!(urls.len(), 1, "only the owned loopback listener");
                    let url = urls[0].as_str().unwrap();
                    let port: u16 = url
                        .strip_prefix("nats://127.0.0.1:")
                        .expect("owned server must advertise loopback only")
                        .parse()
                        .unwrap();
                    assert_ne!(port, 0);
                    if let Ok(c) = async_nats::connect(url).await {
                        assert_eq!(c.server_info().port, port);
                        break c;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(client.server_info().version.starts_with("2.10."));
    let js = async_nats::jetstream::new(client);
    js.create_stream(stream_config()).await.unwrap();
    (server, boss_testing::TestDb::new().await, js)
}
fn stream_config() -> async_nats::jetstream::stream::Config {
    async_nats::jetstream::stream::Config {
        name: boss_nats::durable::STREAM_NAME.into(),
        subjects: vec!["jobs.>".into()],
        storage: async_nats::jetstream::stream::StorageType::Memory,
        ..Default::default()
    }
}
async fn registry(
    db: &boss_testing::TestDb,
    definitions: &[(&str, &str, &str)],
) -> registry::Registry {
    let mut raw = vec![];
    for (name, lane, subject) in definitions {
        let rule = registry::parse_raw(&format!("[[rule]]\nname={name:?}\non_event={subject:?}\ndelivery_lane={lane:?}\n[[rule.do]]\nhandler=\"record\"\n")).unwrap().rules.remove(0);
        authoring::create_draft(&db.pool, &rule, Some("platform"), "emp-test")
            .await
            .unwrap();
        authoring::publish(&db.pool, name, "emp-test")
            .await
            .unwrap();
        raw.push(rule);
    }
    registry::Registry::from_raw(registry::RawRegistry { rules: raw }).unwrap()
}
#[tokio::test]
#[ignore = "explicit worker-local pinned NATS2.10 rehearsal"]
async fn singleton_server_filter_normalization_binds_and_recovers_the_same_floor() {
    let (_server, db, js) = fixture().await;
    let registry = registry(&db, &[("watch", "watch", "jobs.one")]).await;
    let first = delivery::bind_lanes(&db.pool, &js, &registry).await;
    assert!(
        first.is_ok(),
        "singleton broker normalization must be accepted: {first:?}"
    );
    assert_eq!(first.as_ref().unwrap().ready.len(), 1);
    let second = delivery::bind_lanes(&db.pool, &js, &registry)
        .await
        .unwrap();
    assert_eq!(first.unwrap(), second, "reload conserves immutable cutover");
}
/// Backlog 960fabfc: a bound lane whose consumer (or whose whole stream)
/// is gone is refused on every reload, and the refusal leaves NOTHING on
/// the server. `bind_lanes` used to get-or-create first and judge the
/// receipt after, so the refused reload had already put a new-only
/// consumer on the lane's fixed name, unacked state growing on it.
async fn reloads_refuse_and_create_nothing(
    db: &boss_testing::TestDb,
    js: &async_nats::jetstream::Context,
    registry: &registry::Registry,
) {
    for reload in 1..=2 {
        let result = delivery::bind_lanes(&db.pool, js, registry).await.unwrap();
        assert!(result.ready.is_empty(), "reload {reload}: {result:?}");
        assert!(
            result.refused.contains_key("watch"),
            "reload {reload} must refuse the lane: {result:?}"
        );
        let stream = js
            .get_stream(boss_nats::durable::STREAM_NAME)
            .await
            .unwrap();
        let found: Result<
            async_nats::jetstream::consumer::Consumer<
                async_nats::jetstream::consumer::pull::Config,
            >,
            _,
        > = stream.get_consumer("dispatcher-lane-watch").await;
        assert!(
            found.is_err(),
            "reload {reload} must not create the lane consumer it refuses"
        );
    }
}
#[tokio::test]
#[ignore = "explicit worker-local pinned NATS2.10 rehearsal"]
async fn a_lane_whose_consumer_vanished_is_refused_without_creating_one() {
    let (_server, db, js) = fixture().await;
    let registry = registry(&db, &[("watch", "watch", "jobs.one")]).await;
    let bound = delivery::bind_lanes(&db.pool, &js, &registry)
        .await
        .unwrap();
    assert_eq!(bound.ready.len(), 1, "{bound:?}");
    js.get_stream(boss_nats::durable::STREAM_NAME)
        .await
        .unwrap()
        .delete_consumer("dispatcher-lane-watch")
        .await
        .unwrap();
    reloads_refuse_and_create_nothing(&db, &js, &registry).await;
}
#[tokio::test]
#[ignore = "explicit worker-local pinned NATS2.10 rehearsal"]
async fn a_lane_whose_stream_was_recreated_is_refused_without_creating_one() {
    let (_server, db, js) = fixture().await;
    let registry = registry(&db, &[("watch", "watch", "jobs.one")]).await;
    let bound = delivery::bind_lanes(&db.pool, &js, &registry)
        .await
        .unwrap();
    assert_eq!(bound.ready.len(), 1, "{bound:?}");
    js.delete_stream(boss_nats::durable::STREAM_NAME)
        .await
        .unwrap();
    js.create_stream(stream_config()).await.unwrap();
    reloads_refuse_and_create_nothing(&db, &js, &registry).await;
}
#[tokio::test]
#[ignore = "explicit worker-local pinned NATS2.10 rehearsal"]
async fn overlapping_subjects_bind_one_consumer_without_duplicate_delivery() {
    let (_server, db, js) = fixture().await;
    let registry = registry(
        &db,
        &[("wide", "watch", "jobs.>"), ("narrow", "watch", "jobs.one")],
    )
    .await;
    let result = delivery::bind_lanes(&db.pool, &js, &registry).await;
    assert!(
        result.is_ok(),
        "overlapping member subjects need nonoverlapping transport filters: {result:?}"
    );
    let cutovers = result.unwrap().ready;
    assert_eq!(cutovers.len(), 1);
    let stream = js
        .get_stream(boss_nats::durable::STREAM_NAME)
        .await
        .unwrap();
    let consumer: async_nats::jetstream::consumer::Consumer<
        async_nats::jetstream::consumer::pull::Config,
    > = stream.get_consumer(&cutovers[0].consumer).await.unwrap();
    let mut messages = consumer.messages().await.unwrap();
    js.publish("jobs.one", "one".into())
        .await
        .unwrap()
        .await
        .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(2), messages.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(message.payload.as_ref(), b"one");
    message.ack().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), messages.next())
            .await
            .is_err(),
        "overlap must not duplicate one event"
    );
}
#[tokio::test]
#[ignore = "explicit worker-local pinned NATS2.10 rehearsal"]
async fn one_incompatible_lane_does_not_prevent_an_independent_lane_binding() {
    let (_server, db, js) = fixture().await;
    let registry = registry(
        &db,
        &[
            ("bad", "a-bad", "jobs.bad"),
            ("good", "z-good", "jobs.good"),
        ],
    )
    .await;
    let stream = js
        .get_stream(boss_nats::durable::STREAM_NAME)
        .await
        .unwrap();
    let mut config = delivery::consumer_config("a-bad", vec!["jobs.bad".into()]);
    config.description = Some("foreign membership".into());
    stream.create_consumer(config).await.unwrap();
    let result = delivery::bind_lanes(&db.pool, &js, &registry).await;
    assert!(
        result.is_ok(),
        "one lane's refusal must not stop unrelated consumers/schedules: {result:?}"
    );
    let result = result.unwrap();
    assert_eq!(result.ready.len(), 1);
    assert_eq!(result.ready[0].lane, "z-good");
    assert!(result.refused.contains_key("a-bad"));
    let live = boss_dispatcher::liveness::DispatcherLiveness::default();
    live.configure_delivery_lanes(&result.ready);
    live.configure_delivery_lane_refusals(&result.refused);
    live.mark_rules_running();
    live.mark_schedule_running();
    live.mark_delivery_lane("z-good", true);
    assert_eq!(live.snapshot()["rules_running"], true);
    assert_eq!(live.snapshot()["schedule_running"], true);
    assert_eq!(
        live.snapshot()["ready"],
        false,
        "isolated refusal remains visible, not healthy"
    );
    assert!(live.observation()["rules_delivery_lanes"]["lanes"]["a-bad"]["cutover"].is_null());
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM dispatcher_delivery_lanes WHERE name='a-bad'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count, 0, "refused lane never gains a guessed floor");
}

struct RouteReadiness {
    ready: std::sync::atomic::AtomicBool,
    calls: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl delivery::DeliveryRoutes for RouteReadiness {
    async fn permitted(
        &self,
        names: &[String],
        _position: Option<&delivery::Position>,
    ) -> Result<std::collections::BTreeSet<String>, String> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // First refusal establishes the hold. A subsequent authoritative read
        // remains pending longer than the fixture ACK deadline until recovery.
        if call > 0 {
            while !self.ready.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        if self.ready.load(std::sync::atomic::Ordering::SeqCst) {
            Ok(names.iter().cloned().collect())
        } else {
            Err("cutover reader temporarily unavailable".into())
        }
    }
}
#[tokio::test]
#[ignore = "explicit worker-local pinned NATS2.10 rehearsal"]
async fn an_unknown_route_keeps_one_pending_delivery_and_recovers_without_spending_retries() {
    use boss_dispatcher::rules::{
        expr::NoHelpers,
        handler::{HandlerRegistry, RecordingHandler},
        runner::RulesRunner,
    };
    use std::sync::{Arc, atomic::Ordering};
    let (_server, db, js) = fixture().await;
    let registry = registry(&db, &[("watch", "watch", "jobs.one")]).await;
    let stream = js
        .get_stream(boss_nats::durable::STREAM_NAME)
        .await
        .unwrap();
    // Test-only accelerated server budget: three deliveries, two-second ACK deadline.
    // Production retry/ACK bounds remain unchanged. Seven seconds spans the entire fixture budget.
    let mut config = delivery::consumer_config("watch", vec!["jobs.one".into()]);
    config.ack_wait = Duration::from_secs(2);
    config.max_deliver = 3;
    config.description = Some(serde_json::json!({"delivery_lane_members":{"watch":1}}).to_string());
    let mut consumer = stream.create_consumer(config).await.unwrap();
    let cutover = delivery::initial_cutover(
        "watch",
        std::collections::BTreeMap::from([("watch".into(), 1)]),
        stream
            .cached_info()
            .created
            .unix_timestamp_nanos()
            .to_string(),
        consumer.info().await.unwrap(),
    )
    .unwrap();
    let routes = Arc::new(RouteReadiness {
        ready: std::sync::atomic::AtomicBool::new(false),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let handler = RecordingHandler::new("record");
    let mut handlers = HandlerRegistry::new();
    handlers.register(handler.clone());
    let live = Arc::new(boss_dispatcher::liveness::DispatcherLiveness::default());
    live.configure_delivery_lanes(std::slice::from_ref(&cutover));
    let runner = RulesRunner {
        delivery_routes: Some(routes.clone()),
        registry,
        handlers,
        helpers: Arc::new(NoHelpers),
        dead_letters: None,
        live: Some(live.clone()),
        firings: None,
    };
    let publisher = js.clone();
    let execution = tokio::spawn(async move { runner.run_delivery(js, live, Some(cutover)).await });
    // The consumer already exists: publication before pulling is retained.
    publisher
        .publish(
            "jobs.one",
            serde_json::to_vec(
                &serde_json::json!({"id":"held-event","kind":"jobs.one","payload":{}}),
            )
            .unwrap()
            .into(),
        )
        .await
        .unwrap()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while routes.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_secs(7)).await;
    let held = consumer.info().await.unwrap();
    assert_eq!(
        held.delivered.consumer_sequence, 1,
        "holding must not spend redelivery budget"
    );
    assert_eq!(
        held.num_ack_pending, 1,
        "unknown work remains pending, never ACKed away"
    );
    assert!(
        handler.calls().await.is_empty(),
        "unknown route cannot run effects"
    );
    routes.ready.store(true, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(5), async {
        while handler.calls().await.len() != 1
            || consumer.info().await.unwrap().num_ack_pending != 0
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(handler.calls().await[0].triggering_event_id, "held-event");
    assert_eq!(
        consumer.info().await.unwrap().delivered.consumer_sequence,
        1
    );
    execution.abort();
    assert!(execution.await.unwrap_err().is_cancelled());
}
