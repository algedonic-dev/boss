//! The lost-car relaunch sweep's own alarm, pure (backlog 7daa84c1): how
//! many consecutive passes its census has refused, whether that is an
//! alarm yet, the ONE backlog-item it becomes, and the fields that close
//! it when the census reads clean again.
//!
//! WHY. `Conductor::relaunch_lost_car_gates` judges three complete
//! partition censuses before any candidate effect (backlog de67e500).
//! When the census itself refuses — a byte bound on a page or on the
//! union, a total that moves between pages, conflicting originals, or a
//! conductor rolled before the jobs API so the old `ListJobsQuery`
//! answers `step_completed_since` or `metadata_has` with 400 — there is
//! no run to write a refusal on, and reconcile said so only in its
//! journal. Refusing on every pass, an unstamped lost gate-run with no
//! refusals record ages out of `lost_gate::WINDOW_HOURS` with no handback
//! item: a check nobody reads (RELEASE review 5533b8a3, finding 4).
//!
//! NOT EVERY REFUSAL. One refusal during a roll clears on the next pass,
//! and an alarm on it is noise that trains readers to skip the packet.
//! So a refusal is an alarm only after [`LOST_SWEEP_ALARM_PASSES`]
//! consecutive passes spanning at least [`LOST_SWEEP_ALARM_MINUTES`]:
//! both, so neither a faster cadence nor one slow pass trips it alone,
//! and still minutes against the six hours a lost run is owed.
//!
//! Every reconcile is its own process (the cadence loop spawns the verb),
//! so the count cannot live in memory: it is a latch file in the
//! conductor's home, the durable `boss-conductor-home` volume — the
//! shape `carried_verdict::cluster_read_line` keeps for the gate Jobs
//! read. The adapter is `Conductor::sweep_lost_car_gates`.

use super::*;

/// The latch, in the conductor's home: the refusal streak as JSON
/// ([`SweepRefusing`]). Written on every refused pass, removed on the
/// first clean pass that finds no alarm left to close.
pub(crate) const LOST_SWEEP_LATCH: &str = "lost-gate-sweep.refusing";
/// The alarm's dedup key, in its metadata: `true` on the one packet.
/// While a packet carrying it is open no twin is filed; closing it is
/// what re-arms the alarm (the `record_no_departure` shape).
pub(crate) const LOST_SWEEP_ALARM: &str = "lost_sweep_census_refusing";
/// Who closed it, on the step the close completes.
pub(crate) const LOST_SWEEP_CLEARED_BY: &str = "conductor.lost-sweep-alarm";
/// Consecutive refused passes before the alarm (see the module note).
pub(crate) const LOST_SWEEP_ALARM_PASSES: u64 = 3;
/// Minutes the streak must span before the alarm (see the module note).
pub(crate) const LOST_SWEEP_ALARM_MINUTES: i64 = 10;
/// How much of the refusal the latch and the packet quote: the innermost
/// cause's first line. A whole HTTP error body copied into a packet
/// refuses its own write (0ce12d49); the journal keeps the whole chain.
pub(crate) const LOST_SWEEP_CAUSE_QUOTE: usize = 400;

/// The refusal streak, as the latch holds it. `since` is the first
/// refusal of the streak; `passes` counts it. A streak of `0` passes is
/// the state a clean pass leaves when the alarm it owes a close could not
/// be closed: nothing is refusing, and the next clean pass tries again.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SweepRefusing {
    pub(crate) since: Option<DateTime<Utc>>,
    pub(crate) passes: u64,
    pub(crate) cause: String,
}

/// The latch, read. A missing or unreadable latch is no streak: the
/// count restarts, which delays an alarm and never invents one.
pub(crate) fn lost_sweep_read(latch: &Path) -> Option<SweepRefusing> {
    serde_json::from_slice(&fs::read(latch).ok()?).ok()
}

/// The latch, written. The caller says a failure: the cost is the
/// count, which restarts next pass.
pub(crate) fn lost_sweep_write(latch: &Path, refusing: &SweepRefusing) -> Result<()> {
    if let Some(dir) = latch.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(latch, serde_json::to_vec(refusing)?)
        .with_context(|| format!("writing {}", latch.display()))
}

/// PURE: the streak after one more refused pass. A held streak keeps its
/// first refusal's time; none (or one a clean pass reset) starts at `now`.
pub(crate) fn lost_sweep_refused(
    held: Option<&SweepRefusing>,
    cause: &str,
    now: DateTime<Utc>,
) -> SweepRefusing {
    match held.filter(|h| h.passes > 0) {
        Some(h) => SweepRefusing {
            since: h.since.or(Some(now)),
            passes: h.passes + 1,
            cause: cause.to_string(),
        },
        None => SweepRefusing {
            since: Some(now),
            passes: 1,
            cause: cause.to_string(),
        },
    }
}

/// PURE: is the streak an alarm yet? Both thresholds, never one.
pub(crate) fn lost_sweep_alarm_due(r: &SweepRefusing, now: DateTime<Utc>) -> bool {
    r.passes >= LOST_SWEEP_ALARM_PASSES
        && r.since
            .is_some_and(|since| (now - since).num_minutes() >= LOST_SWEEP_ALARM_MINUTES)
}

fn since_said(r: &SweepRefusing) -> String {
    r.since
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| "(unknown)".to_string())
}

/// PURE: the journal's clause for a refused pass — where the streak
/// stands against the alarm, so the line says what happens next.
pub(crate) fn lost_sweep_standing(r: &SweepRefusing, now: DateTime<Utc>) -> String {
    let minutes = r.since.map(|s| (now - s).num_minutes()).unwrap_or(0);
    format!(
        "refused {} consecutive pass(es) over {minutes} min since {}; an alarm is filed at \
         {LOST_SWEEP_ALARM_PASSES} passes and {LOST_SWEEP_ALARM_MINUTES} min",
        r.passes,
        since_said(r)
    )
}

/// The open alarms among `open` (every open backlog-item), by id.
pub(crate) fn lost_sweep_open_alarms(open: &[Value]) -> Vec<String> {
    open.iter()
        .filter(|j| {
            j.pointer(&format!("/metadata/{LOST_SWEEP_ALARM}"))
                .and_then(Value::as_bool)
                == Some(true)
        })
        .filter_map(|j| j.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// PURE: the backlog-item a refusing sweep becomes. It names the
/// refusal, the streak and what is owed while it lasts, because
/// detection is not diagnosis: the reader starts at the packet.
pub(crate) fn lost_sweep_alarm_body(r: &SweepRefusing, now: DateTime<Utc>, owner: &str) -> Value {
    let since = since_said(r);
    let minutes = r.since.map(|s| (now - s).num_minutes()).unwrap_or(0);
    let window = boss_jobs::lost_gate::WINDOW_HOURS;
    json!({
        "kind": "backlog-item",
        "status": "open",
        "title": format!(
            "LOST-GATE SWEEP REFUSING: the conductor's lost-car relaunch census refused {} \
             consecutive passes over {minutes}min",
            r.passes
        ),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        // The platform owner as the registry answers it, never a literal
        // person (3c23662d). No `opened_on`: the create handler stamps it
        // (dd3624a0).
        "owner_id": owner,
        "priority": "standard",
        "tags": ["pipeline", "gate"],
        "metadata": {
            "area": "pipeline",
            LOST_SWEEP_ALARM: true,
            "refusing_since": since,
            "refused_passes": r.passes,
            "cause": r.cause,
            "last_measured_at": now.to_rfc3339(),
            "detail": format!(
                "Since {since} the conductor's lost-car relaunch sweep \
                 (Conductor::relaunch_lost_car_gates, run by every reconcile pass) has refused \
                 its whole candidate census on {} consecutive passes, so in that time no lost \
                 car gate-run was relaunched, declined or handed back. The latest refusal: {}. \
                 While it lasts, a lost gate-run carrying neither a `lost_relaunch` stamp nor a \
                 `lost_relaunch_refusals` record ages out of the {window}h window \
                 (lost_gate::WINDOW_HOURS) with no handback item: read the closed gate-runs \
                 settled `lost` since {since}, and each one past the window is owed a relaunch \
                 or a handback by hand. The census refuses on a byte bound (one page, or the \
                 union of its three partitions), a total that moved between pages, conflicting \
                 originals, or a jobs API older than the conductor answering \
                 `step_completed_since` or `metadata_has` with 400; the conductor's journal \
                 carries the whole error chain on every pass. Filed once while open; it closes \
                 itself on the first pass whose census reads clean. Backlog 7daa84c1.",
                r.passes, r.cause
            ),
        },
    })
}

/// The triage fields that CLOSE the alarm when the census reads clean:
/// the `stale` terminal ("the claim no longer holds"), landed through
/// the step's merge door by `Conductor::close_alarm_stale`.
pub(crate) fn lost_sweep_clear_writes(
    held: &SweepRefusing,
    now: DateTime<Utc>,
) -> Map<String, Value> {
    let mut metadata = Map::new();
    metadata.insert("disposition".into(), json!("stale"));
    metadata.insert(
        "evidence".into(),
        json!(format!(
            "The conductor's lost-car relaunch census read clean at {}; the streak it \
             closes began {} and last refused with: {}. Lost gate-runs still inside the \
             {}h window are relaunched or handed back by the sweep from this pass; any that \
             aged out while it refused are not, and are named by their closed `lost` \
             gate-runs. Closed by machine, not by judgement; a sweep that refuses again files \
             a new packet.",
            now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            since_said(held),
            held.cause,
            boss_jobs::lost_gate::WINDOW_HOURS
        )),
    );
    metadata.insert("cleared_by".into(), json!(LOST_SWEEP_CLEARED_BY));
    metadata
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-19T06:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::minutes(minutes)
    }

    /// A streak keeps its first refusal's time and counts each pass; the
    /// cause is always the latest.
    #[test]
    fn a_refusal_streak_keeps_its_start_and_counts_each_pass() {
        let first = lost_sweep_refused(None, "HTTP 400", at(0));
        assert_eq!((first.since, first.passes), (Some(at(0)), 1));
        let second = lost_sweep_refused(Some(&first), "byte bound", at(2));
        assert_eq!((second.since, second.passes), (Some(at(0)), 2));
        assert_eq!(second.cause, "byte bound");
        // A streak a clean pass reset (its close still owed) starts over.
        let reset = SweepRefusing {
            since: None,
            passes: 0,
            cause: "old".into(),
        };
        let again = lost_sweep_refused(Some(&reset), "HTTP 400", at(30));
        assert_eq!((again.since, again.passes), (Some(at(30)), 1));
    }

    /// BOTH thresholds: a single transient refusal is never an alarm, three
    /// fast passes inside a roll are not, two slow ones are not, and three
    /// passes over ten minutes are.
    #[test]
    fn the_alarm_is_due_only_on_enough_passes_over_enough_minutes() {
        let streak = |passes, since| SweepRefusing {
            since: Some(at(since)),
            passes,
            cause: "c".into(),
        };
        let now = at(60);
        assert!(!lost_sweep_alarm_due(&streak(1, 0), now), "one refusal");
        assert!(!lost_sweep_alarm_due(&streak(2, 0), now), "two slow passes");
        assert!(
            !lost_sweep_alarm_due(&streak(LOST_SWEEP_ALARM_PASSES, 56), now),
            "three passes in four minutes"
        );
        assert!(lost_sweep_alarm_due(
            &streak(LOST_SWEEP_ALARM_PASSES, 60 - LOST_SWEEP_ALARM_MINUTES),
            now
        ));
        let undated = SweepRefusing {
            since: None,
            passes: 9,
            cause: "c".into(),
        };
        assert!(!lost_sweep_alarm_due(&undated, now), "no start, no span");
    }

    #[test]
    fn the_latch_round_trips_and_an_unreadable_one_is_no_streak() {
        let dir = boss_testing::scratch_dir("lost-sweep-latch");
        let latch = dir.join(LOST_SWEEP_LATCH);
        assert_eq!(lost_sweep_read(&latch), None, "absent");
        let streak = lost_sweep_refused(None, "HTTP 400", at(0));
        lost_sweep_write(&latch, &streak).unwrap();
        assert_eq!(lost_sweep_read(&latch), Some(streak));
        fs::write(&latch, "since=garbage").unwrap();
        assert_eq!(lost_sweep_read(&latch), None, "unreadable");
    }

    /// The packet names the refusal and the streak, carries the dedup key
    /// the open list is searched by, and is found by it.
    #[test]
    fn the_alarm_names_its_refusal_and_is_found_by_its_key() {
        let streak = SweepRefusing {
            since: Some(at(0)),
            passes: 6,
            cause: "GET /api/jobs?…step_completed_since=…: HTTP 400".into(),
        };
        let mut body = lost_sweep_alarm_body(&streak, at(12), "emp-owner");
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(body["owner_id"], "emp-owner");
        assert!(body["title"].as_str().unwrap().contains("refused 6"));
        let md = &body["metadata"];
        assert_eq!(md[LOST_SWEEP_ALARM], true);
        assert_eq!(md["refused_passes"], 6);
        assert_eq!(md["refusing_since"], "2026-10-19T06:00:00Z");
        assert!(md["detail"].as_str().unwrap().contains("HTTP 400"));
        body["id"] = json!("alarm-1");
        body["status"] = json!("open");
        let other = json!({"id": "other", "metadata": {"stranded_branch": "fix/x"}});
        assert_eq!(lost_sweep_open_alarms(&[other, body]), ["alarm-1"]);
        let clear = lost_sweep_clear_writes(&streak, at(20));
        assert_eq!(clear["disposition"], "stale");
        assert_eq!(clear["cleared_by"], LOST_SWEEP_CLEARED_BY);
        assert!(clear["evidence"].as_str().unwrap().contains("HTTP 400"));
    }
}
