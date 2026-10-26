//! URGENT FIRST, AT THE DOOR (backlog 90bff1f3).
//!
//! THE CASE. Measured 2026-10-19/20: session B (agent-claude-b)
//! dispatched about 32 standard builds while five urgent builds assigned
//! to it sat READY with no run (c1d16aa6, ab09f1af, 493a02b6, 8cf6faf7,
//! ec1e1980), and it missed a deadlined urgent nomination (d846e0c8).
//! The only signal was prose — `conductor_order_add` keys on its
//! work-session packet — and nothing made B read it. David's rule is
//! "urgent first, always", and the lesson belongs in the verb, not in an
//! agent's instructions.
//!
//! So `boss dispatch` refuses a STANDARD step, before anything is claimed
//! or filed, while the dispatching actor holds ready urgent work it could
//! start instead, and names that work and the command that starts the
//! first of it. `--force-standard --why '<reason>'` is the way past, and
//! the why goes on the record — the standard packet's `dispatch_note`
//! and the run's `urgent_first_override` — after every other door, just
//! before the claim.
//!
//! ONE READ OF THE ACTOR'S OWN QUEUE. The candidates are the rows of
//! `GET /api/jobs/assignments?for=me` — the read `boss orient`'s MY WORK
//! and `dispatch --next` make, whose server expands the caller through
//! the agents registry (design ea906603 Q2), so the alias rule lives in
//! one place. Ranked by `top_board::priority_rank`, the drain order every
//! other surface keeps. A candidate is then CONFIRMED with the reads a
//! dispatch of it would itself make — its packet, the cars that name it,
//! the open runs that name it, and its kind's row when its step carries
//! no projected block — because the queue row does not hold the facts
//! that make urgent work unstartable, and refusing an actor in favour of
//! work it cannot start would teach it to reach for the escape by habit.
//! An actor holding no urgent row pays exactly the one read; one holding
//! some pays until the first is confirmed, at most [`MAX_CANDIDATES`].
//!
//! A CAR IS AS URGENT AS ITS ITEM (review e22dd1c6, F1). Every car is
//! filed `standard`, so ranked on itself the review a trust-boundary hold
//! on an urgent item waits for was refused as standard work. A car is
//! ranked as the better of its own priority and its item's — the target
//! and a candidate alike — at one read, for cars only.
//!
//! WHAT DOES NOT COUNT, each named in the refusal so the reader can see
//! the door looked: a step not READY, or nominated to someone else, or
//! reached only through a role (the actor's own is what is assigned or
//! nominated to it); a packet no longer open; a packet that declares a
//! wait on another (`metadata.waiting_on` — the job edge a design-review
//! block rides, cleared by the dispatcher when that packet closes); an
//! item an open car already carries — held at the dock awaiting review,
//! in flight, or landed awaiting proof (`orient::carried_items`, MY
//! WORK's own words), which the landed and in-flight doors would refuse
//! anyway; a step an open agent-run already names; and a step declaring
//! no agent block, which nothing can dispatch.
//!
//! WHAT IT ADMITS WITHOUT JUDGING, and says so: a queue it cannot read,
//! or read whole (the server cuts it, newest first, at its limit), and a
//! walk that confirmed nothing but could not read every candidate. One
//! confirmed candidate refuses whatever another's read did.
//! The landed-work door is best-effort on its read for the same reason —
//! a jobs API that cannot answer one query must not stop every dispatch
//! — and the door is a guard against not having looked, not against an
//! actor set on passing it, who has the escape and leaves its why on the
//! record. A dispatch handed to an Agent call by the PreToolUse hook is
//! not judged here at all: the hook exits 0 whatever this verb answers,
//! so a refusal there would launch the agent with no run behind it.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

/// The key on the STANDARD packet's metadata that records an override:
/// who took it over which urgent work, and why.
pub(crate) const NOTE_KEY: &str = "dispatch_note";

/// The same record on the run the override filed.
pub(crate) const RUN_KEY: &str = "urgent_first_override";

/// The `kind` an override's note carries, so a reader of `dispatch_note`
/// knows which door wrote it.
pub(crate) const NOTE_KIND: &str = "urgent-first-override";

/// One urgent step the caller could start instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Urgent {
    pub(crate) packet: String,
    pub(crate) title: String,
    pub(crate) workflow: String,
    pub(crate) slug: String,
    pub(crate) priority: String,
    pub(crate) opened_on: Option<chrono::NaiveDate>,
    /// The newest moment the record shows the step's packet moving — a
    /// step completing, or this step being released back to ready. A
    /// step is ready no LATER than that, so `now - ready_since` is a
    /// floor on how long it has waited, and the refusal says "at least".
    pub(crate) ready_since: Option<DateTime<Utc>>,
}

/// What the door found.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Verdict {
    /// Nothing urgent is the caller's to start: admitted as before.
    Clear,
    /// The caller's startable urgent work, drain order first, and the
    /// urgent rows read but not counted, each with why.
    Holding {
        who: String,
        urgent: Vec<Urgent>,
        skipped: Vec<String>,
    },
    /// The queue, or a read that confirms it, could not be read.
    Unread(String),
}

fn text<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}

fn id8(id: &str) -> &str {
    &id[..id.len().min(8)]
}

/// The rows of a `for=me` answer that may be the caller's urgent work
/// waiting to be started: READY, ranked above standard — or a car's
/// review, whose rank is its item's and is read in [`confirm`] (review
/// e22dd1c6 F1: every car is filed standard) — assigned or nominated to
/// one of the ids the answer was read for, and not on the packet being
/// dispatched. One row per step, in the drain order MY WORK and `--next`
/// keep: rank, then admission, then id; so the reviews come last.
pub(crate) fn queued<'a>(for_me: &'a Value, target: &str) -> Vec<&'a Value> {
    let me = crate::top_board::for_ids(for_me);
    let mut seen = std::collections::BTreeSet::new();
    let car_review = |r: &Value| {
        text(r, "/workflow") == boss_jobs::regions::CAR_KIND
            && text(r, "/step/spec_slug") == boss_jobs::car::REVIEW_SLUG
    };
    let mut rows: Vec<&Value> = for_me
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|r| crate::top_board::priority_rank(r) < 2 || car_review(r))
        .filter(|r| text(r, "/step/status") == "ready")
        .filter(|r| me.iter().any(|m| m == text(r, "/step/assignee_id")))
        .filter(|r| text(r, "/job_id") != target)
        .filter(|r| seen.insert(text(r, "/step/id").to_string()))
        .collect();
    rows.sort_by_key(|r| {
        (
            crate::top_board::priority_rank(r),
            text(r, "/opened_on").to_string(),
            text(r, "/job_id").to_string(),
        )
    });
    rows
}

/// Why a queued urgent row is not the caller's to start now, judged on
/// the packet as it stands (`job`), its kind's active row when the step
/// carries no projected block, the cars and the open runs that name it.
/// `None`: it counts.
pub(crate) fn excluded(
    row: &Value,
    job: &Value,
    kind_row: Option<&Value>,
    cars: &[Value],
    runs: &[Value],
    me: &[String],
) -> Option<String> {
    let packet = text(row, "/job_id");
    let slug = text(row, "/step/spec_slug");
    let status = text(job, "/status");
    if status != "open" {
        return Some(format!("its packet is {status}"));
    }
    // The job edge a design-review block rides: the dispatcher clears it
    // to "" when the packet waited on closes, so only a non-empty id
    // waits.
    let waits = text(job, "/metadata/waiting_on").trim();
    if !waits.is_empty() {
        return Some(format!("waits on {} (metadata.waiting_on)", id8(waits)));
    }
    // The step as the packet holds it NOW — the queue row is a copy.
    let step_id = text(row, "/step/id");
    let step = crate::envelope::steps(job)
        .into_iter()
        .find(|s| text(s, "/id") == step_id);
    let Some(step) = step.filter(|s| {
        text(s, "/status") == "ready" && me.iter().any(|m| m == text(s, "/assignee_id"))
    }) else {
        return Some("no longer ready for you on its packet".to_string());
    };
    // An unheld car's review is owed after it lands, and dispatch
    // refuses it before then (`unheld_review_refusal`).
    if crate::dispatch::unheld_review_refusal(text(job, "/kind"), slug, step).is_some() {
        return Some("its car carries no hold — its review is owed after it lands".to_string());
    }
    if let Some(state) = crate::orient::carried_items(cars).get(packet) {
        return Some(state.clone());
    }
    if let Some(run) = runs.iter().find(|r| {
        text(r, "/status") == "open"
            && text(r, "/metadata/packet") == packet
            && text(r, "/metadata/step") == slug
    }) {
        return Some(format!("open run {} names it", id8(text(run, "/id"))));
    }
    if crate::dispatch::settings_for(step, kind_row).is_none() {
        return Some("its step declares no agent block — nothing can dispatch it".to_string());
    }
    None
}

/// The newest moment the packet shows moving: a step's `completed_at`,
/// or this step's release back to ready (`released`, `released_unrun`).
fn ready_since(job: &Value, step: &Value) -> Option<DateTime<Utc>> {
    let parse = |t: &str| {
        DateTime::parse_from_rfc3339(t)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    };
    crate::envelope::steps(job)
        .into_iter()
        .filter_map(|s| s.get("completed_at").and_then(Value::as_str))
        .chain(
            ["/metadata/released/at", "/metadata/released_unrun/at"]
                .iter()
                .filter_map(|p| step.pointer(p).and_then(Value::as_str)),
        )
        .filter_map(parse)
        .max()
}

/// The refusal: the standard step asked for, each urgent item with how
/// long it has been ready at least, the command that starts the first,
/// and the escape.
pub(crate) fn refusal(
    target: &str,
    slug: &str,
    who: &str,
    urgent: &[Urgent],
    skipped: &[String],
    now: DateTime<Utc>,
) -> String {
    let mut out = vec![format!(
        "URGENT FIRST — `{slug}` on {} is standard work, and {} urgent step(s) are ready \
         for {who} with nobody on them (backlog 90bff1f3: urgent first, always):",
        id8(target),
        urgent.len()
    )];
    for u in urgent {
        let waited = match u.ready_since {
            Some(t) => format!(
                "ready at least {}",
                crate::top_board::span((now - t).num_minutes())
            ),
            None => "ready since ? — the record shows no step completing".to_string(),
        };
        let title: String = u.title.chars().take(60).collect();
        out.push(format!(
            "    {} {} {} {} `{}` — {waited}, packet opened {}",
            u.priority,
            u.workflow,
            id8(&u.packet),
            title.trim_end(),
            u.slug,
            u.opened_on
                .map_or_else(|| "?".to_string(), |d| d.to_string())
        ));
    }
    if !skipped.is_empty() {
        out.push(format!(
            "  Read and not counted ({}), because each is not yours to start now:",
            skipped.len()
        ));
        out.extend(skipped.iter().map(|s| format!("    {s}")));
    }
    if let Some(first) = urgent.first() {
        out.push("  Take the first:".to_string());
        out.push(format!(
            "    boss dispatch {} --step {}",
            id8(&first.packet),
            first.slug
        ));
    }
    out.push(
        "  Or, when this standard step genuinely must go first, say why — it is written to \
         this packet's dispatch_note and the run before the claim:"
            .to_string(),
    );
    out.push(format!(
        "    boss dispatch {} --step {slug} --force-standard --why '<reason>'",
        id8(target)
    ));
    out.push("  Nothing was claimed and no run was filed.".to_string());
    out.join("\n")
}

/// The override's record — the same object on the standard packet's
/// `dispatch_note` and the run's `urgent_first_override`. No timestamp
/// of the client's: the write's own event is dated by the server. It is
/// written after every door that can refuse but the claim, and says so:
/// a claim refused after it leaves the note with no run (review e22dd1c6).
pub(crate) fn note(slug: &str, by: &str, why: &str, verdict: &Verdict) -> Value {
    let mut n = json!({ "kind": NOTE_KIND, "step": slug, "by": by, "why": why,
                        "recorded": "before the claim — if the claim was refused, no run took it" });
    match verdict {
        Verdict::Holding { urgent, .. } => {
            n["overrode"] = urgent
                .iter()
                .map(|u| {
                    json!({ "packet": u.packet, "step": u.slug, "priority": u.priority,
                            "title": u.title })
                })
                .collect();
        }
        // Forced without a judgement: the record says the queue was
        // unread rather than claiming nothing was overridden.
        Verdict::Unread(reason) => {
            n["overrode"] = json!([]);
            n["queue_unread"] = json!(reason);
        }
        Verdict::Clear => n["overrode"] = json!([]),
    }
    n
}

/// How many candidates the door reads before it stops (review e22dd1c6
/// F3: three or four reads each, in sequence, uncapped, cost agent-claude
/// 18-24 reads per standard dispatch on 2026-10-20). It stops sooner, at
/// the first it confirms; past this many unconfirmed it says so.
pub(crate) const MAX_CANDIDATES: usize = 5;

/// The word for a [`crate::top_board::priority_rank`].
pub(crate) fn word(rank: u8) -> &'static str {
    match rank {
        0 => "emergency",
        1 => "urgent",
        _ => "standard",
    }
}

/// The item a car carries — the packet it closes, or one piece of.
fn car_item(job: &Value) -> Option<&str> {
    (text(job, "/kind") == boss_jobs::regions::CAR_KIND)
        .then(|| {
            [boss_jobs::car::BACKLOG_ITEM, boss_jobs::car::PARTIAL_ITEM]
                .iter()
                .map(|k| text(job, &format!("/metadata/{k}")))
                .find(|id| !id.is_empty())
        })
        .flatten()
}

/// The rank a dispatch of `job` is judged at (review e22dd1c6 F1). A
/// packet's own priority — except a car, which is the better of its own
/// and its item's: every car is filed `standard` (boss-jobs car.rs), so
/// the review a trust-boundary hold on an URGENT item waits for read as
/// standard work and was refused. One read, for cars only; an item that
/// cannot be read is an error, never a guess of `standard`.
pub(crate) async fn rank<F, Fut>(get: &F, job: &Value) -> Result<u8, String>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Option<Value>>>,
{
    let own = crate::top_board::priority_rank(job);
    let Some(item) = car_item(job) else {
        return Ok(own);
    };
    let path = format!("/api/jobs/{item}");
    match get(path.clone()).await {
        Ok(Some(item)) => Ok(own.min(crate::top_board::priority_rank(&item))),
        Ok(None) => Err(format!("{path} answered nothing")),
        Err(e) => Err(format!("{path}: {e:#}")),
    }
}

/// One candidate, read.
enum Read {
    Counts(Urgent),
    NotCounted(String),
    /// A car whose item is not urgent: not urgent work, said nowhere.
    NotUrgent,
    Unread(String),
}

/// Confirm one candidate with the reads a dispatch of it would make: its
/// packet, for a car its item (its rank), the cars that name it (not for
/// a car — no car names a car), the open runs that name it, and its
/// kind's row when the step carries no projected block, once per kind.
/// Any read that fails makes THIS candidate unread, and nothing else.
async fn confirm<F, Fut>(
    get: &F,
    row: &Value,
    me: &[String],
    kind_rows: &mut std::collections::BTreeMap<String, Option<Value>>,
) -> Read
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Option<Value>>>,
{
    let packet = text(row, "/job_id");
    let read = |path: String| {
        let fut = get(path.clone());
        async move {
            fut.await
                .map_err(|e| format!("{path}: {e:#}"))
                .and_then(|b| b.ok_or_else(|| format!("{path} answered nothing")))
        }
    };
    let list = |path: String| {
        let fut = read(path);
        async move {
            fut.await
                .and_then(|b| crate::train::rows(Some(b)).map_err(|e| format!("{e:#}")))
        }
    };
    let job = match read(format!("/api/jobs/{packet}")).await {
        Ok(job) => job,
        Err(why) => return Read::Unread(why),
    };
    let level = match rank(get, &job).await {
        Ok(r) if r < 2 => r,
        Ok(_) => return Read::NotUrgent,
        Err(why) => return Read::Unread(why),
    };
    // B1 (fc24ec5f): an actor's own held car is somebody else's review
    // work, even when that review was nominated to one of its aliases.
    if text(&job, "/kind") == boss_jobs::regions::CAR_KIND
        && let Some(builder) = boss_jobs::car_release::builder_run(&job)
    {
        let builder = match read(format!("/api/jobs/{builder}")).await {
            Ok(builder) => builder,
            Err(why) => return Read::Unread(why),
        };
        let actor = text(&builder, "/metadata/agent");
        if text(&builder, "/kind") != "agent-run" || actor.trim().is_empty() {
            return Read::Unread("the car's builder run names no agent".into());
        }
        if me.iter().any(|id| id == actor) {
            return Read::NotCounted(
                "you built this car — its review belongs to another actor".into(),
            );
        }
    }
    let cars = match car_item(&job) {
        Some(_) => Vec::new(),
        None => match list(crate::dispatch::cars_for_item_query(packet)).await {
            Ok(cars) => cars,
            Err(why) => return Read::Unread(why),
        },
    };
    let runs = match list(open_runs_query(packet)).await {
        Ok(runs) => runs,
        Err(why) => return Read::Unread(why),
    };
    let step = crate::envelope::steps(&job)
        .into_iter()
        .find(|s| text(s, "/id") == text(row, "/step/id"))
        .cloned()
        .unwrap_or(Value::Null);
    let kind = text(&job, "/kind").to_string();
    // F2: a row that cannot be read is not a row declaring no block —
    // the error is this candidate's, as `next_at` propagates it.
    if crate::dispatch::block_on_step(&step).is_none() && !kind_rows.contains_key(&kind) {
        match get(format!("/api/workflows/{kind}")).await {
            Ok(r) => {
                kind_rows.insert(kind.clone(), r);
            }
            Err(e) => return Read::Unread(format!("/api/workflows/{kind}: {e:#}")),
        }
    }
    let kind_row = kind_rows.get(&kind).and_then(Option::as_ref);
    match excluded(row, &job, kind_row, &cars, &runs, me) {
        Some(why) => Read::NotCounted(why),
        None => Read::Counts(Urgent {
            packet: packet.to_string(),
            title: text(row, "/job_title").to_string(),
            workflow: text(row, "/workflow").to_string(),
            slug: text(row, "/step/spec_slug").to_string(),
            priority: word(level).to_string(),
            opened_on: text(row, "/opened_on").parse().ok(),
            ready_since: ready_since(&job, &step),
        }),
    }
}

/// The door: read the caller's queue, then its candidates in drain order,
/// at most [`MAX_CANDIDATES`] of them, judging each on its own — the
/// first one confirmed holds whatever another's read did, and stops the
/// walk. Confirming none is Clear only when every candidate was read.
pub(crate) async fn judge<F, Fut>(get: F, target: &str) -> Verdict
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Option<Value>>>,
{
    let for_me = match crate::top_board::for_me_answer(get(crate::top_board::for_me_path()).await) {
        Ok(body) => body,
        Err(why) => return Verdict::Unread(why),
    };
    // F4 (review e22dd1c6): the server sorts the queue oldest first and
    // clamps it at its limit, so a cut drops the NEWEST rows — the
    // freshly filed urgent ones. A cut answer is unread, never clear,
    // judged by the one reader every for=me caller shares (backlog
    // 09074ada landed it on main while this car waited; one definition).
    if let Some(why) = crate::top_board::for_me_cut(&for_me) {
        return Verdict::Unread(format!("the queue answer is cut: {why}"));
    }
    let me = crate::top_board::for_ids(&for_me);
    let candidates = queued(&for_me, target);
    let mut kind_rows: std::collections::BTreeMap<String, Option<Value>> = Default::default();
    let mut skipped = Vec::new();
    let mut unread = Vec::new();
    for row in candidates.iter().take(MAX_CANDIDATES) {
        let at = format!(
            "{} {}",
            id8(text(row, "/job_id")),
            text(row, "/step/spec_slug")
        );
        match confirm(&get, row, &me, &mut kind_rows).await {
            Read::Counts(u) => {
                return Verdict::Holding {
                    who: crate::top_board::for_whom(&for_me),
                    urgent: vec![u],
                    skipped,
                };
            }
            Read::NotCounted(why) => skipped.push(format!("{at} — {why}")),
            Read::NotUrgent => {}
            Read::Unread(why) => {
                skipped.push(format!("{at} — unread: {why}"));
                unread.push(format!("{at}: {why}"));
            }
        }
    }
    let rest = candidates.len().saturating_sub(MAX_CANDIDATES);
    if rest > 0 {
        unread.push(format!(
            "{rest} more candidate(s) past the first {MAX_CANDIDATES} were not read"
        ));
    }
    match unread.is_empty() {
        true => Verdict::Clear,
        false => Verdict::Unread(unread.join("; ")),
    }
}

/// The open agent-runs that name `packet` — narrowed at the server by the
/// containment door, and re-read off each row by [`excluded`], since a
/// server that ignored `metadata=` would answer the unfiltered page.
fn open_runs_query(packet: &str) -> String {
    let doc = json!({ "packet": packet }).to_string();
    format!(
        "/api/jobs?kind={}&status=open&metadata={}&limit=50",
        crate::dispatch::RUN_KIND,
        percent_encoding::utf8_percent_encode(&doc, crate::job::QUERY_VALUE)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: &str = "39d0b528-ff69-4cb8-ba82-408b641da66c";
    const URGENT: &str = "c1d16aa6-0000-4000-8000-000000000001";
    const OTHER: &str = "ab09f1af-0000-4000-8000-000000000002";
    const ME: [&str; 2] = ["agent-claude-b", "claude-b@algedonic.dev"];

    fn me() -> Vec<String> {
        ME.iter().map(|s| s.to_string()).collect()
    }

    fn row(job: &str, priority: &str, status: &str, assignee: Option<&str>) -> Value {
        json!({
            "job_id": job, "job_title": "Fix the urgent thing", "workflow": "backlog-item",
            "priority": priority, "opened_on": "2026-10-18",
            "step": { "id": format!("{job}-build"), "spec_slug": "build", "status": status,
                      "assignee_id": assignee, "metadata": {} },
        })
    }

    fn for_me(rows: Vec<Value>) -> Value {
        let n = rows.len();
        json!({ "data": rows, "total": n,
                "for": { "ids": ME, "roles": ["platform-admin"] } })
    }

    /// The urgent packet as the jobs API answers it: open, its build
    /// READY and nominated to the caller's alias, the block projected.
    fn packet(id: &str, metadata: Value) -> Value {
        json!({
            "id": id, "kind": "backlog-item", "title": "Fix the urgent thing",
            "status": "open", "priority": "urgent", "opened_on": "2026-10-18",
            "metadata": metadata,
            "steps": [
                { "id": format!("{id}-triage"), "spec_slug": "triage", "status": "completed",
                  "completed_at": "2026-10-20T10:00:00Z", "metadata": {} },
                { "id": format!("{id}-build"), "spec_slug": "build", "status": "ready",
                  "assignee_id": "claude-b@algedonic.dev",
                  "metadata": { "agent_profile": "builder", "agent_model": "opus-5-5",
                                "agent_budget_usd": 5.0, "agent_effort": "high" } },
            ],
        })
    }

    /// THE CANDIDATES ARE THE CALLER'S OWN READY URGENT STEPS. Measured
    /// shape: the live `for=me` answer for agent-claude on 2026-10-20
    /// held 817 rows — its own nominations, and every step its role
    /// reaches held by others or by nobody — of which seven were urgent,
    /// ready and assigned to it. Only those seven are the actor's to take.
    #[test]
    fn only_the_callers_ready_urgent_steps_are_candidates() {
        let answer = for_me(vec![
            row(URGENT, "urgent", "ready", Some("claude-b@algedonic.dev")),
            row(OTHER, "emergency", "ready", Some("agent-claude-b")),
            row("someone-else-0000", "urgent", "ready", Some("agent-claude")),
            row("role-reached-0000", "urgent", "ready", None),
            row(
                "already-active-00",
                "urgent",
                "active",
                Some("agent-claude-b"),
            ),
            row(
                "standard-work-000",
                "standard",
                "ready",
                Some("agent-claude-b"),
            ),
            row(TARGET, "urgent", "ready", Some("agent-claude-b")),
            // The same step read under both ids is one step.
            row(URGENT, "urgent", "ready", Some("claude-b@algedonic.dev")),
        ]);
        let got: Vec<&str> = queued(&answer, TARGET)
            .iter()
            .filter_map(|r| r.get("job_id").and_then(Value::as_str))
            .collect();
        // In the drain order: the emergency before the urgent one.
        assert_eq!(got, vec![OTHER, URGENT]);
    }

    /// WHAT DOES NOT COUNT, each with its reason — the shapes the item
    /// names: a step an open run carries, a packet waiting on a design,
    /// an item a held car carries; and the ones a stale queue row can
    /// show: a packet closed or a step moved on since the row was read.
    #[test]
    fn unstartable_urgent_work_is_not_counted_and_says_why() {
        let r = row(URGENT, "urgent", "ready", Some("claude-b@algedonic.dev"));
        let clean = packet(URGENT, json!({}));
        assert_eq!(
            excluded(&r, &clean, None, &[], &[], &me()),
            None,
            "it counts"
        );

        let design = "9f1d0c2a-0000-4000-8000-00000000d351";
        let waiting = packet(URGENT, json!({ "waiting_on": design }));
        let why = excluded(&r, &waiting, None, &[], &[], &me()).expect("waits");
        assert!(why.contains("waits on 9f1d0c2a"), "{why}");
        // A cleared wait is the dispatcher's empty string: it counts.
        let cleared = packet(URGENT, json!({ "waiting_on": "" }));
        assert_eq!(excluded(&r, &cleared, None, &[], &[], &me()), None);

        let run = json!({ "id": "5b1d2c3e-0000-4000-8000-000000000001", "kind": "agent-run",
                          "status": "open",
                          "metadata": { "packet": URGENT, "step": "build" } });
        let why =
            excluded(&r, &clean, None, &[], std::slice::from_ref(&run), &me()).expect("carried");
        assert!(why.contains("open run 5b1d2c3e"), "{why}");
        // A run on ANOTHER step of the packet, or a closed one, carries
        // nothing of this step.
        let mut elsewhere = run.clone();
        elsewhere["metadata"]["step"] = json!("triage");
        let mut closed = run;
        closed["status"] = json!("closed");
        assert_eq!(
            excluded(&r, &clean, None, &[], &[elsewhere, closed], &me()),
            None
        );

        // Parked HELD at the dock awaiting its review — the shape the
        // item names, read in MY WORK's words.
        let held_car = json!({
            "id": "e7f3ed8c-0000-4000-8000-0000000000ca", "kind": "ship-a-change",
            "status": "open",
            "metadata": { "backlog_item": URGENT, "branch": "fix/the-urgent-thing" },
            "steps": [{ "id": "c-review", "spec_slug": "review", "title": "Open for review",
                        "status": "ready",
                        "metadata": { "hold": "trust-boundary car: waits for its review" } }],
        });
        let why = excluded(&r, &clean, None, &[held_car], &[], &me()).expect("carried");
        assert!(why.contains("HELD at the dock"), "{why}");
        assert!(why.contains("fix/the-urgent-thing"), "{why}");

        let mut shut = clean.clone();
        shut["status"] = json!("closed");
        assert!(excluded(&r, &shut, None, &[], &[], &me()).is_some());

        let mut moved = clean.clone();
        moved["steps"][1]["assignee_id"] = json!("agent-claude");
        let why = excluded(&r, &moved, None, &[], &[], &me()).expect("not mine now");
        assert!(why.contains("no longer ready for you"), "{why}");

        // No projected block, and no row to read one from: nothing can
        // dispatch it, so the caller is not refused in its favour.
        let mut bare = clean.clone();
        bare["steps"][1]["metadata"] = json!({ "authority_role": "platform-admin" });
        let why = excluded(&r, &bare, None, &[], &[], &me()).expect("undispatchable");
        assert!(why.contains("no agent block"), "{why}");
        let row_block = json!({ "kind": "backlog-item", "steps": [
            { "title": "build", "agent": { "profile": "builder", "model": "opus-5[1m]",
                                           "budget_usd": 5, "effort": "high" } } ] });
        assert_eq!(
            excluded(&r, &bare, Some(&row_block), &[], &[], &me()),
            None,
            "the active row's block makes it dispatchable"
        );
    }

    /// THE REFUSAL NAMES THE WORK, ITS WAIT, THE COMMAND AND THE ESCAPE.
    #[test]
    fn the_refusal_names_each_item_the_first_command_and_the_escape() {
        let now = "2026-10-20T16:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let urgent = vec![
            Urgent {
                packet: URGENT.into(),
                title: "Fix the urgent thing".into(),
                workflow: "backlog-item".into(),
                slug: "build".into(),
                priority: "urgent".into(),
                opened_on: "2026-10-18".parse().ok(),
                ready_since: "2026-10-20T10:00:00Z".parse().ok(),
            },
            Urgent {
                packet: OTHER.into(),
                title: "Another".into(),
                workflow: "user-feedback".into(),
                slug: "triage".into(),
                priority: "urgent".into(),
                opened_on: "2026-10-19".parse().ok(),
                ready_since: None,
            },
        ];
        let skipped = vec!["d846e0c8 build — waits on 9f1d0c2a".to_string()];
        let text = refusal(TARGET, "build", "agent-claude-b", &urgent, &skipped, now);
        assert!(text.contains("URGENT FIRST"), "{text}");
        assert!(text.contains("39d0b528"), "names the standard step: {text}");
        assert!(text.contains("c1d16aa6"), "{text}");
        assert!(text.contains("ready at least 6h"), "{text}");
        assert!(text.contains("ab09f1af"), "{text}");
        assert!(
            text.contains("boss dispatch c1d16aa6 --step build"),
            "{text}"
        );
        assert!(
            text.contains("boss dispatch 39d0b528 --step build --force-standard --why '"),
            "{text}"
        );
        assert!(
            text.contains("waits on 9f1d0c2a"),
            "what was not counted: {text}"
        );
        assert!(text.contains("Nothing was claimed"), "{text}");
    }

    /// THE DOOR END TO END, over a fake read: one urgent row, confirmed
    /// by its packet, no car, no run — holding; and an answer with no
    /// `for` block (a server older than the read) is unread, never clear.
    #[tokio::test]
    async fn the_door_reads_the_queue_then_confirms_each_candidate() {
        let answer = for_me(vec![row(
            URGENT,
            "urgent",
            "ready",
            Some("claude-b@algedonic.dev"),
        )]);
        let urgent = packet(URGENT, json!({}));
        let get = |path: String| {
            let (answer, urgent) = (answer.clone(), urgent.clone());
            async move {
                Ok(Some(match path.as_str() {
                    p if p.starts_with("/api/jobs/assignments?for=me") => answer,
                    p if p == format!("/api/jobs/{URGENT}") => urgent,
                    p if p.starts_with("/api/jobs?") => json!({ "data": [], "total": 0 }),
                    other => panic!("unexpected read {other}"),
                }))
            }
        };
        match judge(get, TARGET).await {
            Verdict::Holding { urgent, .. } => {
                assert_eq!(urgent.len(), 1);
                assert_eq!(urgent[0].packet, URGENT);
                assert_eq!(urgent[0].slug, "build");
            }
            other => panic!("expected holding, got {other:?}"),
        }

        let old = |_path: String| async { Ok(Some(json!({ "data": [], "total": 0 }))) };
        assert!(matches!(judge(old, TARGET).await, Verdict::Unread(_)));
    }

    // ---- review e22dd1c6 (CHANGES at 914c899f): F1-F4 ----------------

    const CAR: &str = "e7f3ed8c-0000-4000-8000-0000000000ca";
    const ITEM: &str = "90bff1f3-0000-4000-8000-000000000003";

    type Reads = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// A fake jobs API: each read is answered by the first route its path
    /// starts with, and logged; a path no route names is an ERROR, so a
    /// read the door was not meant to make shows as one.
    fn served(
        routes: Vec<(String, Result<Value, &'static str>)>,
    ) -> (
        impl Fn(String) -> std::future::Ready<anyhow::Result<Option<Value>>>,
        Reads,
    ) {
        let log: Reads = Default::default();
        let l = log.clone();
        let get = move |path: String| {
            l.lock().unwrap().push(path.clone());
            let hit = routes.iter().find(|(p, _)| path.starts_with(p.as_str()));
            std::future::ready(match hit {
                Some((_, Ok(v))) => Ok(Some(v.clone())),
                Some((_, Err(e))) => Err(anyhow::anyhow!("{e}")),
                None => Err(anyhow::anyhow!("unstubbed read {path}")),
            })
        };
        (get, log)
    }

    fn at(id: &str) -> String {
        format!("/api/jobs/{id}")
    }

    fn empty() -> Result<Value, &'static str> {
        Ok(json!({ "data": [], "total": 0 }))
    }

    /// A car of `item` (named under `key`) at its review step, READY and
    /// nominated to the caller, HELD or not — filed `standard`, as every
    /// car is (boss-jobs car.rs).
    fn car(key: &str, item: &str, held: bool) -> Value {
        let mut md = json!({ "agent_profile": "reviewer", "agent_model": "opus-5-5",
                             "agent_budget_usd": 3.0, "agent_effort": "medium" });
        if held {
            md["hold"] = json!("trust-boundary car: waits for its review");
        }
        json!({
            "id": CAR, "kind": "ship-a-change", "title": "The urgent thing's car",
            "status": "open", "priority": "standard", "opened_on": "2026-10-20",
            "metadata": { key: item, "branch": "fix/the-urgent-thing" },
            "steps": [{ "id": "car-review", "spec_slug": "review", "status": "ready",
                        "assignee_id": "agent-claude-b", "metadata": md }],
        })
    }

    fn car_row() -> Value {
        json!({
            "job_id": CAR, "job_title": "The urgent thing's car", "workflow": "ship-a-change",
            "priority": "standard", "opened_on": "2026-10-20",
            "step": { "id": "car-review", "spec_slug": "review", "status": "ready",
                      "assignee_id": "agent-claude-b", "metadata": {} },
        })
    }

    fn item(priority: &str) -> Value {
        let mut p = packet(ITEM, json!({}));
        p["priority"] = json!(priority);
        p
    }

    /// F1. A CAR IS AS URGENT AS ITS ITEM. Every car is filed standard,
    /// so the review a trust-boundary hold on an urgent item waits for
    /// read as standard work and was refused. The rank is the better of
    /// the car's own and its item's, read once, for cars only; any other
    /// packet is ranked on itself with no read at all.
    #[tokio::test]
    async fn a_car_ranks_as_the_better_of_itself_and_its_item() {
        for key in ["backlog_item", "partial_item"] {
            let (get, reads) = served(vec![(at(ITEM), Ok(item("urgent")))]);
            assert_eq!(rank(&get, &car(key, ITEM, true)).await, Ok(1), "{key}");
            assert_eq!(*reads.lock().unwrap(), vec![at(ITEM)], "one read: {key}");
        }
        let (get, _) = served(vec![(at(ITEM), Ok(item("standard")))]);
        assert_eq!(rank(&get, &car("backlog_item", ITEM, true)).await, Ok(2));

        let (get, reads) = served(vec![]);
        assert_eq!(rank(&get, &packet(URGENT, json!({}))).await, Ok(1));
        assert!(reads.lock().unwrap().is_empty(), "a non-car costs no read");

        // An item that cannot be read is no rank at all — never standard.
        let (get, _) = served(vec![(at(ITEM), Err("502 the proxy"))]);
        let why = rank(&get, &car("backlog_item", ITEM, true))
            .await
            .expect_err("unread");
        assert!(why.contains("502 the proxy"), "{why}");
    }

    /// F1, the other half: a held review on an urgent item's car is
    /// urgent work the caller could start, so it is a candidate — named
    /// at its item's priority; an unheld one (whose review is owed after
    /// it lands, and which dispatch refuses) is not counted, and a car of
    /// a standard item is not urgent work at all.
    #[tokio::test]
    async fn a_held_review_on_an_urgent_items_car_is_urgent_work() {
        let routes = |car: Value, item: Value| {
            vec![
                (
                    "/api/jobs/assignments?for=me".to_string(),
                    Ok(for_me(vec![car_row()])),
                ),
                (at(CAR), Ok(car)),
                (at(ITEM), Ok(item)),
                ("/api/jobs?kind=agent-run".to_string(), empty()),
            ]
        };
        let (get, reads) = served(routes(car("backlog_item", ITEM, true), item("urgent")));
        match judge(get, TARGET).await {
            Verdict::Holding { urgent, .. } => {
                assert_eq!(urgent[0].packet, CAR);
                assert_eq!(urgent[0].slug, "review");
                assert_eq!(urgent[0].priority, "urgent", "its item's priority");
            }
            other => panic!("expected holding, got {other:?}"),
        }
        assert!(
            !reads
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.contains("kind=ship-a-change")),
            "no car names a car: the cars read is not made for one"
        );

        let (get, _) = served(routes(car("backlog_item", ITEM, false), item("urgent")));
        assert_eq!(judge(get, TARGET).await, Verdict::Clear, "unheld");
        let (get, _) = served(routes(car("backlog_item", ITEM, true), item("standard")));
        assert_eq!(judge(get, TARGET).await, Verdict::Clear, "standard item");
    }

    /// B1 (review fc24ec5f): the builder cannot review its own car,
    /// including when the nomination uses another alias of the actor.
    #[tokio::test]
    async fn your_own_cars_review_is_not_urgent_work_you_can_start() {
        for builder in ME {
            let mut own = car("backlog_item", ITEM, true);
            own["metadata"]["agent_run"] = json!("builder-run");
            let (get, _) = served(vec![
                (
                    "/api/jobs/assignments?for=me".into(),
                    Ok(for_me(vec![car_row()])),
                ),
                (at(CAR), Ok(own)),
                (at(ITEM), Ok(item("urgent"))),
                (
                    at("builder-run"),
                    Ok(json!({"kind":"agent-run", "metadata":{"agent":builder}})),
                ),
                ("/api/jobs?kind=agent-run".into(), empty()),
            ]);
            assert_eq!(judge(get, TARGET).await, Verdict::Clear, "{builder}");
        }
    }

    #[tokio::test]
    async fn another_builders_car_counts_and_an_unread_builder_is_said() {
        for builder in [
            Ok(json!({"kind":"agent-run", "metadata":{"agent":"someone-else"}})),
            Err("502 builder unavailable"),
        ] {
            let mut held = car("backlog_item", ITEM, true);
            held["metadata"]["agent_run"] = json!("builder-run");
            let (get, _) = served(vec![
                (
                    "/api/jobs/assignments?for=me".into(),
                    Ok(for_me(vec![car_row()])),
                ),
                (at(CAR), Ok(held)),
                (at(ITEM), Ok(item("urgent"))),
                (at("builder-run"), builder.clone()),
                ("/api/jobs?kind=agent-run".into(), empty()),
            ]);
            match (builder, judge(get, TARGET).await) {
                (Ok(_), Verdict::Holding { .. }) => {}
                (Err(_), Verdict::Unread(why)) => {
                    assert!(why.contains("502 builder unavailable"), "{why}")
                }
                (_, got) => panic!("unexpected builder verdict: {got:?}"),
            }
        }
    }

    /// F2. A Workflow row that cannot be read is not a row that declares
    /// no agent block: the candidate is unread, and with nothing else
    /// confirmed the door says so rather than clearing in silence.
    #[tokio::test]
    async fn a_failed_kind_row_read_is_unread_never_no_block() {
        let mut bare = packet(URGENT, json!({}));
        bare["steps"][1]["metadata"] = json!({ "authority_role": "platform-admin" });
        let (get, _) = served(vec![
            (
                "/api/jobs/assignments?for=me".to_string(),
                Ok(for_me(vec![row(
                    URGENT,
                    "urgent",
                    "ready",
                    Some("agent-claude-b"),
                )])),
            ),
            (at(URGENT), Ok(bare)),
            ("/api/jobs?".to_string(), empty()),
            (
                "/api/workflows/".to_string(),
                Err("500 the registry is down"),
            ),
        ]);
        match judge(get, TARGET).await {
            Verdict::Unread(why) => assert!(why.contains("500 the registry is down"), "{why}"),
            other => panic!("expected unread, got {other:?}"),
        }
    }

    /// F3. Judged per candidate: an unreadable candidate does not unread
    /// one the door confirms, before it or after it; and the walk stops
    /// at the first confirmed, so the reads after it are never made.
    #[tokio::test]
    async fn a_confirmed_candidate_holds_whatever_another_reads() {
        let answer = for_me(vec![
            row(OTHER, "emergency", "ready", Some("agent-claude-b")),
            row(URGENT, "urgent", "ready", Some("agent-claude-b")),
        ]);
        let (get, reads) = served(vec![
            (
                "/api/jobs/assignments?for=me".to_string(),
                Ok(answer.clone()),
            ),
            (at(OTHER), Ok(packet(OTHER, json!({})))),
            (at(URGENT), Err("502 a transient proxy error")),
            ("/api/jobs?".to_string(), empty()),
        ]);
        match judge(get, TARGET).await {
            Verdict::Holding { urgent, .. } => assert_eq!(urgent[0].packet, OTHER),
            other => panic!("expected holding, got {other:?}"),
        }
        assert!(
            !reads.lock().unwrap().contains(&at(URGENT)),
            "stopped at the first confirmed"
        );

        // The unreadable one FIRST: skipped as unread, and the second
        // still holds.
        let (get, _) = served(vec![
            ("/api/jobs/assignments?for=me".to_string(), Ok(answer)),
            (at(OTHER), Err("502 a transient proxy error")),
            (at(URGENT), Ok(packet(URGENT, json!({})))),
            ("/api/jobs?".to_string(), empty()),
        ]);
        match judge(get, TARGET).await {
            Verdict::Holding {
                urgent, skipped, ..
            } => {
                assert_eq!(urgent[0].packet, URGENT);
                assert!(
                    skipped
                        .iter()
                        .any(|s| s.contains("unread") && s.contains("502")),
                    "{skipped:?}"
                );
            }
            other => panic!("expected holding, got {other:?}"),
        }
    }

    /// F3, the bound: candidates past [`MAX_CANDIDATES`] are not read,
    /// and a walk that confirmed none and left some unread is Unread —
    /// it never claims a queue it did not finish reading is clear.
    #[tokio::test]
    async fn the_walk_is_capped_and_a_capped_walk_that_confirms_nothing_is_unread() {
        let ids: Vec<String> = (0..MAX_CANDIDATES + 2)
            .map(|n| format!("c1d16aa6-0000-4000-8000-{n:012}"))
            .collect();
        let rows = ids
            .iter()
            .map(|id| row(id, "urgent", "ready", Some("agent-claude-b")))
            .collect();
        let mut routes = vec![("/api/jobs/assignments?for=me".to_string(), Ok(for_me(rows)))];
        routes.extend(ids.iter().map(|id| {
            (
                at(id),
                Ok(packet(
                    id,
                    json!({ "waiting_on": "9f1d0c2a-0000-4000-8000-00000000d351" }),
                )),
            )
        }));
        routes.push(("/api/jobs?".to_string(), empty()));
        let (get, reads) = served(routes);
        match judge(get, TARGET).await {
            Verdict::Unread(why) => assert!(why.contains("2 more"), "{why}"),
            other => panic!("expected unread, got {other:?}"),
        }
        let packets = reads
            .lock()
            .unwrap()
            .iter()
            .filter(|p| ids.iter().any(|id| **p == at(id)))
            .count();
        assert_eq!(packets, MAX_CANDIDATES, "no packet past the cap is read");
    }

    /// F4. The server clamps the queue at the limit, oldest first, so a
    /// cut drops the NEWEST rows — the freshly filed urgent ones. A cut
    /// answer is unread, never clear: an answer holding as many rows as
    /// were asked for, or one the server marks `truncated`.
    #[tokio::test]
    async fn a_cut_queue_is_unread_never_clear() {
        let full: Vec<Value> = (0..crate::top_board::FOR_ME_LIMIT)
            .map(|n| {
                row(
                    &format!("s-{n}"),
                    "standard",
                    "ready",
                    Some("agent-claude-b"),
                )
            })
            .collect();
        let (get, _) = served(vec![(
            "/api/jobs/assignments?for=me".to_string(),
            Ok(for_me(full)),
        )]);
        match judge(get, TARGET).await {
            Verdict::Unread(why) => assert!(why.contains("1000"), "{why}"),
            other => panic!("expected unread, got {other:?}"),
        }
        let mut marked = for_me(vec![]);
        marked["truncated"] = json!(true);
        let (get, _) = served(vec![(
            "/api/jobs/assignments?for=me".to_string(),
            Ok(marked),
        )]);
        assert!(matches!(judge(get, TARGET).await, Verdict::Unread(_)));
        // A server that counts `total` before its cut (backlog 09074ada)
        // shows the cut there even when it does not say `truncated`: the
        // door judges with the reader MY WORK and --next share, not a
        // copy of it that knew only the flag and a full page.
        let mut short = for_me(vec![]);
        short["total"] = json!(3);
        short["truncated"] = json!(false);
        let (get, _) = served(vec![(
            "/api/jobs/assignments?for=me".to_string(),
            Ok(short),
        )]);
        match judge(get, TARGET).await {
            Verdict::Unread(why) => assert!(why.contains("0 of at least 3"), "{why}"),
            other => panic!("expected unread, got {other:?}"),
        }
    }

    /// THE RECORD carries who, why, the step it took, and what it took
    /// it over — enough to re-read the decision without the session.
    #[test]
    fn the_note_records_the_why_and_what_it_overrode() {
        let v = Verdict::Holding {
            who: "agent-claude-b".into(),
            urgent: vec![Urgent {
                packet: URGENT.into(),
                title: "Fix the urgent thing".into(),
                workflow: "backlog-item".into(),
                slug: "build".into(),
                priority: "urgent".into(),
                opened_on: None,
                ready_since: None,
            }],
            skipped: vec![],
        };
        let n = note(
            "build",
            "agent-claude-b",
            "the deploy window closes at 17:00",
            &v,
        );
        assert_eq!(n["kind"], NOTE_KIND);
        assert_eq!(n["step"], "build");
        assert_eq!(n["by"], "agent-claude-b");
        assert_eq!(n["why"], "the deploy window closes at 17:00");
        assert_eq!(n["overrode"][0]["packet"], URGENT);
        assert_eq!(n["overrode"][0]["step"], "build");
        assert!(
            n["recorded"]
                .as_str()
                .is_some_and(|s| s.contains("before the claim")),
            "the note says where it stands: {n}"
        );
    }
}
