//! WHAT OUTRANKS REGULAR ORDER — the top board's first row (backlog
//! 74569e94, design ea906603 Q1, decided 2026-09-27).
//!
//! David, 2026-09-26: show urgent jobs on the top board. The data was
//! on every job row (measured that morning: 17 of 460 open packets were
//! urgent, every one a backlog-item) and no read answered it — `GET
//! /api/jobs` has no priority parameter, and the HUD is one read by
//! design. So it is a field on `GET /api/yard/regions`, computed here
//! once, and `boss orient` prints the same payload.
//!
//! WHICH PRIORITIES OUTRANK is the station discipline's own rank
//! ([`crate::station_queue::priority_rank`]): a priority that drains
//! before `standard` outranks regular order — `emergency` and `urgent`.
//! `scheduled` ranks AFTER standard, so it is not on the board although
//! it is "not standard": a packet scheduled for later does not jump
//! the queue, and a board that said it did would be the queue
//! disagreeing with itself.
//!
//! Pure: the handler reads the open packets of those priorities (real
//! partition only — a simulated emergency is the sim's, not the
//! operator's) with their steps, and the stations they stand at come
//! from the same pass the marshalling region is judged on.

use boss_core::job::{Job, Priority, Step, StepStatus};
use serde::{Deserialize, Serialize};

use crate::regions::StationReading;

/// Every priority, in the station discipline's order. The rank is an
/// exhaustive match, so a new variant stops the build there; this list
/// is held to it by a test.
const PRIORITIES: [Priority; 4] = [
    Priority::Emergency,
    Priority::Urgent,
    Priority::Standard,
    Priority::Scheduled,
];

/// Whether a packet of this priority drains before regular order.
pub fn outranks_regular_order(p: Priority) -> bool {
    crate::station_queue::priority_rank(p) < crate::station_queue::priority_rank(Priority::Standard)
}

/// The priorities the board lists — the ones the handler narrows its
/// read to, one query each.
pub fn outranking_priorities() -> Vec<Priority> {
    PRIORITIES
        .into_iter()
        .filter(|p| outranks_regular_order(*p))
        .collect()
}

/// Where a packet stands: its first ready-or-active step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct At {
    /// The step's spec slug, where the step has one.
    pub slug: Option<String>,
    pub title: String,
    pub status: StepStatus,
    /// Who holds it, when anyone does.
    pub assignee_id: Option<String>,
    /// WHETHER ANYONE IS ON IT (backlog 8e9d3723): for an ACTIVE step an
    /// agent holds, the open `agent-run` working it — or that none is.
    /// `None` for a ready step (nothing has claimed it) and for a step a
    /// human holds (a human's claim has no run to read).
    #[serde(default)]
    pub run: Option<StepRun>,
    /// WHETHER ANYTHING CAN DISPATCH IT (backlog e2519eca): for a READY
    /// step, [`Dispatch::Undispatchable`] when nothing can hand it to
    /// anyone. `None` for an active step (whose question is its run),
    /// for a step that carries an agent block, and for one a person
    /// holds.
    #[serde(default)]
    pub dispatch: Option<Dispatch>,
}

/// A READY STEP NOTHING CAN DISPATCH (backlog e2519eca, proposal (3) of
/// 8e9d3723). Measured 2026-10-09: ff309ce6 stood at `prove-delivery`
/// READY, held by agent-claude, and `boss dispatch` refused it because
/// that step of backlog-item declares no agent block — so no session
/// could hand it to an agent, no person held it, and the board printed
/// it as `at prove-delivery (ready, agent-claude)`, the same words as a
/// step a dispatch would pick up. The same evening f9d0a9f6 stood in
/// that shape and 62b2a81a at user-feedback's `triage`, which declares
/// no block either.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Dispatch {
    /// The step carries no agent block — neither its own nor its kind's
    /// ACTIVE row's ([`crate::agent_spec::resolved`], which the handler
    /// applies) — so `boss dispatch` refuses it, and no person holds it.
    /// `kind` and `version` name the row the packet runs: the fix is a
    /// block on that step in its kind's row, or a person on the step.
    Undispatchable { kind: String, version: i32 },
    /// It carries no block and is held, but the agents registry was not
    /// read, so whether a person or an agent holds it is not known —
    /// never a guess either way.
    Unread,
}

/// THE RUN ON A CLAIMED STEP (backlog 8e9d3723). Measured 2026-10-09:
/// five urgent items sat at `build` ACTIVE, claimed by agent-claude, for
/// up to two weeks with no run working any of them — each claim had
/// outlived the session that made it — and the board printed them as
/// `at build (active, agent-claude)`, the same words as an item a
/// builder was on that minute, so every drain session read them as in
/// hand. The agent-run packet already says whether a run is alive; this
/// carries that back to the step it claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum StepRun {
    /// An open run names this packet and step and its `building` step
    /// has started.
    Building { run: String },
    /// An open run names it, its `building` step has not started, and
    /// the run is still inside the bound its protocol declares for the
    /// worker's own start receipt (backlog 57326714): `boss dispatch`
    /// files the run with `building` READY, and only the worker's
    /// `--started`, sent after it reads the whole brief, makes it active.
    /// Not stalled — the protocol has not yet asked for that receipt.
    /// `expect_minutes` is the bound, read off the `building` step.
    Starting {
        run: String,
        age_minutes: i64,
        expect_minutes: i64,
    },
    /// An open run names it, and its `building` step never started:
    /// dispatched, never launched (agent-run f58a081d, open 37h with
    /// nothing after its brief) — past its declared start bound, or
    /// declaring none. `age_minutes` is the run's own age at the
    /// server's `now`.
    NeverStarted { run: String, age_minutes: i64 },
    /// No open run names it: the claim outlived whatever made it. A run
    /// is matched on `metadata.packet` and `metadata.step`, which `boss
    /// dispatch` writes; a hand-filed run spelling neither (28ab195a
    /// carries `review.car`) is no run here.
    NoRun,
    /// The runs, or which holders are agents, were not read for this
    /// caller — never a guess either way.
    Unread,
}

impl StepRun {
    /// Whether the claim is stalled: held, and nobody working it.
    pub fn stalled(&self) -> bool {
        matches!(self, StepRun::NoRun | StepRun::NeverStarted { .. })
    }
}

/// What the board reads to say whether a claimed step is worked.
#[derive(Debug, Clone, Copy)]
pub struct Runs<'a> {
    /// The `agent-run` packets, each OPEN one with its steps — every
    /// one, never a page of them: a run missing from a cut page would
    /// read as a claim with no run. Closed runs may ride along and are
    /// passed over.
    pub open: &'a [(Job, Vec<Step>)],
}

/// Every id an agent acts as: the agents registry's ids and their
/// aliases. A holder outside it is a human, whose claim has no run and
/// who can take a step no agent can be dispatched to. Read apart from
/// the runs (backlog e2519eca): it is a registry, not a page of
/// packets, so a cut or narrowed run read does not unread it.
pub type Agents = std::collections::BTreeSet<String>;

/// One packet that outranks regular order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outrank {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub priority: Priority,
    /// The admission instant: the `opened_at` column, else the
    /// `opened_at` metadata stamp, else `opened_on` at midnight —
    /// coarser, and still inside the right day.
    pub opened_at: chrono::DateTime<chrono::Utc>,
    /// Whole minutes from `opened_at` to the server's `now`, so every
    /// reader states one age rather than each clock its own.
    pub age_minutes: i64,
    /// Its first ready-or-active step, or `None` when no step is
    /// workable — a packet between steps says so rather than naming a
    /// step it is not at.
    pub at: Option<At>,
    /// The marshalling stations it stands at, by registry name.
    /// `None` when the stations could not be read — never an empty
    /// list, which would say it stands nowhere.
    pub stations: Option<Vec<String>>,
}

/// The board's row: every OPEN packet in `packets` whose priority
/// outranks regular order, oldest first (ties by id, so two reads of
/// one yard answer in one order). A packet of another priority or
/// status is dropped here too, so the answer never rests on the
/// handler's query alone. `runs` is `None` when the runs could not be
/// read whole for this caller, and `agents` when the agents registry
/// could not be read; every active step then says [`StepRun::Unread`],
/// and a held ready step with no block says [`Dispatch::Unread`] when
/// `agents` is the one missing. The steps' agent blocks are read as
/// handed in, so the handler resolves them against each kind's active
/// row first.
pub fn outranks(
    packets: &[(Job, Vec<Step>)],
    stations: Option<&[StationReading]>,
    runs: Option<Runs<'_>>,
    agents: Option<&Agents>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<Outrank> {
    let mut board: Vec<Outrank> = packets
        .iter()
        .filter(|(job, _)| {
            job.status == boss_core::job::JobStatus::Open && outranks_regular_order(job.priority)
        })
        .map(|(job, steps)| {
            let opened_at = admitted_at(job);
            let id = job.id.to_string();
            Outrank {
                stations: stations.map(|all| {
                    all.iter()
                        .filter(|s| s.members.contains(&id))
                        .map(|s| s.name.clone())
                        .collect()
                }),
                id,
                kind: job.kind.clone(),
                title: job.title.clone(),
                priority: job.priority,
                opened_at,
                age_minutes: (now - opened_at).num_minutes(),
                at: at(steps).map(|(a, step)| At {
                    run: claim_run(&job.id.to_string(), step, runs, agents, now),
                    dispatch: dispatch(job, step, agents),
                    ..a
                }),
            }
        })
        .collect();
    board.sort_by(|a, b| a.opened_at.cmp(&b.opened_at).then_with(|| a.id.cmp(&b.id)));
    board
}

/// The admission instant, from the finest source the row carries.
fn admitted_at(job: &Job) -> chrono::DateTime<chrono::Utc> {
    job.opened_at
        .or_else(|| crate::regions::opened_at(job))
        .unwrap_or_else(|| {
            chrono::DateTime::from_naive_utc_and_offset(
                job.opened_on.and_time(chrono::NaiveTime::MIN),
                chrono::Utc,
            )
        })
}

/// The first ready-or-active step by the protocol's order, and the
/// step it was read from.
fn at(steps: &[Step]) -> Option<(At, &Step)> {
    steps
        .iter()
        .filter(|s| matches!(s.status, StepStatus::Ready | StepStatus::Active))
        .min_by_key(|s| s.sort_order)
        .map(|s| {
            let at = At {
                slug: s.spec_slug.clone(),
                title: s.title.clone(),
                status: s.status,
                assignee_id: s.assignee_id.clone(),
                run: None,
                dispatch: None,
            };
            (at, s)
        })
}

/// Whether anything can dispatch a READY step: an agent block on it
/// says `boss dispatch` can, and a person holding it says a person
/// will. With neither, nothing will — whether nobody holds it or an
/// agent does, since an agent reaches a step without a block only by a
/// session's hand.
fn dispatch(job: &Job, step: &Step, agents: Option<&Agents>) -> Option<Dispatch> {
    if step.status != StepStatus::Ready || crate::agent_spec::projected(&step.metadata).is_some() {
        return None;
    }
    let undispatchable = Dispatch::Undispatchable {
        kind: job.kind.clone(),
        version: job.workflow_version,
    };
    match (step.assignee_id.as_deref(), agents) {
        (None, _) => Some(undispatchable),
        (Some(_), None) => Some(Dispatch::Unread),
        (Some(who), Some(agents)) if agents.contains(who) => Some(undispatchable),
        // A person holds it.
        (Some(_), Some(_)) => None,
    }
}

/// The run on a claimed step of `packet`: judged only for an ACTIVE
/// step an agent holds — a ready step has no claim, and a human's has
/// no run.
///
/// THE ONE PREDICATE (backlog e2519eca item 2). The top board's
/// OUTRANKS row reads it, and so does the caller's own queue —
/// `assignments?for=me`, which `boss orient` prints as NEEDS YOU and MY
/// WORK — so a claim reads STALLED in every section or in none. Until
/// then the queue carried no run, and a claim the board called stalled
/// read as in hand one section down.
pub fn claim_run(
    packet: &str,
    step: &Step,
    runs: Option<Runs<'_>>,
    agents: Option<&Agents>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<StepRun> {
    if step.status != StepStatus::Active {
        return None;
    }
    let (Some(runs), Some(agents)) = (runs, agents) else {
        return Some(StepRun::Unread);
    };
    if !step
        .assignee_id
        .as_deref()
        .is_some_and(|who| agents.contains(who))
    {
        return None;
    }
    // A step with no spec slug cannot be dispatched, so no run names it.
    let slug = step.spec_slug.as_deref();
    let md = |j: &Job, k: &str| {
        j.metadata
            .get(k)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    // Every open run naming this packet and step. One building outranks
    // one starting, which outranks one never started; within a reading,
    // newest first, so the run reported is the latest attempt.
    let rank = |r: &StepRun| match r {
        StepRun::Building { .. } => 0,
        StepRun::Starting { .. } => 1,
        _ => 2,
    };
    let mut on: Vec<(&Job, StepRun)> = runs
        .open
        .iter()
        .filter(|(j, _)| {
            j.status == boss_core::job::JobStatus::Open
                && md(j, "packet").as_deref() == Some(packet)
                && slug.is_some()
                && md(j, "step").as_deref() == slug
        })
        .map(|(j, steps)| (j, reading(j, steps, now)))
        .collect();
    on.sort_by_key(|(j, r)| (rank(r), std::cmp::Reverse(admitted_at(j))));
    Some(on.into_iter().next().map_or(StepRun::NoRun, |(_, r)| r))
}

/// How far one run has got. Building once its `building` step was
/// claimed (active) or has finished. Otherwise it was briefed and not
/// yet launched: STARTING while its age is inside the start bound the
/// step declares, and never started past it or with no bound declared
/// (backlog 57326714). The bound is the one the receipt-overdue rule
/// holds the worker to, so the board turns STALLED when that alarm can
/// first fire, not fifteen minutes before it.
fn reading(run: &Job, steps: &[Step], now: chrono::DateTime<chrono::Utc>) -> StepRun {
    let id = run.id.to_string();
    let building = || {
        steps
            .iter()
            .filter(|s| s.spec_slug.as_deref() == Some("building"))
    };
    if building().any(|s| matches!(s.status, StepStatus::Active | StepStatus::Completed)) {
        return StepRun::Building { run: id };
    }
    let age = now - admitted_at(run);
    match building().find_map(start_bound) {
        Some(expect_minutes)
            if expect_minutes
                .checked_mul(60)
                .is_some_and(|bound| age.num_seconds() <= bound) =>
        {
            StepRun::Starting {
                run: id,
                age_minutes: age.num_minutes(),
                expect_minutes,
            }
        }
        _ => StepRun::NeverStarted {
            run: id,
            age_minutes: age.num_minutes(),
        },
    }
}

/// The minutes a run's worker has to send its start receipt, off the
/// `building` step's [`crate::agent_budget::WORKER_START_EXPECTATION`]
/// (`{ schema = 1, minutes = N }`). `None` for a legacy run that
/// declares none, and for a declaration this reader cannot read — which
/// then reads never started, the louder of the two answers.
fn start_bound(building: &Step) -> Option<i64> {
    let bound = building
        .metadata
        .get(crate::agent_budget::WORKER_START_EXPECTATION)?;
    if !matches!(
        bound.get("schema").and_then(serde_json::Value::as_u64),
        Some(1 | 2)
    ) {
        return None;
    }
    if bound["schema"] == 2
        && !crate::agent_budget::observer_start_expectation_valid(
            bound,
            building.assignee_id.as_deref().unwrap_or(""),
        )
    {
        return None;
    }
    bound
        .get("minutes")
        .and_then(serde_json::Value::as_i64)
        .filter(|m| *m > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{JobId, JobStatus, StepId, Subject};
    use serde_json::json;

    fn t(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    const NOW: &str = "2026-09-27T12:00:00Z";

    fn job(title: &str, priority: Priority, opened_at: Option<&str>) -> Job {
        Job {
            id: JobId::new(),
            kind: "backlog-item".into(),
            workflow_version: 1,
            subject: Subject::new("custom", "s"),
            title: title.into(),
            owner_id: "emp-david".into(),
            status: JobStatus::Open,
            priority,
            opened_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
            opened_at: opened_at.map(t),
            due_on: None,
            closed_on: None,
            metadata: json!({}),
            tags: vec![],
            partition: boss_core::partition::Partition::Real,
        }
    }

    fn step(job: &Job, slug: &str, order: i32, status: StepStatus) -> Step {
        let mut s = Step::new(job.id, "task", slug, order);
        s.id = StepId::new();
        s.spec_slug = Some(slug.into());
        s.title = format!("Do {slug}");
        s.status = status;
        s
    }

    #[test]
    fn emergency_and_urgent_outrank_regular_order_and_scheduled_does_not() {
        assert_eq!(
            outranking_priorities(),
            vec![Priority::Emergency, Priority::Urgent]
        );
        assert!(!outranks_regular_order(Priority::Standard));
        assert!(!outranks_regular_order(Priority::Scheduled));
    }

    /// The variant list is held to the rank's exhaustive match: every
    /// variant appears once, in rank order.
    #[test]
    fn the_priority_list_is_every_variant_in_rank_order() {
        let ranks: Vec<u8> = PRIORITIES
            .iter()
            .map(|p| crate::station_queue::priority_rank(*p))
            .collect();
        assert_eq!(ranks, vec![0, 1, 2, 3]);
    }

    #[test]
    fn the_board_lists_only_what_outranks_oldest_first_with_where_it_stands() {
        let old = job(
            "the old urgent",
            Priority::Urgent,
            Some("2026-09-25T09:00:00Z"),
        );
        let fire = job(
            "the fire",
            Priority::Emergency,
            Some("2026-09-27T11:00:00Z"),
        );
        let routine = job("routine", Priority::Standard, Some("2026-09-20T09:00:00Z"));
        let later = job("later", Priority::Scheduled, Some("2026-09-20T09:00:00Z"));
        let mut closed = job("done", Priority::Urgent, Some("2026-09-19T09:00:00Z"));
        closed.status = JobStatus::Closed;
        let mut held = step(&old, "build", 2, StepStatus::Active);
        held.assignee_id = Some("agent-claude".into());
        let packets = vec![
            (
                fire.clone(),
                vec![step(&fire, "triage", 1, StepStatus::Ready)],
            ),
            (routine.clone(), vec![]),
            (later.clone(), vec![]),
            (closed.clone(), vec![]),
            (
                old.clone(),
                vec![
                    step(&old, "filed", 0, StepStatus::Completed),
                    held,
                    step(&old, "proven", 3, StepStatus::Pending),
                ],
            ),
        ];
        let stations = vec![StationReading {
            name: "backlog".into(),
            members: vec![old.id.to_string(), routine.id.to_string()],
            ..Default::default()
        }];
        let board = outranks(&packets, Some(&stations), None, None, t(NOW));

        let titles: Vec<&str> = board.iter().map(|o| o.title.as_str()).collect();
        assert_eq!(titles, ["the old urgent", "the fire"], "{board:?}");

        let o = &board[0];
        assert_eq!(o.kind, "backlog-item");
        assert_eq!(o.priority, Priority::Urgent);
        assert_eq!(o.age_minutes, 2 * 24 * 60 + 3 * 60);
        let at = o.at.as_ref().expect("a workable step");
        assert_eq!(at.slug.as_deref(), Some("build"));
        assert_eq!(at.title, "Do build");
        assert_eq!(at.status, StepStatus::Active);
        assert_eq!(at.assignee_id.as_deref(), Some("agent-claude"));
        assert_eq!(o.stations.as_deref(), Some(&["backlog".to_string()][..]));

        assert_eq!(board[1].age_minutes, 60);
        assert_eq!(board[1].stations.as_deref(), Some(&[][..]));
    }

    /// A packet with no workable step says so, and stations nobody
    /// could read are unread — never "stands nowhere".
    #[test]
    fn no_workable_step_is_none_and_unread_stations_are_null() {
        let j = job("waiting", Priority::Urgent, None);
        let board = outranks(
            &[(j.clone(), vec![step(&j, "later", 0, StepStatus::Pending)])],
            None,
            None,
            None,
            t(NOW),
        );
        assert_eq!(board.len(), 1);
        assert_eq!(board[0].at, None);
        assert_eq!(board[0].stations, None);
        // No instant on the row: the admission day at midnight.
        assert_eq!(board[0].opened_at, t("2026-09-25T00:00:00Z"));
        let wire = serde_json::to_value(&board[0]).unwrap();
        assert_eq!(wire["stations"], serde_json::Value::Null);
        assert_eq!(wire["priority"], json!("urgent"));
    }

    /// With nothing outranking, the board is an empty list — the
    /// answer, read — which a reader says in words.
    #[test]
    fn nothing_outranking_is_an_empty_list() {
        let j = job("routine", Priority::Standard, None);
        assert!(outranks(&[(j, vec![])], Some(&[]), None, None, t(NOW)).is_empty());
    }

    /// An open agent-run naming `packet` and `slug`, opened at `opened`,
    /// whose `building` step stands at `building`.
    fn run(packet: &Job, slug: &str, opened: &str, building: StepStatus) -> (Job, Vec<Step>) {
        let mut r = job("builder run", Priority::Standard, Some(opened));
        r.kind = crate::agent_budget::RUN_KIND.into();
        r.metadata = json!({ "packet": packet.id.to_string(), "step": slug });
        let steps = vec![
            step(&r, "briefed", 1, StepStatus::Completed),
            step(&r, "building", 2, building),
            step(&r, "reported", 3, StepStatus::Pending),
        ];
        (r, steps)
    }

    /// An urgent item at `slug`, held by `who`, at `status`.
    fn held(title: &str, slug: &str, status: StepStatus, who: &str) -> (Job, Vec<Step>) {
        let j = job(title, Priority::Urgent, Some("2026-09-25T09:00:00Z"));
        let mut s = step(&j, slug, 1, status);
        s.assignee_id = Some(who.into());
        (j.clone(), vec![s])
    }

    /// THE CLAIM SAYS WHETHER ANYONE IS ON IT (backlog 8e9d3723): the
    /// five urgent items measured 2026-10-09 at `build` ACTIVE, held by
    /// agent-claude with no run, printed exactly like the one a builder
    /// was on. Each shape the item names, one packet each: a run
    /// building it; a run briefed and never launched (f58a081d's shape:
    /// `building` still pending, 37h on); a claim with no run at all; a
    /// claim whose only run is for ANOTHER step of the same packet; a
    /// human's claim, which has no run to read; and a ready step, which
    /// nothing has claimed. An agent's alias counts as the agent.
    #[test]
    fn an_active_step_an_agent_holds_names_its_run_or_says_it_has_none() {
        let building = held("building", "build", StepStatus::Active, "agent-claude");
        let briefed = held("briefed", "build", StepStatus::Active, "agent-claude");
        let gone = held("gone", "build", StepStatus::Active, "claude@algedonic.dev");
        let elsewhere = held("elsewhere", "build", StepStatus::Active, "agent-codex");
        let human = held("human", "build", StepStatus::Active, "emp-david");
        let ready = held("ready", "triage", StepStatus::Ready, "agent-claude");
        let packets = vec![
            building.clone(),
            briefed.clone(),
            gone.clone(),
            elsewhere.clone(),
            human.clone(),
            ready.clone(),
        ];
        let live = run(
            &building.0,
            "build",
            "2026-09-27T11:00:00Z",
            StepStatus::Active,
        );
        // An OLDER run on the same step that never started does not hide
        // the one building it.
        let stale = run(
            &building.0,
            "build",
            "2026-09-26T11:00:00Z",
            StepStatus::Pending,
        );
        let never = run(
            &briefed.0,
            "build",
            "2026-09-25T23:00:00Z",
            StepStatus::Pending,
        );
        let other_step = run(
            &elsewhere.0,
            "triage",
            "2026-09-27T10:00:00Z",
            StepStatus::Active,
        );
        let mut closed = run(&gone.0, "build", "2026-09-26T10:00:00Z", StepStatus::Active);
        closed.0.status = JobStatus::Closed;
        let open = vec![live.clone(), stale, never.clone(), other_step, closed];
        let agents: std::collections::BTreeSet<String> =
            ["agent-claude", "claude@algedonic.dev", "agent-codex"]
                .into_iter()
                .map(String::from)
                .collect();
        let board = outranks(
            &packets,
            Some(&[]),
            Some(Runs { open: &open }),
            Some(&agents),
            t(NOW),
        );
        let run_of = |title: &str| {
            board
                .iter()
                .find(|o| o.title == title)
                .and_then(|o| o.at.as_ref())
                .expect("a workable step")
                .run
                .clone()
        };
        assert_eq!(
            run_of("building"),
            Some(StepRun::Building {
                run: live.0.id.to_string()
            })
        );
        assert_eq!(
            run_of("briefed"),
            Some(StepRun::NeverStarted {
                run: never.0.id.to_string(),
                age_minutes: 37 * 60,
            })
        );
        assert_eq!(run_of("gone"), Some(StepRun::NoRun));
        assert_eq!(run_of("elsewhere"), Some(StepRun::NoRun));
        assert_eq!(run_of("human"), None);
        assert_eq!(run_of("ready"), None);
        assert!(run_of("gone").is_some_and(|r| r.stalled()));
        assert!(run_of("briefed").is_some_and(|r| r.stalled()));
        assert!(!run_of("building").is_some_and(|r| r.stalled()));

        // The wire every reader parses: a tagged state, kebab-case.
        let wire = |title: &str| {
            serde_json::to_value(board.iter().find(|o| o.title == title).unwrap()).unwrap()
        };
        assert_eq!(wire("gone")["at"]["run"], json!({ "state": "no-run" }));
        assert_eq!(
            wire("briefed")["at"]["run"],
            json!({ "state": "never-started", "run": never.0.id.to_string(), "age_minutes": 37 * 60 })
        );
        assert_eq!(wire("human")["at"]["run"], serde_json::Value::Null);
    }

    /// A RUN STILL INSIDE ITS OWN START BOUND IS STARTING, NOT STALLED
    /// (backlog 57326714, review c58e1892 F1). Since agent-run.toml
    /// declared `worker_start_expectation = { minutes = 15 }` on
    /// `building` (train #956), `boss dispatch` files the run with
    /// `building` READY and only the worker's `--started` makes it
    /// active — 32 s after filing on the run the review measured, with a
    /// 180 KB brief to read first. The board read every such run
    /// `never-started` from its first second. Each shape, one packet
    /// each: five minutes into a 15-minute bound; sixteen minutes in,
    /// past it; a legacy run that declares no bound, one minute in; and
    /// an older run past its bound beside a newer one inside it, where
    /// the newer is the one named.
    #[test]
    fn a_run_inside_its_declared_start_bound_is_starting_and_past_it_is_stalled() {
        let bound = json!({ (crate::agent_budget::WORKER_START_EXPECTATION): { "schema": 1, "minutes": 15 } });
        let declared = |packet: &Job, opened: &str| {
            let mut r = run(packet, "build", opened, StepStatus::Ready);
            for s in
                r.1.iter_mut()
                    .filter(|s| s.spec_slug.as_deref() == Some("building"))
            {
                s.metadata = bound.clone();
            }
            r
        };
        let fresh = held("fresh", "build", StepStatus::Active, "agent-claude");
        let late = held("late", "build", StepStatus::Active, "agent-claude");
        let legacy = held("legacy", "build", StepStatus::Active, "agent-claude");
        let retried = held("retried", "build", StepStatus::Active, "agent-claude");
        let five = declared(&fresh.0, "2026-09-27T11:55:00Z");
        let sixteen = declared(&late.0, "2026-09-27T11:44:00Z");
        let old_style = run(
            &legacy.0,
            "build",
            "2026-09-27T11:59:00Z",
            StepStatus::Ready,
        );
        let gave_up = declared(&retried.0, "2026-09-27T10:00:00Z");
        let again = declared(&retried.0, "2026-09-27T11:58:00Z");
        let open = vec![
            five.clone(),
            sixteen.clone(),
            old_style.clone(),
            gave_up,
            again.clone(),
        ];
        let agents: std::collections::BTreeSet<String> =
            std::iter::once("agent-claude".to_string()).collect();
        let board = outranks(
            &[fresh, late, legacy, retried],
            Some(&[]),
            Some(Runs { open: &open }),
            Some(&agents),
            t(NOW),
        );
        let run_of = |title: &str| {
            board
                .iter()
                .find(|o| o.title == title)
                .and_then(|o| o.at.as_ref())
                .and_then(|a| a.run.clone())
                .expect("a judged run")
        };
        assert_eq!(
            run_of("fresh"),
            StepRun::Starting {
                run: five.0.id.to_string(),
                age_minutes: 5,
                expect_minutes: 15,
            }
        );
        assert!(!run_of("fresh").stalled());
        assert_eq!(
            run_of("late"),
            StepRun::NeverStarted {
                run: sixteen.0.id.to_string(),
                age_minutes: 16,
            }
        );
        assert!(run_of("late").stalled());
        assert_eq!(
            run_of("legacy"),
            StepRun::NeverStarted {
                run: old_style.0.id.to_string(),
                age_minutes: 1,
            }
        );
        assert_eq!(
            run_of("retried"),
            StepRun::Starting {
                run: again.0.id.to_string(),
                age_minutes: 2,
                expect_minutes: 15,
            }
        );
        let wire = serde_json::to_value(run_of("fresh")).unwrap();
        assert_eq!(
            wire,
            json!({ "state": "starting", "run": five.0.id.to_string(), "age_minutes": 5, "expect_minutes": 15 })
        );
    }

    /// Runs nobody could read are unread on every active step — never
    /// "no run", which is a claim.
    #[test]
    fn unread_runs_are_unread_on_an_active_step_and_absent_on_a_ready_one() {
        let active = held("active", "build", StepStatus::Active, "agent-claude");
        let ready = held("ready", "triage", StepStatus::Ready, "agent-claude");
        let board = outranks(&[active, ready], Some(&[]), None, None, t(NOW));
        let run_of = |title: &str| {
            board
                .iter()
                .find(|o| o.title == title)
                .and_then(|o| o.at.as_ref())
                .and_then(|a| a.run.clone())
        };
        assert_eq!(run_of("active"), Some(StepRun::Unread));
        assert_eq!(run_of("ready"), None);
    }

    /// A READY STEP NOTHING CAN DISPATCH SAYS SO (backlog e2519eca,
    /// proposal (3) of 8e9d3723). ff309ce6's shape, read off the packet:
    /// backlog-item's `prove-delivery` READY, held by agent-claude, with
    /// no agent block — `boss dispatch` refuses it and no person holds
    /// it. One packet per shape: that one; the same step held by nobody;
    /// held by a person, who will take it; a ready step that carries a
    /// block, which a dispatch picks up; and an ACTIVE step with no
    /// block, whose question is its run, not its dispatch.
    #[test]
    fn a_ready_step_with_no_agent_block_and_no_person_on_it_is_undispatchable() {
        let agent_held = held("agent", "prove-delivery", StepStatus::Ready, "agent-claude");
        let mut nobody = held("nobody", "prove-delivery", StepStatus::Ready, "x");
        nobody.1[0].assignee_id = None;
        let person = held("person", "prove-delivery", StepStatus::Ready, "emp-david");
        let mut funded = held("funded", "build", StepStatus::Ready, "agent-claude");
        funded.1[0].metadata = json!({
            "agent_profile": "builder", "agent_model": "opus-5[1m]",
            "agent_budget_usd": 5.0, "agent_effort": "high",
        });
        let active = held("active", "build", StepStatus::Active, "agent-claude");
        let packets = vec![agent_held, nobody, person, funded, active];
        let agents: Agents = ["agent-claude".to_string()].into_iter().collect();

        let dispatch_of = |board: &[Outrank], title: &str| {
            board
                .iter()
                .find(|o| o.title == title)
                .and_then(|o| o.at.as_ref())
                .expect("a workable step")
                .dispatch
                .clone()
        };
        let row = Some(Dispatch::Undispatchable {
            kind: "backlog-item".into(),
            version: 1,
        });
        let board = outranks(&packets, Some(&[]), None, Some(&agents), t(NOW));
        assert_eq!(dispatch_of(&board, "agent"), row);
        assert_eq!(dispatch_of(&board, "nobody"), row);
        assert_eq!(dispatch_of(&board, "person"), None);
        assert_eq!(dispatch_of(&board, "funded"), None);
        assert_eq!(dispatch_of(&board, "active"), None);

        // The wire every reader parses, naming the row to fix.
        let wire =
            serde_json::to_value(board.iter().find(|o| o.title == "agent").unwrap()).unwrap();
        assert_eq!(
            wire["at"]["dispatch"],
            json!({ "state": "undispatchable", "kind": "backlog-item", "version": 1 })
        );

        // With no agents registry, a holder is neither person nor agent:
        // unread, never a guess. Nobody holding it needs no registry.
        let unread = outranks(&packets, Some(&[]), None, None, t(NOW));
        assert_eq!(dispatch_of(&unread, "agent"), Some(Dispatch::Unread));
        assert_eq!(dispatch_of(&unread, "person"), Some(Dispatch::Unread));
        assert_eq!(dispatch_of(&unread, "nobody"), row);
        assert_eq!(dispatch_of(&unread, "funded"), None);
    }
}
