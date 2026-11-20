//! `jobs.end_unlaunched_car` — a car admitted at build ENDS when what it
//! was built for ends before any gate ran: its agent-run closed `refused`
//! or `died`, or its backlog item closed `declined`, `stale` or
//! `duplicate` (backlog 366c9fb1; design 689d1f7f car 1).
//!
//! ## The case
//!
//! `boss dispatch --started` admits the run's car at `build` so the
//! engineering station shows work in build. A builder that then stops
//! without building ends its RUN `refused`; one that goes silent is ended
//! `died`. Neither wrote anything on the CAR, which stood open at `build`
//! for ever, and so did a car whose ITEM was declined under it. This arm
//! ends it through the `abandoned` terminal ship-a-change already has.
//!
//! ## One fence with the gate launch (Codex review of car 0d04ee3e, R4b)
//!
//! The decision is core's (`boss_jobs::car::ending_claim`), pure and
//! pinned there. Its FIRST write claims the car on the gate step, judged
//! on no `gate_run`; a gate launch's FIRST write stamps `gate_run` on that
//! same step, judged on no claim. Exactly one lands, and the loser has
//! written nothing. A repair's adoption is fenced on the same claim.
//!
//! ## Resumable (R4a)
//!
//! Everything after the claim is derived from the CLAIM
//! (`boss_jobs::car::ending_rest`), so a redelivery after any one write
//! — a crash, a refused request — finds the claim and sends what is
//! still owed, the same bytes. A write that fails is an error, which the
//! dispatcher redelivers; a launch or repair that meets the claim
//! finishes it too, so a claimed car is never left standing.
//!
//! Rule shape (`by` names which edge the closing packet is):
//! ```toml
//! on_event = "jobs.job.closed"
//! when = 'kind = "agent-run" AND (outcome = "refused" OR outcome = "died")'
//! [[rule.do]]
//! handler = "jobs.end_unlaunched_car"
//! args = { by = "\"run\"" }
//! ```

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value as ArgValue;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use boss_jobs::car::{self, Ended};
use serde_json::Value;
use std::sync::Arc;

use super::common::{
    Judged, api_client, get_json, row_or_refuse, write_json, write_json_expecting,
};
use super::jobs_auto_park::list_open_cars;

/// The handler's registered name.
pub const HANDLER: &str = "jobs.end_unlaunched_car";

pub struct JobsEndUnlaunchedCar {
    client: boss_core::machine_token::Client,
    jobs_base: String,
}

impl JobsEndUnlaunchedCar {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    async fn read_car(&self, id: &str, rule: &str) -> Result<Value, HandlerError> {
        let c = get_json(
            &self.client,
            &format!("{}/api/jobs/{id}", self.base()),
            rule,
        )
        .await?;
        row_or_refuse(c, &format!("GET /api/jobs/{id}")).map_err(HandlerError::Downstream)
    }

    /// End every car `ended` names that core says may end — or finish
    /// the ending a car already carries; say why of the rest. Returns
    /// the ids of the cars whose ending is now complete.
    pub(crate) async fn end_cars(
        &self,
        ended: Ended<'_>,
        now: chrono::DateTime<chrono::Utc>,
        rule: &str,
    ) -> Result<Vec<String>, HandlerError> {
        let cars = list_open_cars(&self.client, self.base(), true, rule).await?;
        let (edge, named) = ended.edge();
        let mut done = Vec::new();
        for c in cars.iter().filter(|c| {
            c.pointer(&format!("/metadata/{edge}"))
                .and_then(Value::as_str)
                == Some(named)
        }) {
            let id = c.get("id").and_then(Value::as_str).unwrap_or("?");
            if car::ending_of(c).is_none() {
                let claim = match car::ending_claim(c, ended, now) {
                    Ok(claim) => claim,
                    Err(why) => {
                        tracing::info!(rule = %rule, car = %id, "left alone: {why}");
                        continue;
                    }
                };
                let landed = write_json_expecting(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{}{}", self.base(), claim.path),
                    &claim.body,
                    &claim.expect,
                    rule,
                )
                .await?;
                if landed == Judged::Moved {
                    tracing::info!(rule = %rule, car = %id, "the car's gate step changed after \
                        this ending read it — a gate launched, a repair adopted it, or another \
                        ending claimed it first");
                }
            }
            // FROM THE RECORD, NOT THE READ: what is owed is read off the
            // car as it stands after the claim, whoever's claim it is.
            let fresh = self.read_car(id, rule).await?;
            if car::ending_of(&fresh).is_none() {
                tracing::info!(rule = %rule, car = %id, "no ending holds the car — it is the \
                    launch's or the repair's, and nothing is ended");
                continue;
            }
            let owed = car::ending_rest(&fresh).map_err(HandlerError::Downstream)?;
            for w in owed {
                let method = if w.method == "PUT" {
                    reqwest::Method::PUT
                } else {
                    reqwest::Method::PATCH
                };
                write_json(
                    &self.client,
                    method,
                    &format!("{}{}", self.base(), w.path),
                    &w.body,
                    rule,
                )
                .await?;
            }
            tracing::info!(rule = %rule, car = %id, "ended before any gate: abandoned");
            done.push(id.to_string());
        }
        Ok(done)
    }
}

#[async_trait]
impl Handler for JobsEndUnlaunchedCar {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, ArgValue)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        // A malformed close is not something a redelivery can fix, so
        // it is a no-op rather than an error that retries forever.
        let Some(closing) = ctx
            .event_payload
            .get("id")
            .or_else(|| ctx.event_payload.get("job_id"))
            .and_then(Value::as_str)
        else {
            return Ok(());
        };
        let outcome = ctx
            .event_payload
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("closed");
        let ended = match arg(args, "by") {
            Some(ArgValue::String(s)) if s == "run" => Ended::Run {
                run: closing,
                outcome,
            },
            Some(ArgValue::String(s)) if s == "item" => Ended::Item {
                item: closing,
                outcome,
            },
            _ => {
                return Err(HandlerError::Permanent(format!(
                    "{HANDLER}: `by` must be \"run\" or \"item\" — rule authoring, not a \
                     redelivery"
                )));
            }
        };
        // Stamped with the close's own instant, not the moment this
        // handler consumed it, which a redelivery moves.
        let now = ctx.firing_instant()?;
        self.end_cars(ended, now, &ctx.rule_name).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_jobs::JobsRepository as _;
    use serde_json::json;

    const RUN: &str = "1a1d2e3f-4a5b-4c6d-8e7f-0000000000a1";
    const ITEM: &str = "1a1d2e3f-4a5b-4c6d-8e7f-0000000000b1";

    struct Served {
        base: String,
        jobs: Arc<boss_jobs::InMemoryJobs>,
        car: boss_core::job::JobId,
        gate: boss_core::job::StepId,
        abandoned: boss_core::job::StepId,
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-11-09T08:00:00Z")
            .unwrap()
            .into()
    }

    /// A car `boss dispatch --started` admitted for RUN on ITEM, at build
    /// — scope declared, build claimed, gate pending and unbound — in an
    /// in-memory store behind the real jobs router.
    async fn admitted() -> Served {
        use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
        let jobs = Arc::new(boss_jobs::InMemoryJobs::new());
        let mut job = boss_core::job::Job::new(
            "ship-a-change",
            boss_core::job::Subject::new("custom", "fix/a-run-that-stopped"),
            "A car at build",
            "emp-owner",
            boss_core::job::Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 9).unwrap(),
        );
        job.status = boss_core::job::JobStatus::Open;
        job.metadata = json!({
            "branch": "fix/a-run-that-stopped",
            "agent_run": RUN,
            car::BACKLOG_ITEM: ITEM,
            car::ADMITTED_BY: "boss dispatch --started",
        });
        jobs.create_job(&job).await.unwrap();
        let step = |title: &str, slug: &str, order, status| {
            let mut s = boss_core::job::Step::new(job.id, "task", title, order);
            s.spec_slug = Some(slug.into());
            s.status = status;
            s
        };
        use boss_core::job::StepStatus::{Active, Completed, Pending};
        let scope = step(car::SCOPE, car::SCOPE_SLUG, 1, Completed);
        let build = step(car::BUILD, car::BUILD_SLUG, 2, Active);
        let gate = step(car::GATE, car::GATE_SLUG, 3, Pending);
        let mut abandoned = step(
            boss_jobs::car_retire::ABANDONED,
            boss_jobs::car_retire::ABANDONED_SLUG,
            9,
            Pending,
        );
        abandoned.kind = "outcome".into();
        for s in [&scope, &build, &gate, &abandoned] {
            jobs.add_step(s).await.unwrap();
        }
        let mut policy = FakePolicyClient::builder();
        for (action, resource) in [
            (Action::Read, Resource::job()),
            (Action::Update, Resource::job()),
            (Action::Update, Resource::step()),
        ] {
            policy = policy.allow("platform-admin", action, resource, Scope::All);
        }
        let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
        let bus = boss_testing::RecordingEventBus::new();
        let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
        let app = boss_jobs::http::router(boss_jobs::http::JobsApiState::minimal(
            jobs.clone(),
            bus,
            boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Served {
            base,
            jobs,
            car: job.id,
            gate: gate.id,
            abandoned: abandoned.id,
        }
    }

    async fn job_md(s: &Served) -> Value {
        s.jobs.get_job(&s.car).await.unwrap().unwrap().metadata
    }

    async fn step(s: &Served, id: &boss_core::job::StepId) -> boss_core::job::Step {
        s.jobs.get_step(id).await.unwrap().unwrap()
    }

    fn refused() -> Ended<'static> {
        Ended::Run {
            run: RUN,
            outcome: "refused",
        }
    }

    /// A run refused before any gate ends its admitted car through
    /// `abandoned` — claimed on the gate step, evidence and marker
    /// written, the terminal completed.
    #[tokio::test]
    async fn a_run_refused_before_any_gate_ends_its_car() {
        let s = admitted().await;
        let ended = JobsEndUnlaunchedCar::new(&s.base)
            .end_cars(refused(), now(), "test")
            .await
            .unwrap();
        assert_eq!(ended, vec![s.car.to_string()]);
        let md = job_md(&s).await;
        assert_eq!(md[boss_jobs::car_retire::ABANDONED_MARKER], "true", "{md}");
        assert!(
            md[car::ABANDONED_WHY].as_str().unwrap().contains(RUN),
            "{md}"
        );
        let gate = step(&s, &s.gate).await;
        assert_eq!(gate.metadata[car::ENDED_BEFORE_GATE]["id"], RUN);
        let outcome = step(&s, &s.abandoned).await;
        assert_eq!(outcome.status, boss_core::job::StepStatus::Completed);
        assert!(
            outcome.metadata["evidence"]
                .as_str()
                .unwrap()
                .contains("refused")
        );
    }

    /// R4a: the ending is RESUMABLE. Its writer is cut off after the
    /// claim, after the evidence, and after the marker in turn; each
    /// redelivery finds the claim and finishes the rest — once, with the
    /// claim's own words — and a further redelivery changes nothing.
    #[tokio::test]
    async fn a_redelivery_after_any_one_write_finishes_the_ending_once() {
        for landed in 0..3 {
            let s = admitted().await;
            let h = JobsEndUnlaunchedCar::new(&s.base);
            // The first writer: the claim, then `landed` of the rest.
            let read = h.read_car(&s.car.to_string(), "test").await.unwrap();
            let claim = car::ending_claim(&read, refused(), now()).unwrap();
            let wrote = write_json_expecting(
                &h.client,
                reqwest::Method::PATCH,
                &format!("{}{}", s.base, claim.path),
                &claim.body,
                &claim.expect,
                "test",
            )
            .await
            .unwrap();
            assert_eq!(wrote, Judged::Written);
            let claimed = h.read_car(&s.car.to_string(), "test").await.unwrap();
            let owed = car::ending_rest(&claimed).unwrap();
            assert_eq!(owed.len(), 3);
            for w in &owed[..landed] {
                let m = reqwest::Method::from_bytes(w.method.as_bytes()).unwrap();
                write_json(&h.client, m, &format!("{}{}", s.base, w.path), &w.body, "t")
                    .await
                    .unwrap();
            }
            assert_eq!(
                step(&s, &s.abandoned).await.status,
                boss_core::job::StepStatus::Pending,
                "after {landed} write(s) the car still stands"
            );

            // The redelivery — a LATER instant, which must not reword it.
            let later = now() + chrono::Duration::hours(1);
            let ended = h.end_cars(refused(), later, "test").await.unwrap();
            assert_eq!(ended, vec![s.car.to_string()], "after {landed}");
            let outcome = step(&s, &s.abandoned).await;
            assert_eq!(outcome.status, boss_core::job::StepStatus::Completed);
            assert_eq!(
                outcome.metadata["completed_at"],
                claim.body[car::ENDED_BEFORE_GATE]["at"],
                "the claim's stamp, not the redelivery's"
            );
            let md = job_md(&s).await;
            assert_eq!(md[boss_jobs::car_retire::ABANDONED_MARKER], "true");
            assert_eq!(md[car::ABANDONED_WHY], owed[1].body[car::ABANDONED_WHY]);

            // Once more: nothing is owed, nothing fails.
            let before = (job_md(&s).await, step(&s, &s.abandoned).await.metadata);
            h.end_cars(refused(), later, "test").await.unwrap();
            assert_eq!(
                before,
                (job_md(&s).await, step(&s, &s.abandoned).await.metadata)
            );
        }
    }

    /// An ending that lost its claim to a gate launch ends nothing: the
    /// launch's stamp holds the gate step, and the car waits at its gate.
    #[tokio::test]
    async fn a_launched_car_is_left_at_its_gate() {
        let s = admitted().await;
        let h = JobsEndUnlaunchedCar::new(&s.base);
        let read = h.read_car(&s.car.to_string(), "test").await.unwrap();
        let plan = car::gate_launch(&read, "gr-1", "abc", "t", now())
            .unwrap()
            .unwrap();
        let stamped = write_json_expecting(
            &h.client,
            reqwest::Method::PATCH,
            &format!(
                "{}/api/jobs/{}/steps/{}/metadata",
                s.base, s.car, plan.gate_step
            ),
            &plan.gate_merge,
            &plan.fence,
            "test",
        )
        .await
        .unwrap();
        assert_eq!(stamped, Judged::Written);
        let ended = h.end_cars(refused(), now(), "test").await.unwrap();
        assert!(ended.is_empty());
        assert!(job_md(&s).await.get("abandoned").is_none());
        assert!(
            step(&s, &s.gate)
                .await
                .metadata
                .get(car::ENDED_BEFORE_GATE)
                .is_none()
        );
    }

    /// The declined item: the car still building for it ends.
    #[tokio::test]
    async fn a_declined_item_ends_the_car_building_for_it() {
        let s = admitted().await;
        let ended = JobsEndUnlaunchedCar::new(&s.base)
            .end_cars(
                Ended::Item {
                    item: ITEM,
                    outcome: "declined",
                },
                now(),
                "test",
            )
            .await
            .unwrap();
        assert_eq!(ended, vec![s.car.to_string()]);
        let md = job_md(&s).await;
        assert_eq!(md["abandoned"], "true");
        assert!(md[car::ABANDONED_WHY].as_str().unwrap().contains(ITEM));
    }
}
