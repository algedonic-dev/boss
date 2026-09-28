//! NEXT UP — the top board's third row (backlog 74569e94, design
//! ea906603 Q3, decided 2026-09-27; car 3).
//!
//! David, 2026-09-26: show "upcoming events for the IT department that
//! are relevant, like the next train departure". Every one of them was
//! already derivable and none was served: the next board from the
//! cadence rows, the dock and the last departure; a train's arrival from
//! the measured transits; the gate line's wait from the measured gates;
//! the dispatcher's scheduled rules from its runner; a credential's
//! rotation from the registry. So it is a field on `GET
//! /api/yard/regions`, computed here once, and `boss orient` prints the
//! same rows.
//!
//! ONE DEFINITION EACH. The board, the trains and the gates are read off
//! the yard status this same pass built ([`YardStatus`]) — its boarding
//! block is the conductor's decision re-derived and pinned there, its
//! ETA is the arrivals' own measurement, its gate waits are the floor's
//! — so this module converts those answers into instants and never
//! re-decides one. The scheduled rules are the dispatcher's own
//! `next_due` (car 2's read). A credential's rotation comes from the
//! registry row, which declares a policy but no period: a `scheduled`
//! row is listed without a time and says so, because a date the
//! registry does not hold would be a belief typed into the board (Q4's
//! rule for the host windows, applied to rotations).
//!
//! NOTHING SILENTLY ABSENT. A source that could not be read is a row
//! with `unread` set and its reason, never a missing row: an empty list
//! reads as "nothing is coming", which is a claim. A source that WAS
//! read and has nothing coming contributes nothing. An event that is
//! coming but has no time (an ETA the history cannot support, a board
//! waiting on the track) is a row with `at: null` and its `basis`
//! saying why.
//!
//! Pure: the handler reads, this computes.

use boss_core::calendar::Cadence;
use chrono::{DateTime, Duration, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

use crate::credentials::CredentialRow;
use crate::dispatcher_schedule::ScheduledRule;
use crate::yard::{
    BoardingPredicate, Gates, Reading, TrainEta, TrainPhase, TrainStatus, YardStatus,
};

/// How many TIMED rows the board carries — the next six, soonest first
/// (design ea906603 Q3). Rows without a time and unread sources ride
/// after them uncapped: they are few, and each is a thing to know.
pub const CAP: usize = 6;

/// Where a row came from, in words a reader can follow to the source.
pub const SOURCE_CADENCE: &str = "cadence registry + loading dock";
pub const SOURCE_TRAINS: &str = "open trains + measured arrivals";
pub const SOURCE_GATES: &str = "gate-runs + measured gate durations";
pub const SOURCE_DISPATCHER: &str = "dispatcher schedule";
pub const SOURCE_CREDENTIALS: &str = "credentials registry";

/// What kind of event a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NextKind {
    /// The next board on dock depth (the queue-depth cadence rule).
    TrainBoard,
    /// The next clock window a train boards at (the clock cadence rule).
    TrainWindow,
    /// A train on the track and when it is expected to arrive.
    InTransit,
    /// The gate line: the next bay to free.
    Gates,
    /// A dispatcher scheduled rule's next firing.
    Scheduled,
    /// A credential whose rotation is scheduled.
    RotationDue,
}

/// One upcoming event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextEvent {
    pub kind: NextKind,
    pub title: String,
    /// When, UTC — a surface converts it to the viewer's zone. `None`
    /// when no time can be given; `basis` then says why, and `unread`
    /// says whether that is because the source could not be read.
    pub at: Option<DateTime<Utc>>,
    /// `true` when `at` is an estimate (a median, a cooldown landing on
    /// a tick) rather than a time a registry schedules — drawn "~".
    pub estimate: bool,
    /// What the countdown counts to and what `at` rests on — or, with no
    /// `at`, why there is none.
    pub basis: String,
    /// The read the row came from (`SOURCE_*`).
    pub source: String,
    /// The source could not be read, and why. `at` is then null.
    pub unread: Option<String>,
}

/// Everything the row is computed from.
pub struct NextUpInputs<'a> {
    /// This pass's yard status: the boarding block, the trains, the gates.
    pub yard: &'a YardStatus,
    /// The dispatcher's schedule, or why it could not be read.
    pub schedule: &'a Result<Vec<ScheduledRule>, String>,
    /// The credentials registry, or why it could not be read.
    pub credentials: &'a Result<Vec<CredentialRow>, String>,
    pub now: DateTime<Utc>,
}

/// The board's row: timed events soonest first, capped at [`CAP`]; then
/// the events with no time; then the sources that could not be read.
pub fn next_up(inputs: &NextUpInputs<'_>) -> Vec<NextEvent> {
    let now = inputs.now;
    let y = inputs.yard;
    let mut rows = board_rows(&y.boarding, &y.trains, now);
    rows.extend(transit_rows(&y.trains, now));
    rows.extend(gate_row(&y.gates, now));
    rows.extend(scheduled_rows(inputs.schedule));
    rows.extend(rotation_rows(inputs.credentials));

    let (unread, read): (Vec<NextEvent>, Vec<NextEvent>) =
        rows.into_iter().partition(|r| r.unread.is_some());
    let (mut timed, untimed): (Vec<NextEvent>, Vec<NextEvent>) =
        read.into_iter().partition(|r| r.at.is_some());
    // Soonest first; a tie by kind then title, so two reads of one yard
    // answer in one order.
    timed.sort_by(|a, b| (a.at, a.kind, &a.title).cmp(&(b.at, b.kind, &b.title)));
    timed.truncate(CAP);
    timed.into_iter().chain(untimed).chain(unread).collect()
}

fn row(kind: NextKind, title: String, source: &str) -> NextEvent {
    NextEvent {
        kind,
        title,
        at: None,
        estimate: false,
        basis: String::new(),
        source: source.to_string(),
        unread: None,
    }
}

fn unread_row(kind: NextKind, title: &str, source: &str, why: String) -> NextEvent {
    NextEvent {
        basis: format!("{source} could not be read"),
        unread: Some(why),
        ..row(kind, title.to_string(), source)
    }
}

fn cars(n: usize) -> String {
    format!("{n} car{} waiting", if n == 1 { "" } else { "s" })
}

/// A train before its merge holds the track for every departing rule —
/// the conductor's single-track check (`yard::holds_the_track`), read
/// here off the phase the status already judged.
fn holds_the_track(t: &TrainStatus) -> bool {
    matches!(
        t.phase,
        TrainPhase::Boarding | TrainPhase::AwaitingCi | TrainPhase::AwaitingMerge
    )
}

/// The depth board and the clock window, from the boarding block — the
/// conductor's decision as the yard status re-derives it. A time is
/// given only where the block answers one: the cooldown's minutes, or
/// nothing holding (the next tick). Every other hold — the track, the
/// threshold — has no clock, and the block's own `next_board` sentence
/// says what clears it.
fn board_rows(b: &BoardingPredicate, trains: &[TrainStatus], now: DateTime<Utc>) -> Vec<NextEvent> {
    const TITLE: &str = "next train board";
    if b.cadence_reading == Reading::Unread {
        return vec![unread_row(
            NextKind::TrainBoard,
            TITLE,
            SOURCE_CADENCE,
            crate::yard::CADENCE_UNREAD.to_string(),
        )];
    }
    let on_track = trains.iter().filter(|t| holds_the_track(t)).count();
    let mut out = Vec::new();
    if let Some(threshold) = b.dock_threshold {
        out.push(match b.dock_depth {
            None => unread_row(
                NextKind::TrainBoard,
                TITLE,
                SOURCE_CADENCE,
                crate::yard::DEPTH_UNREAD.to_string(),
            ),
            Some(d) => {
                let base = row(
                    NextKind::TrainBoard,
                    format!("{TITLE}, {}", cars(d)),
                    SOURCE_CADENCE,
                );
                let met = i64::try_from(d).unwrap_or(i64::MAX) >= i64::from(threshold);
                if !met || on_track > 0 {
                    NextEvent {
                        basis: b.hold.next_board.clone(),
                        ..base
                    }
                } else if b.hold.last_board_reading == Reading::Unread {
                    unread_row(
                        NextKind::TrainBoard,
                        &base.title,
                        SOURCE_CADENCE,
                        crate::yard::FIRING_UNREAD.to_string(),
                    )
                } else {
                    let (at, basis) = match b.hold.cooldown_remaining_minutes {
                        Some(m) => (
                            now + Duration::minutes(i64::from(m)),
                            format!(
                                "the depth rule's {}-minute cooldown clears{}; the board lands \
                                 on the conductor's next tick after it",
                                b.cooldown_minutes.unwrap_or_default(),
                                b.hold
                                    .last_board_at
                                    .map(|l| format!(" (last board {})", l.format("%H:%MZ")))
                                    .unwrap_or_default()
                            ),
                        ),
                        None => (
                            now,
                            "nothing holds it: it boards on the conductor's next tick".to_string(),
                        ),
                    };
                    NextEvent {
                        at: Some(at),
                        estimate: true,
                        basis,
                        ..base
                    }
                }
            }
        });
    }
    if !b.at_times.is_empty() {
        let title = match b.dock_depth {
            Some(0) => "clock window — the dock is empty, nothing to board".to_string(),
            Some(d) => format!("clock window — boards the {}", cars(d)),
            None => "clock window".to_string(),
        };
        let track = if on_track > 0 {
            "; held while a train holds the track"
        } else {
            ""
        };
        let base = row(NextKind::TrainWindow, title, SOURCE_CADENCE);
        out.push(match next_window(&b.at_times, now) {
            Some(at) => NextEvent {
                at: Some(at),
                basis: format!(
                    "the clock rule's windows ({} UTC); the board lands on the conductor's \
                     next tick after it{track}",
                    b.at_times.join(", ")
                ),
                ..base
            },
            None => NextEvent {
                basis: format!(
                    "the clock rule's at_times ({}) do not read as HH:MM, so no window can be \
                     placed",
                    b.at_times.join(", ")
                ),
                ..base
            },
        });
    }
    out
}

/// The first of `at_times` (HH:MM, UTC) strictly after `now` — today's
/// if one is still ahead, else tomorrow's first. `None` when none parse.
fn next_window(at_times: &[String], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let today = now.date_naive();
    at_times
        .iter()
        .filter_map(|s| NaiveTime::parse_from_str(s, "%H:%M").ok())
        .flat_map(|t| {
            [today, today + Duration::days(1)]
                .map(|d| DateTime::from_naive_utc_and_offset(d.and_time(t), Utc))
        })
        .filter(|at| *at > now)
        .min()
}

/// Each train not yet arrived, at its measured ETA — or with the reason
/// the history cannot give one.
fn transit_rows(trains: &[TrainStatus], now: DateTime<Utc>) -> Vec<NextEvent> {
    trains
        .iter()
        .filter(|t| t.phase != TrainPhase::Arrived)
        .map(|t| {
            let base = row(
                NextKind::InTransit,
                format!("{} — {}, expected", t.title, t.phase.label()),
                SOURCE_TRAINS,
            );
            match &t.eta {
                TrainEta::Estimate {
                    leg,
                    remaining_seconds,
                    basis,
                    overdue,
                    ..
                } => NextEvent {
                    at: Some(now + Duration::seconds(*remaining_seconds)),
                    estimate: true,
                    basis: format!(
                        "{leg}: {basis}{}",
                        if *overdue {
                            " — OVERDUE: past the 90th percentile of every measured arrival"
                        } else {
                            ""
                        }
                    ),
                    ..base
                },
                TrainEta::Unknown { reason } => NextEvent {
                    basis: reason.clone(),
                    ..base
                },
            }
        })
        .collect()
}

/// The gate line: with a queue, the first in line's estimated wait (the
/// floor's own model); with none, the first running gate's expected
/// verdict at the measured median. An idle floor has nothing coming.
fn gate_row(g: &Gates, now: DateTime<Utc>) -> Option<NextEvent> {
    let unmeasured = "no gate duration was measured in this window, so the wait cannot be \
                      estimated";
    if let Some(first) = g.queued.first() {
        let base = row(
            NextKind::Gates,
            format!("{} queued, next bay", g.queued.len()),
            SOURCE_GATES,
        );
        return Some(match first.estimated_wait_seconds {
            Some(s) => NextEvent {
                at: Some(now + Duration::seconds(s)),
                estimate: true,
                basis: format!(
                    "{}'s estimated wait: {} bay(s), a median gate of {} min",
                    first.branch,
                    g.capacity,
                    g.typical_seconds.unwrap_or_default() / 60
                ),
                ..base
            },
            None => NextEvent {
                basis: unmeasured.to_string(),
                ..base
            },
        });
    }
    if g.active.is_empty() {
        return None;
    }
    let base = row(
        NextKind::Gates,
        format!("{} running, first verdict", g.active.len()),
        SOURCE_GATES,
    );
    let Some(typical) = g.typical_seconds else {
        return Some(NextEvent {
            basis: unmeasured.to_string(),
            ..base
        });
    };
    // A run whose start cannot be read is taken as just started — the
    // floor's own rule (`yard::estimated_waits`): over-stating a wait
    // leaves an operator early.
    let left = g
        .active
        .iter()
        .map(|a| {
            let elapsed = DateTime::parse_from_rfc3339(&a.since)
                .map_or(0, |t| (now - t.with_timezone(&Utc)).num_seconds());
            (typical - elapsed).max(0)
        })
        .min()
        .unwrap_or(typical);
    Some(NextEvent {
        at: Some(now + Duration::seconds(left)),
        estimate: true,
        basis: format!(
            "a median gate of {} min over the runs in the window",
            typical / 60
        ),
        ..base
    })
}

/// A cadence that fires hourly or faster: always minutes away, so the
/// rows fold into one rather than fill the board.
fn frequent(cadence: &str) -> bool {
    matches!(
        Cadence::parse(cadence),
        Some(Cadence::Hourly | Cadence::EveryNMinutes(_))
    )
}

/// Names for a title: all of up to three, else three and a count.
fn names(rules: &[&ScheduledRule]) -> String {
    let shown: Vec<&str> = rules.iter().take(3).map(|r| r.name.as_str()).collect();
    match rules.len() {
        n if n > 3 => format!("{} and {} more", shown.join(", "), n - 3),
        _ => shown.join(", "),
    }
}

/// The dispatcher's schedule as rows: the rules due at one instant are
/// one row (the day-boundary rules all fire at midnight UTC), the
/// hourly-or-faster fold into one row at the soonest of them, and a rule
/// the dispatcher could give no time is listed with its reason.
fn scheduled_rows(schedule: &Result<Vec<ScheduledRule>, String>) -> Vec<NextEvent> {
    let rules = match schedule {
        Err(why) => {
            return vec![unread_row(
                NextKind::Scheduled,
                "scheduled rules",
                SOURCE_DISPATCHER,
                why.clone(),
            )];
        }
        Ok(rules) => rules,
    };
    let guard = |group: &[&ScheduledRule]| {
        let guarded: Vec<&str> = group
            .iter()
            .filter(|r| r.when.is_some())
            .map(|r| r.name.as_str())
            .collect();
        if guarded.is_empty() {
            String::new()
        } else {
            format!("; a guard may decline {}", guarded.join(", "))
        }
    };
    let mut out = Vec::new();
    let (fast, slow): (Vec<&ScheduledRule>, Vec<&ScheduledRule>) =
        rules.iter().partition(|r| frequent(&r.cadence));
    let fast_timed: Vec<&ScheduledRule> = fast
        .iter()
        .copied()
        .filter(|r| r.next_due.is_some())
        .collect();
    if let Some(soonest) = fast_timed.iter().min_by_key(|r| r.next_due) {
        out.push(NextEvent {
            at: soonest.next_due,
            basis: format!(
                "the dispatcher's schedule; they fire every hour or faster, so they are one \
                 row — soonest {} ({}){}",
                soonest.name,
                soonest.cadence,
                guard(&fast_timed)
            ),
            ..row(
                NextKind::Scheduled,
                format!("{} frequent rules (hourly or faster)", fast_timed.len()),
                SOURCE_DISPATCHER,
            )
        });
    }
    let mut by_instant: std::collections::BTreeMap<DateTime<Utc>, Vec<&ScheduledRule>> =
        std::collections::BTreeMap::new();
    for r in slow.iter().copied() {
        if let Some(at) = r.next_due {
            by_instant.entry(at).or_default().push(r);
        }
    }
    for (at, group) in by_instant {
        let mut cadences: Vec<&str> = group.iter().map(|r| r.cadence.as_str()).collect();
        cadences.sort_unstable();
        cadences.dedup();
        out.push(NextEvent {
            at: Some(at),
            basis: format!(
                "the dispatcher's schedule ({}){}",
                cadences.join(", "),
                guard(&group)
            ),
            ..row(NextKind::Scheduled, names(&group), SOURCE_DISPATCHER)
        });
    }
    out.extend(rules.iter().filter(|r| r.next_due.is_none()).map(|r| {
        NextEvent {
            basis: r
                .next_due_why
                .clone()
                .unwrap_or_else(|| "the dispatcher gave no next firing and no reason".to_string()),
            ..row(NextKind::Scheduled, r.name.clone(), SOURCE_DISPATCHER)
        }
    }));
    out
}

/// The credentials whose rotation is scheduled. The registry declares
/// the policy and never a period, so a scheduled row has no time and
/// says so; an on-demand row has nothing coming.
fn rotation_rows(credentials: &Result<Vec<CredentialRow>, String>) -> Vec<NextEvent> {
    match credentials {
        Err(why) => vec![unread_row(
            NextKind::RotationDue,
            "credential rotations",
            SOURCE_CREDENTIALS,
            why.clone(),
        )],
        Ok(rows) => rows
            .iter()
            .filter(|c| c.rotation_policy == "scheduled")
            .map(|c| NextEvent {
                basis: format!(
                    "rotation_policy is scheduled, but the registry declares no rotation \
                     period, so when it falls due cannot be said (last rotated {})",
                    c.rotated_at
                        .map_or_else(|| "— none recorded".to_string(), |t| t.to_rfc3339())
                ),
                ..row(
                    NextKind::RotationDue,
                    format!("rotation: {}", c.id),
                    SOURCE_CREDENTIALS,
                )
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yard::{ActiveGate, BoardHold, QueuedGate};

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    const NOW: &str = "2026-09-27T17:08:00Z";

    /// A read boarding block: depth rule 1 car / 30 min cooldown, clock
    /// windows 06:05 and 18:05, `depth` cars waiting.
    fn boarding(depth: Option<usize>, cooldown_left: Option<u32>) -> BoardingPredicate {
        BoardingPredicate {
            dock_threshold: Some(1),
            cooldown_minutes: Some(30),
            at_times: vec!["06:05".into(), "18:05".into()],
            dock_depth: depth,
            threshold_met: depth.map(|d| d >= 1),
            cadence_reading: Reading::Read,
            summary: String::new(),
            hold: BoardHold {
                held_because: cooldown_left.map(|m| format!("cooldown — {m} min left")),
                cooldown_remaining_minutes: cooldown_left,
                cooldown_rule: Some("train-board-on-dock-depth".into()),
                last_board_at: Some(t("2026-09-27T16:46:00Z")),
                last_board_reading: Reading::Read,
                next_board: "boards on the next tick once the cooldown clears".into(),
            },
        }
    }

    fn yard(boarding: BoardingPredicate) -> YardStatus {
        let mut y = crate::yard::build_status(crate::yard::YardInputs::default());
        y.boarding = boarding;
        y.gates = Gates {
            capacity: 2,
            active: vec![],
            queued: vec![],
            typical_seconds: None,
        };
        y
    }

    fn train(title: &str, phase: TrainPhase, eta: TrainEta) -> TrainStatus {
        TrainStatus {
            id: "2f0c1d7e-0000-0000-0000-000000000000".into(),
            title: title.into(),
            phase,
            at_step: None,
            block: None,
            ci_result: None,
            pr_url: None,
            car_count: 3,
            channel: None,
            boarded_at: None,
            eta,
        }
    }

    fn rule(name: &str, cadence: &str, next: Option<&str>) -> ScheduledRule {
        ScheduledRule {
            name: name.into(),
            cadence: cadence.into(),
            next_due: next.map(t),
            next_due_why: next
                .is_none()
                .then(|| "business calendar `us-banking` could not be read".to_string()),
            when: None,
        }
    }

    fn cred(id: &str, policy: &str) -> CredentialRow {
        CredentialRow {
            id: id.into(),
            kind: "forgejo-access-token".into(),
            issuer: "forge".into(),
            principal: "claude".into(),
            scopes: serde_json::json!([]),
            storage_location: "boss/forge-token/token".into(),
            consumers: serde_json::json!([]),
            rotation_policy: policy.into(),
            rotated_at: None,
            notes: String::new(),
        }
    }

    fn run(
        y: &YardStatus,
        schedule: Result<Vec<ScheduledRule>, String>,
        creds: Result<Vec<CredentialRow>, String>,
    ) -> Vec<NextEvent> {
        next_up(&NextUpInputs {
            yard: y,
            schedule: &schedule,
            credentials: &creds,
            now: t(NOW),
        })
    }

    fn of(rows: &[NextEvent], kind: NextKind) -> Vec<&NextEvent> {
        rows.iter().filter(|r| r.kind == kind).collect()
    }

    /// The design's own example: three cars waiting under a cooldown
    /// with 22 minutes left board at 17:30, the 18:05 window after it.
    #[test]
    fn the_next_board_is_when_the_cooldown_clears_and_the_window_is_scheduled() {
        let rows = run(&yard(boarding(Some(3), Some(22))), Ok(vec![]), Ok(vec![]));
        let board = of(&rows, NextKind::TrainBoard);
        assert_eq!(board.len(), 1, "{rows:#?}");
        assert_eq!(board[0].at, Some(t("2026-09-27T17:30:00Z")));
        assert!(board[0].estimate);
        assert!(
            board[0].title.contains("3 cars waiting"),
            "{}",
            board[0].title
        );
        assert!(board[0].basis.contains("cooldown"), "{}", board[0].basis);
        assert_eq!(board[0].source, SOURCE_CADENCE);

        let window = of(&rows, NextKind::TrainWindow);
        assert_eq!(window.len(), 1, "{rows:#?}");
        assert_eq!(window[0].at, Some(t("2026-09-27T18:05:00Z")));
        assert!(!window[0].estimate, "a window is scheduled, not estimated");
        // Soonest first.
        assert_eq!(rows[0].kind, NextKind::TrainBoard);
        assert_eq!(rows[1].kind, NextKind::TrainWindow);
    }

    /// Past the last window of the day, the next is tomorrow's first.
    #[test]
    fn the_window_after_the_last_of_the_day_is_tomorrows_first() {
        let mut y = yard(boarding(Some(0), None));
        y.boarding.hold.next_board = "boards on the next tick once the dock reaches 1".into();
        let rows = next_up(&NextUpInputs {
            yard: &y,
            schedule: &Ok(vec![]),
            credentials: &Ok(vec![]),
            now: t("2026-09-27T19:00:00Z"),
        });
        let window = of(&rows, NextKind::TrainWindow);
        assert_eq!(window[0].at, Some(t("2026-09-28T06:05:00Z")));
        assert!(window[0].title.contains("empty"), "{}", window[0].title);
        // Below the threshold the depth board has no time, and says why.
        let board = of(&rows, NextKind::TrainBoard);
        assert_eq!(board[0].at, None);
        assert_eq!(board[0].unread, None);
        assert!(
            board[0].basis.contains("dock reaches 1"),
            "{}",
            board[0].basis
        );
    }

    /// A train holding the track: the board waits for it and gives no
    /// time of its own, and the train's arrival is the median estimate.
    #[test]
    fn a_train_in_transit_is_its_estimate_and_the_board_waits_on_the_track() {
        let mut y = yard(boarding(Some(2), None));
        y.boarding.hold.held_because = Some("track occupied (1 open train)".into());
        y.boarding.hold.next_board = "boards on the next tick once the track clears".into();
        y.trains = vec![train(
            "PR train 2026-09-27 16:46",
            TrainPhase::AwaitingCi,
            TrainEta::Estimate {
                leg: "boarding → arrival".into(),
                remaining_seconds: 13 * 60,
                remaining_low_seconds: 300,
                remaining_high_seconds: 3000,
                sample_size: 10,
                basis: "median of recent arrivals".into(),
                overdue: false,
            },
        )];
        let rows = run(&y, Ok(vec![]), Ok(vec![]));
        let transit = of(&rows, NextKind::InTransit);
        assert_eq!(transit[0].at, Some(t("2026-09-27T17:21:00Z")));
        assert!(transit[0].estimate);
        assert!(transit[0].title.contains("PR train 2026-09-27 16:46"));
        assert!(transit[0].basis.contains("median"), "{}", transit[0].basis);
        let board = of(&rows, NextKind::TrainBoard);
        assert_eq!(board[0].at, None);
        assert!(board[0].basis.contains("track"), "{}", board[0].basis);
        // The clock window waits on the track too, and says so.
        let window = of(&rows, NextKind::TrainWindow);
        assert!(window[0].basis.contains("track"), "{}", window[0].basis);
    }

    /// An ETA the history cannot support is listed with its reason.
    #[test]
    fn an_unknown_eta_is_a_row_without_a_time_saying_why() {
        let mut y = yard(boarding(Some(0), None));
        y.trains = vec![train(
            "PR train",
            TrainPhase::Deploying,
            TrainEta::Unknown {
                reason: "too little history to measure".into(),
            },
        )];
        let rows = run(&y, Ok(vec![]), Ok(vec![]));
        let transit = of(&rows, NextKind::InTransit);
        assert_eq!(transit[0].at, None);
        assert_eq!(transit[0].unread, None);
        assert!(transit[0].basis.contains("too little history"));
    }

    /// Queued gates: the first in line's estimated wait is the next bay.
    #[test]
    fn the_gate_line_is_the_first_in_lines_wait() {
        let mut y = yard(boarding(Some(0), None));
        y.gates = Gates {
            capacity: 2,
            active: vec![
                ActiveGate {
                    branch: "feat/a".into(),
                    packet_id: "p1".into(),
                    since: "2026-09-27T17:00:00Z".into(),
                    stale: false,
                    train: None,
                },
                ActiveGate {
                    branch: "feat/b".into(),
                    packet_id: "p2".into(),
                    since: "2026-09-27T17:05:00Z".into(),
                    stale: false,
                    train: None,
                },
            ],
            queued: vec![
                QueuedGate {
                    branch: "feat/c".into(),
                    packet_id: "p3".into(),
                    queued_at: "2026-09-27T17:06:00Z".into(),
                    position: 1,
                    waiting_seconds: Some(120),
                    estimated_wait_seconds: Some(7 * 60),
                    train: None,
                },
                QueuedGate {
                    branch: "feat/d".into(),
                    packet_id: "p4".into(),
                    queued_at: "2026-09-27T17:07:00Z".into(),
                    position: 2,
                    waiting_seconds: Some(60),
                    estimated_wait_seconds: Some(12 * 60),
                    train: None,
                },
            ],
            typical_seconds: Some(15 * 60),
        };
        let rows = run(&y, Ok(vec![]), Ok(vec![]));
        let gates = of(&rows, NextKind::Gates);
        assert_eq!(gates.len(), 1);
        assert_eq!(gates[0].at, Some(t("2026-09-27T17:15:00Z")));
        assert!(gates[0].title.contains("2 queued"), "{}", gates[0].title);
        assert!(gates[0].estimate);

        // No line: the first running gate's expected verdict.
        y.gates.queued.clear();
        let rows = run(&y, Ok(vec![]), Ok(vec![]));
        let gates = of(&rows, NextKind::Gates);
        assert_eq!(gates[0].at, Some(t("2026-09-27T17:15:00Z")));
        assert!(gates[0].title.contains("2 running"), "{}", gates[0].title);

        // An idle floor has nothing coming.
        y.gates.active.clear();
        assert!(of(&run(&y, Ok(vec![]), Ok(vec![])), NextKind::Gates).is_empty());
    }

    /// The dispatcher's rules: those firing at one instant are one row,
    /// the hourly-or-faster are folded into one row (the next is always
    /// minutes away), and a rule with no next due is listed with why.
    #[test]
    fn scheduled_rules_group_by_instant_and_fold_the_frequent_ones() {
        let rows = run(
            &yard(boarding(Some(0), None)),
            Ok(vec![
                rule(
                    "sensors-poll",
                    "every-5-minutes",
                    Some("2026-09-27T17:10:00Z"),
                ),
                rule("estate-observe", "hourly", Some("2026-09-27T18:00:00Z")),
                rule("publish-to-github", "daily", Some("2026-09-28T00:00:00Z")),
                rule(
                    "cadence-silence-sweep",
                    "daily",
                    Some("2026-09-28T00:00:00Z"),
                ),
                rule("bank-sweep", "daily", None),
            ]),
            Ok(vec![]),
        );
        let sched = of(&rows, NextKind::Scheduled);
        assert_eq!(sched.len(), 3, "{sched:#?}");
        let frequent = sched.iter().find(|r| r.title.contains("frequent")).unwrap();
        assert_eq!(frequent.at, Some(t("2026-09-27T17:10:00Z")));
        assert!(frequent.title.contains("2 frequent"), "{}", frequent.title);
        assert!(
            frequent.basis.contains("sensors-poll"),
            "{}",
            frequent.basis
        );
        let daily = sched
            .iter()
            .find(|r| r.title.contains("publish-to-github"))
            .unwrap();
        assert_eq!(daily.at, Some(t("2026-09-28T00:00:00Z")));
        assert!(
            daily.title.contains("cadence-silence-sweep"),
            "{}",
            daily.title
        );
        assert!(!daily.estimate);
        assert_eq!(daily.source, SOURCE_DISPATCHER);
        let unknown = sched.iter().find(|r| r.title == "bank-sweep").unwrap();
        assert_eq!(unknown.at, None);
        assert!(unknown.basis.contains("us-banking"), "{}", unknown.basis);
    }

    /// UNREAD IS A ROW. The dispatcher dark and the registry unread each
    /// show as a row with the reason, after every read row.
    #[test]
    fn a_source_that_cannot_be_read_is_an_unread_row_with_its_reason() {
        let rows = run(
            &yard(boarding(Some(0), None)),
            Err("GET http://dispatcher/api/dispatcher/schedule: connection refused".into()),
            Err("credentials registry: connection reset".into()),
        );
        let sched = of(&rows, NextKind::Scheduled);
        assert_eq!(sched.len(), 1);
        assert_eq!(sched[0].at, None);
        assert!(
            sched[0]
                .unread
                .as_deref()
                .unwrap()
                .contains("connection refused")
        );
        let rot = of(&rows, NextKind::RotationDue);
        assert_eq!(rot.len(), 1);
        assert!(
            rot[0]
                .unread
                .as_deref()
                .unwrap()
                .contains("connection reset")
        );
        // Unread rows ride last.
        assert!(rows.last().unwrap().unread.is_some());
        assert!(rows.first().unwrap().unread.is_none());

        // The cadence rows unread: the board is unread, not "nothing boards".
        let mut b = boarding(None, None);
        b.cadence_reading = Reading::Unread;
        let rows = run(&yard(b), Ok(vec![]), Ok(vec![]));
        let board = of(&rows, NextKind::TrainBoard);
        assert_eq!(board.len(), 1);
        assert!(board[0].unread.is_some(), "{board:#?}");
        assert!(of(&rows, NextKind::TrainWindow).is_empty());

        // The dock unread: the depth board is unread.
        let rows = run(&yard(boarding(None, None)), Ok(vec![]), Ok(vec![]));
        let board = of(&rows, NextKind::TrainBoard);
        assert!(
            board[0]
                .unread
                .as_deref()
                .unwrap()
                .contains(crate::yard::DEPTH_UNREAD),
            "{board:#?}"
        );
    }

    /// The registry declares a rotation POLICY and no period: on-demand
    /// rows have nothing coming, a scheduled row is listed without a
    /// time, saying so.
    #[test]
    fn a_scheduled_rotation_without_a_period_is_listed_without_a_time() {
        let rows = run(
            &yard(boarding(Some(0), None)),
            Ok(vec![]),
            Ok(vec![
                cred("boss-dev-forge-token", "on-demand"),
                cred("cloudflare-tunnel-credentials", "scheduled"),
            ]),
        );
        let rot = of(&rows, NextKind::RotationDue);
        assert_eq!(rot.len(), 1, "{rot:#?}");
        assert!(rot[0].title.contains("cloudflare-tunnel-credentials"));
        assert_eq!(rot[0].at, None);
        assert_eq!(rot[0].unread, None);
        assert!(
            rot[0].basis.contains("no rotation period"),
            "{}",
            rot[0].basis
        );
    }

    /// Only the next six timed rows; untimed rows are not cut.
    #[test]
    fn the_timed_rows_are_capped_at_the_next_six() {
        let rules: Vec<ScheduledRule> = (0..9)
            .map(|h| {
                rule(
                    &format!("rule-{h}"),
                    "daily",
                    Some(&format!("2026-09-28T0{h}:30:00Z")),
                )
            })
            .collect();
        let mut rules = rules;
        rules.push(rule("bank-sweep", "daily", None));
        let rows = run(&yard(boarding(Some(0), None)), Ok(rules), Ok(vec![]));
        let timed: Vec<&NextEvent> = rows.iter().filter(|r| r.at.is_some()).collect();
        assert_eq!(timed.len(), CAP, "{rows:#?}");
        assert!(timed.windows(2).all(|w| w[0].at <= w[1].at));
        assert!(rows.iter().any(|r| r.title == "bank-sweep"));
    }
}
