//! Why a board departs nothing — the no-departure line and the declared ordering edge.

use super::*;

// ---------------------------------------------------------------------------
// A BOARD THAT DEPARTS NO TRAIN OPENS NO PACKET
//
// Ten hours of delivery went to this on 2026-09-10 (backlog 4860aff8).
// ONE car sat on the dock that the consist check refused. The board
// cadence is queue-depth based and re-fires every 60 seconds while the
// dock stays deep, and every attempt OPENED a pr-train Job and then
// cancelled it: `pr-train` total passed 982, the newest 100 rows all
// closed inside 99 minutes, 89 `outcome: cancelled`, and 100 of 100
// with no `boarded_jobs`. That is ~1,440 phantom packets a day from one
// unboardable car. It turned the branch sweep's 50-row window over in
// under an hour (02069932) and it lied to every window read — the yard,
// `boss orient`, and any count of how many trains ran.
//
// The refusal itself was already right: no PR opened, no CI spent. What
// it did not skip was the PACKET, because the Job was created BEFORE
// the check ran — for one reason, that the refusal wanted somewhere to
// record itself. The record is cheaper than that:
//
//   - each car keeps its own `skip_reason`, and for a consist refusal
//     the structured `consist_refusal` too — the lint output, on the
//     car whose boarding it blocked, where the operator already looks;
//   - the journal gets ONE line, this one, which names the reason and
//     says outright that no packet was opened, so nobody goes hunting
//     the yard for a train that never existed.
//
// A packet is a fact that something happened; a train that never
// departed is not one. And until it was cancelled it also HELD THE
// TRACK, so a cancel lost to one API blip wedged boarding behind a
// train that had never left the yard.
// ---------------------------------------------------------------------------

// THE REFUSAL ITSELF LIVES IN CORE since backlog 96f02540:
// `boss_jobs::board_decision` holds `NoDeparture`, its line and whether it
// clears itself, beside the `BoardDecision` every board tick records on its
// cadence firing — so the yard and `boss orient` state the board's own
// words instead of re-deriving them from the cadence rows (the measured
// case: "nothing holds it" printed for half an hour while the board
// refused every tick). The split between a refusal that clears itself and
// one that repeats until a person acts (backlog 6baabd43) is documented
// there, beside the match that makes it.
pub(crate) use boss_jobs::board_decision::{
    BoardDecision, NoDeparture, no_departure_line, refusal_persists,
};

/// The environment variable naming the file a board writes its decision
/// to — set by the cadence loop on the child it spawns (`cadence::run_verb`)
/// and read back into the firing's outcome, where the yard reads it
/// (backlog 96f02540). A board run by hand has none set, and its decision
/// is the journal line alone, as before.
pub(crate) const BOARD_DECISION_FILE_ENV: &str = "BOSS_BOARD_DECISION_FILE";

/// Hand this tick's decision back to the cadence loop that spawned the
/// board, when one did. BEST-EFFORT by signature: the decision is the
/// yard's reading of the board, and a write that fails must never fail
/// the board — it says so, and the loop records the exit instead
/// (`cadence::firing_decision`).
pub(crate) fn record_board_decision(decision: &BoardDecision) {
    let Some(path) = std::env::var_os(BOARD_DECISION_FILE_ENV).filter(|p| !p.is_empty()) else {
        return;
    };
    let written = serde_json::to_string(decision)
        .map_err(anyhow::Error::from)
        .and_then(|text| std::fs::write(&path, text).map_err(anyhow::Error::from));
    if let Err(e) = written {
        log(format!(
            "the board's decision ({}) could not be handed to the cadence loop at {}: {e:#}",
            decision.line(),
            Path::new(&path).display()
        ));
    }
}

// ---------------------------------------------------------------------------
// A SKIP THAT REPEATS IS A STALL (backlog 94896e74)
//
// A single skip is ROUTINE and must never fire anything: measured
// 2026-09-22, 52 of 132 trains (39%) carried a skipped branch and still
// departed, and one branch was skipped 23 consecutive times and landed
// fine. What has no escalation is REPETITION. On 2026-09-22 the
// conductor refused to board from 07:01Z to 14:21Z — roughly 420
// identical firings on one conflicted car — diagnosed it perfectly,
// filed ONE alarm at 07:01Z, deduplicated correctly by staying open,
// and nothing read it for seven hours.
//
// So the repair verb is not what is missing (`boss rerail` exists, and
// would not have saved that day: the conflict was real and stopped for
// a human by design). What is missing is the SIGNAL, in two places:
//
//   - the car counts its consecutive skips (`next_skip_count`), so a
//     repeatedly-skipped car LOOKS troubled where an operator already
//     reads the dock, rather than hiding behind one reason string that
//     says nothing about how many windows have refused it;
//   - the alarm ESCALATES on a ladder (`stall_escalation`) instead of
//     merely persisting, so an unread packet's number grows with the
//     wait.
//
// AND THE DEDUP IS NOT BROKEN BY ANY OF IT. The alarm deduplicates by
// staying open, and closing it is what re-arms it; an escalation that
// filed twins would be worse than the silence it replaces. Every write
// here lands on THAT SAME packet, and the ladder is finite, so the
// escalation costs at most three writes however long the stall runs.
// ---------------------------------------------------------------------------

/// How many consecutive windows have skipped this car, counting the one
/// being stamped now.
///
/// Read off the car's own `skips` stamp, which boarding CLEARS in the
/// same write that clears `skip_reason` — so the count is consecutive by
/// construction and a car that rides a train starts again at one. A
/// re-park (`boss_jobs::car::regate_patch`) clears both the same way
/// (backlog 7e941603), so a repaired car starts again at one too. A
/// stamp that is not a non-negative integer reads as no stamp: a
/// malformed value must not paint repetition that did not happen, the
/// same reading `red_trains_of` gives the strike count (2bb0d014).
pub(crate) fn next_skip_count(car: &Value) -> u64 {
    car.get("metadata")
        .and_then(|m| m.get(boss_jobs::car::SKIPS))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_add(1)
}

// ---------------------------------------------------------------------------
// A CAR LEFT BEHIND BY TRAIN AFTER TRAIN RAISES AN ALARM (backlog 2fccbfd6)
//
// Measured 2026-09-27: four released, green-parked cars sat on the dock
// three to four hours while trains departed around them — held on red
// dock re-gates nothing retried — and no alarm fired. The boarding-stall
// alarm cannot see it: it fires when NOTHING departs, and here trains
// kept leaving. And `skips` counted only the cars assembly refused, so
// a car the dock held never counted at all.
//
// So every departure counts, on each car the DOCK held, one more on the
// car's own dock streak ([`LEFT_BEHIND_TRAINS`]), cleared when the car
// boards (94896e74's reading). At [`LEFT_BEHIND_ALARM_TRAINS`] the car
// gets a backlog-item alarm naming WHY, in the words the conductor last
// held it with (a red re-gate's gate-run and failing check among them:
// detection is not diagnosis). Filed once per streak: the alarm's id
// rides on the car, an alarm already open for the car is adopted rather
// than twinned, and boarding clears both with the count.
//
// THE DOCK'S OWN HOLDS ONLY (the adversarial review of car 5eb1967e, M3):
// a red or in-flight re-gate, a stale receipt, a branch on neither
// remote. Not assembly's conflicts — they count `skips`, and 39% of
// trains skip a car once, so an alarm there would be the noise 94896e74
// refused. Not a struck car — the strike rule already waits on a person.
// Not a car held on its declared ordering edge — its author asked for it.
// ---------------------------------------------------------------------------

/// How many consecutive departures may leave a released car behind before
/// an alarm names it. Three trains is about two hours at the measured
/// cadence of 2026-09-27 — long past a round's bounded wait, well short of
/// the three to four hours the four cars sat.
pub(crate) const LEFT_BEHIND_ALARM_TRAINS: u64 = 3;

/// The car's stamp naming the alarm filed for its current streak —
/// spelled in core beside the count, since every write that ends the
/// streak must end it too (round-2 re-review of car 5eb1967e, N2).
pub(crate) const LEFT_BEHIND_ALARM: &str = boss_jobs::car::LEFT_BEHIND_ALARM;

/// The car's count of consecutive departures the DOCK held it through —
/// apart from `skips`, which counts assembly's conflicts (the adversarial
/// review of car 5eb1967e, MEDIUM 3). Cleared when the car boards, when
/// assembly refuses it (`conflict_skip_write`), and when it is re-parked
/// (`boss_jobs::car::regate_patch`) — spelled once, in core, for that.
/// The re-park clears `skips` too, beside it, as boarding does: a re-gate
/// repairs the conflict that count measured (backlog 7e941603).
pub(crate) const LEFT_BEHIND_TRAINS: &str = boss_jobs::car::LEFT_BEHIND_TRAINS;

/// PURE: what assembly writes on a car it refused for a conflict — the
/// reason, its count of consecutive refusals (94896e74), and the END of
/// the dock's streak: a car assembly got to was not held by the dock,
/// so the streak of dock holds is broken (re-review of 5eb1967e, C).
pub(crate) fn conflict_skip_write(reason: &str, skips: u64) -> Vec<(&'static str, Value)> {
    vec![
        ("skip_reason", json!(reason)),
        (boss_jobs::car::SKIPS, json!(skips)),
        (LEFT_BEHIND_TRAINS, Value::Null),
        (LEFT_BEHIND_ALARM, Value::Null),
    ]
}

/// The key a left-behind record carries for a car GARAGED on a red dock
/// re-gate — one only a person or a main move frees (`empty_dock_refusal`).
pub(crate) const GARAGED: &str = "garaged";

/// How many consecutive departures the dock has held this car through,
/// counting the one being stamped now. A stamp that is not a count reads
/// as none, the reading `next_skip_count` gives.
pub(crate) fn next_left_behind_count(car: &Value) -> u64 {
    car.pointer(&format!("/metadata/{LEFT_BEHIND_TRAINS}"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_add(1)
}

/// PURE: is an alarm owed for a car left behind `skips` trains in a row?
/// Once per streak — not when the car already names one.
pub(crate) fn left_behind_alarm_due(car: &Value, skips: u64) -> bool {
    skips >= LEFT_BEHIND_ALARM_TRAINS
        && car
            .pointer(&format!("/metadata/{LEFT_BEHIND_ALARM}"))
            .is_none_or(Value::is_null)
}

/// How much of a reason rides in an alarm's TITLE; the detail has it all.
const LEFT_BEHIND_TITLE_WHY_CHARS: usize = 140;

/// PURE: the backlog-item a car left behind train after train becomes.
pub(crate) fn left_behind_alarm_body(
    car: &Value,
    skips: u64,
    now: DateTime<Utc>,
    owner: &str,
) -> Value {
    let md = car.get("metadata").cloned().unwrap_or(Value::Null);
    let id = car.get("id").and_then(Value::as_str).unwrap_or_default();
    let branch = md.get("branch").and_then(Value::as_str).unwrap_or("?");
    let reason = md
        .get("skip_reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("no reason recorded");
    let red = boss_jobs::dock_red::regate_red(&md);
    // The title names WHY in a few words: a red re-gate by its gate-run
    // and check, anything else by the head of the conductor's reason.
    let why = match &red {
        Some(r) => format!(
            "its dock re-gate {} is {} on {}",
            id8(&r.gate_run),
            r.verdict,
            if r.checks.is_empty() {
                "no check named".to_string()
            } else {
                r.checks.join(", ")
            }
        ),
        None => {
            let mut w: String = reason.chars().take(LEFT_BEHIND_TITLE_WHY_CHARS).collect();
            if reason.chars().count() > LEFT_BEHIND_TITLE_WHY_CHARS {
                w.push('…');
            }
            w
        }
    };
    let mut metadata = json!({
        "area": "pipeline",
        "left_behind_car": id,
        "left_behind_branch": branch,
        "trains_left_behind": skips,
        "skip_reason": reason,
        "last_measured_at": now.to_rfc3339(),
        "detail": format!(
            "Car {} ({branch}) is parked green and released, and {skips} consecutive trains \
             have departed while the dock held it. The conductor's own reason for the last \
             hold, verbatim: {reason}. The count is the car's `{LEFT_BEHIND_TRAINS}`, the \
             dock's holds only, cleared when it boards; this \
             alarm is filed once per streak (its id rides on the car as `{LEFT_BEHIND_ALARM}`). \
             Backlog 2fccbfd6: on 2026-09-27 four cars sat three to four hours like this and \
             nothing said so.",
            id8(id),
        ),
    });
    if let Some(r) = &red {
        metadata["regate_red_gate_run"] = json!(r.gate_run);
        metadata["regate_red_checks"] = json!(r.checks);
        metadata["regate_red_garaged"] = json!(r.garaged);
    }
    json!({
        "kind": "backlog-item",
        "status": "open",
        "title": format!(
            "LEFT BEHIND: {branch} left behind by {skips} consecutive trains — {why}"
        ),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        // The platform owner as the registry answers it, never a literal
        // person — the same as every conductor alarm (3c23662d).
        "owner_id": owner,
        "priority": "urgent",
        "tags": ["pipeline", "dock"],
        "metadata": metadata,
    })
}

/// The stamp a left-behind alarm leaves when the conductor closes it —
/// a machine clear, told apart from a human's answer the way the
/// stranded-green alarm's `cleared_by` is.
pub(crate) const LEFT_BEHIND_CLEARED_BY: &str = "conductor.left-behind-alarm";

/// Open left-behind alarms whose claim no longer holds, each with the
/// branch it named and WHY it ended: `(alarm id, branch, why)`.
///
/// THE HALF THAT WAS MISSING (backlog 7919fdcc, item 6). Boarding, an
/// assembly refusal and a re-park all end the streak on the CAR — the
/// count and the alarm's id come off together — and nothing ended the
/// alarm: the packet stayed open, urgent, naming a car that had long
/// since boarded. The stranded-green alarm learned this on 2026-09-09
/// (e60398dc); an alarm that cannot clear itself is a claim the system
/// stops standing behind the instant it stops being true.
///
/// Judged off the car, read in the same pass: closed, boarded, or its
/// dock streak below [`LEFT_BEHIND_ALARM_TRAINS`] — which it can reach
/// again only by starting over, since the streak only grows until it
/// ends. A car still at or past that count keeps its alarm even when its
/// stamp is missing: that is the stamp that failed to land, which the
/// next departure adopts. A car the list does not carry is left alone —
/// unread is not ended.
pub(crate) fn left_behind_alarms_to_clear(
    open_alarms: &[Value],
    cars: &[Value],
) -> Vec<(String, String, String)> {
    open_alarms
        .iter()
        .filter_map(|a| {
            let car_id = a
                .pointer("/metadata/left_behind_car")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())?;
            let id = a.get("id").and_then(Value::as_str)?;
            let car = cars
                .iter()
                .find(|c| c.get("id").and_then(Value::as_str) == Some(car_id))?;
            let why = left_behind_ended(car)?;
            let branch = car
                .pointer("/metadata/branch")
                .or_else(|| a.pointer("/metadata/left_behind_branch"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            Some((id.to_string(), branch.to_string(), why))
        })
        .collect()
}

/// PURE: why a car's left-behind streak has ended, named from the car —
/// `None` while it stands.
fn left_behind_ended(car: &Value) -> Option<String> {
    let status = car.get("status").and_then(Value::as_str).unwrap_or("");
    if status != "open" {
        return Some(format!(
            "the car is {} — it left the dock",
            if status.is_empty() {
                "unreadable as open"
            } else {
                status
            }
        ));
    }
    if let Some(train) = car
        .pointer("/metadata/train")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    {
        return Some(format!("it boarded train {}", id8(train)));
    }
    let trains = car
        .pointer(&format!("/metadata/{LEFT_BEHIND_TRAINS}"))
        .and_then(Value::as_u64);
    match trains {
        Some(n) if n >= LEFT_BEHIND_ALARM_TRAINS => None,
        Some(n) => Some(format!(
            "its dock streak started over — `{LEFT_BEHIND_TRAINS}` is {n}, below the \
             {LEFT_BEHIND_ALARM_TRAINS} that filed this alarm"
        )),
        None => Some(format!(
            "its dock streak ended — `{LEFT_BEHIND_TRAINS}` is cleared (it boarded, assembly \
             refused it, or it was re-parked)"
        )),
    }
}

/// The triage fields that CLOSE a left-behind alarm whose streak ended:
/// the `stale` terminal ("Closed — the claim no longer holds"), landed
/// through the step's merge door by `step_completion_writes`.
pub(crate) fn left_behind_clear_writes(branch: &str, why: &str) -> Map<String, Value> {
    let mut metadata = Map::new();
    metadata.insert("disposition".into(), json!("stale"));
    metadata.insert(
        "evidence".into(),
        json!(format!(
            "The conductor re-read the car on `{branch}` and it is no longer being left behind: \
             {why}. The claim this alarm carried no longer holds; closed by machine, not by \
             judgement. A new streak that reaches the threshold files a new alarm."
        )),
    );
    metadata.insert("cleared_by".into(), json!(LEFT_BEHIND_CLEARED_BY));
    metadata
}

/// The escalation ladder for an open boarding-stall alarm, in minutes
/// since it was filed. Three rungs, not a rung a minute: the packet is
/// already `priority: urgent` when it is filed, so an escalation is
/// worth a write only when the NUMBER on it has meaningfully grown.
/// The measured stall (07:01Z → 14:21Z) reaches the top rung.
pub(crate) const STALL_ESCALATION_MINS: [i64; 3] = [30, 120, 360];

/// A rung of that ladder, reached and not yet recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StallEscalation {
    /// Which rung: 1, 2 or 3.
    pub level: u64,
    /// How long the alarm has been open, in minutes — the number that
    /// grows, and the one an operator is actually owed.
    pub minutes: i64,
}

/// Which rung an open boarding-stall alarm has reached, when that is
/// HIGHER than the rung it already records — `None` otherwise, which is
/// most windows.
///
/// Dated from the alarm's own `stalled_since` metadata rather than the
/// Job's `opened_on`, which is a DATE and cannot answer a 30-minute
/// question. An alarm filed before this stamp existed has no
/// `stalled_since` and is not judged here at all: the caller stamps it
/// and the next window judges it, which is the honest answer rather
/// than a duration invented from a date.
pub(crate) fn stall_escalation(alarm: &Value, now: DateTime<Utc>) -> Option<StallEscalation> {
    let md = alarm.get("metadata")?;
    let since = md
        .get("stalled_since")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<DateTime<Utc>>().ok())?;
    let minutes = (now - since).num_minutes();
    let recorded = md
        .get("escalation_level")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let level = STALL_ESCALATION_MINS
        .iter()
        .filter(|rung| minutes >= **rung)
        .count() as u64;
    (level > recorded).then_some(StallEscalation { level, minutes })
}

/// The message an escalation writes onto the alarm packet — the wait,
/// the repair, and the current window's own refusal line, so a reader
/// who opens the packet at the escalation needs no second read.
pub(crate) fn stall_escalation_message(escalation: &StallEscalation, line: &str) -> String {
    let StallEscalation { level, minutes } = escalation;
    let rungs = STALL_ESCALATION_MINS.len();
    format!(
        "STILL STALLED — {minutes} minutes after this packet was filed the identical \
         refusal is still firing, and nobody has acted (escalation {level} of {rungs}). \
         The window's line right now:\n\n{line}\n\n\
         REPAIR: each skipped car carries its own skip_reason naming the conflict, and \
         its skips count naming how many windows have refused it. `boss rerail <car>` \
         puts a conflict-skipped car back aboard — new branch from current main, rebase, \
         gate, receipt copied onto the car — and stops for a person on a REAL conflict, \
         which is what that stop is for. Closing this packet re-arms the alarm.\n\n\
         This is an escalation of the packet that was already open, not a new one: no \
         twin was filed, and the ladder is finite, so a stall costs at most {rungs} of \
         these however long it runs (backlog 94896e74)."
    )
}

// ---------------------------------------------------------------------------
// THE DECLARED ORDERING EDGE — a car names the car it boards AFTER
//
// Four cars were held by hand on 2026-09-10 and every one of them was an
// ordering constraint (backlog d3320278; design doc 364f892e, all three
// questions accepted as proposed). `fix/the-reclaim-follows-the-build`
// was gated `--hold` and parked by hand when the dock happened to be
// empty, purely so it would board SOLO as a gate-blind ci.yml change;
// `feat/a-deleted-manifest-leaves-no-object` spent a day waiting for a
// human to notice both halves of a two-part sequence were satisfied. The
// operator WAS the mechanism, and the mechanism was re-reading the dock.
//
// FILTER, DO NOT PLAN (decision 3). Boarding skips a car whose declared
// predecessor has not landed; it does not compute a multi-train
// sequence. A plan buys nothing the filter does not, because the next
// board happens on its own inside the cooldown. If the dock ever grows
// deep enough that planning is visibly better, that is a second car.
//
// AN UNSATISFIABLE EDGE REFUSES AND IS NAMED (decision 2). Boarding
// anyway after a timeout was rejected: it reintroduces exactly the
// collision the edge prevents. So the refusal is LOUD on every 60-second
// board attempt, and it is FOUR refusals rather than one, because an
// operator reading the dock is deciding whether the pipeline is stuck:
//
//   still in flight  — nobody does anything; it departs on its own
//   landed           — satisfied; the car boards (no refusal at all)
//   abandoned        — a human must break the edge; it will never clear
//   no such Job      — a human must fix the reference
//
// Collapsing those into "dependency not met" would build the quiet hold
// this feature exists to remove (CLAUDE.md §Diagnosis: quiet is a loan
// against the next diagnosis).
//
// AND IT MUST NEVER FREEZE A LANDING. A fallible read inside the board
// loop that refused everything it could not evaluate would stop every
// train, and the gate does not run the conductor, so the gate would not
// catch it. Every way the read can fail therefore DEGRADES TOWARD
// BOARDING, loudly: `Unreadable` is a journal line and the car rides.
// The edge's job is to stop a known collision, not to become a new way
// for the pipeline to stop.
//
// THE JUDGEMENT ITSELF LIVES IN CORE since backlog 4142d821 (design
// cf820810 Q7): `boss_jobs::car::boards_after_outcome`, with the four
// situations' words and the fail-open. The dock region asks the same
// question of the same edge, and it said "a train is due" while this
// conductor refused the car every window, because until then only this
// file could answer. What stays here is the conductor's half: the READ
// (`edge_hold` in conductor.rs, and `is_no_such_job` below) and the
// window's own refusal line (`empty_dock_refusal`).
// ---------------------------------------------------------------------------

pub(crate) use boss_jobs::car::{
    EDGE_HOLD, EDGE_HOLD_NEEDS_HUMAN, EDGE_HOLD_WAITING, EdgeHold, EdgeOutcome, Predecessor,
    boards_after_outcome, declared_predecessor,
};

/// PURE: is this jobs-API error the ANSWER "there is no such Job",
/// rather than "I could not ask"? The difference decides whether a car
/// is held for a person to fix or boarded anyway, so it is worth a named
/// function and a test instead of an inline `.contains` at the call site.
///
/// THE PAIR THIS PINS (CLAUDE.md §9a). The status lives in
/// `ApiFailure.kind` as `Failure::Http(404)`, but `api` returns
/// `anyhow::Error` — the classifier's structure is gone by the time a
/// caller sees it, and surfacing it would mean a second shape for every
/// call in this file. So the status is read back out of the message
/// `api_once` builds, and `a_404_is_read_back_out_of_the_message_api_once_builds`
/// constructs that message the way `api_once` does so the two cannot
/// drift silently. Collapsing this properly means `api` handing back the
/// `Failure`; that is a wider change than this car.
pub(crate) fn is_no_such_job(err: &anyhow::Error) -> bool {
    err.chain().any(|c| c.to_string().contains("HTTP 404"))
}

/// PURE: the refusal an empty candidate list deserves.
///
/// `NothingParked` says "an idle window, not a failure" — true when the
/// dock is empty, a LIE when the dock is full of cars the edge filter
/// held, and the difference is exactly what an operator is reading the
/// line to learn. Composed from the structured `EDGE_HOLD` marker on
/// each left-behind entry, never from re-parsing the reason prose.
pub(crate) fn empty_dock_refusal(left_behind: &[Value]) -> NoDeparture {
    let named = |want: &str| -> Vec<String> {
        left_behind
            .iter()
            .filter(|e| e.get(EDGE_HOLD).and_then(Value::as_str) == Some(want))
            .filter_map(|e| e.get("car_id_short").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    };
    let waiting = named(EDGE_HOLD_WAITING);
    // A car GARAGED on a red dock re-gate needs a person too: the dock no
    // longer re-gates it at this head, and a dock of nothing else moves no
    // main to re-gate it on (backlog 2fccbfd6; review of car 5eb1967e, M2).
    let garaged = left_behind
        .iter()
        .filter(|e| e.get(GARAGED).and_then(Value::as_bool) == Some(true))
        .filter_map(|e| e.get("car_id_short").and_then(Value::as_str))
        .map(str::to_string);
    let needs_human: Vec<String> = named(EDGE_HOLD_NEEDS_HUMAN)
        .into_iter()
        .chain(garaged)
        .collect();
    if waiting.is_empty() && needs_human.is_empty() {
        return NoDeparture::NothingParked;
    }
    let mut cars = waiting;
    cars.extend(needs_human.iter().cloned());
    NoDeparture::HeldOnEdges {
        cars: cars.join(", "),
        needs_human: needs_human.join(", "),
    }
}

#[cfg(test)]
mod no_departure_tests {
    use super::{
        CONDUCTOR_LOCK_WAIT, LOCK_CONTENDED_EXIT, NoDeparture, PREFLIGHT_FAIL_EXIT, Phase,
        lock_acquired_line, lock_contended_line, lock_wait_budget, lock_waiting_line,
        no_departure_line,
    };
    use std::time::Duration;

    /// THE FLOOD, in one assertion. Ten hours on 2026-09-10 (4860aff8):
    /// one car the consist check refused, a board firing every 60
    /// seconds, and every attempt opened a pr-train Job and cancelled
    /// it — 982 trains, the newest 100 closed inside 99 minutes, 100 of
    /// 100 with no `boarded_jobs`. The refusal already spends no PR and
    /// no CI; it must spend no PACKET either, and the journal line is
    /// now the only place an operator learns a window refused, so it
    /// has to say all three.
    #[test]
    fn a_refused_consist_opens_no_train_packet_and_says_so() {
        let line = no_departure_line(&NoDeparture::ConsistRefused {
            reason: "consist check: the-live-rules-are-the-authored-rules failed".to_string(),
            cars: 3,
        });
        assert!(line.starts_with("no train departed —"), "{line}");
        assert!(
            line.contains("the-live-rules-are-the-authored-rules"),
            "the refusal names the check that refused: {line}"
        );
        assert!(
            line.contains("No train packet opened"),
            "an operator must not go looking for a train that was never opened: {line}"
        );
        assert!(
            line.contains("3 car(s) stay boardable"),
            "a combination failure strikes nobody: {line}"
        );
    }

    /// An idle window is not a failure, and it is not a train either.
    #[test]
    fn an_idle_window_departs_nothing_and_opens_nothing() {
        let line = no_departure_line(&NoDeparture::NothingParked);
        assert!(line.starts_with("no train departed —"), "{line}");
        assert!(line.contains("not a failure"), "{line}");
        assert!(line.contains("No train packet opened"), "{line}");
    }

    /// Every candidate conflicted: the cars carry their own
    /// `skip_reason`, and the line names them so the journal answers
    /// "which branches" without a second read.
    #[test]
    fn an_all_conflicted_window_names_the_branches() {
        let line = no_departure_line(&NoDeparture::AllConflicted {
            branches: "fix/a, fix/b".to_string(),
        });
        assert!(line.starts_with("no train departed —"), "{line}");
        assert!(line.contains("fix/a, fix/b"), "{line}");
        assert!(line.contains("No train packet opened"), "{line}");
    }

    /// An infrastructure refusal is not a consist failure — it says so,
    /// and it carries the host reason the readiness check measured.
    #[test]
    fn a_host_refusal_carries_the_measured_reason() {
        let line = no_departure_line(&NoDeparture::HostShort {
            reason: "forge: 3.1 GB free, floor is 20 GB".to_string(),
        });
        assert!(line.starts_with("no train departed —"), "{line}");
        assert!(line.contains("3.1 GB free"), "{line}");
        assert!(
            line.contains("before any car was collected"),
            "nobody's car is at fault: {line}"
        );
    }

    /// A lock whose loser logs and leaves is correct ONLY if it
    /// eventually wins. The board fires every 60 seconds and holds the
    /// lock 12–14 seconds in the consist check; the 10-minute reconcile
    /// fired about a second later and lost 55 times in a row, so no
    /// merge, no stall sentinel, no arrival report and no branch sweep
    /// ran for nine hours. The starvable side waits; the side that must
    /// never queue behind a 30-minute deploy does not.
    #[test]
    fn only_the_starvable_phases_wait_for_the_lock() {
        assert_eq!(lock_wait_budget(&Phase::Reconcile), CONDUCTOR_LOCK_WAIT);
        assert_eq!(lock_wait_budget(&Phase::Run), CONDUCTOR_LOCK_WAIT);
        assert_eq!(
            lock_wait_budget(&Phase::Board),
            Duration::ZERO,
            "a board must not queue behind a long reconcile — its firing records \
             boarded-nothing and the next tick re-fires"
        );
        assert_eq!(lock_wait_budget(&Phase::Preflight), Duration::ZERO);
        assert!(
            CONDUCTOR_LOCK_WAIT >= Duration::from_secs(60),
            "the budget has to outlast a board that departs a train, not just one that refuses"
        );
    }

    /// The three lines a contended lock can leave. Each names the
    /// waited time, because "leaving" with no number is what made nine
    /// hours of starvation look like nine hours of 0-second successes.
    #[test]
    fn the_lock_lines_name_the_waited_time() {
        let waiting = lock_waiting_line(Duration::from_secs(120));
        assert!(waiting.contains("120s"), "{waiting}");
        let acquired = lock_acquired_line(Duration::from_secs(13));
        assert!(acquired.contains("13s"), "{acquired}");
        // The zero-wait case keeps the line every journal reader and
        // cadence.rs's own doc comment already greps for.
        let left_at_once = lock_contended_line(Duration::ZERO);
        assert_eq!(
            left_at_once, "another conductor run holds the lock — leaving",
            "the no-wait line is unchanged"
        );
        // A zero-budget phase still burns nanoseconds between reading
        // the clock and failing the try — that is not a wait, and the
        // line must not claim one.
        assert_eq!(
            lock_contended_line(Duration::from_nanos(400)),
            left_at_once,
            "a sub-second elapsed is not a wait"
        );
        let timed_out = lock_contended_line(Duration::from_secs(120));
        assert!(
            timed_out.contains("120s") && timed_out.contains("holds the lock"),
            "{timed_out}"
        );
        assert_ne!(
            LOCK_CONTENDED_EXIT, 0,
            "a pass that waited its whole budget and still never ran must not record rc=0"
        );
        assert_ne!(
            LOCK_CONTENDED_EXIT, PREFLIGHT_FAIL_EXIT,
            "preflight failure already owns its code — two causes must not share one exit"
        );
    }
}

// ---------------------------------------------------------------------------
// The declared ordering edge — the conductor's half: the read and the
// window's line. The four refusals' words and the fail-open are judged in
// core now, and pinned there (`boss_jobs::car::boards_after_tests`).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod boards_after_tests {
    use super::{
        EDGE_HOLD, EDGE_HOLD_NEEDS_HUMAN, EDGE_HOLD_WAITING, NoDeparture, empty_dock_refusal,
        no_departure_line,
    };
    use serde_json::json;

    const PRED: &str = "bbbbbbbb-1111-2222-3333-444444444444";

    /// "NO SUCH JOB" AND "I COULD NOT ASK" MUST NOT BE CONFUSED: the
    /// first holds a car for a person to fix, the second boards it. The
    /// message below is built the way `api_once` builds it, which is what
    /// keeps this classification honest while `api` still flattens its
    /// `Failure` into an `anyhow::Error` (see `is_no_such_job`).
    #[test]
    fn a_404_is_read_back_out_of_the_message_api_once_builds() {
        // Verbatim shape from `api_once`:
        //   anyhow!("{method} {path}: HTTP {status}: {}", body.trim())
        let not_found = anyhow::anyhow!(
            "GET /api/jobs/{PRED}: HTTP 404 Not Found: {{\"error\":\"job not found\"}}"
        );
        assert!(super::is_no_such_job(&not_found));

        for blip in [
            anyhow::anyhow!("GET /api/jobs/{PRED}: HTTP 503 Service Unavailable: "),
            anyhow::anyhow!("error sending request for url (http://sor:7900/api/jobs/x)")
                .context("GET /api/jobs/x"),
            anyhow::anyhow!("job {PRED} came back empty"),
        ] {
            assert!(
                !super::is_no_such_job(&blip),
                "a blip must never read as an absence — it would hold a car for a person \
                 over a transient: {blip}"
            );
        }
    }

    /// An empty dock with no edge holds is still the idle window it
    /// always was — the pre-existing line, unchanged.
    #[test]
    fn an_empty_dock_with_no_edge_hold_is_an_idle_window() {
        assert_eq!(empty_dock_refusal(&[]), NoDeparture::NothingParked);
        let other = json!({"car_id_short": "aaaaaaaa", "reason": "branch fix/x not on fork"});
        assert_eq!(
            empty_dock_refusal(&[other]),
            NoDeparture::NothingParked,
            "a left-behind for some other reason is not an edge hold"
        );
    }

    /// A DOCK FULL OF HELD CARS IS NOT AN IDLE WINDOW, and the window's
    /// own line has to say which half of the hold it is — that is the
    /// decision an operator is reading it to make.
    #[test]
    fn an_empty_dock_held_on_edges_says_whether_a_human_is_needed() {
        let waiting = json!({"car_id_short": "aaaaaaaa", EDGE_HOLD: EDGE_HOLD_WAITING});
        let stuck = json!({"car_id_short": "cccccccc", EDGE_HOLD: EDGE_HOLD_NEEDS_HUMAN});

        let self_clearing = empty_dock_refusal(std::slice::from_ref(&waiting));
        assert_eq!(
            self_clearing,
            NoDeparture::HeldOnEdges {
                cars: "aaaaaaaa".into(),
                needs_human: String::new()
            }
        );
        let line = no_departure_line(&self_clearing);
        assert!(line.contains("no train departed"), "greppable: {line}");
        assert!(line.contains("NOTHING NEEDS DOING"), "{line}");
        assert!(line.contains("aaaaaaaa"), "name the car: {line}");

        let needs_human = empty_dock_refusal(&[waiting, stuck]);
        let line = no_departure_line(&needs_human);
        assert!(line.contains("A HUMAN IS NEEDED for cccccccc"), "{line}");
        assert!(
            !line.contains("NOTHING NEEDS DOING"),
            "one stuck car means the window is not self-clearing: {line}"
        );
        assert!(
            line.contains("aaaaaaaa"),
            "the waiting car is still listed as held: {line}"
        );
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;

    /// THE TOTAL SPLIT, asserted case by case so a new variant cannot
    /// be added without deciding which side it falls on — the compiler
    /// forces the match, and this forces the judgement.
    #[test]
    fn a_refusal_that_clears_itself_is_not_a_stall() {
        assert!(
            !refusal_persists(&NoDeparture::NothingParked),
            "an idle window is not a stall — the next parked car departs, and alarming \
             here is how a check becomes noise"
        );
        assert!(
            !refusal_persists(&NoDeparture::HostShort {
                reason: "9GB free, need 12GB".into()
            }),
            "an infrastructure refusal clears when the host does and says nothing about \
             any branch"
        );
        assert!(
            !refusal_persists(&NoDeparture::HeldOnEdges {
                cars: "fix/a".into(),
                needs_human: String::new()
            }),
            "a car waiting on a predecessor still in flight departs on its own, 60 \
             seconds later"
        );
    }

    /// The three that repeat identically until a person acts. These are
    /// the nine-and-a-half hours.
    #[test]
    fn a_refusal_that_repeats_until_someone_acts_is_a_stall() {
        assert!(
            refusal_persists(&NoDeparture::AllConflicted {
                branches: "fix/a, fix/b, fix/c".into()
            }),
            "every candidate conflicting on the assembled tree will conflict again on \
             the next window, and the next — this is the measured case (6baabd43)"
        );
        assert!(
            refusal_persists(&NoDeparture::ConsistRefused {
                reason: "two rule cars on one train".into(),
                cars: 3
            }),
            "the assembled tree is refused and nothing on the dock can change it"
        );
        assert!(
            refusal_persists(&NoDeparture::HeldOnEdges {
                cars: "fix/a, fix/b".into(),
                needs_human: "fix/b".into()
            }),
            "an edge that can never be satisfied refuses identically forever"
        );
    }

    /// The same variant falls on BOTH sides depending on its content,
    /// which is the reason this is a function over the value rather
    /// than a list of variant names.
    #[test]
    fn held_on_edges_splits_on_whether_anyone_is_needed() {
        let waiting = NoDeparture::HeldOnEdges {
            cars: "fix/a".into(),
            needs_human: String::new(),
        };
        let stuck = NoDeparture::HeldOnEdges {
            cars: "fix/a".into(),
            needs_human: "fix/a".into(),
        };
        assert!(!refusal_persists(&waiting));
        assert!(refusal_persists(&stuck));
        assert_ne!(
            refusal_persists(&waiting),
            refusal_persists(&stuck),
            "a classifier keyed on the variant alone would get one of these wrong"
        );
    }
}

#[cfg(test)]
mod repetition_tests {
    use super::*;

    #[test]
    fn a_first_skip_counts_one_and_a_repeat_counts_up() {
        let fresh = json!({"metadata": {"branch": "fix/a"}});
        assert_eq!(next_skip_count(&fresh), 1, "the first skip is one skip");
        let again = json!({"metadata": {"skips": 1}});
        assert_eq!(next_skip_count(&again), 2);
        let twenty_three = json!({"metadata": {"skips": 22}});
        assert_eq!(next_skip_count(&twenty_three), 23);
    }

    #[test]
    fn a_malformed_count_reads_as_a_first_skip_rather_than_painting_a_stall() {
        for stamp in [json!("lots"), json!(-3), json!(1.5), json!(null)] {
            let car = json!({"metadata": {"skips": stamp}});
            assert_eq!(
                next_skip_count(&car),
                1,
                "a count that is not a count must not invent repetition"
            );
        }
    }

    /// Car 51ad323d as it stood when the third train left it: held on its
    /// red dock re-gate, the reason the conductor wrote on it.
    fn left_car(skips: u64) -> Value {
        let red = boss_jobs::dock_red::RegateRed {
            gate_run: "e77c4e30-349a-4dc5-844f-8117fe63b12d".into(),
            head: "51cac5a1c0b5455defeec10c8ea344e1ee51fe97".into(),
            main: "620603309547562e3d27bde4433fa39cbd4ae6e5".into(),
            verdict: "failed".into(),
            checks: vec!["test".into()],
            garaged: false,
        };
        json!({
            "id": "51ad323d-c65d-44d2-b93d-38c776687eae",
            "metadata": {
                "branch": "feat/the-top-board-shows-what-outranks-what-needs-you-and-what-happens-next-rerail",
                LEFT_BEHIND_TRAINS: skips,
                "skip_reason": red.reason(),
                boss_jobs::dock_red::BASE_REGATE: {"main": red.main, "head": red.head,
                                                   boss_jobs::dock_red::RED: red.to_value()},
            }
        })
    }

    /// THE ALARM THE FOUR CARS NEVER GOT (2fccbfd6): owed at the third
    /// consecutive departure that leaves a car behind, and once per
    /// streak — the car naming an alarm already is not alarmed twice.
    #[test]
    fn a_car_left_behind_by_three_trains_is_owed_one_alarm() {
        assert!(!left_behind_alarm_due(&left_car(1), 2));
        assert!(left_behind_alarm_due(&left_car(2), 3));
        assert!(
            left_behind_alarm_due(&left_car(5), 6),
            "a streak past three that never alarmed"
        );
        let mut alarmed = left_car(3);
        alarmed["metadata"][LEFT_BEHIND_ALARM] = json!("a1a1a1a1");
        assert!(!left_behind_alarm_due(&alarmed, 4), "once per streak");
        alarmed["metadata"][LEFT_BEHIND_ALARM] = Value::Null;
        assert!(
            left_behind_alarm_due(&alarmed, 4),
            "a cleared stamp is no alarm"
        );
    }

    /// THE ALARM CLOSES ITSELF WHEN ITS STREAK ENDS (backlog 7919fdcc,
    /// item 6). Every way the streak ends is read off the car — closed,
    /// boarded, the count cleared or started over — and each is named;
    /// a car still at or past the threshold keeps its alarm even with
    /// its stamp lost (the next departure adopts it), and an alarm whose
    /// car the pass did not read is left alone.
    #[test]
    fn a_left_behind_alarm_clears_when_its_cars_streak_ends() {
        let alarm = |id: &str, car: &str| {
            json!({"id": id, "metadata": {"left_behind_car": car,
                                          "left_behind_branch": "feat/from-alarm"}})
        };
        let car = |id: &str, status: &str, md: Value| {
            let mut metadata = json!({"branch": format!("feat/{id}")});
            for (k, v) in md.as_object().cloned().unwrap_or_default() {
                metadata[k] = v;
            }
            json!({"id": id, "status": status, "metadata": metadata})
        };
        let cars = vec![
            car("landed", "closed", json!({LEFT_BEHIND_TRAINS: 4})),
            car("boarded", "open", json!({"train": "7a7a7a7a-train"})),
            car("cleared", "open", json!({})),
            car("restarted", "open", json!({LEFT_BEHIND_TRAINS: 1})),
            car(
                "standing",
                "open",
                json!({LEFT_BEHIND_TRAINS: 3, LEFT_BEHIND_ALARM: "a-standing"}),
            ),
            car("stamp-lost", "open", json!({LEFT_BEHIND_TRAINS: 5})),
        ];
        let open = vec![
            alarm("a-landed", "landed"),
            alarm("a-boarded", "boarded"),
            alarm("a-cleared", "cleared"),
            alarm("a-restarted", "restarted"),
            alarm("a-standing", "standing"),
            alarm("a-stamp-lost", "stamp-lost"),
            alarm("a-unread", "not-in-this-pass"),
            json!({"id": "a-stranded", "metadata": {"stranded_branch": "feat/x"}}),
        ];
        let got = left_behind_alarms_to_clear(&open, &cars);
        let ids: Vec<&str> = got.iter().map(|(id, _, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["a-landed", "a-boarded", "a-cleared", "a-restarted"],
            "{got:?}"
        );
        let why = |id: &str| got.iter().find(|g| g.0 == id).expect(id).2.clone();
        assert!(why("a-landed").contains("closed"), "{}", why("a-landed"));
        assert!(
            why("a-boarded").contains("7a7a7a7a"),
            "{}",
            why("a-boarded")
        );
        assert!(why("a-cleared").contains("cleared"), "{}", why("a-cleared"));
        assert!(
            why("a-restarted").contains("is 1"),
            "{}",
            why("a-restarted")
        );
        assert_eq!(got[0].1, "feat/landed", "the car's own branch");

        let writes = left_behind_clear_writes("feat/landed", &why("a-landed"));
        assert_eq!(writes["disposition"], "stale");
        assert_eq!(writes["cleared_by"], LEFT_BEHIND_CLEARED_BY);
        assert!(
            writes["evidence"]
                .as_str()
                .is_some_and(|e| e.contains("feat/landed") && e.contains("closed")),
            "{writes:?}"
        );
    }

    /// The alarm names the car, the count, and WHY — the red re-gate and
    /// its failing check when that is the hold — so its reader does not
    /// re-derive what the conductor already knew.
    #[test]
    fn the_alarm_names_the_car_the_trains_and_why() {
        let now: DateTime<Utc> = "2026-09-27T15:10:00Z".parse().unwrap();
        let body = left_behind_alarm_body(&left_car(3), 3, now, "emp-owner");
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(body["owner_id"], "emp-owner");
        let title = body["title"].as_str().unwrap_or_default();
        for want in [
            "LEFT BEHIND",
            "3 consecutive trains",
            "the-top-board",
            "e77c4e30",
            "test",
        ] {
            assert!(title.contains(want), "{want}: {title}");
        }
        let md = &body["metadata"];
        assert_eq!(
            md["left_behind_car"],
            "51ad323d-c65d-44d2-b93d-38c776687eae"
        );
        assert_eq!(md["trains_left_behind"], 3);
        assert_eq!(
            md["regate_red_gate_run"],
            "e77c4e30-349a-4dc5-844f-8117fe63b12d"
        );
        assert_eq!(md["regate_red_checks"], json!(["test"]));
        let detail = md["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("NOT boardable"),
            "the conductor's own words: {detail}"
        );
        // A car held for any other reason is named by that reason.
        let mut conflicted = json!({"id": "c2", "metadata": {
            "branch": "fix/x", LEFT_BEHIND_TRAINS: 4,
            "skip_reason": "branch fix/x is on neither the fork nor the forge"}});
        let other = left_behind_alarm_body(&conflicted, 4, now, "");
        assert!(
            other["title"]
                .as_str()
                .unwrap_or_default()
                .contains("on neither the fork"),
            "{other}"
        );
        assert!(other["metadata"].get("regate_red_gate_run").is_none());
        conflicted["metadata"]["skip_reason"] = Value::Null;
        let bare = left_behind_alarm_body(&conflicted, 4, now, "");
        assert!(
            bare["title"]
                .as_str()
                .unwrap_or_default()
                .contains("no reason recorded"),
            "{bare}"
        );
    }

    /// MEDIUM 3 of the adversarial review of car 5eb1967e: the streak is
    /// the DOCK'S OWN, kept apart from `skips`. Assembly's conflicts count
    /// `skips` (39% of trains skip a car once, 94896e74), and a car two
    /// conflicts deep would otherwise reach an urgent alarm on its first
    /// dock hold. The dock's counter reads only its own stamp.
    #[test]
    fn the_dock_streak_is_counted_apart_from_assemblys_skips() {
        let conflicted = json!({"metadata": {"skips": 2}});
        assert_eq!(
            next_left_behind_count(&conflicted),
            1,
            "two conflicts are no dock streak"
        );
        let held = json!({"metadata": {"skips": 2, LEFT_BEHIND_TRAINS: 2}});
        assert_eq!(next_left_behind_count(&held), 3);
        for stamp in [json!("lots"), json!(-3), json!(null)] {
            let car = json!({"metadata": {LEFT_BEHIND_TRAINS: stamp}});
            assert_eq!(next_left_behind_count(&car), 1);
        }
    }

    /// The re-review of car 5eb1967e, C: a car assembly refused was not
    /// held by the dock, so assembly's write ends the dock's streak —
    /// present-and-null, which the metadata door deletes.
    #[test]
    fn an_assembly_refusal_ends_the_docks_streak() {
        let kv: std::collections::BTreeMap<&str, Value> =
            conflict_skip_write("merge conflict with the consist in: a.rs", 2)
                .into_iter()
                .collect();
        assert_eq!(kv["skips"], 2);
        assert_eq!(
            kv["skip_reason"],
            "merge conflict with the consist in: a.rs"
        );
        assert!(
            kv.get(LEFT_BEHIND_TRAINS).is_some_and(Value::is_null),
            "{kv:?}"
        );
        // The alarm stamp ends with the streak (round-2 re-review, N2).
        assert!(
            kv.get(LEFT_BEHIND_ALARM).is_some_and(Value::is_null),
            "{kv:?}"
        );
    }

    /// MEDIUM 2 of the same review: a dock of nothing but GARAGED cars is
    /// not an idle window. The dock stops re-gating a garaged car at its
    /// head, so only a person — or a main move nothing on this dock will
    /// make — frees it; the window refuses identically until then, which
    /// is what `needs_human` means, so the stall persists and alarms.
    #[test]
    fn an_empty_dock_of_garaged_cars_needs_a_human() {
        let garaged = json!({"car_id_short": "51ad323d", GARAGED: true});
        let refusal = empty_dock_refusal(std::slice::from_ref(&garaged));
        assert_eq!(
            refusal,
            NoDeparture::HeldOnEdges {
                cars: "51ad323d".into(),
                needs_human: "51ad323d".into()
            }
        );
        assert!(refusal_persists(&refusal));
        let line = no_departure_line(&refusal);
        assert!(line.contains("A HUMAN IS NEEDED for 51ad323d"), "{line}");
        assert!(line.contains("garaged"), "{line}");
        let red = json!({"car_id_short": "de38e84e", "reason": "NOT boardable: ..."});
        assert_eq!(
            empty_dock_refusal(&[red]),
            NoDeparture::NothingParked,
            "a red the dock still retries clears itself"
        );
    }

    fn alarm(stalled_since: &str, level: u64) -> Value {
        json!({"metadata": {"stalled_since": stalled_since, "escalation_level": level}})
    }

    fn at(mins: i64) -> DateTime<Utc> {
        "2026-09-22T07:01:00Z".parse::<DateTime<Utc>>().unwrap() + chrono::Duration::minutes(mins)
    }

    #[test]
    fn a_stall_inside_the_first_rung_does_not_escalate() {
        let a = alarm("2026-09-22T07:01:00Z", 0);
        assert!(
            stall_escalation(&a, at(29)).is_none(),
            "the packet was filed 29 minutes ago and says so already — a second write \
             adds nothing"
        );
    }

    #[test]
    fn a_stall_that_outlives_a_rung_escalates_once_and_then_stays_put() {
        let a = alarm("2026-09-22T07:01:00Z", 0);
        let e = stall_escalation(&a, at(35)).expect("30 minutes unread is rung one");
        assert_eq!(e.level, 1);
        assert_eq!(e.minutes, 35);
        let escalated = alarm("2026-09-22T07:01:00Z", 1);
        assert!(
            stall_escalation(&escalated, at(45)).is_none(),
            "the rung is already recorded: escalating again every window would file the \
             noise this packet exists to avoid"
        );
        assert_eq!(
            stall_escalation(&escalated, at(130))
                .expect("two hours is rung two")
                .level,
            2
        );
        assert_eq!(
            stall_escalation(&alarm("2026-09-22T07:01:00Z", 2), at(440))
                .expect("the measured seven hours is the top rung")
                .level,
            3
        );
        assert!(
            stall_escalation(&alarm("2026-09-22T07:01:00Z", 3), at(600)).is_none(),
            "the ladder ends — an unbounded escalation is a write every window forever"
        );
    }

    #[test]
    fn an_alarm_with_no_stamp_cannot_be_judged_and_says_so() {
        let a = json!({"metadata": {}});
        assert!(
            stall_escalation(&a, at(600)).is_none(),
            "an alarm filed by an older conductor has no stalled_since; the caller \
             stamps it rather than guessing a duration"
        );
        let bad = json!({"metadata": {"stalled_since": "not a time"}});
        assert!(stall_escalation(&bad, at(600)).is_none());
    }

    #[test]
    fn the_escalation_sentence_names_the_wait_and_the_repair() {
        let msg = stall_escalation_message(
            &StallEscalation {
                level: 3,
                minutes: 440,
            },
            "no train departed — every candidate was skipped on merge conflicts: fix/a.",
        );
        assert!(msg.contains("440 minutes"), "{msg}");
        assert!(msg.contains("boss rerail"), "the repair verb: {msg}");
        assert!(
            msg.contains("fix/a"),
            "the current window's own line rides along: {msg}"
        );
        assert!(
            msg.contains("no twin"),
            "dedup is by this packet staying open, and the escalation must say it \
             has not broken that: {msg}"
        );
    }
}
