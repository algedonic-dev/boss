//! THE ACT A CAR WAITS ON IS ITS OWNER'S WORK (backlog fb286c15).
//!
//! A landed car whose proof waits on a named actor's act —
//! `waits_on.owner = "emp-david"` — reads in the shed as that actor's
//! move (3881f5c9), but until this module nothing put the act on the
//! actor's queue. Measured 2026-09-26 on the live system of record: two
//! open cars (4b05fe3e, a tenant publish David runs; b94cb42f, a
//! tag-release David opens) stood at a ready `proven` step, and
//! `/api/jobs/assignments?assignee_id=emp-david` returned neither while
//! `assignee_id=agent-claude` returned one of them. The owner learned of
//! the act only by reading the shed, and the agent's My Day carried an
//! act it cannot perform.
//!
//! So the assignments read ROUTES, and writes nothing: a car's READY
//! `proven` step whose wait a named actor owns is listed for that actor
//! (with the wait's words, so the row says what the act is) and for no
//! other queue — not its step assignee's, and not any role's. The
//! ownership is read through [`crate::car::owned_wait`] and never
//! re-derived, so this queue, the shed and `boss orient` agree on whose
//! move a car is. A WORLD wait is nobody's act: it is left exactly where
//! its assignee holds it. An ACTIVE `proven` step was claimed by
//! somebody, and a claim is an explicit act, so it stays with its holder.

use boss_core::job::{Job, JobId, JobStatus, StepStatus};

use crate::car::{OwnedWait, WaitOwner, owned_wait};
use crate::port::{AssignmentRow, JobFilter, JobsError, JobsRepository};
use crate::regions::CAR_KIND;

/// The car step an owned wait holds: proof in production.
const PROVEN_SLUG: &str = "proven";

/// The key an assignments row carries its owned wait under —
/// `{owner, on, car}` — spelled once for the server that writes it and
/// `boss orient`, which reads it.
pub const ROW_KEY: &str = "owned_wait";

/// How many open cars one page of the car read asks for. Paged to the
/// total, so this bounds a round-trip, never the answer.
const CAR_PAGE: i64 = 500;

/// One open car whose proof waits on a named actor's act.
#[derive(Debug, Clone)]
pub struct ActorWait {
    pub car: Job,
    /// The actor whose act it is — `waits_on.owner`, as declared.
    pub actor: String,
    pub wait: OwnedWait,
}

/// Every open car whose wait a named actor owns. Pages to the store's
/// total: a truncated page would answer a smaller question and put an
/// act back on the wrong queue in silence.
pub async fn actor_waits<R: JobsRepository + ?Sized>(
    repo: &R,
) -> Result<Vec<ActorWait>, JobsError> {
    let filter = JobFilter {
        kind: Some(CAR_KIND.to_string()),
        status: Some(JobStatus::Open),
        ..Default::default()
    };
    let mut cars = Vec::new();
    loop {
        let offset = i64::try_from(cars.len()).unwrap_or(i64::MAX);
        let (page, total) = repo.list_jobs(&filter, CAR_PAGE, offset).await?;
        let empty = page.is_empty();
        cars.extend(page);
        if empty || i64::try_from(cars.len()).unwrap_or(i64::MAX) >= total {
            break;
        }
    }
    Ok(cars
        .into_iter()
        .filter_map(|car| {
            let wait = owned_wait(&car.metadata)?;
            let WaitOwner::Actor(actor) = wait.owner.clone() else {
                return None;
            };
            Some(ActorWait { car, actor, wait })
        })
        .collect())
}

/// Is this row the `proven` step an actor's wait holds?
fn is_held_proof(row: &AssignmentRow) -> bool {
    row.step.spec_slug.as_deref() == Some(PROVEN_SLUG) && row.step.status == StepStatus::Ready
}

/// Route one queue's rows by the owned waits. `assignee` is the queue
/// being read (`None` for a roles-only read, which owns no act). Rows a
/// DIFFERENT actor's wait holds are dropped; each ready `proven` step
/// `assignee`'s own waits hold is added once, in admission order with
/// the rest. Returns the routed rows and, for every car routed to
/// `assignee`, its wait — the words the row carries.
pub async fn route<R: JobsRepository + ?Sized>(
    repo: &R,
    assignee: Option<&str>,
    rows: Vec<AssignmentRow>,
) -> Result<(Vec<AssignmentRow>, Vec<(JobId, OwnedWait)>), JobsError> {
    let waits = actor_waits(repo).await?;
    if waits.is_empty() {
        return Ok((rows, Vec::new()));
    }
    let owner_of = |job: &JobId| waits.iter().find(|w| w.car.id == *job);
    let mut out: Vec<AssignmentRow> = rows
        .into_iter()
        .filter(|r| {
            !is_held_proof(r)
                || owner_of(&r.job_id).is_none_or(|w| Some(w.actor.as_str()) == assignee)
        })
        .collect();
    let mut mine = Vec::new();
    for w in waits.iter().filter(|w| Some(w.actor.as_str()) == assignee) {
        let proof =
            repo.list_steps(&w.car.id).await?.into_iter().find(|s| {
                s.spec_slug.as_deref() == Some(PROVEN_SLUG) && s.status == StepStatus::Ready
            });
        let Some(step) = proof else { continue };
        if !out.iter().any(|r| r.step.id == step.id) {
            out.push(AssignmentRow::of(&w.car, step));
        }
        mine.push((w.car.id, w.wait.clone()));
    }
    // Stable, so the store's (opened_on, sort_order) order holds for the
    // rest and an added car sits among them by its age.
    out.sort_by_key(|r| r.opened_on);
    Ok((out, mine))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryJobs;
    use boss_core::job::{Priority, Step, Subject};

    fn car(repo_meta: serde_json::Value) -> Job {
        let mut j = Job::new(
            CAR_KIND,
            Subject::new("custom", "bosspipeline"),
            "a car",
            "emp-1",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 9, 24).unwrap(),
        );
        j.status = JobStatus::Open;
        j.metadata = repo_meta;
        j
    }

    async fn park(repo: &InMemoryJobs, md: serde_json::Value, holder: &str) -> (Job, Step) {
        let job = car(md);
        repo.create_job(&job).await.unwrap();
        let mut s = Step::new(job.id, "task", "Proven in prod", 5).with_assignee(holder);
        s.spec_slug = Some(PROVEN_SLUG.into());
        s.status = StepStatus::Ready;
        repo.add_step(&s).await.unwrap();
        (job, s)
    }

    async fn queue(
        repo: &InMemoryJobs,
        who: &str,
    ) -> (Vec<AssignmentRow>, Vec<(JobId, OwnedWait)>) {
        let rows = repo.list_assignments(Some(who), &[], 100).await.unwrap();
        route(repo, Some(who), rows).await.unwrap()
    }

    #[tokio::test]
    async fn a_wait_an_actor_owns_is_that_actors_row_and_nobody_elses() {
        let repo = InMemoryJobs::new();
        let md = serde_json::json!({ "waits_on": {
            "on": "a tenant publish run (David runs it)", "owner": "emp-david" } });
        let (job, step) = park(&repo, md, "agent-claude").await;

        let (david, words) = queue(&repo, "emp-david").await;
        assert_eq!(david.len(), 1, "the owner's queue carries the act");
        assert_eq!(david[0].step.id, step.id);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].0, job.id);
        assert_eq!(words[0].1.on, "a tenant publish run (David runs it)");

        let (agent, words) = queue(&repo, "agent-claude").await;
        assert!(
            agent.is_empty(),
            "the step's holder no longer lists it: {agent:?}"
        );
        assert!(words.is_empty());
    }

    #[tokio::test]
    async fn a_world_wait_is_nobodys_act_and_stays_where_it_is_held() {
        let repo = InMemoryJobs::new();
        let md = serde_json::json!({ "waits_on": {
            "on": "a Stripe charge", "owner": "world", "seen": "true" } });
        let (_, step) = park(&repo, md, "agent-claude").await;
        for nobody in ["world", "the world", "emp-david"] {
            let (rows, words) = queue(&repo, nobody).await;
            assert!(rows.is_empty() && words.is_empty(), "{nobody}: {rows:?}");
        }
        let (agent, words) = queue(&repo, "agent-claude").await;
        assert_eq!(agent.len(), 1, "an unowned act is not re-routed");
        assert_eq!(agent[0].step.id, step.id);
        assert!(words.is_empty(), "and carries no owned-wait words");
    }
}
