use boss_dispatcher::rules::delivery::{self, Cutover, DeliveryRoutes, PgDeliveryRoutes, Position};
use boss_dispatcher::rules::registry::{Registry, parse_raw};
use std::sync::Arc;

fn cutover() -> Cutover {
    Cutover {
        lane: "estate-watch".into(),
        consumer: "dispatcher-lane-estate-watch".into(),
        stream_created_ns: "1".into(),
        consumer_created_ns: "2".into(),
        created_at: "2026-10-31T09:00:00Z".into(),
        after_stream_sequence: 10,
        members: std::collections::BTreeMap::from([("watch".into(), 1)]),
    }
}

fn position(lane: Option<&str>, sequence: u64) -> Position {
    Position {
        lane: lane.map(str::to_owned),
        stream_created_ns: "1".into(),
        stream_sequence: sequence,
    }
}

#[test]
fn every_position_belongs_to_one_lane_including_old_backlog_and_exact_floor() {
    let receipt = cutover();
    for seq in [0, 9, 10, 11, u64::MAX] {
        let default =
            delivery::eligible(Some("estate-watch"), Some(&receipt), &position(None, seq)).unwrap();
        let isolated = delivery::eligible(
            Some("estate-watch"),
            Some(&receipt),
            &position(Some("estate-watch"), seq),
        )
        .unwrap();
        assert_ne!(
            default, isolated,
            "position {seq} must belong to exactly one consumer"
        );
        assert_eq!(
            default,
            seq <= 10,
            "old backlog still drains on its original consumer"
        );
        assert!(
            !delivery::eligible(
                Some("estate-watch"),
                Some(&receipt),
                &position(Some("unrelated"), seq)
            )
            .unwrap()
        );
    }
    for lane in [None, Some("estate-watch")] {
        assert!(
            delivery::eligible(Some("estate-watch"), None, &position(lane, 11)).is_err(),
            "unknown cutover must HOLD, never assume default"
        );
    }
    let mut changed = position(None, 1);
    changed.stream_created_ns = "recreated".into();
    assert!(delivery::eligible(Some("estate-watch"), Some(&receipt), &changed).is_err());
}

fn native_info() -> async_nats::jetstream::consumer::Info {
    let mut config = delivery::consumer_config("estate-watch", vec!["jobs.estate.observed".into()]);
    config.description = Some(delivery::membership_description(&cutover().members));
    serde_json::from_value(serde_json::json!({
        "stream_name":"BOSS_EVENTS","name":"dispatcher-lane-estate-watch","created":"2026-10-31T09:00:00.123456789Z",
        "config":config,"delivered":{"consumer_seq":0,"stream_seq":10},"ack_floor":{"consumer_seq":0,"stream_seq":10},
        "num_ack_pending":0,"num_redelivered":0,"num_waiting":0,"num_pending":0
    })).unwrap()
}

#[test]
fn crash_before_receipt_recovers_only_an_untouched_new_only_consumer() {
    let mut info = native_info();
    let receipt =
        delivery::initial_cutover("estate-watch", cutover().members, "1".into(), &info).unwrap();
    assert_eq!(receipt.after_stream_sequence, 10);
    assert_eq!(
        receipt.consumer_created_ns,
        info.created.unix_timestamp_nanos().to_string()
    );
    let changed_members = std::collections::BTreeMap::from([("watch".into(), 2)]);
    assert!(
        delivery::initial_cutover("estate-watch", changed_members, "1".into(), &info).is_err(),
        "an untouched orphan still belongs to its original membership/version"
    );
    let description = info.config.description.take();
    assert!(
        delivery::initial_cutover("estate-watch", cutover().members, "1".into(), &info).is_err(),
        "unknown orphan identity cannot be invented"
    );
    info.config.description = description;
    info.delivered.consumer_sequence = 1;
    assert!(
        delivery::initial_cutover("estate-watch", cutover().members, "1".into(), &info).is_err()
    );
    info.delivered.consumer_sequence = 0;
    info.config.deliver_policy = async_nats::jetstream::consumer::DeliverPolicy::All;
    assert!(
        delivery::initial_cutover("estate-watch", cutover().members, "1".into(), &info).is_err()
    );
}

#[test]
fn independent_consumer_keeps_the_existing_settlement_contract() {
    let lane = delivery::consumer_config("estate-watch", vec!["jobs.estate.observed".into()]);
    let default = boss_nats::durable::consumer_config("dispatcher-rules", vec!["jobs.>".into()]);
    assert_eq!(lane.ack_policy, default.ack_policy);
    assert_eq!(lane.ack_wait, default.ack_wait);
    assert_eq!(lane.max_deliver, default.max_deliver);
    assert_eq!(lane.backoff, default.backoff);
    assert_eq!(lane.filter_subjects, vec!["jobs.>"]);
    assert_eq!(
        lane.deliver_policy,
        async_nats::jetstream::consumer::DeliverPolicy::New
    );
}

#[tokio::test]
async fn publication_then_creation_then_persistence_fences_a_cached_default_route() {
    use boss_dispatcher::rules::{
        authoring,
        expr::NoHelpers,
        handler::{HandlerRegistry, RecordingHandler},
        runner::RulesRunner,
    };
    use boss_nats::durable::Settle;
    let db = boss_testing::TestDb::new().await;
    let original = r#"[[rule]]
name="watch"
on_event="jobs.estate.observed"
[[rule.do]]
handler="record"
"#;
    let raw = parse_raw(original).unwrap().rules.remove(0);
    authoring::create_draft(&db.pool, &raw, None, "emp-test")
        .await
        .unwrap();
    authoring::publish(&db.pool, "watch", "emp-test")
        .await
        .unwrap();
    let recorder = RecordingHandler::new("record");
    let mut handlers = HandlerRegistry::new();
    handlers.register(recorder.clone());
    let routes = Arc::new(PgDeliveryRoutes(db.pool.clone()));
    let cached = RulesRunner {
        delivery_routes: Some(routes.clone()),
        registry: Registry::from_toml(original).unwrap(),
        handlers,
        helpers: Arc::new(NoHelpers),
        dead_letters: None,
        live: None,
        firings: None,
    };
    let mut enabled = raw;
    enabled.delivery_lane = Some("estate-watch".into());
    authoring::create_draft(&db.pool, &enabled, None, "emp-test")
        .await
        .unwrap();
    authoring::publish(&db.pool, "watch", "emp-test")
        .await
        .unwrap();
    // The NEW consumer exists, but no receipt is committed yet. The
    // already-running default still has a rule with delivery_lane=None.
    let members = std::collections::BTreeMap::from([("watch".into(), 2)]);
    let mut info = native_info();
    info.config.description = Some(delivery::membership_description(&members));
    let receipt = delivery::initial_cutover("estate-watch", members, "1".into(), &info).unwrap();
    let body = serde_json::json!({"_at":"1900-01-01T00:00:00Z"});
    assert!(matches!(
        cached
            .handle_delivery(
                "jobs.estate.observed",
                "old-cache",
                &body,
                1,
                None,
                Some(&position(None, 11))
            )
            .await,
        Settle::Hold(_)
    ));
    assert!(recorder.calls().await.is_empty());
    sqlx::query("INSERT INTO dispatcher_delivery_lanes (name,receipt) VALUES ($1,$2)")
        .bind("estate-watch")
        .bind(serde_json::to_value(&receipt).unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(matches!(
        cached
            .handle_delivery(
                "jobs.estate.observed",
                "before-floor",
                &body,
                1,
                None,
                Some(&position(None, 10))
            )
            .await,
        Settle::Ack
    ));
    assert_eq!(recorder.calls().await.len(), 1);
    assert!(matches!(
        cached
            .handle_delivery(
                "jobs.estate.observed",
                "after-floor",
                &body,
                1,
                None,
                Some(&position(None, 11))
            )
            .await,
        Settle::Ack
    ));
    assert_eq!(
        recorder.calls().await.len(),
        1,
        "cached default cannot handle new-lane positions"
    );
    let isolated = cached.clone();
    assert!(matches!(
        isolated
            .handle_delivery(
                "jobs.estate.observed",
                "isolated",
                &body,
                1,
                None,
                Some(&position(Some("estate-watch"), 11))
            )
            .await,
        Settle::Ack
    ));
    assert_eq!(
        recorder.calls().await.len(),
        2,
        "event/simulation time cannot override the stream floor"
    );
    let names = vec!["watch".to_owned()];
    assert!(
        routes.permitted(&names, None).await.is_err(),
        "log delivery has no usable JetStream position"
    );
    let restarted = PgDeliveryRoutes(db.pool.clone());
    assert_eq!(
        restarted
            .permitted(&names, Some(&position(None, 10)))
            .await
            .unwrap(),
        std::collections::BTreeSet::from(["watch".to_owned()])
    );
    assert_eq!(
        restarted
            .permitted(&names, Some(&position(Some("estate-watch"), 11)))
            .await
            .unwrap(),
        std::collections::BTreeSet::from(["watch".to_owned()])
    );
    assert!(
        sqlx::query("UPDATE dispatcher_delivery_lanes SET receipt = '{}'::jsonb")
            .execute(&db.pool)
            .await
            .is_err(),
        "a committed floor cannot be rewritten"
    );
    assert!(
        sqlx::query("DELETE FROM dispatcher_delivery_lanes")
            .execute(&db.pool)
            .await
            .is_err(),
        "a committed floor cannot be forgotten"
    );
    sqlx::query("DROP TABLE dispatcher_delivery_lanes")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        routes
            .permitted(&names, Some(&position(None, 10)))
            .await
            .is_err(),
        "an unread route never falls back to default"
    );
}

#[test]
fn a_registry_rule_can_declare_an_independent_delivery_lane() {
    let source = r#"
[[rule]]
name = "watch"
on_event = "jobs.estate.observed"
delivery_lane = "estate-watch"
[[rule.do]]
handler = "estate.compare"
"#;
    let raw = parse_raw(source).expect("delivery isolation is registry data");
    assert_eq!(
        serde_json::to_value(&raw.rules[0]).unwrap()["delivery_lane"],
        "estate-watch"
    );
    Registry::from_toml(source).expect("the runtime reads the same declaration");
}

#[tokio::test]
async fn the_normal_authored_file_seed_preserves_a_lane_without_hand_publication() {
    let db = boss_testing::TestDb::new().await;
    let dir = tempfile::tempdir().unwrap();
    let source = "[[rule]]\nname=\"lane-seed-contract\"\nwhy=\"External ingress/glue: compare an estate observation through the existing registered handler\"\non_event=\"jobs.estate.observed\"\ndelivery_lane=\"estate-watch\"\n[[rule.do]]\nhandler=\"estate.compare\"\n";
    std::fs::write(dir.path().join("lane-seed-contract.toml"), source).unwrap();
    boss_dispatcher::rules::seed::seed_authored_rules(&db.pool, dir.path())
        .await
        .unwrap();
    boss_dispatcher::rules::seed::seed_authored_rules(&db.pool, dir.path())
        .await
        .unwrap();
    let loaded = boss_dispatcher::rules::registry::load_active_rules(&db.pool)
        .await
        .unwrap();
    let rule = loaded
        .rules
        .iter()
        .find(|rule| rule.name == "lane-seed-contract")
        .unwrap();
    assert_eq!(rule.delivery_lane.as_deref(), Some("estate-watch"));
    let mut receipt = cutover();
    receipt.members = std::collections::BTreeMap::from([("lane-seed-contract".into(), 1)]);
    sqlx::query("INSERT INTO dispatcher_delivery_lanes (name,receipt) VALUES ($1,$2)")
        .bind("estate-watch")
        .bind(serde_json::to_value(receipt).unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
    boss_dispatcher::rules::seed::seed_authored_rules(&db.pool, dir.path())
        .await
        .unwrap();
    let active:i64=sqlx::query_scalar("SELECT COUNT(*) FROM dispatcher_rules WHERE name='lane-seed-contract' AND version=1 AND status='active'").fetch_one(&db.pool).await.unwrap();
    assert_eq!(
        active, 1,
        "unchanged normal seed/reconcile remains valid after first activation"
    );
}

#[tokio::test]
async fn an_independent_rule_invokes_while_the_default_handler_is_blocked() {
    use boss_dispatcher::rules::expr::Value;
    use boss_dispatcher::rules::{
        authoring,
        expr::NoHelpers,
        handler::{Handler, HandlerError, HandlerRegistry, InvocationContext, RecordingHandler},
        runner::RulesRunner,
    };
    use boss_nats::durable::Settle;
    struct Blocked {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    #[async_trait::async_trait]
    impl Handler for Blocked {
        fn name(&self) -> &'static str {
            "blocked"
        }
        async fn invoke(
            &self,
            _: &[(String, Value)],
            _: &InvocationContext,
        ) -> Result<(), HandlerError> {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(())
        }
    }
    let db = boss_testing::TestDb::new().await;
    let source = r#"[[rule]]
name="ordinary"
on_event="jobs.ordinary"
[[rule.do]]
handler="blocked"
[[rule]]
name="watch"
on_event="jobs.estate.observed"
delivery_lane="estate-watch"
[[rule.do]]
handler="record"
"#;
    for raw in parse_raw(source).unwrap().rules {
        authoring::create_draft(&db.pool, &raw, None, "emp-test")
            .await
            .unwrap();
        authoring::publish(&db.pool, &raw.name, "emp-test")
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO dispatcher_delivery_lanes (name,receipt) VALUES ($1,$2)")
        .bind("estate-watch")
        .bind(serde_json::to_value(cutover()).unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
    let blocked = Arc::new(Blocked {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let record = RecordingHandler::new("record");
    let mut handlers = HandlerRegistry::new();
    handlers.register(blocked.clone());
    handlers.register(record.clone());
    let runner = RulesRunner {
        registry: Registry::from_toml(source).unwrap(),
        delivery_routes: Some(Arc::new(PgDeliveryRoutes(db.pool.clone()))),
        handlers,
        helpers: Arc::new(NoHelpers),
        dead_letters: None,
        live: None,
        firings: None,
    };
    let default = runner.clone();
    let task = tokio::spawn(async move {
        default
            .handle_delivery(
                "jobs.ordinary",
                "ordinary-event",
                &serde_json::json!({}),
                1,
                None,
                Some(&position(None, 12)),
            )
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        blocked.entered.notified(),
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runner.handle_delivery(
            "jobs.estate.observed",
            "observation-event",
            &serde_json::json!({}),
            1,
            None,
            Some(&position(Some("estate-watch"), 11)),
        ),
    )
    .await;
    blocked.release.notify_one();
    assert!(matches!(task.await.unwrap(), Settle::Ack));
    assert!(matches!(result.unwrap(), Settle::Ack));
    assert_eq!(record.calls().await.len(), 1);
}

#[tokio::test]
async fn refused_rebinding_keeps_one_actual_side_effect_for_the_same_position() {
    use boss_dispatcher::rules::{
        authoring,
        expr::{NoHelpers, Value},
        handler::{Handler, HandlerError, HandlerRegistry, InvocationContext},
        runner::RulesRunner,
    };
    use boss_nats::durable::Settle;
    struct Effect(sqlx::PgPool);
    #[async_trait::async_trait]
    impl Handler for Effect {
        fn name(&self) -> &'static str {
            "effect"
        }
        async fn invoke(
            &self,
            _: &[(String, Value)],
            ctx: &InvocationContext,
        ) -> Result<(), HandlerError> {
            sqlx::query("INSERT INTO test_lane_effects (event_id) VALUES ($1)")
                .bind(&ctx.triggering_event_id)
                .execute(&self.0)
                .await
                .map_err(|e| HandlerError::Downstream(e.to_string()))?;
            Ok(())
        }
    }
    let db = boss_testing::TestDb::new().await;
    sqlx::query("CREATE TABLE test_lane_effects (event_id TEXT NOT NULL)")
        .execute(&db.pool)
        .await
        .unwrap();
    let source = "[[rule]]\nname=\"watch\"\non_event=\"jobs.estate.observed\"\ndelivery_lane=\"estate-watch\"\n[[rule.do]]\nhandler=\"effect\"\n";
    let enabled = parse_raw(source).unwrap().rules.remove(0);
    authoring::create_draft(&db.pool, &enabled, None, "emp-test")
        .await
        .unwrap();
    authoring::publish(&db.pool, "watch", "emp-test")
        .await
        .unwrap();
    sqlx::query("INSERT INTO dispatcher_delivery_lanes (name,receipt) VALUES ($1,$2)")
        .bind("estate-watch")
        .bind(serde_json::to_value(cutover()).unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
    let mut handlers = HandlerRegistry::new();
    handlers.register(Arc::new(Effect(db.pool.clone())));
    let runner = RulesRunner {
        registry: Registry::from_toml(source).unwrap(),
        delivery_routes: Some(Arc::new(PgDeliveryRoutes(db.pool.clone()))),
        handlers,
        helpers: Arc::new(NoHelpers),
        dead_letters: None,
        live: None,
        firings: None,
    };
    let mut default = enabled.clone();
    default.delivery_lane = None;
    authoring::create_draft(&db.pool, &default, None, "emp-test")
        .await
        .unwrap();
    assert!(
        authoring::publish(&db.pool, "watch", "emp-test")
            .await
            .is_err()
    );
    let event_id = "00000000-0000-4000-8000-000000000011";
    assert!(matches!(
        runner
            .handle_delivery(
                "jobs.estate.observed",
                event_id,
                &serde_json::json!({}),
                1,
                None,
                Some(&position(None, 11))
            )
            .await,
        Settle::Ack
    ));
    let after_default: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM test_lane_effects WHERE event_id=$1")
            .bind(event_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        after_default, 0,
        "failed default publication leaves the original isolated binding active"
    );
    assert!(matches!(
        runner
            .handle_delivery(
                "jobs.estate.observed",
                event_id,
                &serde_json::json!({}),
                1,
                None,
                Some(&position(Some("estate-watch"), 11))
            )
            .await,
        Settle::Ack
    ));
    let after_isolated: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM test_lane_effects WHERE event_id=$1")
            .bind(event_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        after_isolated, 1,
        "one immutable position cannot take both routes after a refused rebind"
    );
}
