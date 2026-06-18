//! Synthetic agent oversight for tenants without real
//! `boss-cybernetics` deployed yet. The brewery playground uses
//! this so `/ops` shows what agent oversight LOOKS like before
//! anyone wires their first real agent in. Tenants leaving
//! `demo_agents` absent in the obs config get the real
//! aggregator path — this code is gated behind that flag.
//!
//! Two surfaces:
//!
//! 1. `snapshot()` returns canned VmResult-shaped JSON for
//!    /health, /agents, /queues, /runs, /costs so the
//!    `/api/snapshot` endpoint reads as a populated dashboard
//!    instead of an empty grid.
//!
//! 2. `spawn_telemetry_loop()` pushes a periodic stream of
//!    plausible cybernetics events into the SSE hub —
//!    dispatch-requested → started → completed cycles, periodic
//!    cost.recorded pings. Operators see live agent activity
//!    without any real LLM calls.

use std::time::Duration;

use crate::aggregator::VmResult;
use crate::sse::SseHub;
use boss_core::event::Event;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::time::interval;
use tracing::info;

const VM_ID: &str = "demo-vm";

/// Brewery-flavoured agent roster. Realistic names, budgets, and
/// concurrency caps so the /ops Agents panel reads like a real
/// cybernetics deployment.
fn agent_specs() -> Value {
    json!([
        {
            "id": "agent-hop-sourcing-scout",
            "display_name": "Hop sourcing scout",
            "system_prompt": "Watch supplier price feeds + cone-quality reports; flag any cultivar where the trailing 30-day price climbs >10% or COA fails the brewery's spec.",
            "model": "claude-haiku-4-5",
            "hourly_budget_usd_micros": 250_000,
            "max_concurrent_runs": 3
        },
        {
            "id": "agent-tap-launch-copywriter",
            "display_name": "Tap launch copywriter",
            "system_prompt": "Draft tap-handle copy + social-post variants for new releases. One pass per tap-launch JobKind step.done event.",
            "model": "claude-sonnet-4-6",
            "hourly_budget_usd_micros": 1_500_000,
            "max_concurrent_runs": 2
        },
        {
            "id": "agent-inventory-reorder-advisor",
            "display_name": "Inventory reorder advisor",
            "system_prompt": "Compute days-of-cover per SKU from the rolling consumption window; recommend reorder qtys when projected stockout < lead_time + 5d.",
            "model": "claude-haiku-4-5",
            "hourly_budget_usd_micros": 400_000,
            "max_concurrent_runs": 5
        },
        {
            "id": "agent-tasting-note-synthesizer",
            "display_name": "QA tasting-note synthesizer",
            "system_prompt": "Synthesize tasting-panel notes into a structured QA record. Triggered by step.done.handoff; one run per batch.",
            "model": "claude-sonnet-4-6",
            "hourly_budget_usd_micros": 800_000,
            "max_concurrent_runs": 2
        }
    ])
}

fn queues() -> Value {
    json!([
        { "agent": "agent-hop-sourcing-scout", "depth": 2 },
        { "agent": "agent-tap-launch-copywriter", "depth": 1 },
        { "agent": "agent-inventory-reorder-advisor", "depth": 4 },
        { "agent": "agent-tasting-note-synthesizer", "depth": 1 }
    ])
}

fn runs() -> Value {
    let now = chrono::Utc::now();
    let started_a = (now - chrono::Duration::seconds(42)).to_rfc3339();
    let started_b = (now - chrono::Duration::seconds(11)).to_rfc3339();
    json!([
        { "id": "run-hop-001", "agent": "agent-hop-sourcing-scout", "started_at": started_a },
        { "id": "run-inv-014", "agent": "agent-inventory-reorder-advisor", "started_at": started_b }
    ])
}

fn costs() -> Value {
    json!([
        {
            "agent": "agent-hop-sourcing-scout",
            "cost": { "input_tokens": 14_220, "output_tokens": 2_140, "usd_micros": 8_400 },
            "window": "hour"
        },
        {
            "agent": "agent-tap-launch-copywriter",
            "cost": { "input_tokens": 2_500, "output_tokens": 1_800, "usd_micros": 24_500 },
            "window": "hour"
        },
        {
            "agent": "agent-inventory-reorder-advisor",
            "cost": { "input_tokens": 31_400, "output_tokens": 4_600, "usd_micros": 19_200 },
            "window": "hour"
        },
        {
            "agent": "agent-tasting-note-synthesizer",
            "cost": { "input_tokens": 4_100, "output_tokens": 2_900, "usd_micros": 36_800 },
            "window": "hour"
        },
        {
            "agent": "agent-hop-sourcing-scout",
            "cost": { "input_tokens": 312_000, "output_tokens": 48_000, "usd_micros": 184_000 },
            "window": "day"
        },
        {
            "agent": "agent-tap-launch-copywriter",
            "cost": { "input_tokens": 41_000, "output_tokens": 28_000, "usd_micros": 412_000 },
            "window": "day"
        },
        {
            "agent": "agent-inventory-reorder-advisor",
            "cost": { "input_tokens": 720_000, "output_tokens": 92_000, "usd_micros": 388_000 },
            "window": "day"
        },
        {
            "agent": "agent-tasting-note-synthesizer",
            "cost": { "input_tokens": 78_000, "output_tokens": 58_000, "usd_micros": 644_000 },
            "window": "day"
        }
    ])
}

/// Build the five VmResult arrays the SPA expects. Each carries
/// a single demo VM with status 200; the body matches what
/// boss-cybernetics would return for a real VM.
///
/// `demo_mode: true` is included at the top level so the SPA's
/// `/ops` view can render an honest banner distinguishing the
/// synthetic surface from a real cybernetics deployment. Without
/// it, a first-time visitor would see "4 agents · $X/hr token
/// spend" and reasonably conclude the brewery is running real LLM
/// workers.
pub fn snapshot() -> serde_json::Value {
    json!({
        "demo_mode": true,
        "health": [vm_result(json!({
            "vm_id": VM_ID,
            "status": "healthy",
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }))],
        "agents": [vm_result(agent_specs())],
        "queues": [vm_result(queues())],
        "runs": [vm_result(runs())],
        "costs": [vm_result(costs())],
    })
}

fn vm_result(body: Value) -> Value {
    serde_json::to_value(VmResult {
        vm_id: VM_ID.to_string(),
        status: 200,
        body,
        error: None,
    })
    .unwrap_or(Value::Null)
}

/// Spawn a tokio task that pushes a periodic stream of synthetic
/// cybernetics telemetry events into the SSE hub. The cycle walks
/// dispatch.requested → dispatch.started → dispatch.completed
/// across the four demo agents, interleaved with cost.recorded
/// pings. Cancels cleanly when the watch flips.
pub fn spawn_telemetry_loop(hub: SseHub, tick_seconds: u64, cancel: watch::Receiver<bool>) {
    tokio::spawn(async move {
        info!(tick_seconds, "demo-agents telemetry loop starting");
        let mut tick = interval(Duration::from_secs(tick_seconds.max(1)));
        // Skip the first immediate tick so the first batch lands
        // `tick_seconds` after startup, not at startup.
        tick.tick().await;
        let agents = [
            "agent-hop-sourcing-scout",
            "agent-tap-launch-copywriter",
            "agent-inventory-reorder-advisor",
            "agent-tasting-note-synthesizer",
        ];
        let mut counter: u64 = 0;
        let mut cancel = cancel;
        loop {
            tokio::select! {
                _ = cancel.changed() => {
                    info!("demo-agents telemetry loop stopping");
                    break;
                }
                _ = tick.tick() => {
                    let agent = agents[(counter as usize) % agents.len()];
                    let phase = counter % 3;
                    let kind = match phase {
                        0 => "cybernetics.dispatch.requested",
                        1 => "cybernetics.dispatch.started",
                        _ => "cybernetics.dispatch.completed",
                    };
                    let run_id = format!(
                        "run-{}-{:04}",
                        agent.split('-').next_back().unwrap_or("?"),
                        counter / 3,
                    );
                    let payload = match phase {
                        0 => json!({
                            "agent": agent,
                            "run_id": run_id,
                            "trigger_topic": "step.done.handoff",
                        }),
                        1 => json!({
                            "agent": agent,
                            "run_id": run_id,
                            "model": "claude-haiku-4-5",
                        }),
                        _ => json!({
                            "agent": agent,
                            "run_id": run_id,
                            "duration_ms": 1_400 + (counter % 800),
                            "input_tokens": 4_200 + (counter * 31 % 4_000),
                            "output_tokens": 600 + (counter * 17 % 800),
                            "usd_micros": 8_400 + (counter * 47 % 16_000),
                        }),
                    };
                    // Synthetic demo loop — wall-clock is the
                    // intended source here. These events never land
                    // in audit_log; they only feed the /ops
                    // dashboard's live ticker on tenants without a
                    // real boss-cybernetics deployment.
                    hub.publish(Event::new(
                        "boss-cybernetics/demo-vm",
                        kind,
                        payload,
                        chrono::Utc::now(),
                    ));

                    // After every completed phase, sneak in a
                    // cost.recorded ping so the cost panel ticks
                    // as well.
                    if phase == 2 {
                        let cost = json!({
                            "agent": agent,
                            "run_id": run_id,
                            "input_tokens": 4_200,
                            "output_tokens": 720,
                            "usd_micros": 8_400,
                        });
                        hub.publish(Event::new(
                            "boss-cybernetics/demo-vm",
                            "cybernetics.cost.recorded",
                            cost,
                            chrono::Utc::now(),
                        ));
                    }

                    counter = counter.wrapping_add(1);
                }
            }
        }
    });
}
