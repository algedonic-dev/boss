//! THE GARAGE: what stopped short of the track — held cars, held and
//! stranded gate-runs, runs in limbo, and a repair already in flight.

use super::*;

/// THE GARAGE: work off the main line — cars held on the dock, greens
/// held before parking, greens that never became a car (stranded),
/// reds awaiting rework, and runs the gate never judged (limbo).
/// Troubled when a gate failed, a green is stranded or a run sits in limbo: a green
/// no car claims is a fix about to be rebuilt blind, and "we do not
/// know" is not a verdict. Deliberate holds and failed checks are judged
/// separately: repairing a failure does not make the failed branch healthy.
/// The trend is reds per day.
/// Is a repair already aimed at this branch?
///
/// WHY (backlog aae1515e; David, 2026-09-21: "the Garage has been
/// flashing troubled all day, and it is either too long to have not
/// addressed or we need to have some indication that the fix is in
/// transit"). BOTH halves were true that day, and the second caused the
/// first. The garage went troubled at 08:50Z on a gate-run recorded
/// `lost`; at 18:0xZ the branch was still on the forge and nothing had
/// moved it in nine hours. The surface was ACCURATE the whole time —
/// and it rendered identically at minute five and at hour nine, which
/// is why nine hours passed.
///
/// A troubled region can be cleared only by the fix landing, so a long
/// repair is indistinguishable from total neglect. §Diagnosis already
/// says a troubled packet must look troubled; the corollary is that a
/// troubled thing BEING REPAIRED must look different from one nobody
/// has touched, or the colour stops carrying information and the reader
/// learns to discount it — the same decay as a permanently-red check.
///
/// THE PREDICATE IS CHEAP AND EXACT for this region: a lost or
/// never-judged gate-run whose BRANCH has a newer gate-run still open
/// is under repair. Nothing is inferred — the newer run is the repair,
/// and it is in the same list the region already reads.
fn under_repair(gate_runs: &[Job], branch: &str, packet_id: &str) -> bool {
    gate_runs.iter().any(|r| {
        r.status == boss_core::job::JobStatus::Open
            && r.id.to_string() != packet_id
            && r.metadata.get("branch").and_then(Value::as_str) == Some(branch)
    })
}

pub(crate) fn recorded_review_wait(
    car: &Job,
    steps: &[Step],
    inputs: &RegionInputs<'_>,
) -> Result<RecordedProofWait, String> {
    let review = steps
        .iter()
        .find(|step| step.spec_slug.as_deref() == Some(crate::car::REVIEW_SLUG))
        .ok_or("review step is unread")?;
    let owner = review
        .assignee_id
        .as_deref()
        .filter(|owner| !owner.trim().is_empty())
        .ok_or("review action owner is unread")?;
    if review.status != StepStatus::Active {
        return Err("review has no active claim".into());
    }
    let head = super::judging::exact_source_head(review.metadata.get(crate::car::HOLD_SHA))
        .ok_or("held source head is unread")?;
    let protocols = inputs
        .internal_protocols
        .ok_or("pinned protocol reading is unavailable")?;
    let key = (car.kind.clone(), car.workflow_version);
    let protocol = match protocols.get(&key) {
        Some(Ok(spec)) if spec.kind == car.kind && spec.version == car.workflow_version => spec,
        Some(Ok(_)) => return Err("pinned protocol has a different binding".into()),
        Some(Err(reason)) => return Err(reason.clone()),
        None => {
            return Err(format!(
                "{}@{}: pinned protocol version is unread",
                car.kind, car.workflow_version
            ));
        }
    };
    // A rejected row was never published either (backlog 50ded63d).
    if !protocol.status.was_published() {
        return Err("pinned review protocol was not published".into());
    }
    let hours = protocol
        .steps
        .iter()
        .find(|step| step.title == crate::car::REVIEW_SLUG)
        .and_then(|step| step.duration_hours)
        .filter(|hours| hours.is_finite() && *hours > 0.0)
        .ok_or("pinned review bound is unread")?;
    let candidates = inputs
        .agent_runs
        .ok_or("registered review runs are unread")?
        .iter()
        .filter(|(run, _)| {
            run.kind == "agent-run"
                && run.status == JobStatus::Open
                && run.metadata.get("packet").and_then(Value::as_str)
                    == Some(car.id.to_string().as_str())
                && run.metadata.get("step").and_then(Value::as_str) == Some(crate::car::REVIEW_SLUG)
        })
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return Err("one registered review run is required".into());
    }
    let (run, run_steps) = candidates[0];
    if car.metadata.get("agent_run").and_then(Value::as_str) == Some(run.id.to_string().as_str()) {
        return Err("the car builder cannot supply an independent review wait".into());
    }
    let claim: Step = serde_json::from_value(
        run.metadata
            .get("claim_receipt")
            .cloned()
            .ok_or("registered claim receipt is unread")?,
    )
    .map_err(|_| "registered claim receipt does not decode as a native step")?;
    if claim.id != review.id
        || claim.job_id != car.id
        || claim.spec_slug != review.spec_slug
        || claim.status != StepStatus::Active
        || claim.assignee_id.as_deref() != Some(owner)
        || claim
            .metadata
            .get(crate::car::HOLD_SHA)
            .and_then(Value::as_str)
            != Some(head)
    {
        return Err("registered claim receipt does not bind this owner, step and held head".into());
    }
    let building = run_steps
        .iter()
        .find(|step| {
            step.spec_slug.as_deref() == Some("building")
                && step.status == StepStatus::Active
                && step.assignee_id.as_deref() == Some(owner)
        })
        .ok_or("review worker has no matching active registered claim")?;
    let receipt = &run.metadata["worker_started"];
    let receipt_matches = match receipt.get("schema").and_then(Value::as_u64) {
        Some(1) => receipt.as_object().is_some_and(|fields| fields.len() == 3),
        Some(2) => crate::agent_budget::observer_start_receipt_matches(
            &building.metadata[crate::agent_budget::WORKER_START_EXPECTATION],
            receipt,
            &run.id.to_string(),
            owner,
        ),
        _ => false,
    };
    if !receipt_matches
        || run
            .metadata
            .pointer("/worker_started/run")
            .and_then(Value::as_str)
            != Some(run.id.to_string().as_str())
        || run
            .metadata
            .pointer("/worker_started/actor")
            .and_then(Value::as_str)
            != Some(owner)
    {
        return Err("review worker has no matching active registered claim".into());
    }
    let started_at = run
        .opened_at
        .ok_or("registered review admission clock is unread")?;
    if started_at > inputs.now {
        return Err("registered review clock is in the future".into());
    }
    let next_check_at = chrono::Duration::try_seconds((hours * 3600.0).ceil() as i64)
        .and_then(|duration| started_at.checked_add_signed(duration))
        .ok_or("review deadline is outside the clock range")?;
    if next_check_at <= inputs.now {
        return Err("registered review is past its pinned bound".into());
    }
    Ok(RecordedProofWait {
        packet: car.id.to_string(),
        owner: owner.into(),
        condition_owner: owner.into(),
        title: car.title.clone(),
        condition: format!("independent review at {head}"),
        next_check_at,
        evidence: ProofWaitEvidence::Review {
            run: run.id.to_string(),
            head: head.into(),
            workflow_version: protocol.version,
            started_at,
        },
    })
}

pub(super) fn garage(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    let s = inputs.status;
    let held = s.held_cars.len() + s.held.len();
    let stranded = s.stranded.len();
    let limbo = s.limbo.len();
    let red = s.garage.len();
    let count = held + stranded + limbo + red;
    let reds = inputs
        .gate_runs
        .iter()
        .filter(|r| r.metadata.get("outcome").and_then(Value::as_str) == Some("failed"))
        .filter_map(closed_at);
    let (cur, prev) = count_split(w, reds);
    let trend = rate_trend("reds", w, cur, prev);
    // How many of the troubled members already have a newer gate-run
    // open on their branch — a repair in flight (aae1515e).
    let repairing = s
        .stranded
        .iter()
        .filter(|g| under_repair(inputs.gate_runs, &g.branch, &g.packet_id))
        .count()
        + s.limbo
            .iter()
            .filter(|l| under_repair(inputs.gate_runs, &l.branch, &l.packet_id))
            .count();
    let mut findings = Vec::new();
    if red > 0 {
        let failed = s
            .garage
            .iter()
            .map(|car| {
                format!(
                    "{}: {} (gate {})",
                    car.branch,
                    car.failed_check
                        .as_deref()
                        .unwrap_or("failed check not recorded"),
                    car.packet_id,
                )
            })
            .collect::<Vec<_>>();
        findings.push(Finding::new(
            bands::GARAGE_FAILED,
            None,
            String::new(),
            format!("failed gates requiring rework: {}", failed.join("; ")),
        ));
    }
    if stranded > 0 || limbo > 0 {
        let mut parts = Vec::new();
        if stranded > 0 {
            parts.push(format!(
                "{} that never became a car",
                plural(stranded, "green gate", "green gates")
            ));
        }
        if limbo > 0 {
            parts.push(format!(
                "{} never judged",
                plural(limbo, "gate-run", "gate-runs")
            ));
        }
        // STILL TROUBLED, ANNOTATED — deliberately not a fourth state.
        // A new value in the state vocabulary forces every consumer to
        // handle it (the yard, the world map, orient, any alarm keyed on
        // `troubled`), and one that does not becomes WRONG rather than
        // merely incomplete. The reader's question is "is anyone on it",
        // and a sentence answers that without moving the colour.
        if repairing > 0 {
            parts.push(format!(
                "{repairing} of {} under repair — a newer gate-run is open on that branch",
                stranded + limbo
            ));
        } else {
            parts.push("nothing aimed at any of them".to_string());
        }
        // A green is stranded from the moment it went green; the hold is
        // the grace in which auto-park is still filing its car.
        let since = s
            .stranded
            .iter()
            .map(|g| g.since.as_str())
            .chain(s.limbo.iter().map(|l| l.since.as_str()))
            .filter_map(stamp_instant)
            .min();
        findings.push(Finding::new(
            bands::GARAGE_STRANDED,
            since,
            String::new(),
            parts.join("; "),
        ));
    }
    if !s.held.is_empty() {
        let missing = s.held.iter().map(|held| format!("{} ({}): required registered wait owner, condition, bound and next check are unread", held.packet_id, held.branch)).collect::<Vec<_>>();
        findings.push(Finding::new(
            bands::GARAGE_WAIT_UNQUALIFIED,
            None,
            String::new(),
            missing.join("; "),
        ));
    }
    let mut qualified = Vec::new();
    for held in &s.held_cars {
        if held.kind == crate::yard::CarHoldKind::RegateRed {
            findings.push(Finding::new(
                bands::GARAGE_FAILED,
                None,
                String::new(),
                format!(
                    "{}: failed dock re-gate requires repair — {}",
                    held.car.id, held.reason
                ),
            ));
        }
        let reading = inputs
            .cars
            .iter()
            .find(|(car, _)| car.id.to_string() == held.car.id)
            .ok_or_else(|| "held packet reading is unavailable".to_string())
            .and_then(|(car, steps)| recorded_review_wait(car, steps, inputs));
        match reading {
            Ok(wait) => qualified.push(wait),
            Err(reason) => findings.push(Finding::new(
                bands::GARAGE_WAIT_UNQUALIFIED,
                None,
                String::new(),
                format!("{}: {reason}", held.car.id),
            )),
        }
    }
    if !qualified.is_empty() {
        findings.push(Finding::new(
            bands::GARAGE_WAIT_EXPECTED,
            None,
            String::new(),
            qualified
                .iter()
                .map(|wait| {
                    format!(
                        "{}: {}; owner {}; next check {}",
                        wait.packet, wait.condition, wait.owner, wait.next_check_at
                    )
                })
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    let waiting_why = format!("{held} held, {red} red awaiting rework");
    if held > 0 {
        let since = s
            .held
            .iter()
            .map(|g| g.since.as_str())
            .chain(s.held_cars.iter().map(|h| h.car.parked_since.as_str()))
            .filter_map(stamp_instant)
            .min();
        findings.push(Finding::new(
            bands::GARAGE_WAITING,
            since,
            String::new(),
            waiting_why.clone(),
        ));
    }
    let clear_why = if count > 0 {
        waiting_why
    } else {
        "nothing held, stranded or red".to_string()
    };
    let settled = settle(findings, clear_why, inputs.now);
    let kpi = vec![measure_said(
        "reds awaiting rework",
        count_value(red),
        "cars",
        format!("{} awaiting rework", plural(red, "red", "reds")),
    )];
    let mut reading = region(
        "garage",
        Some(count),
        None,
        "held, stranded or red",
        settled,
        trend,
        kpi,
    );
    reading.waits = qualified;
    reading
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    #[test]
    fn an_active_exact_head_review_uses_its_pinned_protocol_bound() {
        let (car, mut steps) = parked("fix/under-review", json!({}));
        let head = "a".repeat(40);
        steps[0].metadata = json!({"hold": "independent review", "hold_sha": head});
        steps[0].status = StepStatus::Active;
        steps[0].assignee_id = Some("agent-reviewer".into());
        let mut run = job(
            "agent-run",
            "review",
            JobStatus::Open,
            json!({"packet": car.id.to_string(), "step": "review", "claim_receipt": serde_json::to_value(&steps[0]).unwrap()}),
        );
        run.opened_at = Some(t("2026-09-19T11:00:00Z"));
        run.metadata["worker_started"] =
            json!({"run": run.id.to_string(), "actor": "agent-reviewer", "schema": 1});
        let mut building = step(&run, "building", StepStatus::Active, None);
        building.assignee_id = Some("agent-reviewer".into());
        let protocols = vec![WorkflowSpec::platform_seed(
            "ship-a-change",
            "Change",
            "platform",
            vec!["custom".into()],
            vec![crate::registry::StepSpec {
                title: "review".into(),
                duration_hours: Some(2.0),
                ..Default::default()
            }],
        )]
        .into_iter()
        .map(|spec| ((spec.kind.clone(), spec.version), Ok(spec)))
        .collect::<PinnedProtocols>();
        let mut status = empty_status();
        status.held_cars.push(crate::yard::HeldCar {
            car: crate::yard::dock_car(&car),
            reason: "independent review".into(),
            kind: crate::yard::CarHoldKind::Operator,
            release: None,
            auto_release: None,
        });
        let cars = vec![(car, steps)];
        let runs = vec![(run, vec![building])];
        let mut input = inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[]));
        input.agent_runs = Some(&runs);
        input.internal_protocols = Some(&protocols);
        let out = regions(&input);
        let garage = by_name(&out, "garage");
        assert_eq!(garage.state, RegionState::Attention, "{}", garage.why);
        assert!(
            garage.why.contains("agent-reviewer") && garage.why.contains("2026-09-19 13:00"),
            "{}",
            garage.why
        );
        let building_third = stuck(&input)
            .into_iter()
            .find(|third| third.third == "actors-building")
            .unwrap();
        assert_eq!(
            (building_third.stuck, building_third.waiting),
            (0, 1),
            "a qualified review is the same owned wait in the third count"
        );
        let borders = crate::borders::borders(&crate::borders::BorderInputs {
            regions: &input,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
            machines_withheld: None,
        });
        let dock = borders
            .borders
            .iter()
            .find(|border| border.from == "dock" && border.to == "track")
            .unwrap();
        assert_eq!(dock.holds_by_class.unwrap().stuck, 0);
        assert_eq!(dock.holds_by_class.unwrap().person, 1);
        assert_eq!(
            serde_json::to_value(&dock.holds[0]).unwrap()["wait"],
            serde_json::to_value(&garage.waits[0]).unwrap(),
            "the rail conserves the same qualified wait evidence"
        );
        let mut invalid_cars = cars.clone();
        invalid_cars[0].1[0].metadata["hold_sha"] = json!("not-an-exact-source-head");
        let mut invalid_runs = runs.clone();
        invalid_runs[0].0.metadata["claim_receipt"]["metadata"]["hold_sha"] =
            json!("not-an-exact-source-head");
        let mut invalid_input = inputs(&status, &[], &[], &invalid_cars, &[], Some(&[]), Some(&[]));
        invalid_input.agent_runs = Some(&invalid_runs);
        invalid_input.internal_protocols = Some(&protocols);
        assert_eq!(
            by_name(&regions(&invalid_input), "garage").state,
            RegionState::Troubled,
            "two matching invalid strings cannot establish an exact source head"
        );
        let mut red_status = status.clone();
        red_status.held_cars[0].kind = crate::yard::CarHoldKind::RegateRed;
        let mut red_input = inputs(&red_status, &[], &[], &cars, &[], Some(&[]), Some(&[]));
        red_input.agent_runs = Some(&runs);
        red_input.internal_protocols = Some(&protocols);
        let red_map = regions(&red_input);
        let red_garage = by_name(&red_map, "garage");
        assert_eq!(
            red_garage.state,
            RegionState::Troubled,
            "a qualified review cannot clear a failed dock re-gate"
        );
        assert_eq!(
            red_garage.waits, garage.waits,
            "retain the independent review evidence beside the fault"
        );
        let red_third = stuck(&red_input)
            .into_iter()
            .find(|third| third.third == "actors-building")
            .unwrap();
        assert_eq!((red_third.stuck, red_third.waiting), (1, 0));
        let red_rails = crate::borders::borders(&crate::borders::BorderInputs {
            regions: &red_input,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
            machines_withheld: None,
        });
        let red_rail = red_rails
            .borders
            .iter()
            .find(|rail| rail.from == "dock" && rail.to == "track")
            .unwrap();
        assert_eq!(red_rail.holds_by_class.unwrap().stuck, 1);
        assert_eq!(red_rail.holds[0].wait.as_ref(), garage.waits.first());
        let mut wrong_head = runs.clone();
        wrong_head[0].0.metadata["claim_receipt"]["metadata"]["hold_sha"] = json!("b".repeat(40));
        input.agent_runs = Some(&wrong_head);
        assert_eq!(
            by_name(&regions(&input), "garage").state,
            RegionState::Troubled
        );
        input.agent_runs = Some(&runs);
        input.now = t("2026-09-19T13:00:00Z");
        assert_eq!(
            by_name(&regions(&input), "garage").state,
            RegionState::Troubled
        );
        input.now = t(NOW);
        let mut drafts = protocols.clone();
        drafts
            .get_mut(&("ship-a-change".into(), 1))
            .unwrap()
            .as_mut()
            .unwrap()
            .status = crate::registry::WorkflowStatus::Draft;
        input.internal_protocols = Some(&drafts);
        let draft_refused = by_name(&regions(&input), "garage").state == RegionState::Troubled;
        input.internal_protocols = Some(&protocols);
        let mut self_review = cars.clone();
        self_review[0].0.metadata["agent_run"] = json!(runs[0].0.id.to_string());
        input.cars = &self_review;
        let self_review_refused =
            by_name(&regions(&input), "garage").state == RegionState::Troubled;
        assert!(
            draft_refused && self_review_refused,
            "draft refused={draft_refused}; builder self-review refused={self_review_refused}"
        );
        input.cars = &cars;
        let mut observed = runs.clone();
        let run_id = observed[0].0.id.to_string();
        let prompt = "a".repeat(64);
        let snapshot = "b".repeat(64);
        observed[0].1[0].metadata[crate::agent_budget::WORKER_START_EXPECTATION] = json!({
            "schema": 2, "minutes": 15, "actor": "agent-reviewer", "observer": "launcher",
            "prompt_sha256": prompt, "snapshot_sha256": snapshot,
        });
        observed[0].0.metadata["worker_started"] = json!({
            "schema": 2, "run": run_id, "actor": "agent-reviewer", "observer": "launcher",
            "prompt_sha256": prompt, "snapshot_sha256": snapshot,
            "binding": {"job_uid": "job", "pod_uid": "pod", "container_id": "containerd://one"},
            "acknowledgment": {"schema": 1, "start_read": true, "run": run_id,
                "prompt_sha256": prompt, "snapshot_sha256": snapshot},
        });
        input.agent_runs = Some(&observed);
        assert_eq!(
            by_name(&regions(&input), "garage").state,
            RegionState::Attention
        );
        let mut unbound_observer = observed.clone();
        unbound_observer[0].0.metadata["worker_started"]["binding"]["pod_uid"] = json!("");
        input.agent_runs = Some(&unbound_observer);
        assert_eq!(
            by_name(&regions(&input), "garage").state,
            RegionState::Troubled
        );
        for (pointer, value) in [
            ("/claim_receipt", Value::Null),
            ("/claim_receipt/id", json!(uuid::Uuid::new_v4())),
            ("/claim_receipt/job_id", json!(uuid::Uuid::new_v4())),
            ("/claim_receipt/spec_slug", json!("landed-review")),
            ("/claim_receipt/status", json!("ready")),
            ("/claim_receipt/assignee_id", json!("another-reviewer")),
            ("/worker_started/schema", json!(2)),
            ("/worker_started/run", json!(uuid::Uuid::new_v4())),
            ("/worker_started/actor", json!("another-reviewer")),
        ] {
            let mut changed = runs.clone();
            *changed[0].0.metadata.pointer_mut(pointer).unwrap() = value;
            let mut input = inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[]));
            input.internal_protocols = Some(&protocols);
            input.agent_runs = Some(&changed);
            assert_eq!(
                by_name(&regions(&input), "garage").state,
                RegionState::Troubled,
                "an invalid registered review reading must require action: {pointer}"
            );
            let third = stuck(&input)
                .into_iter()
                .find(|third| third.third == "actors-building")
                .unwrap();
            assert_eq!(
                (third.stuck, third.waiting),
                (1, 0),
                "invalid review cannot remain an owned wait: {pointer}"
            );
        }
    }

    #[test]
    fn a_hold_with_no_recorded_internal_wait_requires_action_now() {
        let mut status = empty_status();
        status.held.push(crate::yard::HeldGreen {
            branch: "fix/held-without-owner".into(),
            reason: "waiting for review".into(),
            since: "2026-09-19T11:59:00Z".into(),
            packet_id: "held-gate".into(),
            sha: Some("a".repeat(40)),
        });
        let out = regions(&inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[])));
        let garage = by_name(&out, "garage");
        assert_eq!(garage.state, RegionState::Troubled, "{}", garage.why);
        assert!(
            garage.why.contains("held-gate") && garage.why.contains("registered"),
            "{}",
            garage.why
        );
    }

    /// A judged failure is work to repair now, not a deliberate wait.
    /// Its first hour must not look healthy while the age band warms up.
    #[test]
    fn a_failed_gate_requires_action_without_waiting_for_an_age_band() {
        let mut status = empty_status();
        status.garage.push(crate::yard::GaragedCar {
            branch: "fix/failed-check".into(),
            failed_check: Some("test".into()),
            failed_line: Some("assertion failed".into()),
            since: "2026-09-19T11:55:00Z".into(),
            packet_id: "failed-gate".into(),
            sha: Some("a".repeat(40)),
        });
        let out = regions(&inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[])));
        let garage = by_name(&out, "garage");
        assert_eq!(garage.state, RegionState::Troubled, "{}", garage.why);
        assert!(garage.why.contains("fix/failed-check"), "{}", garage.why);
        assert!(garage.why.contains("test"), "{}", garage.why);

        let clear = empty_status();
        let out = regions(&inputs(&clear, &[], &[], &[], &[], Some(&[]), Some(&[])));
        assert_eq!(by_name(&out, "garage").state, RegionState::Clear);
    }

    /// A stranded green — a green gate no car claims — troubles the
    /// garage; a held car alone asks for attention past its hold.
    #[test]
    fn a_stranded_green_troubles_the_garage() {
        let run = job(
            "gate-run",
            "fix/lost",
            JobStatus::Closed,
            json!({ "branch": "fix/lost", "opened_at": "2026-09-19T09:00:00Z", "closed_at": "2026-09-19T09:30:00Z", "outcome": "completed" }),
        );
        let mut verdict = step(
            &run,
            "record-verdict",
            StepStatus::Completed,
            Some("2026-09-19T09:30:00Z"),
        );
        verdict.metadata = json!({ "verdict": "green" });
        let runs = vec![(run.clone(), vec![verdict])];
        let status = build_status_for(
            YardInputs {
                gate_runs: &runs,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        assert_eq!(status.stranded.len(), 1, "the yard's own stranded lane");
        let rows = vec![run];
        let out = regions(&inputs(&status, &[], &[], &[], &rows, Some(&[]), Some(&[])));
        let garage = by_name(&out, "garage");
        assert_eq!(garage.count, Some(1));
        assert_eq!(garage.state, RegionState::Troubled);
        assert!(garage.why.contains("never became a car"), "{}", garage.why);
        assert!(
            garage.why.contains("nothing aimed at any of them"),
            "and with no newer gate-run on that branch, says nobody is on it — which is \
             the fact nine hours of flashing never carried (aae1515e): {}",
            garage.why
        );
    }

    /// A REPAIR IN FLIGHT LOOKS DIFFERENT FROM NEGLECT (backlog
    /// aae1515e; David, 2026-09-21: "it is either too long to have not
    /// addressed or we need to have some indication that the fix is in
    /// transit").
    ///
    /// The garage went troubled at 08:50Z on a lost gate-run and read
    /// identically at minute five and at hour nine, so nine hours
    /// passed. Still TROUBLED here — deliberately not a fourth state,
    /// which would force every consumer to learn a new value — but the
    /// sentence now answers the reader's actual question.
    #[test]
    fn a_stranded_green_with_a_newer_gate_run_says_a_repair_is_in_flight() {
        let run = job(
            "gate-run",
            "fix/lost",
            JobStatus::Closed,
            json!({ "branch": "fix/lost", "opened_at": "2026-09-19T09:00:00Z", "closed_at": "2026-09-19T09:30:00Z", "outcome": "completed" }),
        );
        let mut verdict = step(
            &run,
            "record-verdict",
            StepStatus::Completed,
            Some("2026-09-19T09:30:00Z"),
        );
        verdict.metadata = json!({ "verdict": "green" });
        let runs = vec![(run.clone(), vec![verdict])];
        let status = build_status_for(
            YardInputs {
                gate_runs: &runs,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        assert_eq!(status.stranded.len(), 1);

        // The repair: a SECOND, still-open gate-run on the same branch.
        let repair = job(
            "gate-run",
            "fix/lost",
            JobStatus::Open,
            json!({ "branch": "fix/lost", "opened_at": "2026-09-19T11:00:00Z" }),
        );
        let rows = vec![run.clone(), repair];
        let out = regions(&inputs(&status, &[], &[], &[], &rows, Some(&[]), Some(&[])));
        let garage = by_name(&out, "garage");
        assert_eq!(
            garage.state,
            RegionState::Troubled,
            "a repair in flight does not make it well — the green is still stranded"
        );
        assert!(
            garage.why.contains("under repair"),
            "but it says someone is on it: {}",
            garage.why
        );
        assert!(
            !garage.why.contains("nothing aimed"),
            "and drops the sentence that would send a reader to look: {}",
            garage.why
        );

        // THE CONTROL, on the same fixture: without the newer run it
        // reads as neglect again. Without this the annotation could be
        // unconditional and nobody would notice.
        let alone = vec![run];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &alone,
            Some(&[]),
            Some(&[]),
        ));
        let garage = by_name(&out, "garage");
        assert!(
            garage.why.contains("nothing aimed at any of them"),
            "the same stranded green with no repair reads as nobody on it: {}",
            garage.why
        );
    }
}
