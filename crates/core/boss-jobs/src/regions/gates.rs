//! THE GATES: the bays in use, read against the policy's bound.

use super::*;

fn recorded_gate_wait(
    active: &crate::yard::ActiveGate,
    inputs: &RegionInputs<'_>,
) -> Result<RecordedProofWait, String> {
    let run = inputs
        .gate_runs
        .iter()
        .find(|run| run.id.to_string() == active.packet_id)
        .ok_or("registered gate packet is unread")?;
    if run.kind != "gate-run"
        || run.status != JobStatus::Open
        || crate::yard::queued_for_a_slot(run)
    {
        return Err("gate has no matching open execution record".into());
    }
    let owner = run.owner_id.as_str();
    if owner.trim().is_empty() {
        return Err("gate action owner is unread".into());
    }
    let head = super::judging::exact_source_head(run.metadata.get("sha"))
        .ok_or("gate exact source head is unread")?;
    if run.metadata.get("branch").and_then(Value::as_str) != Some(active.branch.as_str()) {
        return Err("gate branch does not match its registered execution".into());
    }
    let launched_at = meta_instant(&run.metadata, crate::yard::LAUNCHED_AT)
        .filter(|at| *at <= inputs.now)
        .ok_or("gate observed launch clock is unread or in the future")?;
    if active.launched_at.as_deref().and_then(stamp_instant) != Some(launched_at) {
        return Err("gate launch reading does not match the registered execution".into());
    }
    let protocols = inputs
        .internal_protocols
        .ok_or("pinned gate protocol is unread")?;
    let protocol = match protocols.get(&(run.kind.clone(), run.workflow_version)) {
        Some(Ok(spec))
            if spec.kind == run.kind
                && spec.version == run.workflow_version
                && spec.status != crate::registry::WorkflowStatus::Draft =>
        {
            spec
        }
        Some(Err(reason)) => return Err(reason.clone()),
        _ => return Err("exact published gate protocol is unread".into()),
    };
    let hours = protocol
        .steps
        .iter()
        .find(|step| step.title == "record-verdict")
        .and_then(|step| step.duration_hours)
        .filter(|hours| hours.is_finite() && *hours > 0.0)
        .ok_or("pinned gate receipt bound is unread")?;
    let next_check_at = chrono::Duration::try_seconds((hours * 3600.0).ceil() as i64)
        .and_then(|duration| launched_at.checked_add_signed(duration))
        .ok_or("gate deadline exceeds the clock range")?;
    if next_check_at <= inputs.now {
        return Err(format!(
            "gate receipt is overdue at its pinned bound {next_check_at}"
        ));
    }
    Ok(RecordedProofWait {
        packet: run.id.to_string(),
        owner: owner.into(),
        condition_owner: owner.into(),
        title: run.title.clone(),
        condition: format!("gate receipt at {head}"),
        next_check_at,
        evidence: ProofWaitEvidence::Gate {
            run: run.id.to_string(),
            head: head.into(),
            workflow_version: protocol.version,
            launched_at,
        },
    })
}

/// THE GATES: bays in use, of the policy's bound. Troubled when a bay
/// holds a corpse (`stale` — active past the gate Job's own deadline,
/// `yard::GATE_MAX_ACTIVE_HOURS`) or every bay is in use and no verdict
/// is landing; FULL when every bay is in use and verdicts keep landing
/// (`out`, the rails leaving the gates — [`at_capacity`]); attention when
/// the line for a bay has stood past twice the gate duration. The trend
/// is the gate duration, opened to judged, for runs judged in each
/// window.
pub(super) fn gates(inputs: &RegionInputs<'_>, w: &Windows, out: &[OutRail]) -> Region {
    let g = &inputs.status.gates;
    let capacity = usize::try_from(g.capacity).unwrap_or(0);
    let active = g.active.len();
    // A gate's duration is its RUNNING time, from its Job to its verdict
    // (`yard::run_started`) — a wait in line is the line's, and counted
    // here it taught the floor that gates were slow (backlog 4d088a7e).
    let durations = inputs.gate_runs.iter().filter_map(|run| {
        let started = crate::yard::run_started(run)?;
        let closed = closed_at(run)?;
        let d = (closed - started).num_seconds();
        (d > 0).then_some((closed, d))
    });
    let (cur, prev) = split(w, durations);
    let trend = duration_trend("gate duration", "minutes", cur, prev);
    // When a bay's run began: its Job, else its filing (a run filed
    // before the launch stamp existed).
    let started = |a: &crate::yard::ActiveGate| {
        a.launched_at
            .as_deref()
            .and_then(stamp_instant)
            .or_else(|| stamp_instant(&a.since))
    };
    let stale: Vec<&crate::yard::ActiveGate> = g.active.iter().filter(|a| a.stale).collect();
    let mut findings = Vec::new();
    let mut unread = Vec::new();
    let mut waits = Vec::new();
    for active in &g.active {
        match recorded_gate_wait(active, inputs) {
            Ok(wait) => waits.push(wait),
            Err(reason) => unread.push(Finding::new(
                bands::UNREAD,
                None,
                String::new(),
                format!("{}: {reason}", active.packet_id),
            )),
        }
    }
    if !stale.is_empty() {
        findings.push(Finding::new(
            bands::GATES_CORPSE,
            // A run became a corpse the moment it outlived the deadline,
            // which its Job's clock keeps, not its packet's.
            stale
                .iter()
                .filter_map(|a| started(a))
                .min()
                .map(|s| s + chrono::Duration::hours(crate::yard::GATE_MAX_ACTIVE_HOURS)),
            format!(
                "{} active past {}h",
                plural(stale.len(), "run", "runs"),
                crate::yard::GATE_MAX_ACTIVE_HOURS
            ),
            format!(
                "{} active past the gate deadline — a corpse holding a bay",
                plural(stale.len(), "run", "runs")
            ),
        ));
    }
    findings.extend(unread);
    // THE BAY RUNNING PAST TWICE THE MEDIAN (backlog 4d088a7e) — the
    // yard's own per-bay judgement (`ActiveGate::troubled`), read here
    // rather than re-derived, dated from when the first such run crossed
    // the line. Its RUNNING age, never the packet's: on 2026-09-28 a gate
    // queued 46 minutes and running 14 read as "going for an hour".
    let long: Vec<&crate::yard::ActiveGate> = g
        .active
        .iter()
        .filter(|a| a.troubled && !waits.iter().any(|wait| wait.packet == a.packet_id))
        .collect();
    if let (Some(median), Some(longest)) = (
        g.typical_seconds,
        long.iter().filter_map(|a| a.running_seconds).max(),
    ) {
        let limit = chrono::Duration::seconds(median * bands::RUN_PAST_MEDIAN_TIMES);
        findings.push(Finding::new(
            bands::GATES_RUN_LONG,
            long.iter()
                .filter_map(|a| started(a))
                .min()
                .map(|s| s + limit),
            format!("running {}", bands::duration_text(longest / 60)),
            format!(
                "{} running past {}× the {} minutes a gate runs, the longest for {}",
                plural(long.len(), "run", "runs"),
                bands::RUN_PAST_MEDIAN_TIMES,
                number_text((median as f64 / 60.0).round()),
                bands::duration_text(longest / 60)
            ),
        ));
    }
    // THE TRAIN UNDER TEST stands here (design e765b3fc §2a, car R1): a
    // train at `ci` is at the gates, its own gate riding it, and its own
    // trouble — a red PR, a gate not yet filed — is read where it stands.
    let under_test = trains_in(inputs, "gates");
    findings.extend(train_findings(inputs, &under_test));
    let bays = if g.queued.is_empty() {
        format!("{active} of {capacity} bays in use")
    } else {
        format!(
            "{active} of {capacity} bays in use, {} waiting for a slot",
            plural(g.queued.len(), "run", "runs")
        )
    };
    let what = if under_test.is_empty() {
        bays
    } else {
        let titles: Vec<&str> = under_test.iter().map(|(j, _)| j.title.as_str()).collect();
        format!("{bays} — {} under test", titles.join(", "))
    };
    // EVERY BAY IN USE is the gates doing their job (design e765b3fc
    // §4a, car F1): full while verdicts keep landing, stuck when they do
    // not. The bays filled when the bay that filled them opened
    // ([`onset_of_count`]). `gates-at-bound` turned this amber after 30m
    // until 2026-09-25 — most of a working day, for being used.
    findings.extend(at_capacity(
        active,
        capacity,
        &what,
        onset_of_count(g.active.iter().filter_map(started).collect(), capacity),
        out,
        w.hours,
        inputs.now,
    ));
    // THE LINE, not the bays: worth a look only when its oldest wait has
    // stood past twice what a gate takes now, which is a line growing
    // faster than the bays drain it. A gate duration nobody measured in
    // the window judges no line — a wait against an unknown is not long.
    if let Some(service) = trend.current {
        let oldest = g
            .queued
            .iter()
            .filter_map(|q| stamp_instant(&q.queued_at))
            .min();
        #[allow(clippy::cast_possible_truncation)]
        let limit = (service * bands::LINE_PAST_SERVICE_TIMES as f64).ceil() as i64;
        if let Some(oldest) = oldest.filter(|o| (inputs.now - *o).num_minutes() > limit) {
            let waited = (inputs.now - oldest).num_minutes();
            findings.push(Finding::new(
                bands::GATES_LINE_LONG,
                Some(oldest + chrono::Duration::minutes(limit)),
                format!("oldest wait {}", bands::duration_text(waited)),
                format!(
                    "{} waiting for a bay, the oldest for {} — past {}× the {} minutes a gate takes now",
                    plural(g.queued.len(), "run", "runs"),
                    bands::duration_text(waited),
                    bands::LINE_PAST_SERVICE_TIMES,
                    number_text(service)
                ),
            ));
        }
    }
    let settled = settle(findings, what, inputs.now);
    let queue_wait = queue_wait(inputs, w);
    let kpi = vec![
        measure_said(
            "bays in use",
            count_value(active),
            "bays",
            format!("{active} of {capacity} bays in use"),
        ),
        match trend.current {
            Some(m) => measure_said(
                "gate duration",
                Some(m),
                "minutes",
                format!("gates take {} minutes (median)", number_text(m)),
            ),
            None => measure_said(
                "gate duration",
                None,
                "minutes",
                format!("no gate judged in {}h", w.hours),
            ),
        },
        queue_wait,
    ];
    // The count is the bays in use, with a train under test counted once
    // as the train rather than as its gate-run. The line (a run waiting
    // for a bay, a train with no gate-run yet) stands here too, as the
    // second figure `waiting` (backlog 357fe1da), and a train whose gate
    // has spoken as the third, `judged` (backlog bce67f09) — so the
    // three sum to the members.
    let at = at_the_gates(inputs);
    let figure = |ids: Option<&Ids>| ids.filter(|ids| !ids.is_empty()).map(Ids::len);
    Region {
        waits,
        waiting: figure(at.as_ref().map(|a| &a.line)),
        judged: figure(at.as_ref().map(|a| &a.judged)),
        ..region(
            "gates",
            Some(at.as_ref().map_or(active, |a| a.bays.len())),
            Some((capacity, BoundKind::Capacity)),
            "bays in use",
            settled,
            trend,
            kpi,
        )
    }
}

/// THE QUEUE WAIT (backlog 357fe1da, design 85c3f793 decision 3): the
/// median of `launched_at - opened_at` over the window, from the
/// gate-runs the region already reads — the number the bay-count
/// measurement (366c2ed5) wants. A run that died in line (settled `lost`
/// with no launch, still carrying `queued_at`) waited until it closed and
/// is also counted as lost in line, because a death in line is the most
/// expensive wait there is. While a line stands, its oldest wait sits
/// beside the median. A launched run's wait needs no `queued_at`, which
/// is the waiting MARKER and is cleared at launch (decision 4); an
/// unlaunched one does, because that marker is the only evidence it ever
/// stood in line — a run lost before it took a place died somewhere
/// else, and until backlog bce67f09 it was counted lost in line too.
fn queue_wait(inputs: &RegionInputs<'_>, w: &Windows) -> Measure {
    let waits = inputs.gate_runs.iter().filter_map(|run| {
        let opened = meta_instant(&run.metadata, "opened_at")?;
        match meta_instant(&run.metadata, crate::yard::LAUNCHED_AT) {
            Some(launched) => Some((launched, (launched - opened).num_seconds().max(0), false)),
            None => {
                let lost = md_str(&run.metadata, "outcome") == "lost"
                    && crate::yard::queued_for_a_slot(run);
                let closed = closed_at(run).filter(|_| lost)?;
                Some((closed, (closed - opened).num_seconds().max(0), true))
            }
        }
    });
    let waits: Vec<(Instant, i64, bool)> = waits.collect();
    let lost = waits
        .iter()
        .filter(|(at, _, lost)| *lost && w.current(*at))
        .count();
    let (cur, prev) = split(w, waits.iter().map(|(at, s, _)| (*at, *s)));
    let median = duration_trend("queue wait", "minutes", cur, prev).current;
    let mut text = match median {
        Some(m) => format!(
            "gates wait {} minutes for a bay (median)",
            number_text(m.round())
        ),
        None => format!("no gate launched in {}h", w.hours),
    };
    if lost > 0 {
        text.push_str(&format!(", {} lost in line", plural(lost, "run", "runs")));
    }
    let oldest = inputs
        .status
        .gates
        .queued
        .iter()
        .filter_map(|q| stamp_instant(&q.queued_at))
        .min();
    if let Some(oldest) = oldest {
        text.push_str(&format!(
            ", the oldest in line now {}",
            bands::duration_text((inputs.now - oldest).num_minutes())
        ));
    }
    measure_said("queue wait", median, "minutes", text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    #[test]
    fn a_registered_exact_head_gate_inside_its_pinned_bound_is_an_owned_wait() {
        let head = "a".repeat(40);
        let run = job(
            "gate-run",
            "fix/bounded",
            JobStatus::Open,
            json!({
                "branch": "fix/bounded", "sha": head, "launched_at": "2026-09-19T11:30:00Z",
                "opened_at": "2026-09-19T11:29:00Z",
            }),
        );
        let mut status = empty_status();
        status.gates.typical_seconds = Some(600);
        status.gates.active.push(crate::yard::ActiveGate {
            branch: "fix/bounded".into(),
            packet_id: run.id.to_string(),
            since: "2026-09-19T11:29:00Z".into(),
            stale: false,
            train: None,
            launched_at: Some("2026-09-19T11:30:00Z".into()),
            queued_seconds: Some(60),
            running_seconds: Some(1800),
            troubled: true,
        });
        let protocols = [WorkflowSpec::platform_seed(
            "gate-run",
            "Gate",
            "platform",
            vec!["custom".into()],
            vec![crate::registry::StepSpec {
                title: "record-verdict".into(),
                duration_hours: Some(3.0),
                ..Default::default()
            }],
        )]
        .into_iter()
        .map(|spec| ((spec.kind.clone(), spec.version), Ok(spec)))
        .collect::<PinnedProtocols>();
        let runs = [run];
        let mut input = inputs(&status, &[], &[], &[], &runs, Some(&[]), Some(&[]));
        input.internal_protocols = Some(&protocols);
        let map = regions(&input);
        let gate = by_name(&map, "gates");
        assert_eq!(
            gate.state,
            RegionState::Clear,
            "the measured median cannot turn healthy gate execution inside its recorded bound into a fault: {}",
            gate.why
        );
        assert_eq!(gate.waits.len(), 1);
        assert_eq!(gate.waits[0].packet, runs[0].id.to_string());
        assert_eq!(gate.waits[0].owner, runs[0].owner_id);
        assert_eq!(gate.waits[0].next_check_at, t("2026-09-19T14:30:00Z"));
        input.internal_protocols = None;
        let unread_map = regions(&input);
        let unread = by_name(&unread_map, "gates");
        assert_eq!(unread.state, RegionState::Troubled);
        assert!(
            unread.why.contains("pinned gate protocol is unread"),
            "an unavailable required reading must name that defect: {}",
            unread.why
        );
        assert!(unread.waits.is_empty());
        input.internal_protocols = Some(&protocols);
        let mut overdue = input;
        overdue.now = t("2026-09-19T14:30:00Z");
        let overdue_map = regions(&overdue);
        let overdue_gate = by_name(&overdue_map, "gates");
        assert_eq!(overdue_gate.state, RegionState::Troubled);
        assert!(overdue_gate.why.contains("pinned bound"));
        assert!(overdue_gate.waits.is_empty());
    }

    /// The gates: a stale bay is trouble; full bays settle into attention; the trend is
    /// the run duration from the rows' own stamps.
    #[test]
    fn the_gates_read_the_bound_and_a_stale_bay_is_trouble() {
        let run = |branch: &str, opened: &str, closed: Option<&str>, outcome: &str| {
            let mut md = json!({ "branch": branch, "opened_at": opened });
            if let Some(c) = closed {
                md["closed_at"] = json!(c);
                md["outcome"] = json!(outcome);
            }
            job(
                "gate-run",
                branch,
                if closed.is_some() {
                    JobStatus::Closed
                } else {
                    JobStatus::Open
                },
                md,
            )
        };
        let runs = vec![
            run(
                "fix/a",
                "2026-09-19T09:00:00Z",
                Some("2026-09-19T09:30:00Z"),
                "completed",
            ),
            run(
                "fix/b",
                "2026-09-19T10:00:00Z",
                Some("2026-09-19T10:10:00Z"),
                "failed",
            ),
            run(
                "fix/c",
                "2026-09-18T10:00:00Z",
                Some("2026-09-18T11:00:00Z"),
                "failed",
            ),
            // A corpse: active since well past GATE_MAX_ACTIVE_HOURS.
            run("fix/d", "2026-09-19T01:00:00Z", None, ""),
        ];
        let with_steps: Vec<(Job, Vec<Step>)> = runs
            .iter()
            .map(|j| {
                (
                    j.clone(),
                    vec![step(j, "record-verdict", StepStatus::Ready, None)],
                )
            })
            .collect();
        let status = build_status_for(
            YardInputs {
                gate_runs: &with_steps,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        let out = regions(&inputs(&status, &[], &[], &[], &runs, Some(&[]), Some(&[])));
        let gates = by_name(&out, "gates");
        // No policy in the inputs, so the bound is the compiled
        // fallback — read off the constant, not retyped, because it
        // moves with the policy (3 -> 4 on 2026-09-28, backlog 366c2ed5).
        let bound = usize::try_from(crate::yard::COMPILED_GATE_MAX_CONCURRENT).unwrap();
        assert_eq!(gates.count, Some(1));
        assert_eq!(gates.bound, Some(bound));
        assert_eq!(gates.state, RegionState::Troubled, "{}", gates.why);
        assert!(gates.why.contains("corpse"), "{}", gates.why);
        // The run became a corpse when it outlived the deadline: opened
        // 01:00, deadline 3h, so a corpse since 04:00 — eight hours.
        let band = gates.band.as_ref().unwrap();
        assert_eq!(band.id, "gates-corpse");
        assert_eq!(band.held_minutes, Some(8 * 60));
        assert_eq!(gates.trend.metric, "gate duration");
        assert_eq!(gates.trend.current, Some(30.0));
        assert_eq!(gates.trend.previous, Some(60.0));
        assert_eq!(gates.kpi[0].text, format!("1 of {bound} bays in use"));
        assert_eq!(gates.kpi[1].text, "gates take 30 minutes (median)");
        // The garage's trend: reds per day.
        let garage = by_name(&out, "garage");
        assert_eq!(garage.trend.metric, "reds");
        assert_eq!(garage.trend.current, Some(1.0));
        assert_eq!(garage.trend.previous, Some(1.0));
    }

    // --- full, stuck, and the line (design e765b3fc §4a, car F1) ------

    /// A red gate-run judged at `closed`, twenty minutes after it opened:
    /// one crossing of `gates -> garage`, and one gate-duration sample.
    fn red(closed: &str) -> Job {
        let closed_at = t(closed);
        let opened = (closed_at - chrono::Duration::minutes(20)).to_rfc3339();
        job(
            "gate-run",
            &format!("fix/red-{closed}"),
            JobStatus::Closed,
            json!({ "opened_at": opened, "closed_at": closed, "outcome": "failed" }),
        )
    }

    /// Twenty-four verdicts a quarter hour apart, the last at `last` — a
    /// mean gap of an hour over the day's window.
    fn verdicts_until(last: &str) -> Vec<Job> {
        let last = t(last);
        (0..24)
            .map(|i| red(&(last - chrono::Duration::minutes(15 * i)).to_rfc3339()))
            .collect()
    }

    /// The gates with every bay in use, filled by the bay that opened at
    /// `filled`, and `line` runs waiting since `queued`.
    fn every_bay(filled: &str, line: &[&str]) -> YardStatus {
        let gate = |branch: String, since: &str| crate::yard::ActiveGate {
            packet_id: branch.clone(),
            branch,
            since: since.into(),
            stale: false,
            train: None,
            ..Default::default()
        };
        let mut status = empty_status();
        status.gates.capacity = 3;
        status.gates.active = ["2026-09-19T05:00:00Z", "2026-09-19T05:10:00Z", filled]
            .iter()
            .enumerate()
            .map(|(i, s)| gate(format!("fix/{i}"), s))
            .collect();
        status.gates.queued = line
            .iter()
            .enumerate()
            .map(|(i, q)| crate::yard::QueuedGate {
                branch: format!("fix/q{i}"),
                packet_id: format!("q{i}"),
                queued_at: (*q).into(),
                position: i32::try_from(i + 1).unwrap(),
                waiting_seconds: None,
                estimated_wait_seconds: None,
                train: None,
            })
            .collect();
        status
    }

    fn read_gates(status: &YardStatus, runs: &[Job]) -> Region {
        // These capacity/line fixtures have real registered execution rows,
        // independent of the closed verdict history being measured.
        let mut status = status.clone();
        let mut runs = runs.to_vec();
        for active in &mut status.gates.active {
            let launched = active.launched_at.clone().unwrap_or_else(|| {
                stamp_instant(&active.since)
                    .unwrap()
                    .max(t(NOW) - chrono::Duration::hours(2))
                    .to_rfc3339()
            });
            let run = job(
                "gate-run",
                &active.branch,
                JobStatus::Open,
                json!({
                    "branch": active.branch, "sha": "a".repeat(40),
                    "opened_at": active.since, "launched_at": launched,
                }),
            );
            active.packet_id = run.id.to_string();
            active.launched_at = Some(launched);
            runs.push(run);
        }
        let protocol = WorkflowSpec::platform_seed(
            "gate-run",
            "Gate",
            "platform",
            vec!["custom".into()],
            vec![crate::registry::StepSpec {
                title: "record-verdict".into(),
                duration_hours: Some(3.0),
                ..Default::default()
            }],
        );
        let protocols = [((protocol.kind.clone(), protocol.version), Ok(protocol))].into();
        let mut input = inputs(&status, &[], &[], &[], &runs, Some(&[]), Some(&[]));
        input.internal_protocols = Some(&protocols);
        by_name(&regions(&input), "gates").clone()
    }

    /// EVERY BAY IN USE, VERDICTS LANDING: FULL — the good state, which
    /// until car F1 turned amber after half an hour ("gates-at-bound"). A
    /// line behind it is expected; it stays a pile on the approach.
    #[test]
    fn every_bay_in_use_with_verdicts_landing_reads_full() {
        let status = every_bay("2026-09-19T11:20:00Z", &["2026-09-19T11:50:00Z"]);
        let g = read_gates(&status, &verdicts_until("2026-09-19T11:55:00Z"));
        assert_eq!(g.state, RegionState::Full, "{}", g.why);
        let band = g.band.as_ref().expect("full names its band");
        assert_eq!(band.id, "full");
        assert_eq!(
            band.held_minutes,
            Some(40),
            "full since the bay that filled it"
        );
        assert!(
            g.why
                .starts_with("3 of 3 bays in use, 1 run waiting for a slot"),
            "{}",
            g.why
        );
        assert_eq!(g.bound_kind, Some(BoundKind::Capacity));
        // The retired band cannot be named any more.
        assert!(bands::BANDS.iter().all(|b| b.id != "gates-at-bound"));
    }

    /// Old admission/queue clocks do not make recently launched,
    /// registered executions inside their pinned bounds overdue.
    #[test]
    fn bounded_current_executions_do_not_inherit_the_old_queue_deadline() {
        let status = every_bay("2026-09-19T05:50:00Z", &[]);
        let g = read_gates(&status, &verdicts_until("2026-09-19T05:45:00Z"));
        assert_eq!(g.state, RegionState::Full, "{}", g.why);
        assert_eq!(g.waits.len(), 3);
    }

    /// THE LINE, NOT THE BAYS: a run waiting an hour when a gate takes
    /// twenty minutes has stood past twice the gate duration — attention,
    /// which outranks full, dated from when it crossed (11:00 + 40m).
    #[test]
    fn a_line_past_twice_the_gate_duration_asks_for_attention() {
        let status = every_bay("2026-09-19T11:20:00Z", &["2026-09-19T11:00:00Z"]);
        let g = read_gates(&status, &verdicts_until("2026-09-19T11:55:00Z"));
        assert_eq!(g.state, RegionState::Attention, "{}", g.why);
        let band = g.band.as_ref().unwrap();
        assert_eq!(band.id, "gates-line-long");
        assert_eq!(band.reads, "oldest wait 1h > 2× the median gate duration");
        assert_eq!(band.since.as_deref(), Some("2026-09-19T11:40:00+00:00"));
        assert!(g.why.contains("20 minutes a gate takes now"), "{}", g.why);
        // The same line inside twice the duration asks for nothing.
        let status = every_bay("2026-09-19T11:20:00Z", &["2026-09-19T11:30:00Z"]);
        let g = read_gates(&status, &verdicts_until("2026-09-19T11:55:00Z"));
        assert_eq!(g.state, RegionState::Full, "{}", g.why);
    }

    /// A measured median is context; a valid execution's pinned bound
    /// decides when its receipt is overdue. Queue time is not runtime.
    #[test]
    fn a_bay_past_the_median_inside_its_pinned_bound_remains_healthy() {
        let mut status = every_bay("2026-09-19T11:20:00Z", &[]);
        status.gates.typical_seconds = Some(1200);
        status.gates.active[0].launched_at = Some("2026-09-19T11:14:00Z".into());
        status.gates.active[0].running_seconds = Some(41 * 60);
        status.gates.active[0].troubled = true;
        status.gates.active[1].launched_at = Some("2026-09-19T11:50:00Z".into());
        status.gates.active[1].queued_seconds = Some(3600);
        status.gates.active[1].running_seconds = Some(5 * 60);
        let g = read_gates(&status, &verdicts_until("2026-09-19T11:55:00Z"));
        assert_eq!(g.state, RegionState::Full, "{}", g.why);
        assert_eq!(g.waits.len(), 3);
        // The same bays with nothing past the line read full, as before.
        status.gates.active[0].troubled = false;
        let g = read_gates(&status, &verdicts_until("2026-09-19T11:55:00Z"));
        assert_eq!(g.state, RegionState::Full, "{}", g.why);
    }

    /// A corpse is dated from its JOB's deadline: a run that waited two
    /// hours in line became a corpse three hours after it launched, not
    /// three hours after it was filed.
    #[test]
    fn a_corpse_is_dated_from_its_launch() {
        let mut status = every_bay("2026-09-19T11:20:00Z", &[]);
        status.gates.active[0].stale = true;
        status.gates.active[0].launched_at = Some("2026-09-19T07:00:00Z".into());
        let g = read_gates(&status, &verdicts_until("2026-09-19T11:55:00Z"));
        let band = g.band.as_ref().unwrap();
        assert_eq!(band.id, "gates-corpse");
        assert_eq!(band.since.as_deref(), Some("2026-09-19T10:00:00+00:00"));
    }

    /// A GATE-RUN WAITING FOR A BAY STANDS IN LINE AT THE GATES (backlog
    /// 357fe1da, design 85c3f793 decisions 1 and 2). Measured 2026-09-28:
    /// gate-run 40385690 queued 06:11-06:29 and was settled lost at 06:50
    /// standing on no region, first seen in the garage. The line is placed
    /// at the gates and drawn as a second figure, `waiting` — never counted
    /// against the bays, because three in line behind three full bays is
    /// not "5 of 3 bays in use".
    #[test]
    fn a_run_waiting_for_a_bay_stands_in_line_at_the_gates_and_is_not_a_bay() {
        let status = every_bay(
            "2026-09-19T11:20:00Z",
            &["2026-09-19T11:40:00Z", "2026-09-19T11:50:00Z"],
        );
        let i = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let lookups = Lookups::of(&i);
        assert_eq!(place("q0", &lookups), Placed::At("gates"));
        assert_eq!(place("q1", &lookups), Placed::At("gates"));
        let g = read_gates(&status, &verdicts_until("2026-09-19T11:55:00Z"));
        assert_eq!(g.count, Some(3), "the bays in use, not the line: {}", g.why);
        assert_eq!(g.waiting, Some(2), "the line is its own figure");
        assert_eq!(g.kpi[0].text, "3 of 3 bays in use");
        // No line, no figure: a region without one says nothing.
        let status = every_bay("2026-09-19T11:20:00Z", &[]);
        let g = read_gates(&status, &verdicts_until("2026-09-19T11:55:00Z"));
        assert_eq!(g.waiting, None);
    }

    /// A TRAIN'S OWN GATE IN LINE RIDES THE TRAIN, as it does in a bay:
    /// the train under test is the packet at the gates, counted once, and
    /// in the line rather than a bay while its gate has none.
    #[test]
    fn a_train_gate_in_line_rides_the_train_under_test() {
        let train = job("pr-train", "train under test", JobStatus::Open, json!({}));
        let train_steps = vec![
            step(
                &train,
                "pr",
                StepStatus::Completed,
                Some("2026-09-19T11:01:00Z"),
            ),
            step(&train, "ci", StepStatus::Ready, None),
        ];
        let train_id = train.id.to_string();
        let open = vec![(train, train_steps)];
        let mut status = build_status_for(
            YardInputs {
                open_trains: &open,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        status.gates.capacity = 3;
        status.gates.queued = vec![crate::yard::QueuedGate {
            branch: "train/x".into(),
            packet_id: "train-gate".into(),
            queued_at: "2026-09-19T11:30:00Z".into(),
            position: 1,
            waiting_seconds: None,
            estimated_wait_seconds: None,
            train: Some(train_id.clone()),
        }];
        let i = inputs(&status, &open, &[], &[], &[], Some(&[]), Some(&[]));
        let lookups = Lookups::of(&i);
        assert_eq!(place(&train_id, &lookups), Placed::At("gates"));
        assert_eq!(
            place("train-gate", &lookups),
            Placed::Aboard(train_id.clone())
        );
        let g = by_name(&regions(&i), "gates").clone();
        assert_eq!(g.count, Some(0), "no bay is in use: {}", g.why);
        assert_eq!(g.waiting, Some(1), "the train stands in line");
        assert_eq!(g.judged, None);
    }

    /// A TRAIN WHOSE GATE HAS SPOKEN NEEDS NO BAY AND WAITS IN NO LINE
    /// (backlog bce67f09, from landed-review b5ea94dd). The line is the
    /// queued gate-runs plus a train under test with NO gate-run; a train
    /// whose own gate is judged — green and awaiting forge CI or the
    /// merge, or red and blocked — stands at the gates as the third
    /// figure, `judged`. Until then it read "0 bays in use, 1 waiting",
    /// which the "N waiting for a slot" text, counting only queued runs,
    /// contradicted.
    #[test]
    fn a_train_whose_gate_has_spoken_is_judged_not_in_line() {
        let train = job("pr-train", "train judged", JobStatus::Open, json!({}));
        // CI still pending: the `ci` step is not done, only the gate is.
        let train_steps = vec![
            step(
                &train,
                "pr",
                StepStatus::Completed,
                Some("2026-09-19T11:01:00Z"),
            ),
            step(&train, "ci", StepStatus::Ready, None),
        ];
        let train_id = train.id.to_string();
        let open = vec![(train, train_steps)];
        let status = build_status_for(
            YardInputs {
                open_trains: &open,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        let gate = |outcome: &str| {
            job(
                "gate-run",
                "train/x",
                JobStatus::Closed,
                json!({
                    "train_gate": true,
                    "train": train_id,
                    "opened_at": "2026-09-19T11:02:00Z",
                    "closed_at": "2026-09-19T11:20:00Z",
                    "outcome": outcome,
                }),
            )
        };
        let read = |runs: &[Job]| {
            let i = inputs(&status, &open, &[], &[], runs, Some(&[]), Some(&[]));
            let g = by_name(&regions(&i), "gates").clone();
            (g.count, g.waiting, g.judged)
        };
        assert_eq!(
            read(&[gate("completed")]),
            (Some(0), None, Some(1)),
            "a green train gate, forge CI still out"
        );
        assert_eq!(
            read(&[gate("failed")]),
            (Some(0), None, Some(1)),
            "a red train gate"
        );
        // A gate that produced no verdict judged nothing: the train waits
        // for its next one, in line.
        assert_eq!(read(&[gate("lost")]), (Some(0), Some(1), None));
        // No gate-run at all: in line.
        assert_eq!(read(&[]), (Some(0), Some(1), None));
    }

    /// THE QUEUE WAIT (design 85c3f793 decision 3): the median of
    /// `launched_at - opened_at` over the window, from the gate-runs the
    /// region already reads; a run that died in line (closed `lost`, never
    /// launched) waits until it closed and is counted as lost in line.
    #[test]
    fn the_queue_wait_is_the_median_wait_and_a_death_in_line_is_counted() {
        let run =
            |name: &str, opened: &str, launched: Option<&str>, closed: &str, outcome: &str| {
                let mut md =
                    json!({ "opened_at": opened, "closed_at": closed, "outcome": outcome });
                match launched {
                    Some(l) => md["launched_at"] = json!(l),
                    // Never launched: it took a place in line at filing,
                    // and the marker is cleared only at launch.
                    None => md["queued_at"] = json!(opened),
                }
                job("gate-run", name, JobStatus::Closed, md)
            };
        let runs = [
            // Waited 2, 10 and 30 minutes, then ran.
            run(
                "fix/a",
                "2026-09-19T09:00:00Z",
                Some("2026-09-19T09:02:00Z"),
                "2026-09-19T09:30:00Z",
                "completed",
            ),
            run(
                "fix/b",
                "2026-09-19T10:00:00Z",
                Some("2026-09-19T10:10:00Z"),
                "2026-09-19T10:40:00Z",
                "failed",
            ),
            // Died in line: filed 11:00, settled lost 11:39 — a 39-minute
            // wait, and the most expensive one.
            run(
                "fix/lost",
                "2026-09-19T11:00:00Z",
                None,
                "2026-09-19T11:39:00Z",
                "lost",
            ),
        ];
        let status = empty_status();
        // Lost WITHOUT ever standing in line (backlog bce67f09): no launch
        // and no `queued_at` — it died before it took a place, so it is
        // neither a wait nor a death in line.
        let mut never_queued = run(
            "fix/never-queued",
            "2026-09-19T11:10:00Z",
            None,
            "2026-09-19T11:50:00Z",
            "lost",
        );
        never_queued.metadata["queued_at"] = Value::Null;
        let with_unqueued: Vec<Job> = runs.iter().cloned().chain([never_queued]).collect();
        let g = read_gates(&status, &with_unqueued);
        let wait = g
            .kpi
            .iter()
            .find(|m| m.name == "queue wait")
            .unwrap_or_else(|| panic!("no queue-wait measure: {:?}", g.kpi));
        assert_eq!(wait.unit, "minutes");
        assert_eq!(wait.value, Some(10.0), "the median of 2, 10 and 39");
        assert_eq!(
            wait.text,
            "gates wait 10 minutes for a bay (median), 1 run lost in line"
        );
        // A line standing now puts its oldest wait beside the median.
        let mut status = empty_status();
        status.gates.queued = every_bay("2026-09-19T11:20:00Z", &["2026-09-19T11:30:00Z"])
            .gates
            .queued;
        let g = read_gates(&status, &runs[..2]);
        let wait = g.kpi.iter().find(|m| m.name == "queue wait").unwrap();
        assert_eq!(
            wait.text,
            "gates wait 10 minutes for a bay (median), the oldest in line now 30m"
        );
        // Nothing waited in the window: unknown, never zero.
        let g = read_gates(&empty_status(), &[]);
        let wait = g.kpi.iter().find(|m| m.name == "queue wait").unwrap();
        assert_eq!(wait.value, None);
        assert_eq!(wait.text, "no gate launched in 24h");
    }

    /// Below the bound the rule says nothing: two of three bays in use is
    /// clear, however long ago the last verdict landed.
    #[test]
    fn bays_below_the_bound_are_clear_whatever_the_out_route_does() {
        let mut status = every_bay("2026-09-19T05:50:00Z", &[]);
        status.gates.active.pop();
        let g = read_gates(&status, &verdicts_until("2026-09-19T01:00:00Z"));
        assert_eq!(g.state, RegionState::Clear, "{}", g.why);
        assert!(g.band.is_none());
    }
}
