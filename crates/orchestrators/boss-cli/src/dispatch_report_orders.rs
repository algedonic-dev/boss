//! THE THREE ORDERS A RUN'S ENDING AND ITS REPORT CAN ARRIVE IN, each
//! driven through the real doors (backlog b5a3a174, 2026-10-08).
//!
//! `boss dispatch --report`, the landing rule and the unreported clock
//! each had tests of their own, and each test stubbed the other two
//! with a route that answered `recorded: true`. So nothing could see
//! what they did TOGETHER, which on 2026-10-08 was this: a run landed
//! on a row holding no count, a minute before its own report, and the
//! report — 10,703,524 metered tokens on run ca00400b — was refused on
//! that row with exit 1. 21 of the day's 56 rows read no count or $0.
//!
//! Here the jobs router, the agent-runs door, the agents registry and
//! the login door are the ones `boss_jobs_api` mounts (in memory), the
//! protocol is the shipped `agent-run` row, the rules are read from
//! `infra/dispatcher/rules/`, the handlers are the dispatcher's own,
//! and the report is this crate's verb. What each order must leave:
//!
//!   - ONE row for the run, priced from the count its report carried —
//!     never from a placeholder, and never refusing that report;
//!   - `reported` holding the agent's own handback as its summary;
//!   - `--report` answering Ok when it did its job;
//!   - for a run that never reports, a row that says it holds no price
//!     rather than a zero that reads as one.

// One `#[cfg(test)]` item, so the readers that judge this crate's
// PRODUCTION text (`boss_testing::production_source`) see none of it —
// they read each file by itself and cannot see `main.rs` gate the module.
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use boss_dispatcher::rules::expr::Value as Arg;
    use boss_dispatcher::rules::handler::{Handler, InvocationContext};
    use boss_dispatcher_handlers::handlers::jobs_age_out_step::JobsAgeOutStep;
    use boss_dispatcher_handlers::handlers::jobs_complete_step_from_record::JobsCompleteStepFromRecord;
    use boss_jobs::agent_runs::{
        AgentRun, AgentRunLog, InMemoryAgentRuns, PricingBasis, RateCardRow, RunFilter, TokenUsage,
    };
    use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
    use boss_policy_client::{FakePolicyClient, PolicyClient};
    use reqwest::Method;
    use serde_json::{Value, json};

    use crate::dispatch::{Report, Tokens, report_at};

    /// The login the run signs with and the registered agent it resolves
    /// to — the pod's own pair.
    const LOGIN: &str = "claude@algedonic.dev";
    const AGENT: &str = "agent-claude";
    const PACKET: &str = "b5a3a174-7627-40c8-b1cb-3f8eeff722f0";
    const LANDING: &str = "agent-run-lands-a-report-sent-before-green";
    const UNREPORTED: &str = "agent-run-ends-unreported-when-the-handback-never-arrives";

    struct Stack {
        base: String,
        http: boss_core::machine_token::Client,
        runs: Arc<InMemoryAgentRuns>,
    }

    /// The card's live shape for the declared model: a split is priced at
    /// the two rates.
    fn card() -> Vec<RateCardRow> {
        vec![RateCardRow {
            model: "opus-5[1m]".into(),
            input_usd_micros_per_mtok: 5_000_000,
            output_usd_micros_per_mtok: 25_000_000,
            note: "Claude Opus 5, 1M context".into(),
            blended_input_share_ppm: Some(875_000),
            cache_read_usd_micros_per_mtok: Some(500_000),
            cache_write_usd_micros_per_mtok: Some(6_250_000),
        }]
    }

    async fn serve() -> Stack {
        let kinds = Arc::new(InMemoryWorkflows::for_fixture());
        for spec in boss_jobs::registry::seedable_platform_workflows() {
            kinds.seed(spec).expect("seed platform kind");
        }
        let policy: Arc<dyn PolicyClient> =
            Arc::new(FakePolicyClient::builder().with_default_rules().build());
        let bus = boss_testing::RecordingEventBus::new();
        let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
        let publisher = boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs");
        let state = boss_jobs::http::JobsApiState {
            kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
            ..boss_jobs::http::JobsApiState::minimal(
                Arc::new(InMemoryJobs::new()),
                bus,
                publisher.clone(),
                policy,
                Arc::new(boss_clock_client::WallClockClient),
            )
        };
        let agents = Arc::new(boss_jobs::agents::InMemoryAgents::new().with_agent(AGENT, [LOGIN]));
        let runs =
            Arc::new(InMemoryAgentRuns::new(card()).with_registered_agent(AGENT, "opus-5[1m]"));
        let door = Arc::new(boss_jobs::agents::LoginDoor::new(agents.clone(), publisher));
        let app = boss_jobs::http::router(state)
            .merge(boss_jobs::agent_runs::http::router(
                boss_jobs::agent_runs::http::AgentRunsApiState { log: runs.clone() },
            ))
            .merge(boss_jobs::agents::http::router(
                boss_jobs::agents::http::AgentsApiState {
                    registry: agents,
                    classes: None,
                    departments: None,
                },
            ))
            .layer(axum::middleware::from_fn_with_state(
                door,
                boss_jobs::agents::resolve_login,
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Stack {
            base: format!("http://{addr}"),
            http: boss_dispatcher_handlers::handlers::common::api_client(),
            runs,
        }
    }

    impl Stack {
        async fn api(&self, method: Method, path: &str, body: Option<Value>) -> Option<Value> {
            crate::gate::api_at_signed(
                &self.http,
                &self.base,
                method.clone(),
                path,
                body,
                crate::identity::Signature::As(LOGIN.to_string()),
            )
            .await
            .unwrap_or_else(|e| panic!("{method} {path}: {e:#}"))
        }

        async fn run(&self, id: &str) -> Value {
            self.api(Method::GET, &format!("/api/jobs/{id}"), None)
                .await
                .expect("the run reads")
        }

        fn step<'a>(run: &'a Value, slug: &str) -> &'a Value {
            run["steps"]
                .as_array()
                .expect("steps")
                .iter()
                .find(|s| s["spec_slug"] == json!(slug))
                .unwrap_or_else(|| panic!("the run has a `{slug}` step"))
        }

        async fn finish(&self, id: &str, slug: &str, fields: Value) {
            let run = self.run(id).await;
            let sid = Self::step(&run, slug)["id"].as_str().unwrap().to_string();
            self.api(
                Method::PATCH,
                &format!("/api/jobs/{id}/steps/{sid}/metadata"),
                Some(fields),
            )
            .await;
            self.api(
                Method::PUT,
                &format!("/api/jobs/{id}/steps/{sid}"),
                Some(json!({ "status": "completed" })),
            )
            .await;
        }

        /// A run as `boss dispatch` leaves it: filed, briefed, and
        /// `building` placed with the run's own registered agent.
        async fn dispatched(&self) -> String {
            let made = self
                .api(
                    Method::POST,
                    "/api/jobs",
                    Some(json!({
                        "kind": "agent-run",
                        "subject": { "subject_kind": "custom", "id": "bosspipeline" },
                        "title": "builder run: a run is priced once, from its own report",
                        "owner_id": "emp-david",
                        "priority": "standard",
                        "status": "open",
                        "tags": [],
                        "metadata": {
                            "packet": PACKET, "step": "build", "agent": LOGIN,
                            "model": "opus-5[1m]", "budget_usd": 5.0, "effort": "medium",
                        },
                    })),
                )
                .await
                .expect("the run is filed");
            let id = made["id"].as_str().expect("an id").to_string();
            self.finish(&id, "briefed", json!({ "prompt_bytes": "40763" }))
                .await;
            let run = self.run(&id).await;
            let building = Self::step(&run, "building")["id"]
                .as_str()
                .unwrap()
                .to_string();
            self.api(
                Method::PUT,
                &format!("/api/jobs/{id}/steps/{building}"),
                Some(json!({ "assignee_id": AGENT })),
            )
            .await;
            id
        }

        /// The gate's green, as `agent-run-lands-on-gate-green` writes it,
        /// and then the landing rule's firing on `reported` going ready.
        async fn green(&self, id: &str) {
            self.finish(
                id,
                "building",
                json!({ "result": "gated", "gate_run": { "branch": "fix/the-car" } }),
            )
            .await;
            let run = self.run(id).await;
            let reported = Self::step(&run, "reported");
            assert_eq!(reported["status"], "ready", "the green opens `reported`");
            JobsCompleteStepFromRecord::with_client(self.http.clone(), &self.base)
                .invoke(
                    &rule_args(LANDING),
                    &InvocationContext {
                        event_timestamp: None,
                        rule_name: LANDING.into(),
                        triggering_event_id: "ev-ready".into(),
                        triggering_topic: "step.ready.task".into(),
                        event_payload: json!({
                            "job_id": id, "step_id": reported["id"], "kind": "task",
                            "workflow_kind": "agent-run", "spec_slug": "reported",
                        }),
                    },
                )
                .await
                .expect("the landing rule fires");
        }

        /// The hourly tick of the unreported clock, three hours on.
        async fn tick(&self) {
            let at = (chrono::Utc::now() + chrono::Duration::hours(3)).to_rfc3339();
            JobsAgeOutStep::with_client(self.http.clone(), &self.base)
                .invoke(
                    &rule_args(UNREPORTED),
                    &InvocationContext {
                        event_timestamp: None,
                        rule_name: UNREPORTED.into(),
                        triggering_event_id: "tick-1".into(),
                        triggering_topic: "schedule".into(),
                        event_payload: json!({ "_day": &at[..10], "_at": at }),
                    },
                )
                .await
                .expect("the clock ticks");
        }

        async fn report(&self, id: &str, report: &Report) -> anyhow::Result<()> {
            report_at(
                &self.http,
                &self.base,
                id,
                report,
                None,
                LOGIN,
                chrono::Utc::now(),
            )
            .await
        }

        async fn rows(&self) -> Vec<AgentRun> {
            self.runs
                .list_runs(&RunFilter::default())
                .await
                .expect("lists")
        }
    }

    /// A rule's args as its file under `infra/dispatcher/rules/` authors
    /// them — each an expression, a quoted string literal — so these tests
    /// run the rules that run (CLAUDE.md §9a).
    fn rule_args(name: &str) -> Vec<(String, Arg)> {
        let path = boss_testing::repo_root()
            .join("infra/dispatcher/rules")
            .join(format!("{name}.toml"));
        let text = std::fs::read_to_string(&path).expect("the rule is authored");
        let rule: toml::Value = toml::from_str(&text).expect("the rule parses");
        rule["rule"][0]["do"][0]["args"]
            .as_table()
            .expect("the rule has args")
            .iter()
            .map(|(k, v)| {
                let lit: String = serde_json::from_str(v.as_str().expect("an expression string"))
                    .expect("a string literal");
                (k.clone(), Arg::String(lit))
            })
            .collect()
    }

    const HANDBACK: &str = "item b5a3a174, branch fix/the-car, sha abc1234, gate green";

    /// The agent's own report: its handback and the count it metered.
    fn handback() -> Report {
        Report {
            summary: HANDBACK.into(),
            spend_usd: None,
            meter: None,
            tokens: Some(Tokens::Split {
                input: 740_000,
                output: 21_000,
            }),
        }
    }

    /// 740,000 in at $5 and 21,000 out at $25 per MTok.
    const PRICE: u64 = 4_225_000;

    fn assert_priced_from_the_report(row: &AgentRun) {
        assert_eq!(
            row.run.tokens,
            TokenUsage::Split {
                input: 740_000,
                output: 21_000
            },
            "the row holds the count the report carried"
        );
        assert_eq!(row.usd_micros, Some(PRICE));
        assert_eq!(row.pricing_basis(), Some(PricingBasis::Split));
        assert_eq!(row.run.actor_id.to_string(), AGENT);
        assert_eq!(row.run.branch.as_deref(), Some("fix/the-car"));
        assert_eq!(
            row.run.job_id.map(|j| j.to_string()).as_deref(),
            Some(PACKET)
        );
    }

    /// ORDER ONE — the report, then the green. The report rides the packet
    /// with its finish record; the green lands both.
    #[tokio::test(flavor = "multi_thread")]
    async fn report_then_green_prices_the_run_from_the_report_once() {
        let s = serve().await;
        let id = s.dispatched().await;

        s.report(&id, &handback())
            .await
            .expect("a report before the green is on the packet, and that is its job");
        assert!(s.rows().await.is_empty(), "no terminal yet, no row yet");

        s.green(&id).await;

        let rows = s.rows().await;
        assert_eq!(rows.len(), 1, "one run, one row: {rows:?}");
        assert_priced_from_the_report(&rows[0]);
        assert!(rows[0].run.detail.get("replaced").is_none(), "written once");
        assert_eq!(s.runs.recorded_events().await.len(), 1);

        let run = s.run(&id).await;
        let reported = Stack::step(&run, "reported");
        assert_eq!(reported["status"], "completed");
        assert_eq!(reported["metadata"]["summary"], HANDBACK);
        assert_eq!(reported["metadata"]["handback"], "recorded");
    }

    /// ORDER TWO — the green, then the report: the order that failed on
    /// every run of 2026-10-08. The green finds no report on the packet and
    /// writes nothing — no placeholder row, no placeholder summary — and
    /// the report then completes `reported` in the agent's words and prices
    /// the run.
    #[tokio::test(flavor = "multi_thread")]
    async fn green_then_report_prices_the_run_from_the_report_once() {
        let s = serve().await;
        let id = s.dispatched().await;

        s.green(&id).await;
        assert!(
            s.rows().await.is_empty(),
            "a green with no report on the packet posts no row to stand in for one"
        );
        assert_eq!(
            Stack::step(&s.run(&id).await, "reported")["status"],
            "ready",
            "and `reported` waits for the handback"
        );

        s.report(&id, &handback())
            .await
            .expect("--report exits 0 when it did its job");

        let rows = s.rows().await;
        assert_eq!(rows.len(), 1, "one run, one row: {rows:?}");
        assert_priced_from_the_report(&rows[0]);
        assert_eq!(s.runs.recorded_events().await.len(), 1);

        let run = s.run(&id).await;
        let reported = Stack::step(&run, "reported");
        assert_eq!(reported["status"], "completed");
        assert_eq!(reported["metadata"]["summary"], HANDBACK);
    }

    /// ORDER TWO AS IT WAS MEASURED — a report holding no count had reached
    /// the packet first (the stop hook's, at a background launch: "(the
    /// Agent tool returned no text)", no tokens), so the green landed the
    /// run on a row that priced nothing. The hook no longer sends that
    /// (`dev_hooks_sh`), but any report without a count leaves the same
    /// row, and the door must not let it stand against the real figures:
    /// the agent's report replaces it, once, and exits 0.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_row_that_landed_holding_no_count_is_replaced_by_the_runs_own_report() {
        let s = serve().await;
        let id = s.dispatched().await;

        let launch = Report {
            summary: "(the Agent tool returned no text)".into(),
            spend_usd: None,
            meter: None,
            tokens: None,
        };
        s.report(&id, &launch).await.expect("the launch report");
        s.green(&id).await;
        let rows = s.rows().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].run.tokens, TokenUsage::Unreported);
        assert_eq!(rows[0].usd_micros, None, "no count is no price — not $0");
        let placeholder_at = rows[0].recorded_at;

        // TODAY'S REFUSAL, at this line: "agent_runs already holds run … and
        // this report did NOT change it: the row is insert-once", exit 1.
        s.report(&id, &handback())
            .await
            .expect("the report that carries the run's real usage is not refused");

        let rows = s.rows().await;
        assert_eq!(rows.len(), 1, "still one row: {rows:?}");
        assert_priced_from_the_report(&rows[0]);
        assert_eq!(
            rows[0].run.detail["replaced"]["recorded_at"],
            json!(placeholder_at),
            "the replacement is on the record"
        );
        assert_eq!(
            s.run(&id).await["metadata"]["report"],
            HANDBACK,
            "and the packet holds the handback"
        );

        // ONCE: a third report with another count changes nothing, and says
        // so as the refusal it always was (backlog b4fd594e).
        let other = Report {
            tokens: Some(Tokens::Total(7)),
            ..handback()
        };
        let why = s
            .report(&id, &other)
            .await
            .expect_err("a row that holds a count is not replaced");
        assert!(format!("{why:#}").contains("insert-once"), "{why:#}");
        assert_priced_from_the_report(&s.rows().await[0]);
        assert_eq!(s.runs.recorded_events().await.len(), 2);
    }

    /// ORDER THREE — the green, and no report, ever. The clock ends the run
    /// `unreported`, and the run still has a row: one that holds no count
    /// and says why, so the cost record neither omits the run nor prices it
    /// at a zero nobody measured. A report that arrives after all replaces
    /// that row, once.
    #[tokio::test(flavor = "multi_thread")]
    async fn green_with_no_report_ends_with_a_row_that_says_it_is_unpriced() {
        let s = serve().await;
        let id = s.dispatched().await;
        s.green(&id).await;
        assert!(s.rows().await.is_empty());

        s.tick().await;

        let rows = s.rows().await;
        assert_eq!(rows.len(), 1, "the run is on the cost record: {rows:?}");
        let row = &rows[0];
        assert_eq!(row.run.run_id, id);
        assert_eq!(
            row.run.tokens,
            TokenUsage::Unreported,
            "no count — not a zero"
        );
        assert_eq!(
            row.usd_micros, None,
            "unpriced — not $0 presented as a cost"
        );
        assert!(
            row.run.detail["unpriced"]
                .as_str()
                .is_some_and(|w| w.starts_with("never reported")),
            "the row says why it has no price: {}",
            row.run.detail
        );
        assert_eq!(row.run.actor_id.to_string(), AGENT);
        assert_eq!(row.run.model(), Some("opus-5[1m]"));
        assert_eq!(row.run.branch.as_deref(), Some("fix/the-car"));

        let run = s.run(&id).await;
        let reported = Stack::step(&run, "reported");
        assert_eq!(reported["status"], "completed");
        assert_eq!(reported["metadata"]["handback"], "absent");
        assert!(
            reported["metadata"]["aged_out"]["posted"]
                .as_str()
                .is_some_and(|p| p.starts_with("recorded")),
            "{}",
            reported["metadata"]
        );

        // The handback that arrives after the clock gave up still prices
        // the run.
        s.report(&id, &handback())
            .await
            .expect("a late report still does its job");
        let rows = s.rows().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].usd_micros, Some(PRICE));
        assert!(rows[0].run.detail["replaced"].is_object());
    }
}
