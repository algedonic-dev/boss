//! Agent spend read per protocol and step — car 1 of design 091e8034
//! (backlog 05bf580b).
//!
//! WHY IT EXISTS. After car e6b2066f every run is priced from what it
//! metered and carries the packet it worked (`job_id`) and the step it
//! was dispatched onto (`detail.step`), yet nothing grouped spend by
//! the protocol that packet runs under: `GET /api/agent-runs/cost`
//! answered by model and by branch only, and a deliberately unmatched
//! `workflow_kind` answered the same 812 runs (444 unpriced) as no
//! filter at all, because the parameter did not exist (measured live
//! 2026-11-13, design 091e8034's fold). The efficiency signal David
//! asked for — what a protocol costs to run, step by step — had no
//! reading.
//!
//! THE RULES IT KEEPS, each the existing roll-up's, not a new one:
//! - **Every bucket is a [`GroupSpend`] folded by the one `fold`**, so
//!   an unpriced run makes its protocol's and its step's figure
//!   unknown, with the counter that says why — never a smaller number
//!   that reads as whole.
//! - **Recorded prices, not today's rates.** A run's `usd_micros` is
//!   what the card said when it was recorded; nothing here reprices.
//! - **Nothing is lost to a join.** A run that names no packet, or a
//!   packet the record does not hold, is not dropped as an inner join
//!   would drop it: it lands in an explicit unattributed bucket, and a
//!   run on a known protocol that names no step lands in that
//!   protocol's `no_step` bucket. The buckets always partition the set
//!   they were taken over, which [`ProtocolReading::runs`] lets a
//!   reader check against the summary beside it.
//!
//! Helper attribution (car 2) and the 1-hour cache-write rate (car 3)
//! are the design's other two cars and are not here: every figure in
//! this reading is a run's DIRECT spend, as recorded.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::types::{AgentRun, GroupSpend, blank, fold};

/// The `detail` key a run's step rides under — what `boss dispatch`
/// writes from the step the run was dispatched onto.
pub const STEP_KEY: &str = "step";

/// The key of the bucket for runs that name no packet.
pub const NO_JOB: &str = "no-job";
/// The key of the bucket for runs naming a packet the record does not
/// hold — an id the jobs table cannot place.
pub const UNKNOWN_JOB: &str = "unknown-job";
/// The key of a protocol's bucket for runs that name no step.
pub const NO_STEP: &str = "no-step";

/// Spend on one protocol, whole and step by step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolSpend {
    /// The packet's workflow kind (`jobs.kind`).
    pub workflow_kind: String,
    /// Every run on a packet of this kind; `key` is the kind.
    pub spend: GroupSpend,
    /// One bucket per step slug a run named, slug-ordered.
    pub by_step: Vec<GroupSpend>,
    /// Runs on this protocol whose `detail` names no step. Always
    /// present, zero included, so an absent step reads as a number.
    pub no_step: GroupSpend,
}

/// The whole reading: every run of the set it was taken over, in
/// exactly one of the protocol buckets or one of the two unattributed
/// ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolReading {
    /// Kind-ordered.
    pub by_protocol: Vec<ProtocolSpend>,
    /// Runs that name no packet at all.
    pub no_job: GroupSpend,
    /// Runs naming a packet the record does not hold.
    pub unknown_job: GroupSpend,
}

impl ProtocolReading {
    /// How many runs the reading holds — every bucket's, summed, so a
    /// reader can check it partitions the set beside it.
    pub fn runs(&self) -> u64 {
        self.by_protocol.iter().map(|p| p.spend.runs).fold(
            self.no_job.runs + self.unknown_job.runs,
            u64::saturating_add,
        )
    }
}

/// The step a run names, if any: `detail.step` as a non-blank string.
pub fn step_of(run: &AgentRun) -> Option<&str> {
    run.run
        .detail
        .get(STEP_KEY)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Group `runs` by the kind of the packet each worked and by the step
/// it names. `kinds` is `jobs.kind` for every packet id the record
/// holds ([`super::port::AgentRunLog::job_kinds`]); an id it does not
/// hold is [`UNKNOWN_JOB`], not an error. A pure function of the rows.
pub fn summarize_by_protocol(runs: &[AgentRun], kinds: &HashMap<Uuid, String>) -> ProtocolReading {
    // kind -> (the protocol's bucket, its step buckets, its no-step one)
    let mut protocols: BTreeMap<&str, (GroupSpend, BTreeMap<&str, GroupSpend>, GroupSpend)> =
        BTreeMap::new();
    let mut no_job = blank(NO_JOB);
    let mut unknown_job = blank(UNKNOWN_JOB);

    for run in runs {
        let Some(job) = run.run.job_id else {
            fold(&mut no_job, run);
            continue;
        };
        let Some(kind) = kinds.get(&job) else {
            fold(&mut unknown_job, run);
            continue;
        };
        let (spend, steps, no_step) = protocols
            .entry(kind.as_str())
            .or_insert_with(|| (blank(kind), BTreeMap::new(), blank(NO_STEP)));
        fold(spend, run);
        match step_of(run) {
            Some(slug) => fold(steps.entry(slug).or_insert_with(|| blank(slug)), run),
            None => fold(no_step, run),
        }
    }

    ProtocolReading {
        by_protocol: protocols
            .into_iter()
            .map(|(kind, (spend, steps, no_step))| ProtocolSpend {
                workflow_kind: kind.to_string(),
                spend,
                by_step: steps.into_values().collect(),
                no_step,
            })
            .collect(),
        no_job,
        unknown_job,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runs::types::{NewAgentRun, RunOutcome, TokenUsage, summarize};
    use boss_core::actor::ActorId;
    use chrono::{DateTime, Duration, Utc};

    const BACKLOG: Uuid = Uuid::from_u128(0x05bf_580b_0000_4000_8000_0000_0000_0001);
    const DESIGN: Uuid = Uuid::from_u128(0x05bf_580b_0000_4000_8000_0000_0000_0002);
    const GONE: Uuid = Uuid::from_u128(0x05bf_580b_0000_4000_8000_0000_0000_0003);

    fn kinds() -> HashMap<Uuid, String> {
        HashMap::from([
            (BACKLOG, "backlog-item".to_string()),
            (DESIGN, "design-doc".to_string()),
        ])
    }

    fn at() -> DateTime<Utc> {
        "2026-11-15T03:00:00Z".parse().expect("fixture instant")
    }

    /// A recorded run on `job` at `step`, priced at `usd` micros
    /// (`None`: unpriced), ten minutes long.
    fn run(id: &str, job: Option<Uuid>, step: Option<&str>, usd: Option<u64>) -> AgentRun {
        AgentRun {
            run: NewAgentRun {
                run_id: id.into(),
                actor_id: ActorId::RegisteredAgent("agent-claude".into()),
                model: Some("opus-5-5".into()),
                started_at: at() - Duration::minutes(10),
                finished_at: at(),
                outcome: RunOutcome::Success,
                error: None,
                tokens: TokenUsage::Metered {
                    input: 10,
                    cache_write: 100,
                    cache_read: 1_000,
                    output: 50,
                },
                tool_calls: 4,
                job_id: job,
                branch: None,
                detail: match step {
                    Some(s) => serde_json::json!({ "step": s }),
                    None => serde_json::json!({}),
                },
            },
            usd_micros: usd,
            priced_by: usd.map(|_| "opus-5-5".to_string()),
            budget: None,
            recorded_at: at(),
        }
    }

    fn protocol<'a>(reading: &'a ProtocolReading, kind: &str) -> &'a ProtocolSpend {
        reading
            .by_protocol
            .iter()
            .find(|p| p.workflow_kind == kind)
            .unwrap_or_else(|| panic!("no bucket for {kind}: {reading:#?}"))
    }

    fn step<'a>(p: &'a ProtocolSpend, slug: &str) -> &'a GroupSpend {
        p.by_step
            .iter()
            .find(|s| s.key == slug)
            .unwrap_or_else(|| panic!("no step {slug} on {}: {p:#?}", p.workflow_kind))
    }

    #[test]
    fn two_protocols_and_their_steps_are_read_apart() {
        let runs = [
            run("b1", Some(BACKLOG), Some("build"), Some(1_000)),
            run("b2", Some(BACKLOG), Some("build"), Some(2_000)),
            run("b3", Some(BACKLOG), Some("triage"), Some(300)),
            run("d1", Some(DESIGN), Some("review"), Some(40)),
        ];
        let reading = summarize_by_protocol(&runs, &kinds());

        let kinds: Vec<&str> = reading
            .by_protocol
            .iter()
            .map(|p| p.workflow_kind.as_str())
            .collect();
        assert_eq!(kinds, ["backlog-item", "design-doc"], "kind-ordered");

        let backlog = protocol(&reading, "backlog-item");
        assert_eq!(backlog.spend.key, "backlog-item");
        assert_eq!(backlog.spend.runs, 3);
        assert_eq!(backlog.spend.usd_micros, Some(3_300));
        let slugs: Vec<&str> = backlog.by_step.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(slugs, ["build", "triage"], "slug-ordered");
        assert_eq!(step(backlog, "build").runs, 2);
        assert_eq!(step(backlog, "build").usd_micros, Some(3_000));
        assert_eq!(step(backlog, "triage").usd_micros, Some(300));
        assert_eq!(backlog.no_step.runs, 0);

        let design = protocol(&reading, "design-doc");
        assert_eq!(design.spend.usd_micros, Some(40));
        assert_eq!(step(design, "review").runs, 1);
    }

    /// The inner-join failure, refused: a run with no packet, and a run
    /// naming a packet the record does not hold, are counted in buckets
    /// of their own, and a run naming no step is counted on its
    /// protocol. The reading still partitions the whole set.
    #[test]
    fn a_missing_link_is_an_explicit_bucket_never_a_dropped_run() {
        let runs = [
            run("b1", Some(BACKLOG), Some("build"), Some(1_000)),
            run("b-nostep", Some(BACKLOG), None, Some(7)),
            run("b-blank", Some(BACKLOG), Some("  "), Some(8)),
            run("loose", None, Some("build"), Some(5)),
            run("gone", Some(GONE), Some("build"), Some(9)),
        ];
        let reading = summarize_by_protocol(&runs, &kinds());

        let backlog = protocol(&reading, "backlog-item");
        assert_eq!(
            backlog.spend.runs, 3,
            "a stepless run is still the protocol's"
        );
        assert_eq!(backlog.no_step.key, NO_STEP);
        assert_eq!(backlog.no_step.runs, 2, "an absent and a blank step alike");
        assert_eq!(backlog.no_step.usd_micros, Some(15));
        assert_eq!(backlog.by_step.len(), 1, "no slug bucket for a blank step");

        assert_eq!(reading.no_job.key, NO_JOB);
        assert_eq!(reading.no_job.runs, 1);
        assert_eq!(reading.no_job.usd_micros, Some(5));
        assert_eq!(reading.unknown_job.key, UNKNOWN_JOB);
        assert_eq!(reading.unknown_job.runs, 1);
        assert_eq!(reading.unknown_job.usd_micros, Some(9));

        assert_eq!(
            reading.runs(),
            summarize(&runs).runs,
            "every run is in exactly one bucket"
        );
    }

    /// Unknown is not free, per bucket: one unpriced run makes its
    /// step's and its protocol's figure unknown and is counted, while a
    /// sibling step stays priced.
    #[test]
    fn an_unpriced_run_makes_its_own_buckets_unknown_and_no_others() {
        let runs = [
            run("b1", Some(BACKLOG), Some("build"), Some(1_000)),
            run("b2", Some(BACKLOG), Some("build"), None),
            run("b3", Some(BACKLOG), Some("triage"), Some(300)),
            run("d1", Some(DESIGN), Some("review"), Some(40)),
        ];
        let reading = summarize_by_protocol(&runs, &kinds());
        let backlog = protocol(&reading, "backlog-item");
        assert_eq!(backlog.spend.usd_micros, None);
        assert_eq!(backlog.spend.unpriced_runs, 1);
        assert_eq!(step(backlog, "build").usd_micros, None);
        assert_eq!(step(backlog, "build").unpriced_runs, 1);
        assert_eq!(step(backlog, "triage").usd_micros, Some(300));
        assert_eq!(protocol(&reading, "design-doc").spend.usd_micros, Some(40));
    }

    /// An empty set reads as zero runs in every bucket — a measured
    /// zero, with the two unattributed buckets present rather than
    /// absent.
    #[test]
    fn an_empty_set_is_an_empty_reading_with_both_unattributed_buckets() {
        let reading = summarize_by_protocol(&[], &kinds());
        assert!(reading.by_protocol.is_empty());
        assert_eq!(reading.runs(), 0);
        assert_eq!(reading.no_job.usd_micros, Some(0));
        assert_eq!(reading.unknown_job.usd_micros, Some(0));
        let wire = serde_json::to_value(&reading).expect("serializes");
        assert!(wire.get("no_job").is_some() && wire.get("unknown_job").is_some());
    }
}
