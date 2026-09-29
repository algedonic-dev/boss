//! Split admission — Tier 2 of the experiments program
//! (the network-experiments decision in docs/architecture-decisions.md, decided in packet `574c2adf`;
//! built for packet `6ea5a12a`).
//!
//! Q3 made the experiment a PACKET: kind [`EXPERIMENT_KIND`], carried
//! by the network's own machinery, concluded at its `promoted` /
//! `retired` terminals. Q1 made its arms two versions of ONE kind. So
//! there is no experiments table and no experiment registry — the
//! record is the packet, and the split declaration is the packet's
//! own job metadata:
//!
//! | key                 | meaning                                        |
//! |---------------------|------------------------------------------------|
//! | `kind_under_test`   | the workflow kind whose admission splits       |
//! | `control_version`   | version pinned to the control arm              |
//! | `candidate_version` | version pinned to the candidate arm (a draft,  |
//! |                     | until a promote publishes it)                  |
//! | `split`             | candidate share in percent, 0–100 (default 50) |
//!
//! The window is the packet's OPEN interval: admission consults open
//! experiments only, so opening the packet starts the split and
//! closing it (either terminal) stops it. Both facts are in the log
//! already — no window fields, no scheduler.
//!
//! The coin is the admitted packet's own id through a fixed FNV-1a
//! hash — the same replay-deterministic idiom as the dispatcher's
//! holder spread (`boss-dispatcher/src/dispatcher.rs`, its own copy;
//! the two hash different id spaces and owe each other no equality).
//! Determinism here is belt on top of braces: the arm choice is
//! stamped into job metadata BEFORE the `JOB_CREATED` event is built,
//! so the rebuilder replays the recorded choice and never re-tosses
//! the coin.
//!
//! Everything in this module is a pure function of data it is handed.
//! The admission handler (`http/jobs.rs`) owns the I/O: fetching open
//! experiments, resolving the arm's spec, and failing SAFE — a
//! malformed declaration or a version the registry cannot produce
//! leaves admission exactly as it stood (active version, no stamp),
//! because an experiment must never break the kind it measures.

use boss_core::job::{Job, JobId, JobStatus};

/// The workflow kind whose packets ARE experiment records (Q3).
pub const EXPERIMENT_KIND: &str = "protocol-experiment";

/// Job-metadata key stamped on every packet admitted under an arm.
pub const ARM_KEY: &str = "experiment_arm";

/// Job-metadata key naming WHICH experiment governed the admission.
pub const EXPERIMENT_ID_KEY: &str = "experiment_id";

/// The two arm stamps (Q1: version vs version — there is no third arm).
pub const ARM_CONTROL: &str = "control";
pub const ARM_CANDIDATE: &str = "candidate";

/// A well-formed split declaration read off an open experiment packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperimentSplit {
    /// The experiment packet's id — stamped so a packet's cohort
    /// membership names its experiment, not just its arm.
    pub experiment_id: JobId,
    pub control_version: i32,
    pub candidate_version: i32,
    /// Candidate share, percent 0..=100.
    pub split: u8,
}

impl ExperimentSplit {
    /// The workflow version an arm pins to.
    pub fn version_for(&self, arm: &str) -> i32 {
        if arm == ARM_CANDIDATE {
            self.candidate_version
        } else {
            self.control_version
        }
    }
}

/// The experiment governing admission for `kind`, if any: the OPEN
/// packet of [`EXPERIMENT_KIND`] whose metadata declares a well-formed
/// split over `kind`. When several declare the same kind (an operator
/// error the network still has to admit through), the choice is
/// deterministic — earliest `opened_on`, then smallest id — so every
/// replica and every replay agrees which experiment stamped a packet.
pub fn governing_experiment(experiments: &[Job], kind: &str) -> Option<ExperimentSplit> {
    experiments
        .iter()
        .filter(|j| j.kind == EXPERIMENT_KIND && j.status == JobStatus::Open)
        .filter(|j| {
            j.metadata
                .get("kind_under_test")
                .and_then(serde_json::Value::as_str)
                == Some(kind)
        })
        .filter_map(|j| declared_split(j).map(|s| (j, s)))
        .min_by(|(a, _), (b, _)| {
            a.opened_on
                .cmp(&b.opened_on)
                .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
        })
        .map(|(_, split)| split)
}

/// The split a packet puts IN FORCE, paired with the kind it splits:
/// its declaration read exactly as [`governing_experiment`] reads it,
/// and only while the packet is an OPEN experiment — the one state in
/// which admission consults it. `None` for any other packet.
pub fn split_in_force(job: &Job) -> Option<(String, ExperimentSplit)> {
    if job.kind != EXPERIMENT_KIND || job.status != JobStatus::Open {
        return None;
    }
    let kind = job.metadata.get("kind_under_test")?.as_str()?.to_string();
    Some((kind, declared_split(job)?))
}

/// What a write to a packet NEWLY puts in force: the split `after`
/// declares, when `before` (`None` for an admission) did not already
/// have exactly that split in force. `None` when the write leaves every
/// admission as it was — an annotation on a running experiment, a
/// close, or any write to a packet that splits nothing.
///
/// Backlog ce8b7d66: this is the question the job write doors ask
/// before an experiment may admit packets to a version that was never
/// published. A change of `split` counts — 0 to 100 turns a declared
/// but idle candidate into all of a kind's traffic — and so does
/// opening the packet, since the window IS the open interval.
pub fn newly_in_force(before: Option<&Job>, after: &Job) -> Option<(String, ExperimentSplit)> {
    let now = split_in_force(after)?;
    match before.and_then(split_in_force) {
        Some(was) if was == now => None,
        _ => Some(now),
    }
}

/// The arm a packet id lands in under a candidate share of `split`
/// percent. A pure, fixed function — the same id and split always
/// yield the same arm, on every host and build, which is what makes
/// the split replay-deterministic rather than a random draw.
pub fn arm_for(id: &JobId, split: u8) -> &'static str {
    if stable_hash(id.to_string().as_bytes()) % 100 < u64::from(split) {
        ARM_CANDIDATE
    } else {
        ARM_CONTROL
    }
}

/// Parse the split declaration off one experiment packet's metadata.
/// `None` means "this packet does not (validly) declare a split" —
/// the fail-safe answer, never an error: an experiment record without
/// admission machinery (most of them — gate concurrency, build width)
/// is a legitimate packet, not a defect.
fn declared_split(job: &Job) -> Option<ExperimentSplit> {
    Some(ExperimentSplit {
        experiment_id: job.id,
        control_version: version_field(&job.metadata, "control_version")?,
        candidate_version: version_field(&job.metadata, "candidate_version")?,
        split: split_field(&job.metadata)?,
    })
}

/// An i32 version out of metadata that operators author by hand:
/// accept a JSON number or a numeric string, refuse everything else.
fn version_field(meta: &serde_json::Value, key: &str) -> Option<i32> {
    match meta.get(key)? {
        serde_json::Value::Number(n) => n.as_i64().and_then(|v| i32::try_from(v).ok()),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// The candidate share. Absent means 50 (an even split is the default
/// experiment); present-but-malformed means `None`, which unmakes the
/// whole declaration — a split someone wrote and the machine cannot
/// read must not silently become a different split.
fn split_field(meta: &serde_json::Value) -> Option<u8> {
    match meta.get("split") {
        None | Some(serde_json::Value::Null) => Some(50),
        Some(serde_json::Value::Number(n)) => n
            .as_u64()
            .and_then(|v| u8::try_from(v).ok())
            .filter(|v| *v <= 100),
        Some(serde_json::Value::String(s)) => s.trim().parse::<u8>().ok().filter(|v| *v <= 100),
        Some(_) => None,
    }
}

/// FNV-1a (64-bit): fixed, dependency-free, identical on every host
/// and build — `DefaultHasher` is randomized per process and
/// explicitly unusable where determinism is the point.
fn stable_hash(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{Priority, Subject};
    use chrono::NaiveDate;

    fn experiment(metadata: serde_json::Value) -> Job {
        let mut j = Job::new(
            EXPERIMENT_KIND,
            Subject::new("custom", "proto-x"),
            "Experiment",
            "emp-ceo",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        );
        j.status = JobStatus::Open;
        j.metadata = metadata;
        j
    }

    fn full_declaration() -> serde_json::Value {
        serde_json::json!({
            "kind_under_test": "wholesale-keg-order",
            "control_version": 2,
            "candidate_version": 3,
            "split": 20,
        })
    }

    #[test]
    fn a_declared_open_experiment_governs_its_kind() {
        let e = experiment(full_declaration());
        let got = governing_experiment(std::slice::from_ref(&e), "wholesale-keg-order")
            .expect("declared and open");
        assert_eq!(got.experiment_id, e.id);
        assert_eq!(got.control_version, 2);
        assert_eq!(got.candidate_version, 3);
        assert_eq!(got.split, 20);
    }

    #[test]
    fn it_governs_only_the_kind_it_names() {
        let e = experiment(full_declaration());
        assert_eq!(governing_experiment(std::slice::from_ref(&e), "sale"), None);
    }

    #[test]
    fn a_closed_experiment_governs_nothing() {
        // The window IS the open interval — no separate window fields.
        let mut e = experiment(full_declaration());
        e.status = JobStatus::Closed;
        assert_eq!(
            governing_experiment(std::slice::from_ref(&e), "wholesale-keg-order"),
            None
        );
    }

    #[test]
    fn an_experiment_without_a_split_declaration_is_a_record_not_a_switch() {
        // Most live experiments (gate concurrency, build width) carry
        // no admission declaration at all. They must not split anything.
        let e = experiment(serde_json::json!({ "opened_at": "2026-09-01T00:00:00Z" }));
        assert_eq!(governing_experiment(std::slice::from_ref(&e), "sale"), None);
    }

    #[test]
    fn versions_parse_from_numbers_or_numeric_strings() {
        let mut m = full_declaration();
        m["control_version"] = serde_json::json!("2");
        m["candidate_version"] = serde_json::json!("3");
        let e = experiment(m);
        let got = governing_experiment(std::slice::from_ref(&e), "wholesale-keg-order")
            .expect("numeric strings are how operators type numbers");
        assert_eq!((got.control_version, got.candidate_version), (2, 3));
    }

    #[test]
    fn a_missing_split_defaults_to_an_even_one() {
        let mut m = full_declaration();
        m.as_object_mut().unwrap().remove("split");
        let e = experiment(m);
        assert_eq!(
            governing_experiment(std::slice::from_ref(&e), "wholesale-keg-order")
                .unwrap()
                .split,
            50
        );
    }

    #[test]
    fn a_malformed_split_unmakes_the_declaration() {
        // A split the machine cannot read must not silently become a
        // different split — the declaration fails whole.
        for bad in [
            serde_json::json!(140),
            serde_json::json!(-5),
            serde_json::json!("most"),
            serde_json::json!([50]),
        ] {
            let mut m = full_declaration();
            m["split"] = bad.clone();
            let e = experiment(m);
            assert_eq!(
                governing_experiment(std::slice::from_ref(&e), "wholesale-keg-order"),
                None,
                "split={bad} must invalidate the declaration"
            );
        }
    }

    #[test]
    fn two_experiments_on_one_kind_resolve_deterministically() {
        let mut older = experiment(full_declaration());
        older.opened_on = NaiveDate::from_ymd_opt(2026, 8, 30).unwrap();
        let newer = experiment(full_declaration());
        let a = governing_experiment(&[older.clone(), newer.clone()], "wholesale-keg-order");
        let b = governing_experiment(&[newer, older.clone()], "wholesale-keg-order");
        assert_eq!(a, b, "order of the fetch must not decide the experiment");
        assert_eq!(
            a.unwrap().experiment_id,
            older.id,
            "earliest opened_on wins"
        );
    }

    /// Backlog ce8b7d66: the write doors ask what a write newly puts in
    /// force. Opening a declaring packet does; re-sending the same
    /// declaration does not; widening the split or moving an arm does;
    /// a closed packet or a packet of another kind puts nothing in force.
    #[test]
    fn a_write_newly_puts_a_split_in_force_only_when_it_changes_one() {
        let running = experiment(full_declaration());
        let (kind, split) = newly_in_force(None, &running).expect("an admission opens it");
        assert_eq!(kind, running.metadata["kind_under_test"]);
        assert_eq!((split.control_version, split.candidate_version), (2, 3));

        let mut annotated = running.clone();
        annotated.metadata["note"] = serde_json::json!("halfway");
        assert_eq!(
            newly_in_force(Some(&running), &annotated),
            None,
            "an annotation changes no admission"
        );

        let mut widened = running.clone();
        widened.metadata["split"] = serde_json::json!(100);
        assert!(newly_in_force(Some(&running), &widened).is_some());

        let mut moved = running.clone();
        moved.metadata["candidate_version"] = serde_json::json!(4);
        assert!(newly_in_force(Some(&running), &moved).is_some());

        let mut held = running.clone();
        held.status = JobStatus::Draft;
        assert!(
            newly_in_force(Some(&held), &running).is_some(),
            "opening the packet opens the window"
        );

        let mut closed = running.clone();
        closed.status = JobStatus::Closed;
        assert_eq!(newly_in_force(Some(&running), &closed), None);

        let mut other = running.clone();
        other.kind = "sale".into();
        assert_eq!(newly_in_force(None, &other), None);
    }

    #[test]
    fn the_arm_is_a_pure_function_of_the_id() {
        let id = JobId::new();
        assert_eq!(arm_for(&id, 50), arm_for(&id, 50));
        assert_eq!(arm_for(&id, 0), ARM_CONTROL, "0% candidate share");
        assert_eq!(arm_for(&id, 100), ARM_CANDIDATE, "100% candidate share");
    }

    #[test]
    fn an_even_split_actually_splits() {
        // Not a statistics claim — just that the hash isn't constant:
        // over 1000 random ids at split=50, both arms are populated
        // in a band no uniform hash misses.
        let candidates = (0..1000)
            .filter(|_| arm_for(&JobId::new(), 50) == ARM_CANDIDATE)
            .count();
        assert!(
            (350..=650).contains(&candidates),
            "candidate arm got {candidates}/1000 at split=50"
        );
    }
}
