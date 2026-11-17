//! `jobs.unheld_review_watch` — a READY review step NOBODY holds, waiting
//! past the bound its pinned workflow declares, files one alarm per
//! station (backlog f0e604dc, part 3 of eb86a174).
//!
//! THE CASE. Measured 2026-11-15 ~20:08Z over the 137 open
//! `ship-a-change` cars (triage run ee18d394, all rows read): 27 ready
//! review steps had no holder — 14 `review`, 13 `landed-review` — while
//! both steps declare `duration_hours = 2.0`. Re-read at 20:15Z by this
//! car's builder from `GET /api/jobs/queue-age` (every stamp `exact`):
//! the held `review`s had waited 2.6 to 588 hours, the `landed-review`s
//! 7.2 to 79 hours. No alarm read any of them. `agent-held-real-work-is-
//! watched-hourly` reads only AGENT-held steps (a step nobody holds has
//! no agent), and `an-idle-actor-holding-urgent-nominations-is-alarmed`
//! keys on an actor, which an unheld step does not have. And the parked
//! return of stale nominations to their station (car ff8d4bef,
//! `jobs.return_stranded_review`) makes this case MORE common: a step it
//! returns is ready, nobody's, and still unread.
//!
//! WHICH STEPS — the rule's args are the declaration:
//! - `kind`: the packet kind whose open board is read (`ship-a-change`).
//! - `profile`: the agent profile a step's agent block must declare
//!   (`reviewer`) — the same predicate the station pull's own-review
//!   exclusion reads, so no step slug is matched for membership.
//! - `marked.<slug> = "<key>"`: a step of that slug waits on a reviewer
//!   ONLY while its metadata marks `<key>` (`boss_jobs::stranded::marked`,
//!   the one marker reading). A car's `review` step is ready on every
//!   parked car, and an UNHELD one waits on its train, not on a reader —
//!   measured on ff8d4bef's own car at 20:15Z, the one unheld review in
//!   the read. Only a HELD car's review waits for a reviewer's verdict.
//!
//! THE BOUND IS THE STEP'S OWN DECLARATION, read from the workflow
//! version the packet is PINNED to (`GET /api/workflows/{kind}/versions/
//! {v}`, the step whose `title` is the slug), never from the active row —
//! the protocol's comment beside `duration_hours` says so. A step whose
//! pinned version declares no bound is not judged, and is counted.
//!
//! THE WAIT is the step's recorded ready flip, `since` off the queue-age
//! lens (`became_ready_at`, else its `updated_at` lower bound — the lens
//! labels which). A nomination returned to the station does not flip the
//! status, so the wait it already served still counts. A step the lens
//! does not carry cannot be dated and is not judged; when NONE of the
//! candidates can be dated the read is refused, not passed.
//!
//! ONE ALARM PER STATION (the step's `at`), held by staying OPEN: a
//! station with an open alarm is not alarmed again, and closing it
//! re-arms the watch — the `jobs.idle_actor_watch` idiom, so a queue of
//! thirty late reviews is one packet, not the flat list shouted.
//!
//! AN ALARM, NOT A LIMIT. It assigns, claims, nominates and moves
//! nothing; it files a packet naming every late step, oldest first.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use boss_jobs::channels::InputChannel;
use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

use super::common::{
    api_client, get_json, jobs_where_slim, open_jobs_of_kind, owner_for_filing, post_json,
    rows_or_refuse, with_lane,
};

/// The handler's registered name.
pub const HANDLER: &str = "jobs.unheld_review_watch";

/// The `scope` every alarm this handler files carries.
pub const SCOPE: &str = "unheld-review";

/// The alarm-metadata key naming the station whose queue is late — the
/// dedup key, read over OPEN backlog items only.
pub const STATION_KEY: &str = "unheld_review_station";

/// The arg prefix narrowing one slug to a marked step: `marked.<slug>`.
pub const MARKED_PREFIX: &str = "marked.";

/// The step-metadata key naming the station a step stands at.
const AT_KEY: &str = "at";

/// The key the schedule runner writes the firing instant under, on
/// every sub-day tick.
const TICK_AT: &str = "_at";

/// The rule's declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub kind: String,
    pub profile: String,
    /// slug → the metadata key that must be marked for that slug's step
    /// to be waiting on a reviewer.
    pub marked: BTreeMap<String, String>,
}

/// Parse the rule's args. `kind` and `profile` are required; a
/// `marked.<slug>` that is not a non-empty string refuses by name.
pub fn declaration(args: &[(String, Value)]) -> Result<Declaration, String> {
    let text = |name: &str, v: Option<&Value>| match v {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        Some(other) => Err(format!(
            "`{name}` must be a non-empty string, got a {} ({other:?})",
            other.kind()
        )),
        None => Err(format!("`{name}` is not declared on the rule")),
    };
    let mut marked = BTreeMap::new();
    for (key, value) in args {
        if let Some(slug) = key.strip_prefix(MARKED_PREFIX) {
            if slug.is_empty() {
                return Err(format!("`{key}` names no step slug"));
            }
            marked.insert(slug.to_string(), text(key, Some(value))?);
        }
    }
    Ok(Declaration {
        kind: text("kind", arg(args, "kind"))?,
        profile: text("profile", arg(args, "profile"))?,
        marked,
    })
}

/// A ready step of the declared profile that nobody holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Unheld {
    pub packet: String,
    pub packet_title: String,
    pub version: Option<i64>,
    pub step_id: String,
    pub slug: String,
    /// The step's `at`, or `<kind>/<slug>` when it stands nowhere named.
    pub station: String,
}

fn text(v: &Json, k: &str) -> String {
    v.get(k).and_then(Json::as_str).unwrap_or("").to_string()
}

/// The unheld candidates on the open board — before any bound is read.
/// Pure.
pub fn unheld_steps(decl: &Declaration, packets: &[Json]) -> Vec<Unheld> {
    let mut out = Vec::new();
    for job in packets {
        for step in job
            .get("steps")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
        {
            let md = step.get("metadata").cloned().unwrap_or(Json::Null);
            let slug = text(step, "spec_slug");
            let holder = text(step, "assignee_id");
            if text(step, "status") != "ready"
                || !holder.trim().is_empty()
                || md.get("agent_profile").and_then(Json::as_str) != Some(decl.profile.as_str())
            {
                continue;
            }
            if let Some(key) = decl.marked.get(&slug)
                && boss_jobs::stranded::marked(&md, key).is_none()
            {
                continue;
            }
            let station = md
                .get(AT_KEY)
                .and_then(Json::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{}/{slug}", decl.kind));
            out.push(Unheld {
                packet: text(job, "id"),
                packet_title: text(job, "title"),
                version: job.get("workflow_version").and_then(Json::as_i64),
                step_id: text(step, "id"),
                slug,
                station,
            });
        }
    }
    out
}

/// The `duration_hours` a workflow version body declares for the step
/// whose `title` is `slug`. `None` when it declares none.
pub fn declared_hours(spec: &Json, slug: &str) -> Option<f64> {
    spec.get("steps")
        .and_then(Json::as_array)?
        .iter()
        .find(|s| s.get("title").and_then(Json::as_str) == Some(slug))?
        .get("duration_hours")
        .and_then(Json::as_f64)
        .filter(|h| *h > 0.0)
}

/// step id → the instant it has waited since, off the queue-age rows.
pub fn ready_since(rows: &[Json]) -> BTreeMap<String, DateTime<Utc>> {
    rows.iter()
        .filter_map(|r| {
            let since = r
                .get("since")
                .and_then(Json::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())?;
            Some((text(r, "step_id"), since.with_timezone(&Utc)))
        })
        .filter(|(id, _)| !id.is_empty())
        .collect()
}

/// One unheld step past its bound.
#[derive(Debug, Clone, PartialEq)]
pub struct Late {
    pub step: Unheld,
    pub since: DateTime<Utc>,
    pub waited_hours: f64,
    pub bound_hours: f64,
}

/// What one judgement read: the late steps, and how many candidates it
/// could not judge for want of a bound or a stamp.
#[derive(Debug, Default, PartialEq)]
pub struct Judged {
    pub late: Vec<Late>,
    pub unbounded: usize,
    pub undated: usize,
}

/// Judge each candidate against its pinned bound. `bounds` maps
/// (version, slug) to the declared hours. Pure.
pub fn judge(
    candidates: Vec<Unheld>,
    bounds: &BTreeMap<(i64, String), f64>,
    since: &BTreeMap<String, DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Judged {
    let mut j = Judged::default();
    for step in candidates {
        let Some(bound) = step
            .version
            .and_then(|v| bounds.get(&(v, step.slug.clone())))
            .copied()
        else {
            j.unbounded += 1;
            continue;
        };
        let Some(at) = since.get(&step.step_id).copied() else {
            j.undated += 1;
            continue;
        };
        let waited = (now - at).num_seconds() as f64 / 3600.0;
        if waited > bound {
            j.late.push(Late {
                step,
                since: at,
                waited_hours: (waited * 10.0).round() / 10.0,
                bound_hours: bound,
            });
        }
    }
    j.late.sort_by_key(|l| l.since);
    j
}

/// The late steps, by the station they stand at, oldest first.
pub fn by_station(late: Vec<Late>) -> BTreeMap<String, Vec<Late>> {
    let mut out: BTreeMap<String, Vec<Late>> = BTreeMap::new();
    for l in late {
        out.entry(l.step.station.clone()).or_default().push(l);
    }
    out
}

/// The stations an OPEN alarm already names.
pub fn alarmed_stations(open: &[Json]) -> BTreeSet<String> {
    open.iter()
        .filter_map(|row| {
            row.pointer(&format!("/metadata/{STATION_KEY}"))
                .and_then(Json::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .collect()
}

/// The alarm one station's late queue files. `late` is oldest first.
pub fn alarm_body(
    kind: &str,
    station: &str,
    late: &[Late],
    owner: &str,
    now: DateTime<Utc>,
    rule: &str,
) -> Json {
    let id8 = |s: &str| s[..s.len().min(8)].to_string();
    let slugs: BTreeSet<&str> = late.iter().map(|l| l.step.slug.as_str()).collect();
    let slugs = slugs
        .into_iter()
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let oldest = &late[0];
    let lines: Vec<String> = late
        .iter()
        .map(|l| {
            format!(
                "`{}` on {} ({}), ready since {}: {}h against {}h",
                l.step.slug,
                l.step.packet,
                l.step.packet_title,
                l.since.to_rfc3339(),
                l.waited_hours,
                l.bound_hours
            )
        })
        .collect();
    let steps: Vec<Json> = late
        .iter()
        .map(|l| {
            json!({
                "packet": l.step.packet,
                "packet_title": l.step.packet_title,
                "step": l.step.step_id,
                "step_slug": l.step.slug,
                "ready_since": l.since.to_rfc3339(),
                "waited_hours": l.waited_hours,
                "bound_hours": l.bound_hours,
            })
        })
        .collect();
    let detail = format!(
        "Raised by {HANDLER} (rule {rule}, backlog f0e604dc) at {now}: {n} READY {kind} \
         review step(s) at station {station} are held by NOBODY and have waited past the bound \
         their pinned workflow declares. Oldest first: {lines}. A station holds unplaced work \
         until an executor pulls it, so a review no reviewer pulls waits in silence; this \
         alarm is that read. Pull or dispatch a reviewer for each (`boss dispatch <car> --step \
         <slug>`). It names the steps at filing; while it stays open the station is not \
         alarmed again, and closing it re-arms the watch. It assigns and moves nothing.",
        now = now.to_rfc3339(),
        n = late.len(),
        lines = lines.join("; "),
    );
    let metadata = json!({
        "area": "jobs",
        "scope": SCOPE,
        STATION_KEY: station,
        "workflow": kind,
        "steps": steps,
        "measured_at": now.to_rfc3339(),
        "detail": detail,
    });
    json!({
        "kind": "backlog-item",
        "title": format!(
            "UNHELD REVIEW: {n} ready {kind} {slugs} past their bound with no reviewer — \
             oldest {h}h ({id}, {t})",
            n = late.len(),
            h = oldest.waited_hours,
            id = id8(&oldest.step.packet),
            t = oldest.step.packet_title,
        ),
        "subject": {"subject_kind": "custom", "id": station},
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": with_lane(metadata, InputChannel::Telemetry),
    })
}

pub struct JobsUnheldReviewWatch {
    client: boss_core::machine_token::Client,
    jobs_base: String,
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl JobsUnheldReviewWatch {
    pub fn new(
        jobs_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            owner,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }
}

#[async_trait]
impl Handler for JobsUnheldReviewWatch {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let decl = declaration(args).map_err(HandlerError::Permanent)?;
        let now = ctx
            .event_payload
            .get(TICK_AT)
            .and_then(Json::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
            .ok_or_else(|| {
                HandlerError::Permanent(format!(
                    "the firing carries no `{TICK_AT}` — {HANDLER} judges hours and needs a \
                     sub-day cadence (hourly, every-<n>-minutes)"
                ))
            })?;

        let packets = open_jobs_of_kind(&self.client, self.base(), &decl.kind, rule).await?;
        let candidates = unheld_steps(&decl, &packets);
        if candidates.is_empty() {
            tracing::info!(rule, cars = packets.len(), "{HANDLER}: no unheld review");
            return Ok(());
        }

        let path = "/api/jobs/queue-age";
        let lens = get_json(&self.client, &format!("{}{path}", self.base()), rule).await?;
        let rows: Vec<Json> =
            rows_or_refuse(&lens, &format!("GET {path}")).map_err(HandlerError::Downstream)?;
        let since = ready_since(&rows);

        // Each pinned version once, and only the ones a candidate needs.
        let mut bounds: BTreeMap<(i64, String), f64> = BTreeMap::new();
        let versions: BTreeSet<i64> = candidates.iter().filter_map(|c| c.version).collect();
        for v in versions {
            let spec = get_json(
                &self.client,
                &format!("{}/api/workflows/{}/versions/{v}", self.base(), decl.kind),
                rule,
            )
            .await?;
            for c in candidates.iter().filter(|c| c.version == Some(v)) {
                if let Some(h) = declared_hours(&spec, &c.slug) {
                    bounds.insert((v, c.slug.clone()), h);
                }
            }
        }

        let n = candidates.len();
        let judged = judge(candidates, &bounds, &since, now);
        if judged.undated == n {
            // No evidence is not a pass: a lens that dates none of the
            // ready steps the board shows is a dark or narrowed read.
            return Err(HandlerError::Downstream(format!(
                "GET {path} dated none of the {n} unheld ready step(s) the open {} board \
                 holds ({} row(s) read) — refusing rather than reading them as on time",
                decl.kind,
                rows.len()
            )));
        }

        let open = jobs_where_slim(
            &self.client,
            self.base(),
            &format!("kind=backlog-item&status=open&metadata_has={STATION_KEY}"),
            rule,
        )
        .await?;
        let alarmed = alarmed_stations(&open);
        let owner = owner_for_filing(self.owner.as_ref(), rule).await;
        let late_n = judged.late.len();
        let mut filed = 0usize;
        for (station, late) in by_station(judged.late) {
            if alarmed.contains(&station) {
                continue;
            }
            post_json(
                &self.client,
                &format!("{}/api/jobs", self.base()),
                &alarm_body(&decl.kind, &station, &late, &owner, now, rule),
                rule,
            )
            .await?;
            filed += 1;
        }
        tracing::info!(
            rule,
            unheld = n,
            late = late_n,
            unbounded = judged.unbounded,
            undated = judged.undated,
            filed,
            "{HANDLER}: read {} open {} packet(s)",
            packets.len(),
            decl.kind
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::listing_stub;
    use super::*;

    const NOW: &str = "2026-11-15T20:15:00+00:00";
    const REVIEW_AT: &str = "9deb0155-9804-4488-a043-14add3a8c5be";
    const LANDED_AT: &str = "f675e19a-7c36-453f-bbac-e206aced63ab";

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn args() -> Vec<(String, Value)> {
        vec![
            ("kind".into(), Value::String("ship-a-change".into())),
            ("profile".into(), Value::String("reviewer".into())),
            ("marked.review".into(), Value::String("hold".into())),
        ]
    }

    fn decl() -> Declaration {
        declaration(&args()).unwrap()
    }

    /// A car in the shape the live board had at 20:15Z: `review` and
    /// `landed-review` both carrying the reviewer agent block and their
    /// station's `at`.
    fn car(id: &str, slug: &str, assignee: Option<&str>, extra: Json) -> Json {
        let at = if slug == "review" {
            REVIEW_AT
        } else {
            LANDED_AT
        };
        let mut md = json!({"agent_profile": "reviewer", "at": at});
        if let (Json::Object(m), Json::Object(x)) = (&mut md, extra) {
            m.extend(x);
        }
        json!({
            "id": id, "kind": "ship-a-change", "status": "open", "title": format!("car {id}"),
            "workflow_version": 17,
            "steps": [
                {"id": format!("{id}-gate"), "spec_slug": "gate", "status": "completed",
                 "metadata": {}},
                {"id": format!("{id}-{slug}"), "spec_slug": slug, "status": "ready",
                 "assignee_id": assignee, "metadata": md},
            ],
        })
    }

    fn spec() -> Json {
        json!({"kind": "ship-a-change", "version": 17, "steps": [
            {"title": "gate"},
            {"title": "review", "duration_hours": 2.0},
            {"title": "landed-review", "duration_hours": 2.0},
        ]})
    }

    fn lens(rows: &[(&str, &str)]) -> Json {
        let data: Vec<Json> = rows
            .iter()
            .map(|(step, since)| json!({"step_id": step, "since": since, "exact": true}))
            .collect();
        json!({"data": data, "total": rows.len()})
    }

    #[test]
    fn the_declaration_names_its_kind_its_profile_and_each_marked_slug() {
        let d = decl();
        assert_eq!(d.kind, "ship-a-change");
        assert_eq!(d.profile, "reviewer");
        assert_eq!(
            d.marked,
            BTreeMap::from([("review".to_string(), "hold".to_string())])
        );
        for missing in ["kind", "profile"] {
            let a: Vec<_> = args().into_iter().filter(|(k, _)| k != missing).collect();
            assert!(declaration(&a).unwrap_err().contains(missing));
        }
        let mut bad = args();
        bad.push(("marked.landed-review".into(), Value::Int(3)));
        assert!(
            declaration(&bad)
                .unwrap_err()
                .contains("marked.landed-review")
        );
    }

    /// The case: an unheld landed review and a HELD car's review are
    /// waiting on a reviewer. An unheld car's review waits on its train,
    /// a nominated or claimed step has a taker, and a builder step is
    /// not a review.
    #[test]
    fn only_a_ready_unheld_reviewer_step_is_a_candidate_and_review_only_when_held() {
        let packets = vec![
            car("landed", "landed-review", None, json!({})),
            car(
                "held",
                "review",
                None,
                json!({"hold": "touches an ops verb"}),
            ),
            car("boarding", "review", None, json!({})),
            car("released", "review", None, json!({"hold": false})),
            car("nominated", "landed-review", Some("agent-codex"), json!({})),
            car("blank", "landed-review", Some("  "), json!({})),
            car(
                "builder",
                "landed-review",
                None,
                json!({"agent_profile": "builder"}),
            ),
        ];
        let got: Vec<String> = unheld_steps(&decl(), &packets)
            .into_iter()
            .map(|u| u.step_id)
            .collect();
        assert_eq!(
            got,
            vec!["landed-landed-review", "held-review", "blank-landed-review"]
        );
        let u = &unheld_steps(&decl(), &packets[..1])[0];
        assert_eq!(u.station, LANDED_AT);
        assert_eq!(u.version, Some(17));
    }

    #[test]
    fn the_bound_is_the_pinned_steps_own_declaration() {
        assert_eq!(declared_hours(&spec(), "review"), Some(2.0));
        assert_eq!(declared_hours(&spec(), "gate"), None);
        assert_eq!(declared_hours(&spec(), "nonesuch"), None);
    }

    #[test]
    fn a_step_is_late_only_past_its_bound_and_one_without_a_bound_or_stamp_is_counted() {
        let packets = vec![
            car("old", "landed-review", None, json!({})),
            car("fresh", "landed-review", None, json!({})),
            car("nostamp", "landed-review", None, json!({})),
        ];
        let mut unpinned = car("unpinned", "landed-review", None, json!({}));
        unpinned["workflow_version"] = Json::Null;
        let mut all = packets.clone();
        all.push(unpinned);
        let bounds = BTreeMap::from([((17, "landed-review".to_string()), 2.0)]);
        let since = ready_since(
            lens(&[
                ("old-landed-review", "2026-11-15T12:57:00Z"),
                ("fresh-landed-review", "2026-11-15T19:00:00Z"),
                ("unpinned-landed-review", "2026-11-10T00:00:00Z"),
            ])["data"]
                .as_array()
                .unwrap(),
        );
        let j = judge(unheld_steps(&decl(), &all), &bounds, &since, at(NOW));
        let late: Vec<&str> = j.late.iter().map(|l| l.step.packet.as_str()).collect();
        assert_eq!(late, vec!["old"]);
        assert_eq!(j.late[0].waited_hours, 7.3);
        assert_eq!(j.late[0].bound_hours, 2.0);
        assert_eq!((j.unbounded, j.undated), (1, 1));
    }

    #[test]
    fn one_alarm_per_station_names_every_late_step_oldest_first() {
        let packets = vec![
            car("young", "landed-review", None, json!({})),
            car("oldest", "landed-review", None, json!({})),
            car(
                "held",
                "review",
                None,
                json!({"hold": "trust-boundary car"}),
            ),
        ];
        let bounds = BTreeMap::from([
            ((17, "landed-review".to_string()), 2.0),
            ((17, "review".to_string()), 2.0),
        ]);
        let since = ready_since(
            lens(&[
                ("young-landed-review", "2026-11-15T10:00:00Z"),
                ("oldest-landed-review", "2026-11-12T13:03:00Z"),
                ("held-review", "2026-10-22T03:50:00Z"),
            ])["data"]
                .as_array()
                .unwrap(),
        );
        let stations =
            by_station(judge(unheld_steps(&decl(), &packets), &bounds, &since, at(NOW)).late);
        assert_eq!(stations.len(), 2, "{stations:?}");
        let landed = &stations[LANDED_AT];
        let order: Vec<&str> = landed.iter().map(|l| l.step.packet.as_str()).collect();
        assert_eq!(order, vec!["oldest", "young"]);
        let body = alarm_body(
            "ship-a-change",
            LANDED_AT,
            landed,
            "emp-owner",
            at(NOW),
            "r",
        );
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(body["priority"], "urgent");
        assert_eq!(body["owner_id"], "emp-owner");
        let m = &body["metadata"];
        assert_eq!(m[STATION_KEY], LANDED_AT);
        assert_eq!(m["scope"], SCOPE);
        assert_eq!(m["steps"].as_array().unwrap().len(), 2);
        assert_eq!(m["steps"][0]["step"], "oldest-landed-review");
        let title = body["title"].as_str().unwrap();
        assert!(
            title.starts_with(
                "UNHELD REVIEW: 2 ready ship-a-change `landed-review` past their bound"
            ),
            "{title}"
        );
        assert!(
            title.contains("oldest 79.2h (oldest, car oldest)"),
            "{title}"
        );
        let detail = m["detail"].as_str().unwrap();
        assert!(detail.contains("car young"), "{detail}");
        assert!(detail.contains("boss dispatch"), "{detail}");
    }

    // -- end to end, against the stub jobs API ---------------------------

    const OPEN: &str = "/api/jobs?kind=ship-a-change&status=open";
    const LENS: &str = "/api/jobs/queue-age";
    const SPEC: &str = "/api/workflows/ship-a-change/versions/17";
    const ALARMS: &str =
        "/api/jobs?kind=backlog-item&status=open&metadata_has=unheld_review_station";

    fn ctx() -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: "an-unheld-ready-review-past-its-bound-is-alarmed".into(),
            triggering_event_id: format!("clock-tick:{NOW}"),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_day": "2026-11-15", "_at": NOW}),
        }
    }

    fn handler(stub: &listing_stub::Stub) -> Arc<JobsUnheldReviewWatch> {
        JobsUnheldReviewWatch::new(
            stub.base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    fn board(rows: Vec<Json>) -> Json {
        let n = rows.len();
        json!({"data": rows, "total": n})
    }

    fn live() -> Vec<(&'static str, Json)> {
        vec![
            (
                OPEN,
                board(vec![
                    car("a", "landed-review", None, json!({})),
                    car("b", "landed-review", None, json!({})),
                    car("h", "review", None, json!({"hold": "trust-boundary car"})),
                ]),
            ),
            (
                LENS,
                lens(&[
                    ("a-landed-review", "2026-11-15T12:00:00Z"),
                    ("b-landed-review", "2026-11-15T13:00:00Z"),
                    ("h-review", "2026-11-15T19:00:00Z"),
                ]),
            ),
            (SPEC, spec()),
        ]
    }

    #[tokio::test]
    async fn a_late_unheld_queue_files_one_alarm_for_its_station() {
        let mut answers = live();
        answers.push((ALARMS, listing_stub::empty_listing()));
        let stub = listing_stub::serve(answers).await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(
            sent.len(),
            1,
            "two late landed reviews, one alarm; the held review is fresh: {sent:?}"
        );
        assert_eq!(sent[0].0, "POST /api/jobs");
        assert_eq!(sent[0].1["metadata"][STATION_KEY], LANDED_AT);
        assert_eq!(sent[0].1["metadata"]["steps"].as_array().unwrap().len(), 2);
    }

    /// Held by staying open: a station whose alarm is open is not
    /// alarmed again.
    #[tokio::test]
    async fn a_station_with_an_open_alarm_is_not_alarmed_again() {
        let mut answers = live();
        answers.push((
            ALARMS,
            board(vec![
                json!({"id": "al-1", "kind": "backlog-item", "status": "open",
                "metadata": {"scope": SCOPE, STATION_KEY: LANDED_AT}}),
            ]),
        ));
        let stub = listing_stub::serve(answers).await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    /// No evidence is not a pass: a lens that dates none of the ready
    /// steps is refused, not read as "all on time".
    #[tokio::test]
    async fn a_lens_that_dates_no_candidate_is_refused() {
        let stub = listing_stub::serve(vec![
            live().remove(0),
            (LENS, listing_stub::empty_listing()),
            (SPEC, spec()),
            (ALARMS, listing_stub::empty_listing()),
        ])
        .await;
        let err = handler(&stub).invoke(&args(), &ctx()).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::Downstream(ref m) if m.contains("queue-age")),
            "{err}"
        );
        assert!(stub.writes().is_empty());
    }

    #[tokio::test]
    async fn a_lens_with_no_data_array_refuses_by_name() {
        let stub = listing_stub::serve(vec![
            live().remove(0),
            (LENS, listing_stub::no_data_array()),
        ])
        .await;
        listing_stub::assert_refused_by_name(
            handler(&stub).invoke(&args(), &ctx()).await,
            "GET /api/jobs/queue-age",
        );
        assert!(stub.writes().is_empty());
    }

    /// The shipped rule: hourly, reading ship-a-change's reviewer steps,
    /// a car's `review` only while it is held — and the bound it judges
    /// is the protocol's own `duration_hours`, so the rule declares none.
    #[test]
    fn the_shipped_rule_watches_reviewer_steps_and_a_review_only_when_held() {
        let raw =
            boss_dispatcher::rules::registry::parse_raw_path(boss_testing::dispatcher_rules_dir())
                .expect("parse the shipped rule registry");
        let rule = raw
            .rules
            .iter()
            .find(|r| r.do_steps.iter().any(|d| d.handler == HANDLER))
            .expect("a shipped rule invokes this handler");
        let args: Vec<(String, Value)> = rule
            .do_steps
            .iter()
            .flat_map(|d| d.args.iter())
            .map(|(k, v)| {
                (
                    k.clone(),
                    Value::String(v.trim().trim_matches('"').to_string()),
                )
            })
            .collect();
        let d = declaration(&args).unwrap_or_else(|e| panic!("{}: {e}", rule.name));
        assert_eq!(d.kind, "ship-a-change");
        assert_eq!(d.profile, "reviewer");
        assert_eq!(d.marked.get("review").map(String::as_str), Some("hold"));
        assert!(
            !args.iter().any(|(k, _)| k.contains("hours")),
            "the bound is the pinned step's duration_hours, never a second copy on the rule"
        );
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = JobsUnheldReviewWatch::new(
            "http://unused",
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        assert_eq!(h.name(), HANDLER);
        assert!(
            crate::cascade::handler_emits().contains_key(HANDLER),
            "the cascade table knows what this handler emits"
        );
    }
}
