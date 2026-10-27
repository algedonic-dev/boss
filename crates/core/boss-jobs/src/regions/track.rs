//! THE TRACK: trains in transit — and the train-gate wait, read off the
//! same markers and the same hold as `yard.ts::trainGateTroubled`, which
//! the tests below pin against the TypeScript.

use super::*;

/// WHY the conductor has not filed a train's gate yet, verbatim from
/// the last failed launch — cleared the pass it is filed (0d16df6f).
/// The conductor writes it (`boss-cli/src/train_gate.rs` names this
/// constant), the yard reads it (`yard.ts::readTrainGate`), and so does
/// the track region below.
pub const TRAIN_GATE_WAIT_REASON: &str = "train_gate_wait_reason";
/// The gate could not be filed and CI alone judged the train — stamped
/// loudly on the train. Same three readers.
pub const TRAIN_GATE_FALLBACK: &str = "train_gate_fallback";

/// When the conductor last filed the train's gate (RFC3339). The
/// conductor writes it (`boss-cli/src/train_gate.rs::KEY_LAUNCHED_AT`
/// names this constant), and a relaunched train's wait is dated from it.
pub const TRAIN_GATE_LAUNCHED_AT: &str = "train_gate_launched_at";

/// The train's PLACE in the gate line (design 1e787fc2): set only by the
/// bound refusal, or by a gate-run that ended owed a relaunch, and
/// cleared by every other launch outcome (backlog 82071657). The
/// conductor writes it (`boss-cli/src/train_gate.rs::KEY_BOUND_WAIT`
/// names this constant); it is the one marker a wait for a bay is read
/// by, because [`TRAIN_GATE_WAIT_REASON`] is written on EVERY failed
/// launch (review a2ace778 of car 1585a455).
pub const TRAIN_GATE_BOUND_WAIT: &str = "train_gate_bound_wait";

/// Each gate-run that ended without a usable verdict, `{gate_run, why,
/// at}`, appended by the conductor as it owes the train a relaunch
/// (`boss-cli/src/train_gate.rs::KEY_REFUSALS` names this constant). A
/// relaunch's wait is dated from the latest `at` (review a2ace778).
pub const TRAIN_GATE_REFUSALS: &str = "train_gate_refusals";

/// HOW LONG A TRAIN MAY WAIT FOR A BAY BEFORE THE WAIT IS TROUBLE — ONE
/// number, the waiting band's own hold (backlog 19227ca8). Since design
/// 1e787fc2 (0f4224bc, train #1051) a train at a full bound holds the
/// first place in the gate line and takes the next bay that frees: a
/// short wait is the normal path, and drawing it as trouble from its
/// first minute made the floor red every time the bays were busy.
/// `yard.ts::TRAIN_GATE_WAIT_MINUTES` is held equal to this by a test
/// below, because the TypeScript cannot import it.
pub const TRAIN_GATE_WAIT_MINUTES: i64 = bands::TRACK_GATE_WAITING.hold_minutes;

/// WHEN A TRAIN BEGAN WAITING FOR ITS GATE, from what the record holds:
/// the latest of its PR opening (the `pr` step, when the conductor first
/// asks for a gate), its last gate launch, and its last refusal (a
/// gate-run that ended without a usable verdict owes the train a
/// relaunch, and the wait for it begins there — dated from the launch
/// before, a one-minute wait read as trouble at once, review a2ace778).
/// The marker itself carries no stamp, so this is a LOWER bound on the
/// true onset — it can only say a wait began earlier than it did, never
/// hide one. `None` when none is recorded. Only a COMPLETED step and an
/// RFC3339 stamp count; `yard.ts::readTrainGate`'s `wait_since` reads the
/// same three facts by the same two rules.
pub fn train_gate_wait_since(md: &Value, steps: &[Step]) -> Option<chrono::DateTime<chrono::Utc>> {
    let pr = step_done_at(find_step(steps, "pr", "Open the batched PR"));
    let launched = meta_instant(md, TRAIN_GATE_LAUNCHED_AT);
    let refused = md
        .get(TRAIN_GATE_REFUSALS)
        .and_then(Value::as_array)
        .and_then(|rs| rs.iter().filter_map(|r| meta_instant(r, "at")).max());
    pr.max(launched).max(refused)
}

/// WHY A TRAIN'S GATE IS NOT FILED, as its markers say it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateUnfiled {
    /// The gate could not be filed and CI alone judged the train.
    Fallback,
    /// The train holds its place in the gate line ([`TRAIN_GATE_BOUND_WAIT`]).
    AtBound,
    /// A launch failed for any other reason.
    LaunchFailing,
}

/// [`GateUnfiled`], read off a train's metadata, in that order: the
/// fallback outranks everything; the place in line ([`TRAIN_GATE_BOUND_WAIT`])
/// is the ONE marker of a wait for a bay; a wait reason with no place or
/// bound run is a launch that failed for some other reason. An uncertain
/// CREATE retains its reason beside the run already filed, so that reason
/// alone cannot say the gate is unfiled (review 0e73d943). Keyed on the reason alone,
/// as it was, every failed launch read as a wait (review a2ace778 of car
/// 1585a455). A blank string is no marker, as `readTrainGate`'s `text()`
/// reads it; `yard.ts::trainGateTroubled` reads the same three in the
/// same order.
pub fn train_gate_unfiled(md: &Value) -> Option<GateUnfiled> {
    let said = |k: &str| !md_str(md, k).is_empty();
    if said(TRAIN_GATE_FALLBACK) {
        Some(GateUnfiled::Fallback)
    } else if said(TRAIN_GATE_BOUND_WAIT) {
        Some(GateUnfiled::AtBound)
    } else if said(TRAIN_GATE_WAIT_REASON) && !said("train_gate_run") {
        Some(GateUnfiled::LaunchFailing)
    } else {
        None
    }
}

/// WHAT A TRAIN'S OWN TROUBLE SAYS, judged in the region the train
/// STANDS in (design e765b3fc §2a, car R1): a block the yard names
/// (`TrainStatus::block` — a red PR, a deploy refusal, a converge
/// overdue, a stall past the policy), a train judged by CI alone, a
/// train gate whose launch is failing, a train gate the conductor is
/// still retrying against the bound ([`train_gate_unfiled`] tells the
/// last three apart). A red PR is a train at the gates, a deploy
/// refusal one on the track; a troubled packet must look troubled where
/// it is drawn, not in the region it used to be counted in.
pub(super) fn train_findings(
    inputs: &RegionInputs<'_>,
    trains: &[&(Job, Vec<Step>)],
) -> Vec<Finding> {
    let ids: Vec<String> = trains.iter().map(|(j, _)| j.id.to_string()).collect();
    let statuses: Vec<&crate::yard::TrainStatus> = inputs
        .status
        .trains
        .iter()
        .filter(|t| ids.contains(&t.id))
        .collect();
    let blocked: Vec<&str> = statuses
        .iter()
        .filter(|t| t.block.is_some())
        .map(|t| t.title.as_str())
        .collect();
    // Every train still waiting for a bay, with when its wait began: the
    // finding carries the OLDEST onset, so `settle` holds it for the
    // band's period — the same [`TRAIN_GATE_WAIT_MINUTES`] the yard's
    // `trainGateTroubled` judges by — and a wait inside it is said as
    // `settling`, not drawn (backlog 19227ca8). This settle is the ONE
    // place the Rust side judges that hold: the one-train port of the
    // predicate it replaced had no production caller (review a2ace778,
    // finding 4) and was deleted rather than kept beside it. One train
    // whose onset the record does not hold makes the onset unknown,
    // stated at once rather than guessed.
    let unfiled = |want: GateUnfiled| {
        trains
            .iter()
            .filter(move |(j, _)| train_gate_unfiled(&j.metadata) == Some(want))
    };
    let waiting: Vec<(&str, Option<Instant>)> = unfiled(GateUnfiled::AtBound)
        .map(|(j, s)| (j.title.as_str(), train_gate_wait_since(&j.metadata, s)))
        .collect();
    let mut findings = Vec::new();
    if !blocked.is_empty() {
        // A block that carries its own start (a deploy refusal, a stall)
        // says how long; the others are stated without one.
        let since = statuses
            .iter()
            .filter_map(|t| match &t.block {
                Some(crate::yard::TrainBlock::DeployBlocked { since, .. }) => {
                    since.as_deref().and_then(stamp_instant)
                }
                Some(crate::yard::TrainBlock::Stalled { since }) => stamp_instant(since),
                _ => None,
            })
            .min();
        findings.push(Finding::new(
            bands::TRACK_BLOCKED,
            since,
            String::new(),
            format!("blocked: {}", blocked.join(", ")),
        ));
    }
    // The gate markers, split by what they mean (decision 1): CI alone
    // judged the train — degraded, ours — is trouble; a gate the
    // conductor is still retrying against the bound is a wait to watch.
    let fallback: Vec<&str> = unfiled(GateUnfiled::Fallback)
        .map(|(j, _)| j.title.as_str())
        .collect();
    if !fallback.is_empty() {
        findings.push(Finding::new(
            bands::TRACK_GATE_FALLBACK,
            None,
            String::new(),
            format!(
                "judged by CI alone, the gate not filed: {}",
                fallback.join(", ")
            ),
        ));
    }
    // A launch that failed for any reason but the bound holds no place in
    // the line and waits for nothing a bay will bring, so it settles for
    // nothing: trouble the pass it is recorded (review a2ace778 of car
    // 1585a455 — keyed on the reason, a missing manifest read `settling`
    // for ten minutes).
    let failing: Vec<String> = unfiled(GateUnfiled::LaunchFailing)
        .map(|(j, _)| {
            format!(
                "{} ({})",
                j.title,
                md_str(&j.metadata, TRAIN_GATE_WAIT_REASON)
            )
        })
        .collect();
    if !failing.is_empty() {
        findings.push(Finding::new(
            bands::TRACK_GATE_LAUNCH_FAILING,
            None,
            String::new(),
            format!("gate cannot be filed: {}", failing.join(", ")),
        ));
    }
    if !waiting.is_empty() {
        let since = waiting
            .iter()
            .map(|(_, s)| *s)
            .collect::<Option<Vec<Instant>>>()
            .and_then(|s| s.into_iter().min());
        let titles: Vec<&str> = waiting.iter().map(|(t, _)| *t).collect();
        findings.push(Finding::new(
            bands::TRACK_GATE_WAITING,
            since,
            String::new(),
            format!("gate not filed yet: {}", titles.join(", ")),
        ));
    }
    findings
}

/// THE TRACK: trains in transit — merged, deploying and converging
/// ([`train_region`]); a train being made up stands at the dock and one
/// under its train gate at the gates (design e765b3fc §2a, car R1).
/// Troubled or attention by the trains on it ([`train_findings`]). The
/// trend is the time at CI — the `pr` stamp to the `ci` verdict — for
/// trains that arrived in each window.
///
/// NO CAPACITY BOUND SINCE R1. The single track the bound of one drew is
/// the conductor's PRE-MERGE hold (`yard::holds_the_track`: a merged
/// train holds nothing, and the next consist merges on top of it) — and
/// R1 places a pre-merge train at the dock or the gates, where the
/// conductor has it. What is left on the track is merged trains in
/// transit, and two of those at once is the conductor working, not a
/// track over its bound; a "2 of 1" would read as trouble that is not
/// there. So the track counts, and a train on it is judged by its own
/// findings (a deploy refused, a converge overdue, a stall), not by a
/// capacity it no longer has. The gates stay the capacity station a full
/// line is read at (car F1).
pub(super) fn track(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    let on_track = trains_in(inputs, "track");
    let ci_times = inputs.closed_trains.iter().filter_map(|(j, s)| {
        if j.metadata.get("outcome").and_then(Value::as_str) != Some("arrived") {
            return None;
        }
        let pr = step_done_at(find_step(s, "pr", "Open the batched PR"))?;
        let ci = step_done_at(find_step(s, "ci", "CI verdict"))?;
        let d = (ci - pr).num_seconds();
        (d >= 0).then_some((closed_at(j)?, d))
    });
    let (cur, prev) = split(w, ci_times);
    let trend = duration_trend("time at CI", "minutes", cur, prev);
    let count = count_in(inputs, "track").unwrap_or(on_track.len());
    // AN EMPTY TRACK SAYS WHERE THE TRAIN IS (backlog 3eddffc4). On
    // 2026-10-01 this read "no train in transit" while PR train 02:41
    // stood at its CI verdict — at the gates, by R1 — and `boss orient`
    // listed it IN TRANSIT. Orient now reads the track's own predicate,
    // and the track names a train under test rather than denying one.
    let under_test: Vec<&str> = trains_in(inputs, "gates")
        .iter()
        .map(|(j, _)| j.title.as_str())
        .collect();
    let empty = if under_test.is_empty() {
        "no train on the track".to_string()
    } else {
        format!(
            "no train on the track; {} {} under test at the gates",
            under_test.join(", "),
            if under_test.len() == 1 { "is" } else { "are" }
        )
    };
    let clear_why = if count == 0 {
        empty.clone()
    } else {
        plural(count, "train in transit", "trains in transit")
    };
    let settled = settle(train_findings(inputs, &on_track), clear_why, inputs.now);
    // THE KPI: how long the oldest train in transit has stood at the
    // stage it is at — from its last completed step (the stage's start),
    // else from its own opening.
    let at_stage = on_track
        .iter()
        .filter_map(|(j, s)| {
            let start = s
                .iter()
                .filter_map(|st| step_done_at(Some(st)))
                .max()
                .or_else(|| opened_at(j))?;
            Some((inputs.now - start).num_minutes().max(0))
        })
        .max();
    #[allow(clippy::cast_precision_loss)]
    let kpi = vec![match at_stage {
        Some(m) => measure_said(
            "train age at its stage",
            Some(m as f64),
            "minutes",
            format!("oldest train {m} minutes at its stage"),
        ),
        None => measure_said("train age at its stage", None, "minutes", empty),
    }];
    region(
        "track",
        Some(count),
        None,
        "trains in transit",
        settled,
        trend,
        kpi,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    fn read(open: &[(Job, Vec<Step>)], closed: &[(Job, Vec<Step>)]) -> Regions {
        let status = build_status_for(
            YardInputs {
                open_trains: open,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        regions(&inputs(
            &status,
            open,
            closed,
            &[],
            &[],
            Some(&[]),
            Some(&[]),
        ))
    }

    /// A train at `ci`: its PR open, its verdict not in.
    fn under_test(title: &str, metadata: Value, ci: StepStatus) -> (Job, Vec<Step>) {
        let train = job("pr-train", title, JobStatus::Open, metadata);
        let mut verdict = step(&train, "ci", ci, None);
        if ci == StepStatus::Completed {
            verdict.completed_at = Some(t("2026-09-19T11:20:00Z"));
            verdict.metadata = json!({ "result": "failing" });
        }
        let steps = vec![
            step(
                &train,
                "collect",
                StepStatus::Completed,
                Some("2026-09-19T11:00:00Z"),
            ),
            step(
                &train,
                "pr",
                StepStatus::Completed,
                Some("2026-09-19T11:01:00Z"),
            ),
            verdict,
            step(&train, "merged", StepStatus::Ready, None),
        ];
        (train, steps)
    }

    /// A train merged at `merged`, deploying.
    fn in_transit(title: &str, merged: &str) -> (Job, Vec<Step>) {
        let train = job("pr-train", title, JobStatus::Open, json!({}));
        let steps = vec![
            step(
                &train,
                "pr",
                StepStatus::Completed,
                Some("2026-09-19T10:01:00Z"),
            ),
            step(
                &train,
                "ci",
                StepStatus::Completed,
                Some("2026-09-19T10:20:00Z"),
            ),
            step(&train, "merged", StepStatus::Completed, Some(merged)),
            step(&train, "deployed", StepStatus::Ready, None),
        ];
        (train, steps)
    }

    /// A TRAIN'S TROUBLE IS READ WHERE IT STANDS (design e765b3fc §2a,
    /// car R1). A red PR is a train at the gates, so it troubles the
    /// gates and the track stays clear with nothing on it; a gate the
    /// conductor has not filed yet asks for attention there; one judged
    /// by CI alone troubles it — the yard's own predicate and the ported
    /// client one, split by what each means.
    #[test]
    fn a_blocked_train_or_an_unfiled_train_gate_is_read_at_the_gates_where_it_stands() {
        let out = read(
            &[under_test("train #470", json!({}), StepStatus::Completed)],
            &[],
        );
        let gates = by_name(&out, "gates");
        // The train under test, whose gate has already judged it red: it
        // needs no bay and waits in no line, so it stands as the gates'
        // third figure, `judged` — never in the line (backlog bce67f09).
        assert_eq!(
            (gates.count, gates.waiting, gates.judged),
            (Some(0), None, Some(1)),
            "the judged train"
        );
        assert_eq!(gates.state, RegionState::Troubled, "{}", gates.why);
        assert!(gates.why.contains("blocked: train #470"), "{}", gates.why);
        let track = by_name(&out, "track");
        assert_eq!(
            (track.count, track.state),
            (Some(0), RegionState::Clear),
            "{}",
            track.why
        );

        // CI still pending, and the conductor waiting to file the gate:
        // the bound refusal writes both keys (conductor.rs).
        let waiting = under_test(
            "train #471",
            json!({
                TRAIN_GATE_WAIT_REASON: "bound: 3 running",
                TRAIN_GATE_BOUND_WAIT: "bound: 3 running",
            }),
            StepStatus::Ready,
        );
        let out = read(std::slice::from_ref(&waiting), &[]);
        let gates = by_name(&out, "gates");
        // The conductor retrying against a full bound is a wait to
        // watch, not trouble (design 62de32ae, decision 1). The marker
        // carries no stamp; the wait is dated from the PR's opening
        // (11:01), so at 12:00 it has held 59 minutes (19227ca8).
        assert_eq!(gates.state, RegionState::Attention, "{}", gates.why);
        assert!(
            gates.why.contains("gate not filed yet: train #471"),
            "{}",
            gates.why
        );
        let band = gates.band.as_ref().unwrap();
        assert_eq!(band.id, "track-gate-waiting");
        assert_eq!(band.held_minutes, Some(59));

        // CI alone judged the train — the gate could not be filed at
        // all, a degraded verdict that is ours: trouble.
        let mut fallback = waiting;
        fallback.0.metadata = json!({ TRAIN_GATE_FALLBACK: "gate unavailable; CI alone" });
        let out = read(&[fallback], &[]);
        let gates = by_name(&out, "gates");
        assert_eq!(gates.state, RegionState::Troubled, "{}", gates.why);
        assert!(gates.why.contains("judged by CI alone"), "{}", gates.why);
        // A blank marker is no marker (yard.ts `text()`).
        assert_eq!(
            train_gate_unfiled(&json!({
                TRAIN_GATE_WAIT_REASON: "",
                TRAIN_GATE_BOUND_WAIT: "",
            })),
            None
        );
        assert_eq!(
            train_gate_unfiled(&json!({
                TRAIN_GATE_FALLBACK: "CI alone",
                TRAIN_GATE_BOUND_WAIT: "bound",
            })),
            Some(GateUnfiled::Fallback)
        );
    }

    /// An uncertain native CREATE can leave its reason beside the run
    /// the conductor already bound (ae37ab73 / edf2146e, review 0e73d943).
    /// A diagnostic about that CREATE is not an unfiled gate.
    #[test]
    fn a_filed_gate_is_not_an_unfiled_launch_when_create_was_uncertain() {
        let run = "815e87cb";
        let why = format!(
            "native Job at the immutable name does not match the recorded launch \
             (gate-run {run} was filed first; inspect its recorded outcome)"
        );
        let filed = under_test(
            "train with an uncertain CREATE",
            json!({
                "train_gate_run": run,
                TRAIN_GATE_WAIT_REASON: why,
            }),
            StepStatus::Ready,
        );
        assert_eq!(train_gate_unfiled(&filed.0.metadata), None);
        let out = read(std::slice::from_ref(&filed), &[]);
        let gates = by_name(&out, "gates");
        assert_ne!(
            gates.band.as_ref().map(|band| band.id.as_str()),
            Some("track-gate-launch-failing"),
            "{}",
            gates.why
        );
        assert!(!gates.why.contains("gate cannot be filed"), "{}", gates.why);
        assert_eq!(md_str(&filed.0.metadata, TRAIN_GATE_WAIT_REASON), why);
    }

    /// ONLY THE BOUND REFUSAL SETTLES (review a2ace778 of car 1585a455,
    /// finding 1). The conductor writes `train_gate_wait_reason` on EVERY
    /// failed launch — a manifest that does not render, kubectl, RBAC, a
    /// recovery hold — and `train_gate_bound_wait` only when the bound
    /// refused it (backlog 82071657). Keyed on the reason, a broken launch
    /// read `settling` for ten minutes; keyed on the place in line, it is
    /// trouble the pass it happens, however recent the launch.
    #[test]
    fn a_failed_launch_is_trouble_at_once_and_only_the_bound_refusal_settles() {
        let fault = "rendering the runner manifest: /opt/boss/infra/gate-runner/gate-runner.yaml: \
                     No such file or directory";
        let broken = under_test(
            "train #476",
            json!({
                TRAIN_GATE_WAIT_REASON: fault,
                "train_gate_launch_failures": 1,
                TRAIN_GATE_LAUNCHED_AT: "2026-09-19T11:59:00Z",
            }),
            StepStatus::Ready,
        );
        assert_eq!(
            train_gate_unfiled(&broken.0.metadata),
            Some(GateUnfiled::LaunchFailing)
        );
        let out = read(std::slice::from_ref(&broken), &[]);
        let gates = by_name(&out, "gates");
        assert_eq!(gates.state, RegionState::Troubled, "{}", gates.why);
        assert_eq!(
            gates.band.as_ref().map(|b| b.id.as_str()),
            Some("track-gate-launch-failing")
        );
        assert!(
            gates
                .why
                .contains(&format!("gate cannot be filed: train #476 ({fault})")),
            "{}",
            gates.why
        );
        assert!(!gates.why.contains("settling"), "{}", gates.why);

        // The same minute-old launch refused at the bound: settling.
        let why = "the cluster is at its gate bound (3 running of 3: a, b, c)";
        let mut at_bound = broken;
        at_bound.0.metadata = json!({
            TRAIN_GATE_WAIT_REASON: why,
            TRAIN_GATE_BOUND_WAIT: why,
            TRAIN_GATE_LAUNCHED_AT: "2026-09-19T11:59:00Z",
        });
        assert_eq!(
            train_gate_unfiled(&at_bound.0.metadata),
            Some(GateUnfiled::AtBound)
        );
        let out = read(std::slice::from_ref(&at_bound), &[]);
        let gates = by_name(&out, "gates");
        assert_eq!(gates.state, RegionState::Clear, "{}", gates.why);
        assert!(gates.why.contains("settling"), "{}", gates.why);
    }

    /// A RELAUNCH WAITS FROM ITS REFUSAL (review a2ace778, finding 2).
    /// A gate-run that ends without a usable verdict is recorded in
    /// `train_gate_refusals` with its `at`, and the train takes the first
    /// place in line for its relaunch. Dated from the PREVIOUS launch, a
    /// one-minute wait after a refusal read as trouble at once; the
    /// refusal is part of the record, so the wait is dated from it too.
    #[test]
    fn a_relaunch_after_a_refusal_is_dated_from_the_refusal() {
        let why = "gate-run 0123abcd ended without a usable verdict (the Job ended without a \
                   verdict); the train holds the first place in the gate line for its relaunch";
        let mut relaunch = under_test("train #477", json!({}), StepStatus::Ready);
        relaunch.0.metadata = json!({
            TRAIN_GATE_BOUND_WAIT: why,
            TRAIN_GATE_LAUNCHED_AT: "2026-09-19T11:20:00Z",
            TRAIN_GATE_REFUSALS: [
                { "gate_run": "0123abcd", "why": "the Job ended", "at": "2026-09-19T11:40:00Z" },
                { "gate_run": "4567ef01", "why": "the Job ended", "at": "2026-09-19T11:57:00Z" },
                { "gate_run": "garbled", "at": "yesterday" },
            ],
        });
        assert_eq!(
            train_gate_wait_since(&relaunch.0.metadata, &relaunch.1),
            Some(t("2026-09-19T11:57:00Z"))
        );
        // The place in line alone — the refusal pass writes no reason —
        // is a wait in the line, not a train with nothing to say.
        let out = read(std::slice::from_ref(&relaunch), &[]);
        let gates = by_name(&out, "gates");
        assert_eq!(gates.state, RegionState::Clear, "{}", gates.why);
        assert!(
            gates
                .why
                .contains("settling: gate not filed yet: train #477 — held 3m of the 10m"),
            "{}",
            gates.why
        );
    }

    /// A STEP NOT DONE DATES NOTHING, and a stamp is RFC3339 or nothing
    /// (review a2ace778, finding 3): `step_done_at` reads only a
    /// completed step, and `parse_instant` only RFC3339 — the yard's
    /// `readTrainGate` is held to both by the TypeScript's own test.
    #[test]
    fn a_wait_is_dated_only_from_a_completed_step_and_a_strict_stamp() {
        let mut train = under_test("train #478", json!({}), StepStatus::Ready);
        for s in &mut train.1 {
            if s.spec_slug.as_deref() == Some("pr") {
                s.status = StepStatus::Active;
            }
        }
        train.0.metadata = json!({ TRAIN_GATE_LAUNCHED_AT: "2026-09-19" });
        assert_eq!(train_gate_wait_since(&train.0.metadata, &train.1), None);
    }

    /// A TRAIN BRIEFLY WAITING FOR A BAY IS NOT TROUBLE (backlog
    /// 19227ca8). Since design 1e787fc2 a train at a full bound holds
    /// the first place in the gate line and takes the next bay: a wait of
    /// a few minutes is the normal path. The region and the one-train
    /// predicate judge it by ONE number, [`TRAIN_GATE_WAIT_MINUTES`] —
    /// the waiting band's own hold — so a wait inside it is `settling`
    /// and clear, and one past it is attention; the yard's TypeScript is
    /// held to the same number.
    #[test]
    fn a_train_briefly_waiting_for_a_bay_is_settling_not_trouble() {
        let why = "the cluster is at its gate bound (3 running of 3: a, b, c) — the train \
                   holds the first place in the gate line and takes the next bay that frees";
        let md = json!({ TRAIN_GATE_WAIT_REASON: why, TRAIN_GATE_BOUND_WAIT: why });
        // The PR opened 11:01, and at NOW (12:00) the wait has held 59
        // minutes: past the hold, a wait to watch.
        let mut short = under_test("train #475", md, StepStatus::Ready);
        let since = train_gate_wait_since(&short.0.metadata, &short.1);
        assert_eq!(since, Some(t("2026-09-19T11:01:00Z")));
        let out = read(std::slice::from_ref(&short), &[]);
        assert_eq!(by_name(&out, "gates").state, RegionState::Attention);
        // A relaunch waits again: dated from the last launch, the later.
        short.0.metadata = json!({
            TRAIN_GATE_WAIT_REASON: why,
            TRAIN_GATE_BOUND_WAIT: why,
            TRAIN_GATE_LAUNCHED_AT: "2026-09-19T11:56:00Z",
        });
        let out = read(std::slice::from_ref(&short), &[]);
        let gates = by_name(&out, "gates");
        assert_eq!(gates.state, RegionState::Clear, "{}", gates.why);
        assert!(
            gates
                .why
                .contains("settling: gate not filed yet: train #475 — held 4m of the 10m"),
            "{}",
            gates.why
        );
        assert_eq!(TRAIN_GATE_WAIT_MINUTES, 10);
        // The yard's TypeScript judges by the same number.
        let yard =
            std::fs::read_to_string(boss_testing::repo_root().join("apps/web/src/it/yard/yard.ts"))
                .expect("yard.ts is in the tree");
        assert!(
            yard.contains(&format!(
                "export const TRAIN_GATE_WAIT_MINUTES = {TRAIN_GATE_WAIT_MINUTES};"
            )),
            "yard.ts::TRAIN_GATE_WAIT_MINUTES drifted from track.rs::TRAIN_GATE_WAIT_MINUTES"
        );
        for key in [
            TRAIN_GATE_LAUNCHED_AT,
            TRAIN_GATE_REFUSALS,
            TRAIN_GATE_BOUND_WAIT,
        ] {
            assert!(
                yard.contains(&format!("md.{key}")),
                "yard.ts no longer reads {key}, which this region judges a train gate's wait by"
            );
        }
    }

    /// THE TRACK HOLDS MERGED TRAINS IN TRANSIT, and carries no capacity
    /// bound since R1: two merged trains converging at once is the
    /// conductor working, not a track over its bound — clear, counted,
    /// with the oldest train's age at its stage as the KPI. A pre-merge
    /// train is not on it.
    #[test]
    fn the_track_holds_merged_trains_in_transit_with_no_capacity_bound() {
        let out = read(
            &[
                in_transit("train #472", "2026-09-19T11:00:00Z"),
                in_transit("train #473", "2026-09-19T11:30:00Z"),
                under_test("train #474", json!({}), StepStatus::Ready),
            ],
            &[],
        );
        let track = by_name(&out, "track");
        assert_eq!(track.count, Some(2), "{}", track.why);
        assert_eq!(track.state, RegionState::Clear, "{}", track.why);
        assert_eq!((track.bound, track.bound_kind), (None, None));
        assert_eq!(track.why, "2 trains in transit");
        assert_eq!(track.unit, "trains in transit");
        // train #472 merged at 11:00 and NOW is 12:00.
        assert_eq!(track.kpi[0].value, Some(60.0));
        assert_eq!(track.kpi[0].text, "oldest train 60 minutes at its stage");

        let out = read(&[], &[]);
        assert_eq!(by_name(&out, "track").count, Some(0));
    }

    /// THE MEASURED SCENE (backlog 3eddffc4, 2026-10-01 03:26Z, at
    /// David's "is the map accurate?"): PR train 2026-10-01 02:41 at its
    /// CI verdict. It stands at the gates (car R1); the track read "no
    /// train in transit" while `boss orient` listed the train IN TRANSIT.
    /// Orient now asks the track's own predicate — `yard::in_transit`,
    /// merged — and the empty track names the train under test.
    #[test]
    fn an_empty_track_names_the_train_under_test_and_orients_predicate_agrees() {
        let scene = under_test("PR train 2026-10-01 02:41", json!({}), StepStatus::Ready);
        assert!(
            !crate::yard::in_transit(&scene.1),
            "the predicate orient's IN TRANSIT reads: not merged, not in transit"
        );
        let out = read(std::slice::from_ref(&scene), &[]);
        let track = by_name(&out, "track");
        assert_eq!(track.count, Some(0), "{}", track.why);
        assert_eq!(
            track.why,
            "no train on the track; PR train 2026-10-01 02:41 is under test at the gates"
        );
        assert_eq!(track.kpi[0].text, track.why);
        let gates = by_name(&out, "gates");
        assert_eq!(
            (gates.count, gates.waiting),
            (Some(0), Some(1)),
            "counted once, there — in the line, its gate holding no bay"
        );

        let merged = in_transit("train #472", "2026-09-19T11:00:00Z");
        assert!(crate::yard::in_transit(&merged.1));
        let out = read(&[], &[]);
        assert_eq!(by_name(&out, "track").why, "no train on the track");
    }
}
