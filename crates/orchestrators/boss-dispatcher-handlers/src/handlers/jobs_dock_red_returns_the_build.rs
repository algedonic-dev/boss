//! `jobs.dock_red_returns_the_build` — a car the dock RETURNED, its
//! re-gate red twice in a row on the same check, sends its item's `build`
//! step back to the item's station with the failing check attached, where
//! a builder pulls it as that car's repair (backlog 8f3d731a).
//!
//! WHY. The dock re-gated garaged car aab5f34e red nine times in a row
//! (23:16Z on 2026-11-17 to 10:11Z on 2026-11-18), every time on `test`
//! and the same pin, about three hours of gate time spent on a car that could not
//! pass without a code change: the garage stopped only the same-head
//! retry, and every main move was one more fresh base. The red-gate
//! return (`a-red-gate-returns-the-build-to-its-station`) covers a run a
//! red gate ENDED, and a dock re-gate ends no run. The conductor now
//! stops re-gating a car whose retry went red on a check the red before
//! it named, and writes that red `returned` (`boss_jobs::dock_red`); this
//! handler is the half that moves the item.
//!
//! THE SAME RETURN AS A CHANGES VERDICT'S (`jobs_changes_return_the_build`,
//! whose plan and writes it shares): an ACTIVE build whose run edge names
//! no open run is released to `ready` and nobody's, judged on the edge as
//! read; a READY build gets the note alone; the note follows the release,
//! so a step carrying it was returned already. Only the facts and the note
//! differ.
//!
//! THE FACTS, read off the car as it stands now, not the event: open,
//! its re-gate red standing `returned`, the stamp still at the head that
//! red judged (a stamp that moved on has a re-gate of a new head in
//! flight), and an item named as `backlog_item` — the one link the repair
//! door (`boss dispatch <item>`, which admits a garaged dock red as the
//! car's REPAIR) follows.
//!
//! THE NOTE IS THE RED-GATE RETURN'S CARRY SHAPE. It lands under the same
//! key, `returned_for_red`, with the red gate-run as `gate_run: {id,
//! branch}` beside the failing `checks`, the head and the main it judged;
//! the next builder's brief prints the step's metadata verbatim, so the
//! repair starts from the red.

use super::jobs_changes_return_the_build::{
    Returned, plan_by, read_job, return_build, short, text,
};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value as ArgValue;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::car_release;
use boss_jobs::dock_red::{self, RegateRed};
use serde_json::{Value as Json, json};
use std::sync::Arc;

/// The handler's registered name.
pub const HANDLER: &str = "jobs.dock_red_returns_the_build";

/// Where the return's evidence lands on the item's build step: the key
/// the red-gate return writes (`a-red-gate-returns-the-build-to-its-station`),
/// because it is the same fact — a red sent this build back.
pub const NOTE_KEY: &str = "returned_for_red";

/// PURE: the returned red a car stands on, or `None` for any other
/// packet, or a red the dock still retries.
pub(crate) fn returned_red(car: &Json) -> Option<RegateRed> {
    if text(car, "/kind") != "ship-a-change" {
        return None;
    }
    car.get("metadata")
        .and_then(dock_red::regate_red)
        .filter(|r| r.returned)
}

/// PURE: the item whose build is owed the repair of `car`, read off the
/// car as it stands (module doc, THE FACTS); `Err` names the fact that
/// failed.
pub(crate) fn owed_item(car: &Json) -> Result<(String, RegateRed), String> {
    let id = short(text(car, "/id"));
    let status = text(car, "/status");
    if status != "open" {
        return Err(format!("car {id} is {status:?}, not open"));
    }
    let Some(red) = returned_red(car) else {
        return Err(format!("car {id} stands on no returned dock red"));
    };
    let stamped = text(car, &format!("/metadata/{}/head", dock_red::BASE_REGATE));
    if stamped != red.head {
        return Err(format!(
            "car {id}'s red judged {}, and its stamp has moved on to {} — that re-gate's verdict \
             is the one to await",
            short(&red.head),
            short(stamped)
        ));
    }
    let item = text(car, &format!("/metadata/{}", boss_jobs::car::BACKLOG_ITEM));
    if item.is_empty() {
        return Err(format!(
            "car {id} names no `{}`, the one link the repair door follows",
            boss_jobs::car::BACKLOG_ITEM
        ));
    }
    Ok((item.to_string(), red))
}

/// PURE: does `step` already carry this red's note?
fn noted(step: &Json, red: &RegateRed) -> bool {
    text(step, &format!("/metadata/{NOTE_KEY}/gate_run/id")) == red.gate_run
}

/// PURE: the merge that records the return on the item's build step.
pub(crate) fn note_body(
    car: &Json,
    item: &str,
    red: &RegateRed,
    released: Option<&str>,
    at: Option<&str>,
    rule: &str,
) -> Json {
    let builder_run = car_release::builder_run(car);
    let built_by = Some(text(car, "/metadata/built_by")).filter(|b| !b.is_empty());
    let checks = if red.checks.is_empty() {
        "no check named".to_string()
    } else {
        red.checks.join(", ")
    };
    json!({ NOTE_KEY: {
        "gate_run": {"id": red.gate_run, "branch": text(car, "/metadata/branch")},
        "checks": red.checks,
        "car": text(car, "/id"),
        "head": red.head,
        "main": red.main,
        "builder_run": builder_run,
        "built_by": built_by,
        "released": released,
        "at": at,
        "rule": rule,
        "why": format!(
            "car {}'s dock re-gate {} went {} on {checks} at head {} on main {}, its second in a \
             row to fail that check, so the dock stopped re-gating it and this build is back at \
             its station: `boss dispatch {item}` admits it as that car's REPAIR on its own \
             branch (built by {})",
            short(text(car, "/id")),
            short(&red.gate_run),
            red.verdict,
            short(&red.head),
            short(&red.main),
            built_by.or(builder_run).unwrap_or("an unrecorded builder"),
        ),
    }})
}

pub struct JobsDockRedReturnsTheBuild {
    client: boss_core::machine_token::Client,
    jobs_base: String,
}

impl JobsDockRedReturnsTheBuild {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }
}

#[async_trait]
impl Handler for JobsDockRedReturnsTheBuild {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        _args: &[(String, ArgValue)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        if returned_red(&ctx.event_payload).is_none() {
            return Ok(());
        }
        let car_id = text(&ctx.event_payload, "/id");
        let Some(car) = read_job(&self.client, self.base(), car_id).await? else {
            return Ok(());
        };
        let (item_id, red) = match owed_item(&car) {
            Ok(owed) => owed,
            Err(why) => {
                tracing::info!(rule, car = %car_id,
                    "jobs.dock_red_returns_the_build: no repair to route: {why}");
                return Ok(());
            }
        };
        let Some(item) = read_job(&self.client, self.base(), &item_id).await? else {
            return Ok(());
        };
        let at = ctx.firing_instant().ok().map(|t| t.to_rfc3339());
        let note =
            |released: Option<&str>| note_body(&car, &item_id, &red, released, at.as_deref(), rule);
        match return_build(
            &self.client,
            self.base(),
            rule,
            &item_id,
            plan_by(&item, |step| noted(step, &red)),
            note,
        )
        .await?
        {
            Returned::Left(why) => {
                tracing::info!(rule, car = %car_id, item = %item_id,
                    "jobs.dock_red_returns_the_build: left the build: {why}");
            }
            Returned::Noted { released } => {
                tracing::info!(rule, car = %car_id, item = %item_id, gate_run = %red.gate_run,
                    released = released.as_deref().unwrap_or("nobody"),
                    "jobs.dock_red_returns_the_build: returned the item's build to its station \
                     for the car's repair");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::jobs_changes_return_the_build::Plan;
    use crate::handlers::jobs_reclaim_abandoned_step::release_put;
    use axum::Router;
    use axum::body::Bytes;
    use axum::http::{HeaderMap, Method, StatusCode, Uri};
    use axum::response::IntoResponse;
    use std::sync::Mutex;

    const RULE: &str = "a-dock-red-returns-the-build-to-its-station";
    const CAR: &str = "aab5f34e-9526-418a-8d63-379db8e4f0af";
    const ITEM: &str = "b513514c-230d-4be0-bd97-af93b11ae005";
    const BUILDER: &str = "bfe87c82-7fea-4359-b210-eaee31ad1774";
    const REPAIR: &str = "37fd4a74-3777-422e-a31c-d0ad2ac4698d";
    const RED: &str = "33f1a846-0000-4000-8000-000000000000";
    const BUILD_STEP: &str = "00000000-0000-4000-8000-000000000b1d";
    const HEAD: &str = "bc7ec5551111111111111111111111111111111";
    const MAIN: &str = "597184ab2222222222222222222222222222222";

    fn red(returned: bool) -> RegateRed {
        RegateRed {
            gate_run: RED.into(),
            head: HEAD.into(),
            main: MAIN.into(),
            verdict: "failed".into(),
            checks: vec!["test".into()],
            garaged: true,
            returned,
        }
    }

    /// Car aab5f34e as the dock leaves it after its second red on `test`
    /// (backlog 8f3d731a): parked, its stamp at the head the red judged.
    fn car(returned: bool) -> Json {
        json!({"id": CAR, "kind": "ship-a-change", "status": "open",
               "metadata": {"branch": "fix/every-executor-seat-comes-back-on-a-pod-roll",
                            "agent_run": BUILDER, "built_by": "claude@algedonic.dev",
                            "backlog_item": ITEM,
                            dock_red::BASE_REGATE: {"main": MAIN, "head": HEAD,
                                "gate_run": RED, "reds": 1,
                                dock_red::RED: red(returned).to_value()}},
               "steps": []})
    }

    fn item(status: &str, holder: Option<&str>, edge: Option<&str>) -> Json {
        let mut md = json!({"agent_profile": "builder", "completion_contract":
                            "item-awaits-its-car-v1"});
        if let Some(e) = edge {
            md["agent_run"] = json!(e);
        }
        json!({"id": ITEM, "kind": "backlog-item", "status": "open",
               "steps": [{"id": BUILD_STEP, "spec_slug": "build", "status": status,
                          "assignee_id": holder, "metadata": md}]})
    }

    #[test]
    fn only_a_returned_red_on_a_car_is_read() {
        assert_eq!(returned_red(&car(true)), Some(red(true)));
        assert_eq!(returned_red(&car(false)), None, "garaged, still retried");
        let mut run = car(true);
        run["kind"] = json!("agent-run");
        assert_eq!(returned_red(&run), None, "only a car");
    }

    /// The repair door's facts: each one that fails is named.
    #[test]
    fn the_item_is_owed_only_on_the_repair_doors_facts() {
        assert_eq!(owed_item(&car(true)), Ok((ITEM.to_string(), red(true))));
        let mut closed = car(true);
        closed["status"] = json!("closed");
        let mut moved = car(true);
        moved["metadata"][dock_red::BASE_REGATE]["head"] = json!("0ec95d0c");
        let mut partial = car(true);
        partial["metadata"]["backlog_item"] = Json::Null;
        for (car, says) in [
            (closed, "not open"),
            (car(false), "no returned dock red"),
            (moved, "moved on"),
            (partial, "names no `backlog_item`"),
        ] {
            let why = owed_item(&car).unwrap_err();
            assert!(why.contains(says), "{says}: {why}");
        }
    }

    /// The note is the red-gate return's carry shape — `gate_run: {id,
    /// branch}` under `returned_for_red` — with the failing checks beside
    /// it, and the plan reads it back as done for this red alone.
    #[test]
    fn the_note_carries_the_red_and_its_failing_check() {
        let body = note_body(
            &car(true),
            ITEM,
            &red(true),
            Some("agent-claude"),
            None,
            RULE,
        );
        let note = &body[NOTE_KEY];
        assert_eq!(note["gate_run"]["id"], json!(RED));
        assert_eq!(
            note["gate_run"]["branch"],
            json!("fix/every-executor-seat-comes-back-on-a-pod-roll")
        );
        assert_eq!(note["checks"], json!(["test"]));
        assert_eq!(note["head"], json!(HEAD));
        assert_eq!(note["builder_run"], json!(BUILDER));
        assert_eq!(note["released"], json!("agent-claude"));
        let why = note["why"].as_str().unwrap();
        let dispatch = format!("boss dispatch {ITEM}");
        for want in [dispatch.as_str(), "REPAIR", "test", "33f1a846"] {
            assert!(why.contains(want), "{want}: {why}");
        }
        assert_eq!(body.as_object().unwrap().len(), 1, "one key: {body}");

        let mut returned = item("ready", None, Some(BUILDER));
        returned["steps"][0]["metadata"][NOTE_KEY] = note.clone();
        assert!(matches!(
            plan_by(&returned, |s| noted(s, &red(true))),
            Plan::Leave(_)
        ));
        // A red-gate return's note for another gate-run is not this one.
        let mut earlier = item("active", Some("agent-claude"), Some(BUILDER));
        earlier["steps"][0]["metadata"][NOTE_KEY] =
            json!({"gate_run": {"id": "3b5932e6", "branch": "fix/x"}});
        assert!(matches!(
            plan_by(&earlier, |s| noted(s, &red(true))),
            Plan::Release { .. }
        ));
    }

    /// The jobs API's answers this handler reads and writes, logged as
    /// `METHOD /path` with each body.
    async fn api(
        car: Json,
        item: Json,
        runs: Vec<Json>,
    ) -> (String, Arc<Mutex<Vec<(String, Json)>>>) {
        let log: Arc<Mutex<Vec<(String, Json)>>> = Arc::default();
        let l = log.clone();
        let app = Router::new().fallback(
            move |method: Method, uri: Uri, _headers: HeaderMap, body: Bytes| {
                let (log, car, item, runs) = (l.clone(), car.clone(), item.clone(), runs.clone());
                async move {
                    let path = uri.path().to_string();
                    let parsed: Json = serde_json::from_slice(&body).unwrap_or(Json::Null);
                    log.lock()
                        .unwrap()
                        .push((format!("{method} {path}"), parsed));
                    if method == Method::GET {
                        let id = path.trim_start_matches("/api/jobs/");
                        let row = [&car, &item]
                            .into_iter()
                            .chain(runs.iter())
                            .find(|r| r["id"] == json!(id));
                        return match row {
                            Some(r) => axum::Json(r.clone()).into_response(),
                            None => StatusCode::NOT_FOUND.into_response(),
                        };
                    }
                    if method == Method::PUT {
                        return StatusCode::NO_CONTENT.into_response();
                    }
                    axum::Json(json!({})).into_response()
                }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), log)
    }

    async fn fire(base: &str, payload: Json) -> Result<(), HandlerError> {
        let ctx = InvocationContext {
            rule_name: RULE.into(),
            triggering_event_id: "evt-dock-red-1".into(),
            triggering_topic: "jobs.job.updated".into(),
            event_payload: payload,
            event_timestamp: Some("2026-11-18T00:43:31Z".parse().unwrap()),
        };
        Arc::new(JobsDockRedReturnsTheBuild {
            client: crate::handlers::common::api_client(),
            jobs_base: base.into(),
        })
        .invoke(&[], &ctx)
        .await
    }

    fn run(id: &str, status: &str) -> Json {
        json!({"id": id, "kind": "agent-run", "status": status})
    }

    const PUT_BUILD: &str = "PUT /api/jobs/b513514c-230d-4be0-bd97-af93b11ae005/steps/00000000-0000-4000-8000-000000000b1d";
    const PATCH_BUILD: &str = "PATCH /api/jobs/b513514c-230d-4be0-bd97-af93b11ae005/steps/00000000-0000-4000-8000-000000000b1d/metadata";

    /// THE CASE (car aab5f34e, item b513514c, 2026-11-18): the item's
    /// build stood active on the actor that had dispatched it, its builder
    /// run closed. The returned red sends it back to its station, nobody's,
    /// and the note follows with the failing check — the repair the drain
    /// session dispatched by hand (run 37fd4a74).
    #[tokio::test]
    async fn a_returned_red_sends_a_held_build_back_with_the_failing_check() {
        let (base, log) = api(
            car(true),
            item("active", Some("agent-claude"), Some(BUILDER)),
            vec![run(BUILDER, "closed")],
        )
        .await;
        fire(&base, car(true)).await.expect("returned");
        let reqs: Vec<String> = log.lock().unwrap().iter().map(|r| r.0.clone()).collect();
        let put = reqs.iter().position(|r| r == PUT_BUILD).expect("released");
        let patch = reqs.iter().position(|r| r == PATCH_BUILD).expect("noted");
        assert!(put < patch, "the release first, then the note: {reqs:?}");
        let sent = |req: &str| {
            log.lock()
                .unwrap()
                .iter()
                .find(|r| r.0 == req)
                .map(|r| r.1.clone())
                .unwrap()
        };
        assert_eq!(sent(PUT_BUILD), release_put(), "ready, and nobody's");
        let note = sent(PATCH_BUILD);
        assert_eq!(note[NOTE_KEY]["gate_run"]["id"], json!(RED));
        assert_eq!(note[NOTE_KEY]["checks"], json!(["test"]));
        assert_eq!(note[NOTE_KEY]["released"], json!("agent-claude"));
    }

    /// A red the dock still retries, a repair already running on the
    /// build, and a build already returned for this red write nothing.
    #[tokio::test]
    async fn no_return_owed_writes_nothing() {
        let (base, log) = api(car(false), item("active", None, Some(BUILDER)), vec![]).await;
        fire(&base, car(false)).await.expect("a no-op");
        assert!(log.lock().unwrap().is_empty(), "not even a read");

        let (base, log) = api(
            car(true),
            item("active", Some("agent-claude"), Some(REPAIR)),
            vec![run(REPAIR, "open")],
        )
        .await;
        fire(&base, car(true)).await.expect("left");
        assert!(
            log.lock().unwrap().iter().all(|r| r.0.starts_with("GET ")),
            "{:?}",
            log.lock().unwrap()
        );

        let mut done = item("ready", None, None);
        done["steps"][0]["metadata"][NOTE_KEY] =
            note_body(&car(true), ITEM, &red(true), None, None, RULE)[NOTE_KEY].clone();
        let (base, log) = api(car(true), done, vec![]).await;
        fire(&base, car(true)).await.expect("left");
        assert!(
            log.lock().unwrap().iter().all(|r| r.0.starts_with("GET ")),
            "{:?}",
            log.lock().unwrap()
        );
    }
}
