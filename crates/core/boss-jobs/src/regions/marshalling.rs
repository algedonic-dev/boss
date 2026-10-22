//! MARSHALLING: packets standing at a station other than the dock,
//! counted once each and never a packet another region holds.

use super::*;

pub(super) fn unread_station_flow(station: &StationReading) -> Option<String> {
    (!station.members.is_empty() && station.served.is_none()).then(|| {
        format!(
            "station {}: {} standing — {}",
            station.name,
            station.members.len(),
            station
                .flow_unread
                .as_deref()
                .unwrap_or("flow reading unavailable"),
        )
    })
}

/// A station holding work that nothing left in the window: the case the
/// owner-and-bound judgement below decides.
pub(super) fn not_draining(station: &StationReading) -> bool {
    !station.members.is_empty() && station.served == Some(0)
}

/// ONE QUALIFIED WAIT: a step standing at a station, waiting on its
/// named owner, inside the bound its packet's pinned protocol declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OwnedWait {
    /// `design-doc “title” step answer`, the packet by protocol + title.
    pub what: String,
    /// The assignee, else `role <code>`.
    pub owner: String,
    /// When the declared bound runs out: the next check this wait owes.
    pub due: Instant,
}

/// How a step names its packet in a sentence: protocol, title, step.
fn named(step: &StandingStep) -> String {
    format!(
        "{} “{}” step {}",
        step.kind,
        step.title,
        step.step.as_deref().unwrap_or("(no slug)")
    )
}

/// The bound the packet's PINNED protocol declares for the step, in
/// hours — `wall_clock_hours`, else `duration_hours`, the order the
/// executors pace by (`registry::StepSpec`). Never the StepType kind's
/// typical duration, which paces the simulator and records nothing about
/// this protocol, and never today's active row: a missing bound is
/// refused, not invented (design 5da17ace: no unbounded exemption).
fn declared_bound(step: &StandingStep, protocols: Option<&PinnedProtocols>) -> Result<f64, String> {
    let binding = format!("{}@{}", step.kind, step.version);
    let spec = match protocols
        .ok_or("the pinned protocols could not be read")?
        .get(&(step.kind.clone(), step.version))
    {
        Some(Ok(spec)) if spec.kind == step.kind && spec.version == step.version => spec,
        Some(Ok(_)) => {
            return Err(format!(
                "{binding}: the registry returned a different binding"
            ));
        }
        Some(Err(reason)) => return Err(reason.clone()),
        None => return Err(format!("{binding}: pinned protocol version is unread")),
    };
    // A rejected row was never published either (backlog 50ded63d).
    if !spec.status.was_published() {
        return Err(format!("{binding} was never published"));
    }
    let slug = step
        .step
        .as_deref()
        .ok_or_else(|| format!("{binding}: the step names no protocol slug"))?;
    spec.steps
        .iter()
        .find(|s| s.title == slug)
        .and_then(|s| s.wall_clock_hours.or(s.duration_hours))
        .filter(|hours| hours.is_finite() && *hours > 0.0)
        .ok_or_else(|| format!("{binding} declares no bound for step {slug}"))
}

/// IS THIS ONE STEP A QUALIFIED WAIT (backlog 6254e602)? Owned — an
/// assignee, else its audience role — dated from its recorded ready
/// flip, and inside its declared bound. Every other answer is the reason
/// it is not, in a sentence that names the packet.
fn owned_wait(
    step: &StandingStep,
    protocols: Option<&PinnedProtocols>,
    now: Instant,
) -> Result<OwnedWait, String> {
    let what = named(step);
    let nonblank = |v: &Option<String>| v.clone().filter(|v| !v.trim().is_empty());
    let owner = nonblank(&step.assignee)
        .or_else(|| nonblank(&step.role).map(|role| format!("role {role}")))
        .ok_or_else(|| format!("{what}: no owner — no assignee and no audience role"))?;
    let hours = declared_bound(step, protocols).map_err(|why| format!("{what}: {why}"))?;
    let since = match &step.since {
        Ok(Some(since)) => *since,
        Ok(None) => {
            return Err(format!(
                "{what}, owner {owner}: its ready instant is not on the record, so its bound cannot be dated"
            ));
        }
        // The read failed, not the record (backlog 35d29890): the
        // sentence names the read and its error.
        Err(why) => {
            return Err(format!(
                "{what}, owner {owner}: the queue-age read failed ({why}), so its ready instant is unread and its bound cannot be dated"
            ));
        }
    };
    #[allow(clippy::cast_possible_truncation)]
    let bound = chrono::Duration::try_seconds((hours * 3600.0).ceil() as i64)
        .ok_or_else(|| format!("{what}: its {hours}h bound is out of range"))?;
    let due = since + bound;
    if due <= now {
        return Err(format!(
            "{what}: waiting on {owner} {}, past its {hours}h bound",
            bands::duration_text((now - since).num_minutes().max(0))
        ));
    }
    Ok(OwnedWait { what, owner, due })
}

/// IS THIS MEMBER A QUALIFIED WAIT? Every step that put it at the
/// station must be one; a member none of whose steps was read is not —
/// nothing names who it waits on.
pub(super) fn member_wait(
    station: &StationReading,
    member: &str,
    protocols: Option<&PinnedProtocols>,
    now: Instant,
) -> Result<Vec<OwnedWait>, Vec<String>> {
    let steps: Vec<&StandingStep> = station
        .standing
        .iter()
        .filter(|s| s.packet == member)
        .collect();
    if steps.is_empty() {
        return Err(vec![format!(
            "{member}: no standing step read, so nothing names who it waits on"
        )]);
    }
    let (ok, err): (Vec<_>, Vec<_>) = steps
        .into_iter()
        .map(|s| owned_wait(s, protocols, now))
        .partition(Result::is_ok);
    if err.is_empty() {
        Ok(ok.into_iter().filter_map(Result::ok).collect())
    } else {
        Err(err.into_iter().filter_map(Result::err).collect())
    }
}

/// A STATION THAT SERVED NOTHING, JUDGED: `Ok` with its waits when every
/// member is a qualified wait, else `Err` with the reason for each one
/// that is not — the worst genuine finding kept (design 5da17ace).
pub(super) fn station_wait(
    station: &StationReading,
    protocols: Option<&PinnedProtocols>,
    now: Instant,
) -> Result<Vec<OwnedWait>, Vec<String>> {
    let (ok, err): (Vec<_>, Vec<_>) = station
        .members
        .iter()
        .map(|m| member_wait(station, m, protocols, now))
        .partition(Result::is_ok);
    if err.is_empty() {
        Ok(ok.into_iter().filter_map(Result::ok).flatten().collect())
    } else {
        Err(err.into_iter().filter_map(Result::err).flatten().collect())
    }
}

/// How many packets a sentence names before it says how many more: a
/// station can hold hundreds, and a header naming each is unreadable.
const NAMED: usize = 3;

/// `items`, the first [`NAMED`] of them, then how many more.
pub(super) fn capped(items: &[String]) -> String {
    let mut out = items.iter().take(NAMED).cloned().collect::<Vec<_>>();
    if items.len() > NAMED {
        out.push(format!("and {} more", items.len() - NAMED));
    }
    out.join("; ")
}

/// The waits, as a sentence: who each is on and when its bound runs out,
/// the soonest first — that is the next check the station owes.
pub(super) fn waits_text(waits: &[OwnedWait]) -> String {
    let mut waits = waits.to_vec();
    waits.sort_by_key(|w| w.due);
    capped(
        &waits
            .iter()
            .map(|w| {
                format!(
                    "{}, owner {}, inside its bound until {}",
                    w.what,
                    w.owner,
                    w.due.format("%Y-%m-%d %H:%M UTC")
                )
            })
            .collect::<Vec<_>>(),
    )
}

/// MARSHALLING: packets standing at a station other than the dock,
/// counted once each. The marshalling yard's grammar decides the
/// state: a station over its WIP limit, or holding work that nothing
/// left in the window (not draining), is troubled — unless every step
/// standing there waits on its named owner inside its declared bound,
/// which is attention ([`station_wait`], backlog 6254e602); any station
/// holding work that drains is clear. The trend is packets served per day, summed over the
/// stations whose flow the cube can count.
pub(super) fn marshalling(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    const UNIT: &str = "packets at stations";
    // THE PARTITION (design 62de32ae decision 4): each station holding
    // only what is marshalling's — never a packet still in receiving or
    // held by a region of its own.
    let stations = match marshalling_view(inputs) {
        Ok(view) => view,
        Err(why) => {
            return region(
                "marshalling",
                None,
                None,
                UNIT,
                unread_settled(why, inputs.now),
                Trend {
                    metric: "served".to_string(),
                    unit: "per day".to_string(),
                    current: None,
                    previous: None,
                    samples: 0,
                    previous_samples: 0,
                },
                vec![measure("oldest at a station", None, "hours")],
            );
        }
    };
    let distinct: std::collections::BTreeSet<&str> = stations
        .iter()
        .flat_map(|s| s.members.iter().map(String::as_str))
        .collect();
    let served: Option<i64> = stations
        .iter()
        .filter_map(|s| s.served)
        .reduce(|a, b| a + b);
    let previous: Option<i64> = stations
        .iter()
        .filter_map(|s| s.previous_served)
        .reduce(|a, b| a + b);
    let to_rate = |n: Option<i64>| {
        n.and_then(|n| usize::try_from(n).ok())
            .map(|n| w.per_day(n))
    };
    let trend = Trend {
        metric: "served".to_string(),
        unit: "per day".to_string(),
        current: to_rate(served),
        previous: to_rate(previous),
        samples: served.and_then(|n| usize::try_from(n).ok()).unwrap_or(0),
        previous_samples: previous.and_then(|n| usize::try_from(n).ok()).unwrap_or(0),
    };
    let over: Vec<&str> = stations
        .iter()
        .filter(|s| s.over_limit)
        .map(|s| s.name.as_str())
        .collect();
    // A STATION THAT SERVED NOTHING IS JUDGED ON WHAT STANDS THERE
    // (backlog 6254e602): every step waiting on its named owner inside
    // its declared bound is a qualified wait, attention; anything else is
    // not draining, troubled, and says which packet and why.
    let (mut stuck, mut owned): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for s in stations.iter().filter(|s| not_draining(s)) {
        match station_wait(s, inputs.internal_protocols, inputs.now) {
            Ok(waits) => owned.push(format!("{}: {}", s.name, waits_text(&waits))),
            Err(why) => stuck.push(format!("{} ({})", s.name, capped(&why))),
        }
    }
    let holding = stations.iter().filter(|s| !s.members.is_empty()).count();
    let n = distinct.len();
    let mut findings = Vec::new();
    let unread = stations
        .iter()
        .filter_map(unread_station_flow)
        .collect::<Vec<_>>();
    if !over.is_empty() {
        findings.push(Finding::new(
            bands::MARSHALLING_OVER_LIMIT,
            None,
            String::new(),
            format!("over the WIP limit: {}", over.join(", ")),
        ));
    }
    if !stuck.is_empty() {
        findings.push(Finding::new(
            bands::MARSHALLING_NOT_DRAINING,
            None,
            String::new(),
            format!(
                "not draining — nothing left in {}h: {}",
                w.hours,
                stuck.join(", ")
            ),
        ));
    }
    if !owned.is_empty() {
        findings.push(Finding::new(
            bands::MARSHALLING_OWNED_WAIT,
            None,
            String::new(),
            format!(
                "nothing left in {}h, every step there waiting on its named owner inside its declared bound — {}",
                w.hours,
                owned.join(" · ")
            ),
        ));
    }
    if !unread.is_empty() {
        findings.push(Finding::new(
            bands::UNREAD,
            None,
            String::new(),
            unread.join("; "),
        ));
    }
    // Packets standing at stations that drain is the yard working —
    // clear (decision 1).
    let clear_why = if holding > 0 {
        format!(
            "{} standing at {}",
            plural(n, "packet", "packets"),
            plural(holding, "station", "stations")
        )
    } else {
        "nothing waiting at any station".to_string()
    };
    let mut settled = settle(findings, clear_why, inputs.now);
    if !unread.is_empty()
        && settled
            .band
            .as_ref()
            .is_some_and(|band| band.id != bands::UNREAD.id)
    {
        settled.why = format!("{}; {}", settled.why, unread.join("; "));
    }
    // THE KPI (decision 9): the oldest packet standing at a station, in
    // hours from its opening — of the members the partition left here.
    let oldest = stations
        .iter()
        .flat_map(|s| s.members.iter().filter_map(|m| s.opened.get(m)))
        .min()
        .map(|t| (inputs.now - t).num_hours().max(0));
    #[allow(clippy::cast_precision_loss)]
    let mut kpi = vec![match oldest {
        Some(h) => measure("oldest at a station", Some(h as f64), "hours"),
        None if n == 0 => measure_said(
            "oldest at a station",
            None,
            "hours",
            "nothing standing".to_string(),
        ),
        None => measure("oldest at a station", None, "hours"),
    }];
    // BLINDNESS COUNTED (decision 11): the stations whose flow nobody
    // can count are drawn `?`, and the header says how many — a zero
    // is stated too, because it is a reading.
    let blind = stations.iter().filter(|s| unjudged(s)).count();
    kpi.push(measure_said(
        "stations unjudged",
        count_value(blind),
        "stations",
        if blind == 0 {
            "every station judged".to_string()
        } else {
            format!("{} unjudged", plural(blind, "station", "stations"))
        },
    ));
    // EACH STATION AT MARSHALLING'S OWN MEMBERS (the rest of decision
    // 5): the partition, per place, so the interior can draw the head's
    // count instead of each station's full depth.
    let places = stations
        .iter()
        .map(|s| Place {
            name: s.name.clone(),
            count: s.members.len(),
        })
        .collect();
    Region {
        places,
        ..region("marshalling", Some(n), None, UNIT, settled, trend, kpi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    #[test]
    fn unread_required_station_flow_requires_action_with_its_exact_reason() {
        let status = empty_status();
        for reason in [
            None,
            Some("native membership starting state exceeds row bound"),
        ] {
            let mut station = holding("q.platform-admin.task", &["packet"]);
            station.served = None;
            station.flow_unread = reason.map(str::to_string);
            let stations = [station];
            let out = regions(&inputs(
                &status,
                &[],
                &[],
                &[],
                &[],
                Some(&[]),
                Some(&stations),
            ));
            let reading = by_name(&out, "marshalling");
            assert_eq!(
                reading.state,
                RegionState::Troubled,
                "required flow cannot be read as healthy: {reason:?}"
            );
            assert_eq!(
                reading.count,
                Some(1),
                "the observed membership remains known"
            );
            assert!(reading.why.contains("q.platform-admin.task"));
            assert!(reading.why.contains(reason.unwrap_or("unavailable")));
            let input = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&stations));
            let summary = stuck(&input)
                .into_iter()
                .find(|third| third.third == "queue-management")
                .unwrap();
            assert_eq!(summary.unknown, vec![reading.why.clone()]);
        }
    }

    #[test]
    fn unread_flow_does_not_hide_an_observed_wip_fault() {
        let status = empty_status();
        let mut station = holding("q.platform-admin.task", &["packet"]);
        station.served = None;
        station.over_limit = true;
        station.flow_unread = Some("native membership starting state exceeds byte bound".into());
        let stations = [station];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&[]),
            Some(&stations),
        ));
        let reading = by_name(&out, "marshalling");
        assert_eq!(reading.state, RegionState::Troubled);
        assert_eq!(
            reading.band.as_ref().unwrap().id,
            bands::MARSHALLING_OVER_LIMIT.id
        );
        assert!(reading.why.contains("WIP limit"));
        assert!(
            reading
                .why
                .contains("native membership starting state exceeds byte bound")
        );
    }

    /// Marshalling: packets counted once across stations; a station
    /// nothing leaves is trouble; served per day sums the countable
    /// stations.
    #[test]
    fn marshalling_counts_packets_once_and_a_station_not_draining_is_trouble() {
        let stations = vec![
            StationReading {
                name: "q.platform-admin.task".into(),
                over_limit: false,
                members: vec!["p1".into(), "p2".into()],
                served: Some(4),
                flow_unread: None,
                previous_served: Some(2),
                opened: [("p1".to_string(), t("2026-09-18T12:00:00Z"))].into(),
                standing: Vec::new(),
            },
            StationReading {
                name: "design-review".into(),
                over_limit: false,
                members: vec!["p2".into()],
                served: Some(1),
                flow_unread: None,
                previous_served: Some(0),
                opened: [("p2".to_string(), t("2026-09-19T06:00:00Z"))].into(),
                standing: Vec::new(),
            },
            StationReading {
                name: "my-watchlist".into(),
                over_limit: false,
                members: vec![],
                served: None,
                flow_unread: None,
                previous_served: None,
                opened: Default::default(),
                standing: Vec::new(),
            },
        ];
        let status = empty_status();
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&[]),
            Some(&stations),
        ));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.count, Some(2), "p2 stands at two stations, counted once");
        // Standing at stations that drain is the yard working.
        assert_eq!(m.state, RegionState::Clear, "{}", m.why);
        assert_eq!(m.kpi[0].text, "oldest at a station 24 hours");
        assert_eq!(m.trend.current, Some(5.0));
        assert_eq!(m.trend.previous, Some(2.0));

        let mut stuck = stations.clone();
        stuck[1].served = Some(0);
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&[]),
            Some(&stuck),
        ));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.state, RegionState::Troubled);
        assert!(m.why.contains("design-review"), "{}", m.why);
    }

    // --- a station that served nothing: who owns what stands there ------
    //
    // Backlog 6254e602 (split from df8fe965, design 5da17ace): live on
    // 2026-10-17 the region read TROUBLED (not-draining) on
    // q.platform-admin.answer-question, which held five steps owned by
    // emp-david — waiting on David read red, though red means an
    // immediate action. The not-draining judgement now reads each
    // standing step's owner and the bound its pinned protocol declares.

    /// The protocol the answer-question packets are pinned to, its
    /// `answer` step declaring `hours` (or nothing).
    fn pinned(hours: Option<f64>) -> PinnedProtocols {
        [WorkflowSpec::platform_seed(
            "design-doc",
            "Design",
            "platform",
            vec!["custom".into()],
            vec![crate::registry::StepSpec {
                title: "answer".into(),
                duration_hours: hours,
                ..Default::default()
            }],
        )]
        .into_iter()
        .map(|spec| ((spec.kind.clone(), spec.version), Ok(spec)))
        .collect()
    }

    fn waiting(
        packet: &str,
        assignee: Option<&str>,
        role: Option<&str>,
        since: &str,
    ) -> StandingStep {
        StandingStep {
            packet: packet.into(),
            kind: "design-doc".into(),
            version: 1,
            title: format!("question {packet}"),
            step: Some("answer".into()),
            assignee: assignee.map(str::to_string),
            role: role.map(str::to_string),
            since: Ok(Some(t(since))),
        }
    }

    /// A station holding `steps`, nothing served in the window.
    fn quiet(name: &str, steps: Vec<StandingStep>) -> StationReading {
        let mut members: Vec<String> = steps.iter().map(|s| s.packet.clone()).collect();
        members.dedup();
        StationReading {
            name: name.into(),
            members,
            served: Some(0),
            previous_served: Some(0),
            standing: steps,
            ..Default::default()
        }
    }

    fn judged(stations: &[StationReading], protocols: &PinnedProtocols) -> Regions {
        let status = empty_status();
        let mut input = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(stations));
        input.internal_protocols = Some(protocols);
        regions(&input)
    }

    fn third_of(out: &Regions) -> ThirdStuck {
        out.stuck
            .iter()
            .find(|third| third.third == "queue-management")
            .cloned()
            .unwrap()
    }

    /// OWNED, INSIDE ITS BOUND: steps waiting on their named owner, an
    /// hour into the 24h their protocol declares, are an understood wait
    /// — attention naming the owner and when the bound runs out, never
    /// red; the station glyph is not a failed machine, and the stuck
    /// summary counts them waiting, not stuck.
    #[test]
    fn a_station_whose_steps_wait_on_their_named_owner_inside_the_bound_is_attention() {
        let station = quiet(
            "q.platform-admin.answer-question",
            vec![
                waiting(
                    "q1",
                    Some("emp-david"),
                    Some("platform-admin"),
                    "2026-09-19T11:00:00Z",
                ),
                waiting(
                    "q2",
                    Some("emp-david"),
                    Some("platform-admin"),
                    "2026-09-19T10:00:00Z",
                ),
            ],
        );
        let out = judged(&[station], &pinned(Some(24.0)));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.state, RegionState::Attention, "{}", m.why);
        assert_eq!(
            m.band.as_ref().unwrap().id,
            bands::MARSHALLING_OWNED_WAIT.id
        );
        assert!(
            m.why.contains("q.platform-admin.answer-question")
                && m.why.contains("emp-david")
                && m.why.contains("2026-09-20 10:00"),
            "names the station, the owner and when the bound runs out: {}",
            m.why
        );
        assert_ne!(
            machine_state(
                &out,
                "marshalling",
                "station:q.platform-admin.answer-question"
            ),
            MachineState::Failed
        );
        let q = third_of(&out);
        assert_eq!((q.stuck, q.waiting), (0, 2), "{q:?}");
    }

    /// The owner may be the role the step's audience names, when nobody
    /// has been assigned it: still a named owner.
    #[test]
    fn a_step_owned_by_its_audience_role_inside_its_bound_is_attention_naming_the_role() {
        let station = quiet(
            "q.platform-admin.answer-question",
            vec![waiting(
                "q1",
                None,
                Some("platform-admin"),
                "2026-09-19T11:00:00Z",
            )],
        );
        let out = judged(&[station], &pinned(Some(24.0)));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.state, RegionState::Attention, "{}", m.why);
        assert!(m.why.contains("platform-admin"), "{}", m.why);
    }

    /// UNOWNED: no assignee and no audience role — nobody is waited on,
    /// so the station is not draining and stays red.
    #[test]
    fn a_station_whose_step_names_no_owner_stays_troubled() {
        let station = quiet(
            "q.platform-admin.answer-question",
            vec![waiting("q1", None, None, "2026-09-19T11:00:00Z")],
        );
        let out = judged(&[station], &pinned(Some(24.0)));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.state, RegionState::Troubled, "{}", m.why);
        assert_eq!(
            m.band.as_ref().unwrap().id,
            bands::MARSHALLING_NOT_DRAINING.id
        );
        assert!(m.why.contains("no owner"), "{}", m.why);
        assert_eq!(
            machine_state(
                &out,
                "marshalling",
                "station:q.platform-admin.answer-question"
            ),
            MachineState::Failed
        );
        assert_eq!(third_of(&out).stuck, 1);
    }

    /// OVERDUE: the same owner, past the bound — red, and it says by how
    /// much and whose it is.
    #[test]
    fn a_step_waiting_on_its_owner_past_the_bound_stays_troubled() {
        let station = quiet(
            "q.platform-admin.answer-question",
            vec![waiting(
                "q1",
                Some("emp-david"),
                Some("platform-admin"),
                "2026-09-17T12:00:00Z",
            )],
        );
        let out = judged(&[station], &pinned(Some(24.0)));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.state, RegionState::Troubled, "{}", m.why);
        assert_eq!(
            m.band.as_ref().unwrap().id,
            bands::MARSHALLING_NOT_DRAINING.id
        );
        assert!(
            m.why.contains("emp-david") && m.why.contains("past its 24h bound"),
            "{}",
            m.why
        );
        assert_eq!(third_of(&out).stuck, 1);
    }

    /// No bound declared, no recorded onset, no pinned protocol read:
    /// a wait nobody can prove is inside its bound is not qualified —
    /// red, saying which fact is missing (design 5da17ace: never an
    /// unbounded exemption).
    #[test]
    fn a_wait_whose_bound_or_onset_is_not_on_the_record_stays_troubled() {
        let owned = || waiting("q1", Some("emp-david"), None, "2026-09-19T11:00:00Z");
        let no_bound = judged(&[quiet("s", vec![owned()])], &pinned(None));
        let m = by_name(&no_bound, "marshalling");
        assert_eq!(m.state, RegionState::Troubled, "{}", m.why);
        assert!(m.why.contains("declares no bound"), "{}", m.why);

        let mut unrecorded = owned();
        unrecorded.since = Ok(None);
        let out = judged(&[quiet("s", vec![unrecorded])], &pinned(Some(24.0)));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.state, RegionState::Troubled, "{}", m.why);
        assert!(m.why.contains("ready instant"), "{}", m.why);

        // The onset read FAILED (backlog 35d29890): still troubled, and
        // the sentence names the read and its error, never the record.
        let mut unread = owned();
        unread.since = Err("pool timed out".into());
        let out = judged(&[quiet("s", vec![unread])], &pinned(Some(24.0)));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.state, RegionState::Troubled, "{}", m.why);
        assert!(
            m.why.contains("the queue-age read failed (pool timed out)"),
            "{}",
            m.why
        );
        assert!(!m.why.contains("not on the record"), "{}", m.why);

        let mut other = owned();
        other.version = 7;
        let out = judged(&[quiet("s", vec![other])], &pinned(Some(24.0)));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.state, RegionState::Troubled, "{}", m.why);
        assert!(m.why.contains("design-doc@7"), "{}", m.why);

        // A member with no standing step read at all.
        let mut bare = quiet("s", vec![]);
        bare.members = vec!["q9".into()];
        let out = judged(&[bare], &pinned(Some(24.0)));
        assert_eq!(by_name(&out, "marshalling").state, RegionState::Troubled);
    }

    /// MIXED: the worst genuine finding is kept — one owned wait inside
    /// its bound beside one overdue step, at one station or across two,
    /// is red, and the qualified packet is still counted waiting.
    #[test]
    fn a_qualified_wait_beside_an_overdue_one_is_troubled() {
        let inside = waiting("q1", Some("emp-david"), None, "2026-09-19T11:00:00Z");
        let overdue = waiting("q2", Some("emp-david"), None, "2026-09-17T11:00:00Z");
        let one = judged(
            &[quiet(
                "q.platform-admin.answer-question",
                vec![inside.clone(), overdue.clone()],
            )],
            &pinned(Some(24.0)),
        );
        let two = judged(
            &[quiet("a", vec![inside]), quiet("b", vec![overdue])],
            &pinned(Some(24.0)),
        );
        for out in [one, two] {
            let m = by_name(&out, "marshalling");
            assert_eq!(m.state, RegionState::Troubled, "{}", m.why);
            assert_eq!(
                m.band.as_ref().unwrap().id,
                bands::MARSHALLING_NOT_DRAINING.id
            );
            let q = third_of(&out);
            assert_eq!((q.stuck, q.waiting), (1, 1), "{q:?}");
        }
    }

    /// An unread intake leaves marshalling unknown rather than counting
    /// packets that may still be receiving's — no evidence is not a pass.
    #[test]
    fn marshalling_without_the_inbound_read_is_unknown_not_a_double_count() {
        let stations = vec![holding("q.platform-admin.task", &["p1"])];
        let status = empty_status();
        let out = regions(&inputs(&status, &[], &[], &[], &[], None, Some(&stations)));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.count, None);
        assert_eq!(m.state, RegionState::Troubled);
        assert!(m.why.contains("still in receiving"), "{}", m.why);
        assert!(m.places.is_empty(), "an unread partition has no places");
    }

    /// EACH STATION AT ITS PARTITIONED COUNT. Live on 2026-09-24 the
    /// marshalling platforms drew 562 standings under a head of 236:
    /// each station drew its full depth, because the station reads carry
    /// no partition. The server holds it, so it says per station how many
    /// of MARSHALLING'S members stand there — an untriaged packet at the
    /// same station is receiving's and is not among them. A packet at
    /// two stations is at both, and the places sum past the head by
    /// exactly the packets that stand twice.
    #[test]
    fn marshalling_says_how_many_of_its_own_members_stand_at_each_station() {
        let untriaged = admitted(
            "backlog-item",
            "untriaged",
            &[("triage", "task", StepStatus::Ready)],
        );
        let triaged = admitted(
            "backlog-item",
            "triaged",
            &[
                ("triage", "task", StepStatus::Completed),
                ("build", "task", StepStatus::Ready),
            ],
        );
        let (u, tr) = (untriaged.0.id.to_string(), triaged.0.id.to_string());
        let inbound = vec![untriaged, triaged];
        let stations = vec![
            holding("q.platform-admin.task", &[&u, &tr, "other"]),
            holding("a.platform-admin.opus", &[&tr]),
            holding("design-review", &[]),
        ];
        let status = empty_status();
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&inbound),
            Some(&stations),
        ));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.count, Some(2), "{}", m.why);
        let places: Vec<(&str, usize)> = m
            .places
            .iter()
            .map(|p| (p.name.as_str(), p.count))
            .collect();
        assert_eq!(
            places,
            vec![
                ("q.platform-admin.task", 2),
                ("a.platform-admin.opus", 1),
                ("design-review", 0),
            ],
            "every station, in the registry's order, at marshalling's own members"
        );
        let drawn: usize = m.places.iter().map(|p| p.count).sum();
        assert_eq!(
            drawn - m.count.unwrap(),
            1,
            "the one packet at two stations"
        );
        // Nothing else has places it does not declare.
        assert!(by_name(&out, "receiving").places.is_empty());
    }

    /// STATIONS THE SERVER CANNOT JUDGE ARE COUNTED IN THE HEADER
    /// (decision 11): blindness is itself a KPI. A station is unjudged by
    /// the same predicate that draws its glyph `?`, so the header and the
    /// glyphs cannot disagree about how many there are.
    #[test]
    fn marshalling_counts_the_stations_it_cannot_judge_in_its_header() {
        let blind = |name: &str| StationReading {
            name: name.into(),
            served: None,
            previous_served: None,
            ..Default::default()
        };
        let stations = vec![
            blind("design-review"),
            blind("a.platform-admin.opus"),
            holding("q.platform-admin.task", &[]),
        ];
        let status = empty_status();
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&[]),
            Some(&stations),
        ));
        let m = by_name(&out, "marshalling");
        let unjudged = m
            .kpi
            .iter()
            .find(|k| k.name == "stations unjudged")
            .expect("the header counts the stations it cannot judge");
        assert_eq!(unjudged.value, Some(2.0));
        assert_eq!(unjudged.text, "2 stations unjudged");
        let glyphs = m
            .machines
            .iter()
            .filter(|g| g.state == MachineState::Unknown)
            .count();
        assert_eq!(glyphs, 2, "the header's count is the glyphs' count");

        let judged = vec![holding("q.platform-admin.task", &[])];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&[]),
            Some(&judged),
        ));
        let k = by_name(&out, "marshalling")
            .kpi
            .iter()
            .find(|k| k.name == "stations unjudged")
            .cloned()
            .expect("a zero is a reading too");
        assert_eq!(
            (k.value, k.text.as_str()),
            (Some(0.0), "every station judged")
        );
    }
}
