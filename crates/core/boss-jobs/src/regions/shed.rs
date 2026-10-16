//! THE SHED: landed cars awaiting proof, and the three places they stand
//! in — `boss orient`'s `shed_place`, moved here so the CLI and the read
//! share one classification.

use super::*;

/// Where a landed, unproven car stands — the yard's inspection shed
/// read in words (`apps/web/src/it/yard/yard-shed.ts`, `shedPlace`).
/// The three places are disjoint and total, the probe winning over an
/// event (a probe can be run; an event has to happen). `Unproven` is
/// the forgotten case, and the only troubled one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShedPlace {
    /// A probe is recorded; `last` is the failed attempt's `why`, if
    /// any (a succeeding attempt completes the step and the car leaves).
    ProbePending { last: Option<String> },
    /// An attempt exists, but its required exit outcome is unread.
    ProbeUnread,
    /// The probe ran and exited 75: NOT YET — early, not wrong.
    ProbeNotYet { said: String },
    /// Only an event is recorded: prose naming what has to happen.
    WaitingOn(String),
    /// Neither — no probe, no event; nothing mechanical can settle it.
    Unproven,
}

/// Classify a car's metadata into its shed place.
pub fn shed_place(md: &Value) -> ShedPlace {
    let probe = md_str(md, crate::car::PROOF_PROBE);
    let event = md_str(md, crate::car::PROOF_EVENT);
    if !probe.is_empty() {
        let attempt = md.get("proof_attempt");
        if attempt
            .and_then(|attempt| attempt.get("environment"))
            .and_then(Value::as_bool)
            == Some(true)
            && attempt
                .and_then(|attempt| attempt.get("exit"))
                .and_then(Value::as_i64)
                .is_none()
        {
            return ShedPlace::ProbeUnread;
        }
        let not_yet = attempt.is_some_and(crate::car::attempt_said_not_yet);
        let last = attempt
            .and_then(|a| a.get("why"))
            .and_then(Value::as_str)
            .filter(|w| !w.is_empty())
            .map(str::to_string);
        if not_yet
            && (crate::car::wait_observer_unread(md)
                || attempt.and_then(|a| a.get("waits_on_exit")) == Some(&Value::Null))
        {
            return ShedPlace::ProbeUnread;
        }
        if not_yet {
            return ShedPlace::ProbeNotYet {
                said: last.unwrap_or_else(|| "the probe said not yet".to_string()),
            };
        }
        if let Some(attempt) = attempt {
            match attempt.get("exit").and_then(Value::as_i64) {
                None => return ShedPlace::ProbeUnread,
                Some(exit) if exit != 0 => {
                    return ShedPlace::ProbePending {
                        last: Some(last.unwrap_or_else(|| format!("probe exited {exit}"))),
                    };
                }
                _ => {}
            }
        }
        return ShedPlace::ProbePending { last };
    }
    if !event.is_empty() {
        return ShedPlace::WaitingOn(event.to_string());
    }
    ShedPlace::Unproven
}

/// The cars standing in the inspection shed: open cars whose live step
/// is `proven`. ONE predicate (CLAUDE.md §9a) — the shed region reads
/// it for its count, and the arrivals -> shed border
/// (`crate::borders`) reads it for what is waiting to cross.
pub(crate) fn awaiting_proof(cars: &[(Job, Vec<Step>)]) -> Vec<&(Job, Vec<Step>)> {
    cars.iter()
        .filter(|(j, s)| {
            j.status == boss_core::job::JobStatus::Open
                && s.iter()
                    .find(|st| matches!(st.status, StepStatus::Ready | StepStatus::Active))
                    .is_some_and(|st| st.spec_slug.as_deref() == Some("proven"))
        })
        .collect()
}

/// THE SHED: landed cars awaiting proof — open cars whose live step is
/// `proven`. Troubled when one is UNPROVEN (no probe, no event: nothing
/// mechanical can settle it) or its probe is FAILING, or when a car
/// past [`PROOF_STALE_HOURS`] waits on something that is OURS; attention
/// when a car is past the band and none is ours, clear while none is past it. The trend is cars proven per day.
///
/// TROUBLED MEANS OURS (backlog 3881f5c9). Until 2026-09-23 every stale
/// car troubled the shed, so it read red while its own words said
/// "waiting on the world, not on us" — six honest waits (a Stripe
/// charge, a release David opens, his destructive prune, a tenant
/// publish, a red crawl, a failed publish) painted exactly like a broken
/// probe. A stale wait is someone else's move only when the car
/// DECLARES it and names the OWNER, and — for a wait on the world —
/// something OBSERVES it (`boss_jobs::car::owned_wait`; a named actor's
/// act needs no observer, 2026-09-24); it stays theirs until the event is
/// seen while the probe still says not yet, or it outlives a max wait
/// the car itself declared.
pub(super) fn shed(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    let awaiting = awaiting_proof(inputs.cars);
    let mut unproven = Vec::new();
    let mut failing = Vec::new();
    let mut unread = Vec::new();
    let mut unowned_now = Vec::new();
    let mut unobserved_now = Vec::new();
    let mut overdue_now = Vec::new();
    let mut seen_now = Vec::new();
    let mut recorded_waits = Vec::new();
    let mut unqualified_waits = Vec::new();
    for (job, steps) in &awaiting {
        match super::stale_proof::recorded_proof_wait(job, steps, inputs.now) {
            Ok(Some(wait)) => recorded_waits.push(wait),
            Err(why) => unqualified_waits.push(format!("{}: {why}", job.title)),
            Ok(None) => {}
        }
    }
    for (j, _) in &awaiting {
        let branch = j
            .metadata
            .get("branch")
            .and_then(Value::as_str)
            .unwrap_or(j.title.as_str());
        let age_seconds = proof_opened_at(j).map(|opened| (inputs.now - opened).num_seconds());
        match proof_fault(&j.metadata, age_seconds) {
            Some(ProofFault::Unproven) => unproven.push(branch),
            Some(ProofFault::Unread) => unread.push(branch),
            Some(ProofFault::Failing) => failing.push(branch),
            Some(ProofFault::Unowned) => unowned_now.push(branch),
            Some(ProofFault::Unobserved) => unobserved_now.push(branch),
            Some(ProofFault::Overdue(wait)) => overdue_now.push((wait, age_seconds, branch)),
            Some(ProofFault::SatisfiedCondition(on)) => seen_now.push((on, branch)),
            None => {}
        }
    }
    let proven = inputs
        .cars
        .iter()
        .filter_map(|(_, s)| step_done_at(find_step(s, "proven", "Proven in production")));
    let (cur, prev) = count_split(w, proven);
    let trend = rate_trend("proven", w, cur, prev);
    // STALE: awaiting proof for longer than a day, whatever its place.
    // Collected over every awaiting car rather than only the `not yet`
    // ones, so an old car is named even when its place would otherwise
    // read as healthy progress.
    let mut stale: Vec<(i64, &str)> = Vec::new();
    // Of the stale ones, how many have NEVER had their probe run. A
    // probe that ran and said `not yet` is the world answering; a probe
    // with no attempt on record is us not asking. Same age, opposite
    // meaning, and until 2026-09-22 the same sentence.
    let mut never_probed = 0usize;
    // Of the stale ones that were told not yet, those told so without a
    // break for longer than [`NOT_YET_STARVED_HOURS`] — a probe that
    // cannot pass looks exactly like this, so it is ours to read, not
    // the world's to answer (adef5ddf). Longest streak first.
    let mut starved: Vec<(crate::car::NotYetStreak, &str)> = Vec::new();
    // Of those told not yet, the ones whose OWN declared wait is already
    // in the record (b461341d) — the probe cannot see what the car said
    // it was waiting for, which is ours at any streak length. A declared
    // wait not yet seen lands in neither list: its patience is stated.
    let mut seen: Vec<(String, &str)> = Vec::new();
    // Of those told not yet, the ones that DECLARED a wait with no
    // `seen` check (e9b164a1). The declaration exempts them from the
    // streak bound and hands the judgement to that check — so without
    // one nothing can ever say the event arrived, and the exemption is
    // an escape hatch that silences the label forever. Writing the
    // check is ours, so they are counted, not folded into the world's.
    let mut unobserved: Vec<&str> = Vec::new();
    // WHOSE MOVE (3881f5c9): of those told not yet, the ones with NO
    // declared wait (not yet starved — nothing on the car says whose
    // move it is), the ones that declared and observe a wait but name no
    // owner, the ones past the max wait they declared, and — the only
    // ones that are not ours — those waiting on their declared owner.
    let mut undeclared: Vec<&str> = Vec::new();
    let mut prose_only: Vec<&str> = Vec::new();
    let mut unowned: Vec<&str> = Vec::new();
    let mut overdue: Vec<(crate::car::OwnedWait, i64, &str)> = Vec::new();
    let mut theirs: Vec<(crate::car::OwnedWait, &str)> = Vec::new();
    // The age of the oldest stale car that is OURS — what the ours band
    // measures. Until 2026-09-24 it read the oldest car of all: "oldest
    // open 168h > the 24h proof band, ours to move", held 6d, where the
    // 168h car was a Stripe wait on the world and the one car that was
    // ours was 40h old (3881f5c9).
    let mut oldest_ours: Option<i64> = None;
    for (j, _) in &awaiting {
        let branch = j
            .metadata
            .get("branch")
            .and_then(Value::as_str)
            .unwrap_or(j.title.as_str());
        let Some(opened) = proof_opened_at(j) else {
            continue;
        };
        let hours = (inputs.now - opened).num_hours();
        if hours >= PROOF_STALE_HOURS {
            stale.push((hours, branch));
            let class = match proof_fault(&j.metadata, Some((inputs.now - opened).num_seconds())) {
                Some(ProofFault::Overdue(wait)) => StaleProof::Overdue(wait),
                _ => stale_proof(&j.metadata, hours),
            };
            if !matches!(
                class,
                StaleProof::Theirs(_)
                    | StaleProof::Unproven
                    | StaleProof::Failing
                    | StaleProof::Unread
            ) {
                oldest_ours = oldest_ours.max(Some(hours));
            }
            match class {
                // Named by the sentences above this one, which lead
                // whenever any car is unproven or failing.
                StaleProof::Unproven | StaleProof::Failing | StaleProof::Unread => {}
                StaleProof::NeverProbed => never_probed += 1,
                StaleProof::ProseOnly => prose_only.push(branch),
                StaleProof::Starved(streak) => starved.push((streak, branch)),
                StaleProof::Seen(on) => seen.push((on, branch)),
                StaleProof::Unobserved => unobserved.push(branch),
                StaleProof::Undeclared => undeclared.push(branch),
                StaleProof::Unowned => unowned.push(branch),
                StaleProof::Overdue(o) => overdue.push((o, hours, branch)),
                StaleProof::Theirs(o) => theirs.push((o, branch)),
            }
        }
    }
    stale.sort_by_key(|(hours, _)| std::cmp::Reverse(*hours));
    starved.sort_by_key(|(s, _)| std::cmp::Reverse(s.hours));

    let n = awaiting.len();
    // THE KPI (decision 9): cars owed a proof whose move is OURS — the
    // present proof faults and every stale car whose wait is not a
    // declared, observed, owned one, each packet counted once.
    let ours = awaiting
        .iter()
        .filter(|(job, steps)| {
            super::stale_proof::recorded_proof_wait(job, steps, inputs.now).is_err()
                || super::stale_proof::proof_requires_action(
                    &job.metadata,
                    proof_opened_at(job).map(|opened| (inputs.now - opened).num_seconds()),
                )
        })
        .count();
    let kpi = vec![measure_said(
        "cars owed a proof that are ours",
        count_value(ours),
        "cars",
        format!(
            "{} owed a proof {} ours",
            plural(ours, "car", "cars"),
            if ours == 1 { "is" } else { "are" }
        ),
    )];
    // When the stale band was crossed: a car crosses it PROOF_STALE_HOURS
    // after it opened, so the oldest stale car's crossing is the onset.
    let stale_since = stale
        .first()
        .map(|(hours, _)| inputs.now - chrono::Duration::hours(*hours - PROOF_STALE_HOURS));
    // Which band the shed's sentence falls under, and its onset. The
    // unproven and the failing carry no onset the record holds — the
    // car's landing is not stamped on it — so they are stated at once.
    let (decided_by, why): (Option<(bands::Band, Option<Instant>, String)>, String) = if !unproven
        .is_empty()
    {
        (
            Some((bands::SHED_UNPROVEN, None, String::new())),
            format!("UNPROVEN — no probe, no event: {}", unproven.join(", ")),
        )
    } else if !failing.is_empty() {
        (
            Some((bands::SHED_FAILING, None, String::new())),
            format!("probe FAILING: {}", failing.join(", ")),
        )
    } else if !unread.is_empty() {
        (
            Some((bands::SHED_UNREAD, None, String::new())),
            format!("proof outcome unread: {}", unread.join(", ")),
        )
    } else if !unowned_now.is_empty() {
        (
            Some((bands::SHED_UNOWNED, None, String::new())),
            format!(
                "{} declared no owner for their wait — {}; naming one (world, or the actor whose act it is) is ours",
                unowned_now.len(),
                unowned_now.join(", "),
            ),
        )
    } else if !unobserved_now.is_empty() {
        (
            Some((bands::SHED_UNOBSERVED, None, String::new())),
            format!(
                "{} declared a wait with no seen check (world observer) — {}; nothing can ever say its event arrived, so writing one is ours",
                unobserved_now.len(),
                unobserved_now.join(", "),
            ),
        )
    } else if !overdue_now.is_empty() {
        (
            Some((bands::SHED_WAIT_OVERDUE, None, String::new())),
            format!(
                "past the max wait they declared: {}",
                overdue_now
                    .iter()
                    .map(|(wait, age, branch)| format!(
                        "{branch}, waiting on {}: {}; {}m against {}h",
                        wait.owner,
                        wait.on,
                        age.unwrap_or_default() / 60,
                        wait.max_wait_hours.unwrap_or_default()
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        )
    } else if !seen_now.is_empty() {
        (
            Some((bands::SHED_CONDITION_SEEN, None, String::new())),
            format!(
                "told not yet AFTER their required condition was seen: {}; the probe cannot see its own event, so it is ours to read",
                seen_now
                    .iter()
                    .map(|(on, branch)| format!("{branch}, waiting on {on}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        )
    } else if !unqualified_waits.is_empty() {
        (
            Some((bands::SHED_WAIT_UNQUALIFIED, None, String::new())),
            unqualified_waits.join("; "),
        )
    } else if !recorded_waits.is_empty() && ours == 0 {
        (
            Some((bands::SHED_WAIT_EXPECTED, None, String::new())),
            recorded_waits
                .iter()
                .map(|wait| {
                    format!(
                        "waiting on {}: {} ({}); owner {}; next check {}",
                        wait.condition_owner,
                        wait.condition,
                        wait.title,
                        wait.owner,
                        wait.next_check_at
                    )
                })
                .collect::<Vec<_>>()
                .join("; "),
        )
    } else if let Some((oldest, branch)) = stale.first().copied() {
        // The age is the finding, so it leads — and the oldest car is
        // named, because "9 awaiting proof" sends a reader to a list
        // while "105h, fix/x" sends them to a car.
        // WHOSE MOVE IS IT. The age says something is stuck; this says
        // whether anyone here can unstick it, and since 3881f5c9 it
        // also decides the colour: only a declared, observed, owned
        // wait is someone else's move, and a shed of nothing else is
        // attention, not troubled. Every other answer is ours, each named
        // for what there is to do — run the probe, read one that cannot
        // pass (adef5ddf), read one blind to its own event (b461341d),
        // write the seen check (e9b164a1), or declare whose move it is.
        let mut ours: Vec<String> = Vec::new();
        if never_probed > 0 {
            ours.push(format!(
                "{never_probed} never probed — nothing has run their proof, which is on us"
            ));
        }
        if let Some((longest, branch)) = starved.first() {
            ours.push(format!(
                "{} told not yet without a break past {NOT_YET_STARVED_HOURS}h — longest {}h \
                 over {} runs, {branch}; a probe that cannot pass says exactly this, so it is \
                 ours to read, not the world's",
                starved.len(),
                longest.hours,
                longest.runs
            ));
        }
        if let Some((on, branch)) = seen.first() {
            ours.push(format!(
                "{} told not yet AFTER what they declared they wait on was seen in the record \
                 — {branch}, waiting on {on}; the probe cannot see its own event, so it is ours \
                 to read",
                seen.len()
            ));
        }
        if let Some(branch) = unobserved.first() {
            ours.push(format!(
                "{} declared a wait with no seen check — {branch}; nothing can ever say its \
                 event arrived, so writing one is ours (boss car waits-on --seen)",
                unobserved.len()
            ));
        }
        if let Some(branch) = undeclared.first() {
            ours.push(format!(
                "{} told not yet with no declared wait — {branch}; saying whose move it is \
                 is ours (boss car waits-on)",
                undeclared.len()
            ));
        }
        if let Some(branch) = prose_only.first() {
            ours.push(format!(
                "{} wait on an event only prose names, with no probe — {branch}; nothing \
                 observes it, so it is ours",
                prose_only.len()
            ));
        }
        if let Some(branch) = unowned.first() {
            ours.push(format!(
                "{} declared no owner for their wait — {branch}; naming one (world, or the \
                 actor whose act it is) is ours",
                unowned.len()
            ));
        }
        if let Some((o, hours, branch)) = overdue.first() {
            ours.push(format!(
                "{} past the max wait they declared — {branch}, waiting on {}: {}, {hours}h \
                 against {}h; ours to read",
                overdue.len(),
                o.owner,
                o.on,
                o.max_wait_hours.unwrap_or_default()
            ));
        }
        // Named one by one: a wait on the world and a wait on David's
        // act are different errands, and "6 waiting" sends no one to
        // either.
        let waits = theirs
            .iter()
            .map(|(o, branch)| format!("waiting on {}: {} ({branch})", o.owner, o.on))
            .collect::<Vec<_>>()
            .join("; ");
        // Past the band and none ours is ATTENTION, not trouble: the
        // line is crossed, and the move is the declared owner's.
        let (band, whose) = if ours.is_empty() {
            (bands::SHED_THEIRS_STALE, format!("none ours — {waits}"))
        } else if theirs.is_empty() {
            (bands::SHED_OURS_STALE, ours.join("; "))
        } else {
            (
                bands::SHED_OURS_STALE,
                format!(
                    "{}; the rest wait on their declared owner — {waits}",
                    ours.join("; ")
                ),
            )
        };
        // Each band measures the cars it names: the ours band the oldest
        // car that is ours, from when THAT car crossed the line.
        let (since, measured) = match oldest_ours {
            Some(h) if band == bands::SHED_OURS_STALE => (
                Some(inputs.now - chrono::Duration::hours(h - PROOF_STALE_HOURS)),
                format!("oldest ours open {h}h"),
            ),
            _ => (stale_since, format!("oldest open {oldest}h")),
        };
        (
            Some((band, since, measured)),
            format!(
                "{} of {n} open past {PROOF_STALE_HOURS}h — oldest {oldest}h, {branch} — {whose}",
                stale.len()
            ),
        )
    } else if n > 0 {
        // Landed and inside the day, each with a way to settle: the shed
        // working (decision 1).
        (
            None,
            plural(n, "landed car awaiting proof", "landed cars awaiting proof"),
        )
    } else {
        (None, "every landed car is proven".to_string())
    };
    // An immediate ownership/observer fault does not erase the owned
    // waits beside it. Keep their conditions visible while preserving
    // the genuine fault as the region's deciding finding.
    let why = if decided_by
        .as_ref()
        .is_some_and(|(band, _, _)| *band != bands::SHED_WAIT_EXPECTED)
        && !recorded_waits.is_empty()
    {
        let waits = recorded_waits
            .iter()
            .map(|wait| {
                format!(
                    "waiting on {}: {} ({})",
                    wait.condition_owner, wait.condition, wait.title
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        format!("{why}; the rest wait on their declared owner — {waits}")
    } else {
        why
    };
    let settled = match decided_by {
        Some((band, since, measured)) => settle(
            vec![Finding::new(band, since, measured, why)],
            String::new(),
            inputs.now,
        ),
        None => settle(Vec::new(), why, inputs.now),
    };
    // THE SHED'S THREE PLACES (the rest of decision 5), disjoint and
    // summing to the head: the review drew five wagons under SHED 11,
    // and with these the region map can say which place its slice
    // falls short in rather than leave the reader to recount.
    let places = SHED_PLACES
        .iter()
        .map(|name| Place {
            name: (*name).to_string(),
            count: awaiting
                .iter()
                .filter(|(j, _)| shed_station(&shed_place(&j.metadata)) == *name)
                .count(),
        })
        .collect();
    Region {
        waits: recorded_waits,
        places,
        ..region(
            "shed",
            Some(n),
            None,
            "cars awaiting proof",
            settled,
            trend,
            kpi,
        )
    }
}

/// The shed's places as the floor names its stations
/// (`apps/web/src/it/yard/yard-shed.ts`, `ShedPlace`), in its order.
pub const SHED_PLACES: [&str; 3] = ["inspection-shed", "siding-event", "siding-no-probe"];

/// Which of [`SHED_PLACES`] a car stands in: a probe, run or not, is the
/// inspection shed; an event alone is the event siding; neither is the
/// no-probe siding — the floor's `shedPlace`, over [`shed_place`].
pub fn shed_station(place: &ShedPlace) -> &'static str {
    match place {
        ShedPlace::ProbePending { .. } | ShedPlace::ProbeNotYet { .. } | ShedPlace::ProbeUnread => {
            SHED_PLACES[0]
        }
        ShedPlace::WaitingOn(_) => SHED_PLACES[1],
        ShedPlace::Unproven => SHED_PLACES[2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    #[test]
    fn a_failed_observer_is_unread_for_world_and_named_actor_waits() {
        for owner in ["world", "actor-observer"] {
            let metadata = json!({
                "proof_probe": "check-proof",
                "proof_attempt": {"exit": 75, "not_yet": true, "waits_on_exit": 2},
                "waits_on": {"on": "the condition", "owner": owner, "seen": "read-condition"}
            });
            assert_eq!(
                shed_place(&metadata),
                ShedPlace::ProbeUnread,
                "an observer failure is not missing ownership: {owner}"
            );
        }
    }

    /// The recorded exit is the evidence. A waiting flag cannot hide a
    /// failed proof, and a flag without an outcome is not an observation.
    #[test]
    fn a_waiting_flag_cannot_hide_a_failed_or_unread_proof() {
        let car = |exit: Value| {
            let job = job(
                "ship-a-change",
                "fix/proof",
                JobStatus::Open,
                observed_wait_fixture(json!({
                    "branch": "fix/proof", "merged": true,
                    "opened_at": "2026-09-19T11:00:00Z",
                    "proof_probe": "check-the-event",
                    "proof_attempt": {
                        "at": NOW, "probe": "check-the-event",
                        "exit": exit, "not_yet": true, "why": "waiting flag retained", "waits_on_exit": 1,
                    },
                    "waits_on": {"on": "the event", "seen": "check-the-event", "owner": "world", "max_wait_hours": 24},
                })),
            );
            let steps = vec![step(&job, "proven", StepStatus::Ready, None)];
            (job, steps)
        };
        let status = empty_status();
        for exit in [json!(1), Value::Null] {
            let cars = vec![car(exit)];
            let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
            let shed = by_name(&out, "shed");
            assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        }
        let cars = vec![car(json!(75))];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        assert_eq!(by_name(&out, "shed").state, RegionState::Attention);
    }

    #[test]
    fn a_recorded_next_check_qualifies_an_observed_wait_and_a_missed_check_requires_action() {
        let status = empty_status();
        for (next_check, expected) in [
            ("2026-09-19T13:00:00Z", RegionState::Attention),
            ("2026-09-19T11:30:00Z", RegionState::Troubled),
        ] {
            let j = job(
                "ship-a-change",
                "feat/observed",
                JobStatus::Open,
                json!({
                    "branch": "feat/observed", "merged": true,
                    "opened_at": "2026-09-19T10:00:00Z", "proof_probe": "check-proof",
                    "proof_attempt": {"at": "2026-09-19T11:00:00Z", "probe": "check-proof", "exit": 75,
                        "waits_on_probe": "check-event", "waits_on_exit": 1, "waits_on_observation": "unseen"},
                    "waits_on": {"on": "the ordinary batch", "seen": "check-event", "owner": "world",
                        "max_wait_hours": 26, "next_check_at": next_check}
                }),
            );
            let mut claimed = step(&j, "proven", StepStatus::Active, None);
            claimed.assignee_id = Some("agent-codex".into());
            let cars = vec![(j, vec![claimed])];
            let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
            let shed = by_name(&out, "shed");
            assert_eq!(
                shed.state, expected,
                "next check {next_check}: {}",
                shed.why
            );
            let input = inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[]));
            let rails = crate::borders::borders(&crate::borders::BorderInputs {
                regions: &input,
                firings: Some(&[]),
                dispatcher_firings: Some(&[]),
                machines_withheld: None,
            });
            let rail = rails
                .borders
                .iter()
                .find(|rail| rail.from == "arrivals" && rail.to == "shed")
                .unwrap();
            assert_eq!(
                rail.holds[0].wait.as_ref(),
                shed.waits.first(),
                "the border and shed use the same qualified observation; overdue checks carry none"
            );
            if expected == RegionState::Attention {
                let wire = serde_json::to_value(shed).unwrap();
                assert_eq!(wire["waits"][0]["packet"], cars[0].0.id.to_string());
                assert_eq!(wire["waits"][0]["owner"], "agent-codex");
                assert_eq!(wire["waits"][0]["condition"], "the ordinary batch");
                assert_eq!(wire["waits"][0]["next_check_at"], next_check);
                assert_eq!(wire["waits"][0]["evidence"]["probe"], "check-event");
            }
        }
    }

    #[test]
    fn a_refused_observer_requires_action_before_the_age_band() {
        let status = empty_status();
        let car = |observer_exit: Value| {
            let job = job(
                "ship-a-change",
                "fix/observer",
                JobStatus::Open,
                observed_wait_fixture(json!({
                    "branch": "fix/observer", "merged": true,
                    "opened_at": "2026-09-19T11:00:00Z",
                    "proof_probe": "check-the-proof",
                    "proof_attempt": {"at": NOW, "probe": "check-the-proof", "exit": 75,
                        "not_yet": true, "waits_on_exit": observer_exit},
                    "waits_on": {"on": "the event", "seen": "check-the-event",
                        "owner": "world", "max_wait_hours": 24}
                })),
            );
            let steps = vec![step(&job, "proven", StepStatus::Ready, None)];
            (job, steps)
        };
        let cars = vec![car(Value::Null)];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        assert_eq!(by_name(&out, "shed").state, RegionState::Troubled);
        assert_eq!(
            by_name(&out, "shed").kpi[0].value,
            Some(1.0),
            "the action count must conserve the present unread fault"
        );
        let cars = vec![car(json!(1))];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        assert_eq!(by_name(&out, "shed").state, RegionState::Attention);
    }

    #[test]
    fn a_declared_world_wait_without_an_owner_requires_action_now() {
        let status = empty_status();
        let car = |owner: Value| {
            let job = job(
                "ship-a-change",
                "fix/unowned-wait",
                JobStatus::Open,
                observed_wait_fixture(json!({
                    "branch": "fix/unowned-wait", "merged": true,
                    "opened_at": "2026-09-19T11:00:00Z",
                    "proof_probe": "check-the-proof",
                    "proof_attempt": {"at": NOW, "probe": "check-the-proof", "exit": 75,
                        "not_yet": true, "waits_on_exit": 1},
                    "waits_on": {"on": "the event", "seen": "check-the-event",
                        "owner": owner, "max_wait_hours": 24}
                })),
            );
            let steps = vec![step(&job, "proven", StepStatus::Ready, None)];
            (job, steps)
        };
        let cars = vec![car(Value::Null)];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(shed.why.contains("owner") && shed.why.contains("fix/unowned-wait"));
        let cars = vec![car(json!("world"))];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        assert_eq!(by_name(&out, "shed").state, RegionState::Attention);
    }

    #[test]
    fn a_world_wait_without_an_observer_requires_action_now() {
        let status = empty_status();
        let car = |observer: Value| {
            let job = job(
                "ship-a-change",
                "fix/unobserved-wait",
                JobStatus::Open,
                observed_wait_fixture(json!({
                    "branch": "fix/unobserved-wait", "merged": true,
                    "opened_at": "2026-09-19T11:00:00Z",
                    "proof_probe": "check-the-proof",
                    "proof_attempt": {"at": NOW, "probe": "check-the-proof", "exit": 75,
                        "not_yet": true, "waits_on_exit": 1},
                    "waits_on": {"on": "the event", "seen": observer,
                        "owner": "world", "max_wait_hours": 24}
                })),
            );
            let steps = vec![step(&job, "proven", StepStatus::Ready, None)];
            (job, steps)
        };
        let cars = vec![car(Value::Null)];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(shed.why.contains("observer") && shed.why.contains("fix/unobserved-wait"));
        let cars = vec![car(json!("check-the-event"))];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        assert_eq!(by_name(&out, "shed").state, RegionState::Attention);
    }

    /// The shed counts open cars at `proven`; an unproven one troubles
    /// it; the trend is cars proven per day.
    #[test]
    fn the_shed_counts_cars_awaiting_proof_and_an_unproven_one_troubles_it() {
        let car = |branch: &str, md: Value, proven_at: Option<&str>| {
            let mut md = md;
            md["branch"] = json!(branch);
            let open = proven_at.is_none();
            let j = job(
                "ship-a-change",
                branch,
                if open {
                    JobStatus::Open
                } else {
                    JobStatus::Closed
                },
                md,
            );
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-19T06:00:00Z"),
                ),
                step(
                    &j,
                    "review",
                    StepStatus::Completed,
                    Some("2026-09-19T07:00:00Z"),
                ),
                if let Some(at) = proven_at {
                    step(&j, "proven", StepStatus::Completed, Some(at))
                } else {
                    step(&j, "proven", StepStatus::Ready, None)
                },
            ];
            (j, s)
        };
        let cars = vec![
            car("fix/a", json!({ "proof_probe": "true" }), None),
            car(
                "fix/b",
                json!({ "proof_event": "the next red train" }),
                None,
            ),
            car("fix/c", json!({}), Some("2026-09-19T08:00:00Z")),
            car("fix/d", json!({}), Some("2026-09-18T08:00:00Z")),
        ];
        let status = empty_status();
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.count, Some(2));
        // Landed inside the day, each with a way to settle: the shed
        // working, which is clear (design 62de32ae, decision 1).
        assert_eq!(shed.state, RegionState::Clear, "{}", shed.why);
        assert_eq!(shed.unit, "cars awaiting proof");
        assert_eq!(shed.kpi[0].text, "0 cars owed a proof are ours");
        assert_eq!(shed.trend.metric, "proven");
        assert_eq!(shed.trend.current, Some(1.0));
        assert_eq!(shed.trend.previous, Some(1.0));

        let mut with_unproven = cars.clone();
        with_unproven.push(car("fix/e", json!({}), None));
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &with_unproven,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.count, Some(3));
        assert_eq!(shed.state, RegionState::Troubled);
        assert!(
            shed.why.contains("UNPROVEN") && shed.why.contains("fix/e"),
            "{}",
            shed.why
        );
    }

    /// A DISPROVED CAR LEAVES THE SHED, AND IS NOT COUNTED PROVEN (backlog
    /// 08664157). Car 6b23d135 landed and its claim was measured false;
    /// with no terminal it stood at `proven`, unproven and two days old,
    /// "ours" for ever. Closed through `disproved` — `proven` skipped, the
    /// terminal completed, `merged` still "true" — it is in neither the
    /// count nor the ours KPI, and the proven-per-day trend does not
    /// credit it with a proof it never had. It is still in `cars`: the
    /// record keeps it; only the shed stops reading it as owed.
    #[test]
    fn a_disproved_car_leaves_the_shed_and_is_not_counted_proven() {
        let md = json!({
            "branch": "fix/dev-pod-not-first-evicted",
            "merged": "true",
            "opened_at": "2026-09-17T12:00:00Z",
        });
        let standing = {
            let j = job("ship-a-change", "fix/dev-pod", JobStatus::Open, md.clone());
            let s = vec![
                step(
                    &j,
                    "review",
                    StepStatus::Completed,
                    Some("2026-09-18T07:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
                step(&j, "disproved", StepStatus::Pending, None),
            ];
            (j, s)
        };
        let status = empty_status();
        let before = vec![standing];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &before,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.count, Some(1));
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);

        let mut closed_md = md;
        closed_md["outcome"] = json!("disproved");
        closed_md["disproved"] = json!("true");
        let disproved = {
            let j = job("ship-a-change", "fix/dev-pod", JobStatus::Closed, closed_md);
            let s = vec![
                step(
                    &j,
                    "review",
                    StepStatus::Completed,
                    Some("2026-09-18T07:00:00Z"),
                ),
                step(
                    &j,
                    "proven",
                    StepStatus::Skipped,
                    Some("2026-09-19T11:00:00Z"),
                ),
                step(
                    &j,
                    "disproved",
                    StepStatus::Completed,
                    Some("2026-09-19T11:00:00Z"),
                ),
            ];
            (j, s)
        };
        let after = vec![disproved];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &after,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.count, Some(0), "{}", shed.why);
        assert_eq!(shed.state, RegionState::Clear, "{}", shed.why);
        assert_eq!(shed.kpi[0].text, "0 cars owed a proof are ours");
        assert_eq!(shed.trend.current, Some(0.0), "a disproof is not a proof");
    }

    /// THE SHED'S COLOUR FOLLOWS WHOSE MOVE THE WAIT IS (backlog
    /// 3881f5c9). Measured 2026-09-23 ~16:05Z: the shed read troubled
    /// with the words "every one asked and was told not yet — waiting on
    /// the world, not on us" — the region said the waits were not ours
    /// and painted them red anyway, so red stopped meaning ours to fix.
    /// Troubled is now only ours; a declared, observed, owned wait —
    /// on the world or on a named actor's act — asks for attention (never trouble) and says whose,
    /// until it runs past a max wait the car itself declared.
    #[test]
    fn the_shed_is_troubled_only_by_waits_that_are_ours() {
        let car = |branch: &str, on: &str, owner: Value, max: Value| {
            let md = observed_wait_fixture(json!({
                "branch": branch, "merged": true, "opened_at": "2026-09-15T10:00:00Z",
                "proof_probe": "true",
                "proof_attempt": {
                    "at": "2026-09-19T11:00:00Z", "not_yet": true, "exit": 75, "probe": "true", "waits_on_exit": 1,
                    (crate::car::NOT_YET_SINCE): "2026-09-15T11:00:00Z",
                    (crate::car::NOT_YET_RUNS): 96,
                },
                (crate::car::WAITS_ON): {
                    "on": on, "seen": "true",
                    (crate::car::WAITS_ON_OWNER): owner,
                    (crate::car::WAITS_ON_MAX_WAIT_HOURS): max,
                },
            }));
            let j = job("ship-a-change", branch, JobStatus::Open, md);
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-15T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            (j, s)
        };
        let status = empty_status();
        let read = |cars: &[(Job, Vec<Step>)]| {
            let out = regions(&inputs(&status, &[], &[], cars, &[], Some(&[]), Some(&[])));
            by_name(&out, "shed").clone()
        };
        let world = || {
            car(
                "feat/stripe",
                "a Stripe charge",
                json!("world"),
                Value::Null,
            )
        };
        let david = || {
            car(
                "feat/release",
                "a cut-a-release packet",
                json!("emp-david"),
                Value::Null,
            )
        };

        // THEIRS: the world's event and a named actor's act, 98h open.
        let shed = read(&[world(), david()]);
        // Past the band and not ours: ATTENTION, never trouble — and the
        // band that decided it is named, with how long it has been past.
        assert_eq!(
            shed.state,
            RegionState::Attention,
            "declared, observed, owned waits are not ours: {}",
            shed.why
        );
        let band = shed
            .band
            .as_ref()
            .expect("a non-clear state names its band");
        assert_eq!(band.id, "shed-wait-expected");
        assert_eq!(
            band.held_minutes, None,
            "a wait declaration does not record alarm onset"
        );
        assert_eq!(shed.waits.len(), 2);
        assert_eq!(shed.kpi[0].value, Some(0.0), "none of them is ours");
        assert!(
            shed.why
                .contains("waiting on the world: a Stripe charge (feat/stripe)")
                && shed
                    .why
                    .contains("waiting on emp-david: a cut-a-release packet"),
            "and it says whose move each one is: {}",
            shed.why
        );

        // PAST ITS OWN DECLARED PATIENCE: 98h open against a 72h max.
        let late = car("feat/late", "a Stripe charge", json!("world"), json!(72));
        let shed = read(&[late, david()]);
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(
            shed.why.contains("past the max wait they declared")
                && shed.why.contains("feat/late")
                && shed.why.contains("the rest wait on their declared owner"),
            "{}",
            shed.why
        );

        // An unread latest attempt preserves the qualified wait beside it.
        let (mut unread, steps) = world();
        unread.metadata["proof_attempt"]["waits_on_exit"] = Value::Null;
        let shed = read(&[(unread, steps), david()]);
        assert_eq!(shed.state, RegionState::Troubled);
        assert!(
            shed.why
                .contains("waiting on emp-david: a cut-a-release packet"),
            "{}",
            shed.why
        );

        // NO OWNER: the prose may name David, the field does not.
        let unowned = car(
            "feat/unowned",
            "a release (David opens it)",
            Value::Null,
            Value::Null,
        );
        let shed = read(&[unowned]);
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(
            shed.why.contains("declared no owner") && shed.why.contains("feat/unowned"),
            "an owner is declared, never read out of prose: {}",
            shed.why
        );

        // AN EVENT ONLY PROSE NAMES: no probe runs, so no `seen` check
        // does either — nothing observes it (the dev-door login car,
        // 2026-09-23, is this shape).
        let (mut j, s) = world();
        j.metadata = json!({
            "branch": "feat/prose", "merged": true, "opened_at": "2026-09-15T10:00:00Z",
            "proof_event": "David logs in through the dev door",
        });
        let shed = read(&[(j, s), david()]);
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(
            shed.why.contains("only prose names")
                && shed.why.contains("feat/prose")
                && shed
                    .why
                    .contains("waiting on emp-david: a cut-a-release packet"),
            "{}",
            shed.why
        );
    }

    /// THE OURS BAND MEASURES THE OLDEST CAR THAT IS OURS (backlog
    /// 3881f5c9). Measured 2026-09-24 16:42Z: the band read "oldest open
    /// 168h > the 24h proof band, ours to move", held 6d — and the 168h
    /// car was the Stripe wait, declared on the world, while the one car
    /// that was ours was 40h old. The header's number must be the number
    /// of the thing it names, or checking the state against it (the
    /// point of naming the band) checks it against the wrong car.
    #[test]
    fn the_ours_band_reads_the_age_of_the_oldest_car_that_is_ours() {
        // NOW is 2026-09-19T12:00:00Z.
        let car = |branch: &str, opened: &str, owner: Value| {
            let j = job(
                "ship-a-change",
                branch,
                JobStatus::Open,
                observed_wait_fixture(json!({
                    "branch": branch, "merged": true, "opened_at": opened,
                    "proof_probe": "true",
                    "proof_attempt": {
                        "at": "2026-09-19T11:00:00Z", "not_yet": true, "exit": 75,
                        "probe": "true", "waits_on_exit": 1,
                        (crate::car::NOT_YET_SINCE): "2026-09-19T10:00:00Z",
                        (crate::car::NOT_YET_RUNS): 2,
                    },
                    (crate::car::WAITS_ON): {
                        "on": "an event", "seen": "true",
                        (crate::car::WAITS_ON_OWNER): owner,
                    },
                })),
            );
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-12T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            (j, s)
        };
        let status = empty_status();
        // Missing ownership is now an immediate fault, independent of
        // age. Keep this age-band control about a genuinely unrun proof:
        // the old observed world wait cannot supply its onset.
        let mut ours = car("fix/ours-young", "2026-09-17T20:00:00Z", json!("world"));
        ours.0
            .metadata
            .as_object_mut()
            .unwrap()
            .remove("proof_attempt");
        let cars = vec![
            car("feat/theirs-old", "2026-09-12T12:00:00Z", json!("world")),
            ours,
        ];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        let band = shed.band.as_ref().expect("a troubled shed names its band");
        assert_eq!(band.id, "shed-ours-stale");
        assert!(
            band.reads.contains("oldest ours open 40h") && !band.reads.contains("168h"),
            "the ours band measures the ours car: {}",
            band.reads
        );
        assert_eq!(
            band.held_minutes,
            Some(16 * 60),
            "40h open, past 24h for 16h — not the theirs car's 6d"
        );
        assert!(
            shed.why.contains("oldest 168h, feat/theirs-old"),
            "the sentence still leads with the oldest car of all: {}",
            shed.why
        );
    }

    /// A car awaiting proof PAST A DAY troubles the shed, even when its
    /// probe is answering `not yet` — early, not wrong, and the
    /// commonest place a car stops (backlog 488d42e6).
    ///
    /// Measured 2026-09-22: nine cars at `proven`, every one merged,
    /// aged 9.0h to 105.7h, and the shed said `9 landed cars awaiting
    /// proof` — the words it prints ten minutes after a landing. The
    /// age had roughly doubled since the packet was filed while the
    /// COUNT fell, so the rate was the finding and nothing showed it.
    #[test]
    fn a_car_awaiting_proof_past_a_day_troubles_the_shed() {
        // NOW is 2026-09-19T12:00:00Z.
        let aged = |branch: &str, opened: &str| {
            let j = job(
                "ship-a-change",
                branch,
                JobStatus::Open,
                json!({
                    "branch": branch,
                    "merged": true,
                    "opened_at": opened,
                    "proof_probe": "true",
                    // `not yet` — the place that was never troubled.
                    "proof_attempt": { "not_yet": true, "exit": 75 },
                }),
            );
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-17T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            (j, s)
        };
        let status = empty_status();

        // FRESH: landed this morning, still working. Must stay clear, or
        // the signal fires on every landing and stops meaning anything.
        let fresh = vec![aged("fix/fresh", "2026-09-19T06:00:00Z")];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &fresh,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(
            shed.state,
            RegionState::Clear,
            "a car six hours old is working, not troubled: {}",
            shed.why
        );

        // STALE: two days at `not yet`.
        let stale = vec![
            aged("fix/fresh", "2026-09-19T06:00:00Z"),
            aged("feat/two-days", "2026-09-17T10:00:00Z"),
        ];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &stale,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(
            shed.state,
            RegionState::Troubled,
            "a car past {PROOF_STALE_HOURS}h must trouble the shed: {}",
            shed.why
        );
        assert!(
            shed.why.contains("feat/two-days"),
            "and NAME the oldest — a count sends a reader to a list, a branch sends them \
             to a car: {}",
            shed.why
        );
        assert!(
            shed.why.contains("50h"),
            "carrying its age, which is the finding: {}",
            shed.why
        );
        assert!(
            !shed.why.contains("fix/fresh"),
            "and not the fresh one, which is not the problem: {}",
            shed.why
        );

        // A CAR WITH NO `opened_at` IS SKIPPED, not treated as
        // infinitely old. An absent timestamp is not evidence of age,
        // and reading it as one would trouble the shed for a missing
        // field — the zero-means-unknown defect, in a region card.
        let no_clock = {
            let j = job(
                "ship-a-change",
                "fix/no-clock",
                JobStatus::Open,
                json!({
                    "branch": "fix/no-clock",
                    "merged": true,
                    "proof_probe": "true",
                    "proof_attempt": { "not_yet": true, "exit": 75 },
                }),
            );
            let st = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-17T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            vec![(j, st)]
        };
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &no_clock,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(
            shed.state,
            RegionState::Clear,
            "a car with no opened_at has no age, so it is not past any band — never \
             troubled for a field it does not carry: {}",
            shed.why
        );
    }

    /// The shed classification is total and the probe wins over an
    /// event — the same rule `boss orient` prints by (it now calls this).
    #[test]
    fn the_shed_place_is_total_and_the_probe_wins() {
        assert_eq!(shed_place(&json!({})), ShedPlace::Unproven);
        assert_eq!(
            shed_place(&json!({ "proof_event": "next red train" })),
            ShedPlace::WaitingOn("next red train".into())
        );
        assert_eq!(
            shed_place(&json!({ "proof_probe": "true", "proof_event": "x" })),
            ShedPlace::ProbePending { last: None }
        );
        assert_eq!(
            shed_place(
                &json!({ "proof_probe": "true", "proof_attempt": { "exit": 75, "why": "not yet: no train" } })
            ),
            ShedPlace::ProbeNotYet {
                said: "not yet: no train".into()
            }
        );
        assert_eq!(
            shed_place(
                &json!({ "proof_probe": "true", "proof_attempt": { "exit": 1, "why": "FAILED" } })
            ),
            ShedPlace::ProbePending {
                last: Some("FAILED".into())
            }
        );
    }

    /// THE SHED'S PLACES ARE DISJOINT AND SUM TO ITS COUNT — the three
    /// the floor draws, named as the floor names them, so the region map
    /// can say which of them its slice falls short in (the review drew 5
    /// wagons under a SHED 11).
    #[test]
    fn the_shed_says_how_many_cars_stand_in_each_of_its_three_places() {
        let awaiting = |branch: &str, md: Value| {
            let mut md = md;
            md["branch"] = json!(branch);
            let j = job("ship-a-change", branch, JobStatus::Open, md);
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-19T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            (j, s)
        };
        let cars = vec![
            awaiting("fix/a", json!({ "proof_probe": "true" })),
            awaiting(
                "fix/b",
                json!({ "proof_probe": "true", "proof_event": "both" }),
            ),
            awaiting("fix/c", json!({ "proof_event": "the next red train" })),
            awaiting("fix/d", json!({})),
        ];
        let status = empty_status();
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        let shed = by_name(&out, "shed");
        let places: Vec<(&str, usize)> = shed
            .places
            .iter()
            .map(|p| (p.name.as_str(), p.count))
            .collect();
        assert_eq!(
            places,
            vec![
                ("inspection-shed", 2),
                ("siding-event", 1),
                ("siding-no-probe", 1)
            ]
        );
        assert_eq!(
            Some(shed.places.iter().map(|p| p.count).sum::<usize>()),
            shed.count
        );
    }
}
