//! Exercises the cross-VM ingress bridge end-to-end with an InMemoryEventBus.
//!
//! A message published as an S1 envelope to the bus must be consumed by
//! ingress and flow through the Cybernetics loop like any other submit.

use std::sync::Arc;
use std::time::Duration;

use boss_core::agent::{AgentId, AgentSpec, Message};
use boss_core::event::Event;
use boss_core::port::{EventBus, EventStream};
use boss_cybernetics::Cybernetics;
use boss_cybernetics::ingress::{Ingress, envelope};
use boss_cybernetics::telemetry::MESSAGE_ENQUEUED;
use boss_events::bus::InMemoryEventBus;
use boss_events::dispatcher::StubDispatcher;
use boss_events::ledger::InMemoryCostLedger;
use boss_events::queue::InMemoryMessageQueue;
use boss_events::registry::InMemoryAgentRegistry;
use tokio::sync::watch;

fn planner() -> AgentSpec {
    AgentSpec {
        id: AgentId::try_new("planner").unwrap(),
        display_name: "Planner".into(),
        system_prompt: String::new(),
        model: "test".into(),
        hourly_budget_usd_micros: 1_000_000,
        max_concurrent_runs: 1,
    }
}

async fn expect_kind(stream: &mut Box<dyn EventStream>, kind: &str) -> Event {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(e)) if e.kind == kind => return e,
            Ok(Some(_)) => continue,
            Ok(None) => panic!("stream closed before observing {kind}"),
            Err(_) => panic!("timed out waiting for {kind}"),
        }
    }
}

#[tokio::test]
async fn envelope_published_to_bus_reaches_submit() {
    let vm = "vm-test";
    let bus: Arc<InMemoryEventBus> = Arc::new(InMemoryEventBus::new(64));
    let queue = Arc::new(InMemoryMessageQueue::new());
    let ledger = Arc::new(InMemoryCostLedger::new());
    let dispatcher = Arc::new(StubDispatcher::default());
    let registry = Arc::new(InMemoryAgentRegistry::new([planner()]));
    let clock = Arc::new(boss_clock_client::WallClockClient);
    let publisher = Arc::new(boss_core::publisher::DomainPublisher::new(
        bus.clone(),
        format!("cybernetics/{}", vm),
    ));
    let cyb = Arc::new(Cybernetics::new(
        vm,
        queue.clone(),
        ledger,
        dispatcher,
        registry,
        publisher.clone(),
        clock,
    ));

    // Observe telemetry by subscribing to the same bus.
    let mut tele = bus.subscribe("cybernetics.>").await.unwrap();

    // Start both the Cybernetics loop and the ingress bridge.
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let runner = cyb.clone();
    let loop_rx = cancel_rx.clone();
    let loop_handle = tokio::spawn(async move {
        let _ = runner.run(loop_rx).await;
    });

    let ingress_bus: Arc<dyn EventBus> = bus.clone();
    let attached = Ingress::new(cyb.clone()).attach(ingress_bus).await.unwrap();
    let ingress_rx = cancel_rx.clone();
    let ingress_handle = tokio::spawn(async move {
        let _ = attached.run(ingress_rx).await;
    });

    // Publish a cross-VM S1 envelope targeted at this VM's planner.
    let msg = Message::new(
        AgentId::try_new("planner").unwrap(),
        "work.plan",
        serde_json::json!({"n": 7}),
    );
    let env = envelope(vm, "test-sender", &msg);
    assert_eq!(env.kind, "boss.s1.vm-test.planner.work.plan");
    bus.publish(env).await.unwrap();

    // Ingress should deliver the message to Cybernetics; observe enqueued.
    let enq = expect_kind(&mut tele, MESSAGE_ENQUEUED).await;
    assert_eq!(enq.payload["agent"], "planner");
    assert_eq!(enq.payload["kind"], "work.plan");
    assert_eq!(
        enq.payload["message_id"].as_str().unwrap(),
        msg.id.to_string()
    );

    // Clean shutdown.
    let _ = cancel_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_millis(500), loop_handle).await;
    let _ = tokio::time::timeout(Duration::from_millis(500), ingress_handle).await;
}

#[tokio::test]
async fn malformed_envelope_is_dropped_not_fatal() {
    let vm = "vm-test";
    let bus: Arc<InMemoryEventBus> = Arc::new(InMemoryEventBus::new(64));
    let queue = Arc::new(InMemoryMessageQueue::new());
    let ledger = Arc::new(InMemoryCostLedger::new());
    let dispatcher = Arc::new(StubDispatcher::default());
    let registry = Arc::new(InMemoryAgentRegistry::new([planner()]));
    let clock = Arc::new(boss_clock_client::WallClockClient);
    let publisher = Arc::new(boss_core::publisher::DomainPublisher::new(
        bus.clone(),
        format!("cybernetics/{}", vm),
    ));
    let cyb = Arc::new(Cybernetics::new(
        vm,
        queue.clone(),
        ledger,
        dispatcher,
        registry,
        publisher.clone(),
        clock,
    ));
    let mut tele = bus.subscribe("cybernetics.>").await.unwrap();

    let (cancel_tx, cancel_rx) = watch::channel(false);
    let ingress_bus: Arc<dyn EventBus> = bus.clone();
    let attached = Ingress::new(cyb.clone()).attach(ingress_bus).await.unwrap();
    let ingress_handle = tokio::spawn(async move {
        let _ = attached.run(cancel_rx).await;
    });

    // Garbage payload on a matching subject.
    bus.publish(Event::new(
        "bad-sender",
        format!("boss.s1.{vm}.planner.broken"),
        serde_json::json!({"not": "a message"}),
        chrono::Utc::now(),
    ))
    .await
    .unwrap();

    // Then a valid one.
    let msg = Message::new(
        AgentId::try_new("planner").unwrap(),
        "work.ok",
        serde_json::json!({}),
    );
    bus.publish(envelope(vm, "t", &msg)).await.unwrap();

    // We should see the good message enqueued (the bad one was skipped).
    let enq = expect_kind(&mut tele, MESSAGE_ENQUEUED).await;
    assert_eq!(enq.payload["kind"], "work.ok");

    let _ = cancel_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_millis(500), ingress_handle).await;
}
