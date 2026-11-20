//! `jobs.changes_return_the_build` — a reviewer's CHANGES verdict on a
//! held car returns its item's `build` step to the item's station, where
//! a builder pulls it as that car's repair (backlog c2b21676, part 2 of
//! eb86a174 "the factory routes stranded work itself").
//!
//! WHY. A CHANGES verdict woke only `jobs.release_on_review`, whose door
//! refuses it and records the refusal on the car's review step. The
//! item's build stayed ACTIVE on the actor that had dispatched it, naming
//! a builder run that had already landed on its green, so the repair the
//! door admits (`boss dispatch <item>` reads a current CHANGES as the
//! car's REPAIR and briefs its findings verbatim) waited for a conductor
//! to read the car. Measured 2026-11-15 20:04Z: review run cc5a878a
//! asked for changes on car 1d5379b5, and item 76f89301's build stood
//! active on agent-claude, its run e5191d86 closed.
//!
//! THE STATION, NOT A NAME. The step goes back to `ready` and nobody's,
//! the state its station holds unplaced work in. The dispatcher nominates
//! nobody (backlog e1eaf72b, pinned by `the_dispatcher_nominates_nobody`),
//! the builder's session may be gone, and the repair brief carries
//! everything the repair needs, so any builder the claim admits can do
//! it. The builder is NAMED in the note, for the reader.
//!
//! THE REPAIR DOOR'S FACTS, read before anything is written
//! ([`owed_item`]): the car is open and held at its review step
//! (`jobs_release_on_review::held_review`), the verdict is from the run
//! that step was dispatched to and not the car's builder run, it read the
//! head the hold stands at, and the car answers an item as
//! `backlog_item` or, for one piece of it, `partial_item` — the two
//! links `boss dispatch`'s repair door follows (backlog 390f9e55). This
//! handler only routes: the door judges again when the
//! step is pulled, and refuses a repair no longer owed.
//!
//! THE WRITES ([`plan`]). An ACTIVE build whose run edge names no open
//! run is RELEASED (`jobs_reclaim_abandoned_step::release_put`), judged
//! against the edge as read so a repair dispatched in between is left
//! as it stands; then the [`NOTE_KEY`] note lands through the step merge
//! door. A READY build gets the note alone. The release goes FIRST, so
//! a note on the step says the release landed: an ACTIVE step already
//! carrying this review's note was taken AFTER the return, and is never
//! released again; a release whose note failed to land is finished by a
//! redelivery, which finds the step ready and writes the note.
//!
//! WHAT IT LEAVES, logged and unwritten: a car not owed the repair
//! (each fact names itself), an item or build not open, and a build
//! whose run edge names an OPEN run — a repair already running, or the
//! builder's own run not yet reported (the reviewer run's later writes,
//! its report and its landing, fire the rule again).

use super::common::{
    Judged, dispatcher_reader_header, sim_origin_value, write_json, write_json_expecting,
};
use super::jobs_reclaim_abandoned_step::release_put;
use super::jobs_release_on_review::{held_review, refuses_for_good};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value as ArgValue;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::car_release;
use serde_json::{Map, Value as Json, json};
use std::sync::Arc;

/// The handler's registered name.
pub const HANDLER: &str = "jobs.changes_return_the_build";

/// Where the return's evidence lands on the item's build step — not the
/// reclaim's `reclaimed`, the hand verb's `released`, the urgent watch's
/// `released_unrun` or the stranded-review return's
/// `returned_to_station`, so a reader can tell which door moved it.
pub const NOTE_KEY: &str = "returned_for_changes";

/// The item's step a car is built on (`backlog-item.toml`'s `build`).
const ITEM_BUILD_SLUG: &str = "build";

/// The run edge `boss dispatch` writes on the step it dispatches.
pub(crate) const EDGE_KEY: &str = "agent_run";

pub(crate) fn text<'a>(v: &'a Json, pointer: &str) -> &'a str {
    v.pointer(pointer)
        .and_then(Json::as_str)
        .map(str::trim)
        .unwrap_or_default()
}

pub(crate) fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

/// The CHANGES a reviewer's agent-run records, as its own write carries it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Changes {
    pub run: String,
    pub car: String,
    pub head: String,
}

/// PURE: the CHANGES `run` records on a car, or `None` for any other
/// packet, verdict, or a review naming no car or no head.
pub(crate) fn changes_of(run: &Json) -> Option<Changes> {
    let key = car_release::REVIEW_KEY;
    let field = |name: &str| text(run, &format!("/metadata/{key}/{name}"));
    let (id, car, head) = (text(run, "/id"), field("car"), field("reviewed_sha"));
    (text(run, "/kind") == "agent-run"
        && field("verdict") == "changes"
        && !id.is_empty()
        && !car.is_empty()
        && !head.is_empty())
    .then(|| Changes {
        run: id.to_string(),
        car: car.to_string(),
        head: head.to_string(),
    })
}

/// PURE: the item whose build is owed the repair of `car` on `changes`,
/// by the facts the repair door stands on (module doc); `Err` names the
/// fact that failed.
pub(crate) fn owed_item(car: &Json, changes: &Changes) -> Result<String, String> {
    let id = short(text(car, "/id"));
    let status = text(car, "/status");
    if status != "open" {
        return Err(format!("car {id} is {status:?}, not open"));
    }
    if held_review(car).is_none() {
        return Err(format!(
            "car {id}'s review step holds nothing, and a review repair is owed only on a held car"
        ));
    }
    let run = changes.run.as_str();
    match car_release::dispatched_reviewer(car) {
        Some(r) if r.eq_ignore_ascii_case(run) => {}
        other => {
            return Err(format!(
                "run {} is not the reviewer car {id}'s review step was dispatched to ({})",
                short(run),
                other.map_or("none", short)
            ));
        }
    }
    if car_release::builder_run(car) == Some(run) {
        return Err(format!("run {} built car {id}", short(run)));
    }
    let held_at = car_release::hold_sha(car).unwrap_or_default();
    if held_at != changes.head {
        return Err(format!(
            "the verdict read {}, and car {id}'s hold stands at {} — the repair is in and owes a \
             review",
            short(&changes.head),
            short(held_at)
        ));
    }
    // The closing edge, else the piece's (backlog 390f9e55): a car built
    // for part of an item is owed its repair as much as the last car is.
    let item = [boss_jobs::car::BACKLOG_ITEM, boss_jobs::car::PARTIAL_ITEM]
        .into_iter()
        .map(|key| text(car, &format!("/metadata/{key}")))
        .find(|item| !item.is_empty())
        .unwrap_or_default();
    if item.is_empty() {
        return Err(format!(
            "car {id} names no `{}` or `{}`, the links the repair door follows",
            boss_jobs::car::BACKLOG_ITEM,
            boss_jobs::car::PARTIAL_ITEM
        ));
    }
    Ok(item.to_string())
}

/// What the rule does with the item's build step.
#[derive(Debug, PartialEq)]
pub(crate) enum Plan<'a> {
    /// Not this rule's to move, with why.
    Leave(String),
    /// Already at its station: the note alone.
    Note(&'a Json),
    /// Held: release it, unless its run edge names an open run.
    Release {
        step: &'a Json,
        edge: Option<&'a str>,
    },
}

/// PURE: does `step` already carry this verdict's note?
fn noted(step: &Json, changes: &Changes) -> bool {
    let note = format!("/metadata/{NOTE_KEY}");
    step.pointer(&note)
        .is_some_and(|n| text(n, "/review") == changes.run && text(n, "/head") == changes.head)
}

/// PURE: the plan for `item`'s build on `changes` (module doc).
pub(crate) fn plan<'a>(item: &'a Json, changes: &Changes) -> Plan<'a> {
    plan_by(item, |step| noted(step, changes))
}

/// PURE: the plan for `item`'s build, where `done` says whether the step
/// already carries THIS return's note — the one fact that differs between
/// the returns that share it (a CHANGES verdict here, a dock red in
/// `jobs_dock_red_returns_the_build`, backlog 8f3d731a).
pub(crate) fn plan_by<'a>(item: &'a Json, done: impl Fn(&Json) -> bool) -> Plan<'a> {
    let id = short(text(item, "/id"));
    let status = text(item, "/status");
    if status != "open" {
        return Plan::Leave(format!("item {id} is {status:?}, not open"));
    }
    let Some(step) = item
        .get("steps")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .find(|s| text(s, "/spec_slug") == ITEM_BUILD_SLUG)
    else {
        return Plan::Leave(format!("item {id} has no `{ITEM_BUILD_SLUG}` step"));
    };
    let done = done(step);
    match text(step, "/status") {
        "ready" if done => Plan::Leave("already returned for this verdict".into()),
        "ready" => Plan::Note(step),
        // The note follows the release, so a held step carrying it was
        // taken after the return: that holder's, never released again.
        "active" if done => Plan::Leave(
            "returned for this verdict, and claimed again since: the holder's to answer".into(),
        ),
        "active" => Plan::Release {
            step,
            edge: Some(text(step, &format!("/metadata/{EDGE_KEY}"))).filter(|e| !e.is_empty()),
        },
        other => Plan::Leave(format!("item {id}'s build is {other:?}, not open")),
    }
}

/// PURE: the merge that records the return on the item's build step.
/// The review's findings stay on the review run, the one place they are
/// written; the repair brief prints them.
pub(crate) fn note_body(
    car: &Json,
    item: &str,
    changes: &Changes,
    released: Option<&str>,
    at: Option<&str>,
    rule: &str,
) -> Json {
    let builder_run = car_release::builder_run(car);
    let built_by = Some(text(car, "/metadata/built_by")).filter(|b| !b.is_empty());
    json!({ NOTE_KEY: {
        "review": changes.run,
        "car": text(car, "/id"),
        "branch": text(car, "/metadata/branch"),
        "head": changes.head,
        "builder_run": builder_run,
        "built_by": built_by,
        "released": released,
        "at": at,
        "rule": rule,
        "why": format!(
            "review run {} asked for changes on car {} at {}, so this build is back at its \
             station: `boss dispatch {item}` admits it as that car's REPAIR on its own branch, \
             and its brief carries the review's findings verbatim (built by {})",
            short(&changes.run),
            short(text(car, "/id")),
            short(&changes.head),
            built_by.or(builder_run).unwrap_or("an unrecorded builder"),
        ),
    }})
}

pub struct JobsChangesReturnTheBuild {
    client: boss_core::machine_token::Client,
    jobs_base: String,
}

impl JobsChangesReturnTheBuild {
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

/// `GET {jobs}/api/jobs/{id}` as the dispatcher's reader: the row, or
/// `None` for a 404 (the packet is gone). A refusal for good is
/// permanent; anything else not 2xx is retried.
pub(crate) async fn read_job(
    client: &boss_core::machine_token::Client,
    base: &str,
    id: &str,
) -> Result<Option<Json>, HandlerError> {
    let url = format!("{base}/api/jobs/{id}");
    let resp = client
        .get(&url)
        .header("x-boss-user", dispatcher_reader_header())
        .header("x-sim-origin", sim_origin_value())
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        let said = format!(
            "GET {url} returned {status}: {}",
            resp.text().await.unwrap_or_default()
        );
        return Err(if refuses_for_good(status) {
            HandlerError::Permanent(said)
        } else {
            HandlerError::Downstream(said)
        });
    }
    resp.json()
        .await
        .map(Some)
        .map_err(|e| HandlerError::Downstream(format!("GET {url} is not JSON: {e}")))
}

/// What a return did with the item's build.
#[derive(Debug, PartialEq)]
pub(crate) enum Returned {
    /// Nothing written, with why.
    Left(String),
    /// The note landed; `released` names the holder a release took the
    /// step from, `None` when it was already at its station.
    Noted { released: Option<String> },
}

/// Carry out `plan` on `item_id`'s build — the release (judged on the run
/// edge as read) and then the note `note(released)` builds — for every
/// return that sends an item's build back to its station (module doc,
/// THE WRITES). An open run on the edge, or a step that moved between
/// the read and the release, leaves it.
pub(crate) async fn return_build(
    client: &boss_core::machine_token::Client,
    base: &str,
    rule: &str,
    item_id: &str,
    plan: Plan<'_>,
    note: impl FnOnce(Option<&str>) -> Json,
) -> Result<Returned, HandlerError> {
    let (step, released) = match plan {
        Plan::Leave(why) => return Ok(Returned::Left(why)),
        Plan::Note(step) => (step, None),
        Plan::Release { step, edge } => {
            // An unreadable or vanished run is read as open: the step is
            // left rather than taken on a guess.
            if let Some(run) = edge
                && read_job(client, base, run)
                    .await?
                    .is_none_or(|r| text(&r, "/status") != "closed")
            {
                return Ok(Returned::Left(format!("its run {} is open", short(run))));
            }
            // Judged on the edge as read: a repair dispatched since
            // writes its own run there, and is left as it stands.
            let mut expect = Map::new();
            expect.insert(EDGE_KEY.into(), edge.map_or(Json::Null, |e| json!(e)));
            let url = format!("{base}/api/jobs/{item_id}/steps/{}", text(step, "/id"));
            let put = write_json_expecting(
                client,
                reqwest::Method::PUT,
                &url,
                &release_put(),
                &expect,
                rule,
            )
            .await?;
            if put == Judged::Moved {
                return Ok(Returned::Left(
                    "it moved between the read and the release".into(),
                ));
            }
            (
                step,
                Some(text(step, "/assignee_id")).filter(|h| !h.is_empty()),
            )
        }
    };
    let url = format!(
        "{base}/api/jobs/{item_id}/steps/{}/metadata",
        text(step, "/id")
    );
    // The note lands whatever the step holds now: the release above
    // already moved it, and a ready step's note judges nothing.
    write_json(client, reqwest::Method::PATCH, &url, &note(released), rule).await?;
    Ok(Returned::Noted {
        released: released.map(str::to_string),
    })
}

#[async_trait]
impl Handler for JobsChangesReturnTheBuild {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        _args: &[(String, ArgValue)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let Some(changes) = changes_of(&ctx.event_payload) else {
            return Ok(());
        };
        let Some(car) = read_job(&self.client, self.base(), &changes.car).await? else {
            return Ok(());
        };
        let item_id = match owed_item(&car, &changes) {
            Ok(item) => item,
            Err(why) => {
                tracing::info!(rule, review = %changes.run, car = %changes.car,
                    "jobs.changes_return_the_build: no repair to route: {why}");
                return Ok(());
            }
        };
        let Some(item) = read_job(&self.client, self.base(), &item_id).await? else {
            return Ok(());
        };
        let at = ctx.firing_instant().ok().map(|t| t.to_rfc3339());
        let note = |released: Option<&str>| {
            note_body(&car, &item_id, &changes, released, at.as_deref(), rule)
        };
        match return_build(
            &self.client,
            self.base(),
            rule,
            &item_id,
            plan(&item, &changes),
            note,
        )
        .await?
        {
            Returned::Left(why) => {
                tracing::info!(rule, review = %changes.run, item = %item_id,
                    "jobs.changes_return_the_build: left the build: {why}");
            }
            Returned::Noted { released } => {
                tracing::info!(rule, review = %changes.run, car = %changes.car, item = %item_id,
                    released = released.as_deref().unwrap_or("nobody"),
                    "jobs.changes_return_the_build: returned the item's build to its station \
                     for the car's repair");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Bytes;
    use axum::http::{HeaderMap, Method, StatusCode, Uri};
    use axum::response::IntoResponse;
    use std::sync::Mutex;

    const RULE: &str = "a-changes-verdict-returns-the-build-to-its-station";
    const CAR: &str = "c0ffee00-0000-4000-8000-000000000000";
    const ITEM: &str = "17e30000-0000-4000-8000-000000000000";
    const BUILDER: &str = "b0b0b0b0-0000-4000-8000-000000000000";
    const REPAIR: &str = "4e9a1400-0000-4000-8000-000000000000";
    const REVIEWER: &str = "5eed5eed-0000-4000-8000-000000000000";
    const OTHER: &str = "0ther000-0000-4000-8000-000000000000";
    const REVIEW_STEP: &str = "00000000-0000-4000-8000-000000000003";
    const BUILD_STEP: &str = "00000000-0000-4000-8000-000000000b1d";
    const H1: &str = "1111111111111111111111111111111111111111";
    const H2: &str = "2222222222222222222222222222222222222222";

    fn held_car() -> Json {
        json!({"id": CAR, "kind": "ship-a-change", "status": "open",
               "metadata": {"branch": "feat/x", "agent_run": BUILDER,
                            "built_by": "claude@algedonic.dev", "backlog_item": ITEM},
               "steps": [{"id": REVIEW_STEP, "spec_slug": "review",
                          "title": boss_jobs::car::REVIEW, "status": "active",
                          "metadata": {"hold": "trust-boundary car", "hold_sha": H1,
                                       "agent_run": REVIEWER}}]})
    }

    fn verdict(id: &str, v: &str, sha: &str) -> Json {
        json!({"id": id, "kind": "agent-run", "status": "open",
               "metadata": {"packet": CAR, "step": "review",
                   "review": {"car": CAR, "branch": "feat/x", "reviewed_sha": sha,
                              "verdict": v, "findings": "B1: the guard reads the wrong key",
                              "at": "2026-11-15T20:04:34Z"}}})
    }

    fn changes() -> Changes {
        changes_of(&verdict(REVIEWER, "changes", H1)).unwrap()
    }

    fn item(status: &str, holder: Option<&str>, edge: Option<&str>) -> Json {
        let mut md = json!({"agent_profile": "builder", "completion_contract":
                            "item-awaits-its-car-v1"});
        if let Some(e) = edge {
            md[EDGE_KEY] = json!(e);
        }
        json!({"id": ITEM, "kind": "backlog-item", "status": "open",
               "steps": [{"id": "00000000-0000-4000-8000-00000000071a", "spec_slug": "triage",
                          "status": "completed", "metadata": {}},
                         {"id": BUILD_STEP, "spec_slug": "build", "status": status,
                          "assignee_id": holder, "metadata": md}]})
    }

    #[test]
    fn only_a_changes_verdict_naming_a_car_and_a_head_is_read() {
        assert_eq!(
            changes(),
            Changes {
                run: REVIEWER.into(),
                car: CAR.into(),
                head: H1.into()
            }
        );
        assert!(changes_of(&verdict(REVIEWER, "release", H1)).is_none());
        assert!(changes_of(&verdict(REVIEWER, "changes", "")).is_none());
        let mut car = verdict(REVIEWER, "changes", H1);
        car["kind"] = json!("ship-a-change");
        assert!(changes_of(&car).is_none(), "only an agent-run's verdict");
    }

    /// The repair door's facts: each one that fails is named, and the
    /// item is answered only when all hold.
    #[test]
    fn the_item_is_owed_only_on_the_repair_doors_facts() {
        assert_eq!(owed_item(&held_car(), &changes()), Ok(ITEM.to_string()));

        let mut closed = held_car();
        closed["status"] = json!("closed");
        let mut unheld = held_car();
        unheld["steps"][0]["metadata"]["hold"] = Json::Null;
        let mut moved = held_car();
        moved["steps"][0]["metadata"]["hold_sha"] = json!(H2);
        // A PARTIAL CAR IS OWED ITS ITEM'S REPAIR TOO (backlog 390f9e55):
        // car 5a1cbc09 answers 47601829 as `partial_item`, and its CHANGES
        // (run 75a760c9) returned nothing while this read followed
        // `backlog_item` alone. The repair door now reads both edges.
        let mut partial = held_car();
        partial["metadata"]["backlog_item"] = Json::Null;
        partial["metadata"]["partial_item"] = json!(ITEM);
        assert_eq!(owed_item(&partial, &changes()), Ok(ITEM.to_string()));
        let mut no_item = held_car();
        no_item["metadata"]["backlog_item"] = Json::Null;
        no_item["metadata"]["no_item_reason"] = json!("a chore");
        for (car, says) in [
            (closed, "not open"),
            (unheld, "holds nothing"),
            (moved, "the repair is in"),
            (no_item, "names no `backlog_item` or `partial_item`"),
        ] {
            let why = owed_item(&car, &changes()).unwrap_err();
            assert!(why.contains(says), "{says}: {why}");
        }
        let other = changes_of(&verdict(OTHER, "changes", H1)).unwrap();
        assert!(
            owed_item(&held_car(), &other)
                .unwrap_err()
                .contains("not the reviewer")
        );
        let mut by_builder = held_car();
        by_builder["steps"][0]["metadata"]["agent_run"] = json!(BUILDER);
        let builder = changes_of(&verdict(BUILDER, "changes", H1)).unwrap();
        assert!(
            owed_item(&by_builder, &builder)
                .unwrap_err()
                .contains("built car")
        );
    }

    #[test]
    fn a_held_build_is_released_and_a_waiting_one_is_noted() {
        let held = item("active", Some("agent-claude"), Some(BUILDER));
        assert!(matches!(
            plan(&held, &changes()),
            Plan::Release {
                edge: Some(BUILDER),
                ..
            }
        ));
        let waiting = item("ready", None, Some(BUILDER));
        assert!(matches!(plan(&waiting, &changes()), Plan::Note(_)));
        for status in ["pending", "completed", "skipped"] {
            assert!(
                matches!(plan(&item(status, None, None), &changes()), Plan::Leave(_)),
                "{status}"
            );
        }
        let mut closed = item("active", Some("agent-claude"), Some(BUILDER));
        closed["status"] = json!("closed");
        assert!(matches!(plan(&closed, &changes()), Plan::Leave(_)));
    }

    /// The note follows the release: a held step carrying this verdict's
    /// note was claimed after the return, and a ready one is done.
    #[test]
    fn a_build_already_returned_for_this_verdict_is_left() {
        let note = note_body(&held_car(), ITEM, &changes(), None, None, RULE)[NOTE_KEY].clone();
        for status in ["ready", "active"] {
            let mut step = item(status, Some("agent-claude"), Some(REPAIR));
            step["steps"][1]["metadata"][NOTE_KEY] = note.clone();
            assert!(
                matches!(plan(&step, &changes()), Plan::Leave(_)),
                "{status}"
            );
        }
        // A new verdict at a new head is a new return.
        let newer = changes_of(&verdict(REVIEWER, "changes", H2)).unwrap();
        let mut step = item("ready", None, Some(BUILDER));
        step["steps"][1]["metadata"][NOTE_KEY] = note;
        assert!(matches!(plan(&step, &newer), Plan::Note(_)));
    }

    #[test]
    fn the_note_names_the_review_the_car_and_its_builder() {
        let body = note_body(
            &held_car(),
            ITEM,
            &changes(),
            Some("agent-claude"),
            Some("2026-11-15T20:05:00+00:00"),
            RULE,
        );
        let note = &body[NOTE_KEY];
        assert_eq!(note["review"], json!(REVIEWER));
        assert_eq!(note["car"], json!(CAR));
        assert_eq!(note["branch"], json!("feat/x"));
        assert_eq!(note["head"], json!(H1));
        assert_eq!(note["builder_run"], json!(BUILDER));
        assert_eq!(note["built_by"], json!("claude@algedonic.dev"));
        assert_eq!(note["released"], json!("agent-claude"));
        assert_eq!(note["rule"], json!(RULE));
        let why = note["why"].as_str().unwrap();
        assert!(why.contains(&format!("boss dispatch {ITEM}")), "{why}");
        assert!(why.contains("REPAIR"), "{why}");
        assert!(
            body.as_object().unwrap().len() == 1,
            "one key, no nomination: {body}"
        );
    }

    /// The jobs API's answers this handler reads and writes. Every
    /// request is logged as `METHOD /path` with its body and its
    /// expectation header.
    struct Api {
        base: String,
        log: Arc<Mutex<Vec<(String, Json, Option<String>)>>>,
    }

    impl Api {
        fn requests(&self) -> Vec<String> {
            self.log
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.0.clone())
                .collect()
        }
        fn sent(&self, req: &str) -> Vec<(Json, Option<String>)> {
            self.log
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.0 == req)
                .map(|r| (r.1.clone(), r.2.clone()))
                .collect()
        }
    }

    /// `runs` are the agent-runs the API serves by id; `put` is the step
    /// PUT's answer (204 unless a test says the step raced).
    async fn api(car: Json, item: Json, runs: Vec<Json>, put: StatusCode) -> Api {
        let log: Arc<Mutex<Vec<(String, Json, Option<String>)>>> = Arc::default();
        let l = log.clone();
        let app = Router::new().fallback(
            move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let (log, car, item, runs) = (l.clone(), car.clone(), item.clone(), runs.clone());
                async move {
                    let path = uri.path().to_string();
                    let expect = headers
                        .get(boss_jobs::step_metadata_write::EXPECT_HEADER)
                        .and_then(|h| h.to_str().ok())
                        .map(str::to_string);
                    let parsed: Json = serde_json::from_slice(&body).unwrap_or(Json::Null);
                    log.lock()
                        .unwrap()
                        .push((format!("{method} {path}"), parsed, expect));
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
                        if put == StatusCode::CONFLICT {
                            return (
                                put,
                                axum::Json(json!({"code": "step_changed",
                                                  "error": "the step changed"})),
                            )
                                .into_response();
                        }
                        return put.into_response();
                    }
                    axum::Json(json!({})).into_response()
                }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Api {
            base: format!("http://{addr}"),
            log,
        }
    }

    async fn fire(api: &Api, payload: Json) -> Result<(), HandlerError> {
        let ctx = InvocationContext {
            rule_name: RULE.into(),
            triggering_event_id: "evt-review-1".into(),
            triggering_topic: "jobs.job.updated".into(),
            event_payload: payload,
            event_timestamp: Some("2026-11-15T20:04:35Z".parse().unwrap()),
        };
        Arc::new(JobsChangesReturnTheBuild {
            client: crate::handlers::common::api_client(),
            jobs_base: api.base.clone(),
        })
        .invoke(&[], &ctx)
        .await
    }

    fn run(id: &str, status: &str) -> Json {
        json!({"id": id, "kind": "agent-run", "status": status})
    }

    const PUT_BUILD: &str = "PUT /api/jobs/17e30000-0000-4000-8000-000000000000/steps/00000000-0000-4000-8000-000000000b1d";
    const PATCH_BUILD: &str = "PATCH /api/jobs/17e30000-0000-4000-8000-000000000000/steps/00000000-0000-4000-8000-000000000b1d/metadata";

    /// THE CASE (car 1d5379b5, item 76f89301, 2026-11-15): the build is
    /// held by the actor that dispatched it, on a builder run that has
    /// closed. It goes back to its station, nobody's, judged on the run
    /// edge it was read with, and the note follows.
    #[tokio::test]
    async fn a_changes_verdict_returns_a_held_build_to_its_station() {
        let api = api(
            held_car(),
            item("active", Some("agent-claude"), Some(BUILDER)),
            vec![run(BUILDER, "closed")],
            StatusCode::NO_CONTENT,
        )
        .await;
        fire(&api, verdict(REVIEWER, "changes", H1))
            .await
            .expect("returned");
        let reqs = api.requests();
        let put = reqs.iter().position(|r| r == PUT_BUILD).expect("released");
        let patch = reqs.iter().position(|r| r == PATCH_BUILD).expect("noted");
        assert!(put < patch, "the release first, then the note: {reqs:?}");
        let (body, expect) = api.sent(PUT_BUILD).remove(0);
        assert_eq!(body, release_put(), "ready, and nobody's — no nomination");
        assert!(
            expect.as_deref().is_some_and(|e| e.contains(BUILDER)),
            "judged on the run edge as read: {expect:?}"
        );
        let (note, _) = api.sent(PATCH_BUILD).remove(0);
        assert_eq!(note[NOTE_KEY]["review"], json!(REVIEWER));
        assert_eq!(note[NOTE_KEY]["released"], json!("agent-claude"));
    }

    /// …and the same for a car answering its item as ONE PIECE of it
    /// (backlog 390f9e55; car 5a1cbc09, item 47601829): its CHANGES
    /// returns the item's build exactly as a closing car's does.
    #[tokio::test]
    async fn a_changes_verdict_on_a_partial_car_returns_its_items_build() {
        let mut car = held_car();
        car["metadata"]["backlog_item"] = Json::Null;
        car["metadata"]["partial_item"] = json!(ITEM);
        let api = api(
            car,
            item("active", Some("agent-claude"), Some(BUILDER)),
            vec![run(BUILDER, "closed")],
            StatusCode::NO_CONTENT,
        )
        .await;
        fire(&api, verdict(REVIEWER, "changes", H1))
            .await
            .expect("returned");
        let reqs = api.requests();
        assert!(reqs.iter().any(|r| r == PUT_BUILD), "released: {reqs:?}");
        assert!(reqs.iter().any(|r| r == PATCH_BUILD), "noted: {reqs:?}");
    }

    /// A build already at its station gets the note and no write of its
    /// status or holder.
    #[tokio::test]
    async fn a_waiting_build_gets_the_note_alone() {
        let api = api(
            held_car(),
            item("ready", None, Some(BUILDER)),
            vec![],
            StatusCode::NO_CONTENT,
        )
        .await;
        fire(&api, verdict(REVIEWER, "changes", H1))
            .await
            .expect("noted");
        assert!(api.sent(PUT_BUILD).is_empty(), "{:?}", api.requests());
        let (note, _) = api.sent(PATCH_BUILD).remove(0);
        assert_eq!(note[NOTE_KEY]["released"], Json::Null);
    }

    /// A run open on the build — a repair already dispatched, or the
    /// builder's own run not yet reported — leaves it untouched.
    #[tokio::test]
    async fn a_build_with_an_open_run_is_left() {
        for (edge, status) in [(REPAIR, "open"), (BUILDER, "open")] {
            let api = api(
                held_car(),
                item("active", Some("agent-claude"), Some(edge)),
                vec![run(edge, status)],
                StatusCode::NO_CONTENT,
            )
            .await;
            fire(&api, verdict(REVIEWER, "changes", H1))
                .await
                .expect("left");
            assert!(
                api.requests().iter().all(|r| r.starts_with("GET ")),
                "{edge}: {:?}",
                api.requests()
            );
        }
    }

    /// A repair dispatched between the read and the release moves the
    /// edge, and the judged PUT is refused: nothing else is written.
    #[tokio::test]
    async fn a_build_that_moved_before_the_release_is_left() {
        let api = api(
            held_car(),
            item("active", Some("agent-claude"), Some(BUILDER)),
            vec![run(BUILDER, "closed")],
            StatusCode::CONFLICT,
        )
        .await;
        fire(&api, verdict(REVIEWER, "changes", H1))
            .await
            .expect("left");
        assert_eq!(api.sent(PUT_BUILD).len(), 1);
        assert!(api.sent(PATCH_BUILD).is_empty(), "{:?}", api.requests());
    }

    /// A RELEASE reads nothing at all; a CHANGES the repair door would
    /// refuse reads the car and writes nothing.
    #[tokio::test]
    async fn no_repair_owed_writes_nothing() {
        let api = api(
            held_car(),
            item("active", Some("agent-claude"), Some(BUILDER)),
            vec![run(BUILDER, "closed")],
            StatusCode::NO_CONTENT,
        )
        .await;
        fire(&api, verdict(REVIEWER, "release", H1))
            .await
            .expect("a no-op");
        assert!(api.requests().is_empty(), "{:?}", api.requests());
        fire(&api, verdict(REVIEWER, "changes", H2))
            .await
            .expect("a no-op");
        assert_eq!(api.requests(), vec![format!("GET /api/jobs/{CAR}")]);
    }
}
