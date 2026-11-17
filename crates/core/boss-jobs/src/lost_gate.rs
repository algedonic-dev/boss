//! THE ONE DECISION about a lost car gate: relaunch it once, on the same
//! sha, with its park intent — or hand it back to an agent, or decline
//! and say why (backlog 565131e1; Q3 of design 393bdef6, answered by
//! David 2026-10-16).
//!
//! WHY. Measured 2026-10-09..16: eleven car gates settled `lost` (the
//! runner died, or no Job ever carried the run — an infrastructure
//! death, never a verdict on the branch). Ten were relaunched BY HAND by
//! an agent 2-50 minutes later, about 155 minutes of delay in all, and
//! one (b18838e1) was never relaunched. Each relaunch was mechanical: the
//! same branch, the same sha, the same `--park-*` prose. Mechanical
//! operations belong to the machine (CLAUDE.md §Diagnosis).
//!
//! WHO ACTS. The conductor: it already holds the gates Role and owns
//! `launch_gate(branch, sha, Requester::Car, marks)`, the path the dock's
//! re-gate uses (owner decision on the packet, `owner_decision_20261016`).
//! This module is the pure half, so it lives in core beside
//! [`crate::stranded::park_intent`] and is tested without a socket; the
//! conductor reads the facts, asks [`answer`], and writes what it says.
//!
//! EXACTLY ONCE PER (BRANCH, SHA). The relaunched run carries
//! [`RELAUNCH_OF`] naming the lost run, and the lost run carries
//! [`STAMP`] once the machine has acted on it. So:
//! - a lost run already stamped is never looked at again;
//! - a lost run that IS a relaunch (it carries `relaunch_of`) is the
//!   second loss on that sha: the machine files ONE pipeline-failure item
//!   naming both gate-runs and stops ([`Answer::FileFailure`]);
//! - a relaunch the machine could not start is the same hand-back;
//! - a hand-back is ONE item per lost run whatever answers are lost on
//!   the way: it is filed under [`handback_item_id`], its request is on
//!   the record ([`FILING`]) before the POST, and a later pass replays
//!   that request and recovers that item (backlog cedc8e69);
//! - a later hand re-gate of a sha the machine already relaunched is the
//!   agent's, and its loss is declined rather than relaunched again.
//!
//! NEVER RELAUNCH when the branch moved or is gone, a newer gate-run
//! exists for the branch, the sha already landed, a car already vouches
//! green for the head, or the car was withdrawn. Train gates are not
//! this module's: the conductor relaunches those itself, bounded by
//! `train_gate::MAX_RELAUNCHES`, and [`crate::stranded::is_train_gate`]
//! is the one predicate that tells them apart.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

/// The key a machine relaunch carries, naming the lost gate-run it
/// replaces. The one fact that makes "once per (branch, sha)" readable
/// off the record.
pub const RELAUNCH_OF: &str = "relaunch_of";

/// The stamp the conductor writes on a LOST gate-run once it has acted
/// on it: `{action, at, run?, item?, reason?}`. Its presence ends the
/// question for that run — every later pass skips it silently.
pub const STAMP: &str = "lost_relaunch";

/// Stamp actions, as written in [`STAMP`]`.action`.
pub const RELAUNCHED: &str = "relaunched";
pub const DECLINED: &str = "declined";
pub const FAILURE_FILED: &str = "failure_filed";
/// The relaunch was attempted and could not be started; the next pass
/// hands it back as [`Answer::FileFailure`].
pub const LAUNCH_FAILED: &str = "launch_failed";
/// Written on the lost run BEFORE the launch, so an act whose outcome
/// was never recorded — the new run's marks refused AND the outcome stamp
/// refused, or the conductor dying mid-launch — reads as the machine's
/// unfinished try and is handed back, never as "a newer gate exists".
pub const ATTEMPTING: &str = "attempting";
/// Written on the lost run BEFORE the hand-back's POST, carrying the
/// whole request (`request`, whose `id` is [`handback_item_id`]): the
/// filing is one logical command, and a pass that cannot read its answer
/// — the admission committed and the response died, or the
/// [`FAILURE_FILED`] stamp after it was refused — leaves this behind, so
/// the next pass replays THAT request and recovers THAT item rather than
/// filing another (backlog cedc8e69). Unfinished: owed past the window.
pub const FILING: &str = "filing";
/// Launches refused before anything was filed (the gate bound, train
/// priority unreadable): `{count, first_at, last: [{at, cause}]}`. Not a
/// try — retried every pass — but when the window closes on them the
/// loss is handed back naming them, never dropped.
pub const REFUSALS: &str = "lost_relaunch_refusals";
/// How many refusal causes the record keeps (the latest).
pub const REFUSALS_KEPT: usize = 5;
/// `true` exactly while the [`STAMP`] says the machine's own act is
/// unfinished ([`ATTEMPTING`], [`LAUNCH_FAILED`]), and deleted by every
/// other write. The conductor's census reads `metadata_has=` this key
/// instead of every stamp ever written: finished stamps are re-read on
/// every pass and grow forever (~11 a week, ~27KB each, measured
/// 2026-10-17), and the metadata filter cannot reach `STAMP.action`
/// (a containment document is flat; backlog de67e500).
pub const UNFINISHED: &str = "lost_relaunch_unfinished";

/// The ONE metadata write after a refused launch: the refusal appended to
/// [`REFUSALS`] (bounded), and the [`ATTEMPTING`] stamp cleared, so the
/// refused try is not read next pass as an attempt with no outcome.
pub fn refusal_patch(lost: &Value, now: DateTime<Utc>, cause: &str) -> Value {
    let prior = lost.pointer(&format!("/metadata/{REFUSALS}"));
    let count = prior
        .and_then(|p| p.get("count"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    let stamp = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let first_at = prior
        .and_then(|p| p.get("first_at"))
        .cloned()
        .unwrap_or_else(|| json!(stamp));
    let mut last: Vec<Value> = prior
        .and_then(|p| p.get("last"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    last.push(json!({"at": stamp, "cause": cause}));
    let skip = last.len().saturating_sub(REFUSALS_KEPT);
    let last: Vec<Value> = last.into_iter().skip(skip).collect();
    json!({
        REFUSALS: {"count": count, "first_at": first_at, "last": last},
        STAMP: Value::Null,
        UNFINISHED: Value::Null,
    })
}

fn refusals_said(run: &Value) -> Option<String> {
    let r = run.pointer(&format!("/metadata/{REFUSALS}"))?;
    let count = r.get("count").and_then(Value::as_u64).filter(|n| *n > 0)?;
    let first = r.get("first_at").and_then(Value::as_str).unwrap_or("?");
    let causes: Vec<String> = r
        .get("last")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|e| {
            format!(
                "{}: {}",
                e.get("at").and_then(Value::as_str).unwrap_or("?"),
                e.get("cause").and_then(Value::as_str).unwrap_or("?")
            )
        })
        .collect();
    Some(format!(
        "{count} refusal(s) since {first}; the latest: {}",
        causes.join("; ")
    ))
}

/// How long after its loss a gate-run is still the machine's to
/// relaunch. A lost run older than this is history: relaunching it the
/// first time this code is deployed would launch a week of stale gates.
/// Six hours is twice the gate's own three-hour deadline — a loss the
/// conductor could not act on in that time (bays full, forge dark) has
/// been seen by an agent already.
pub const WINDOW_HOURS: i64 = 6;

/// The metadata keys a relaunch copies from the lost run besides the
/// `park_*` keys: the car's brake (a hold BESIDE park intent is the
/// car's, 486dde37), the builder run the green lands, and the delivery
/// channel when one was stamped.
const CARRIED_KEYS: [&str; 3] = ["hold", "agent_run", "delivery_channel"];

/// Where the car for the lost run's branch stands, as the conductor read
/// it. `VouchesForHead` is `gate::vouching_car`'s answer (a car carries a
/// current green for this exact head), read by the caller because the
/// receipt rule lives in the CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarStanding {
    /// No car carries the branch — a first gate; its green files one.
    NoCar,
    /// An open car carries the branch (parked, boarded or building).
    Open,
    /// An open car already carries a current green for this head.
    VouchesForHead { car: String },
    /// The branch's car landed.
    Landed,
    /// Every car for the branch is closed and none landed: retired,
    /// abandoned or cancelled — withdrawn.
    Withdrawn,
}

/// PURE: where the car for `branch` stands, from every car (any status)
/// whose `metadata.branch` is that branch. The vouch is the caller's.
pub fn car_standing(cars: &[Value], branch: &str) -> CarStanding {
    let mine: Vec<&Value> = cars
        .iter()
        .filter(|c| c.pointer("/metadata/branch").and_then(Value::as_str) == Some(branch))
        .collect();
    if mine.is_empty() {
        CarStanding::NoCar
    } else if mine.iter().any(|c| crate::car::is_landed(c)) {
        CarStanding::Landed
    } else if mine.iter().any(|c| crate::car::is_open(c)) {
        CarStanding::Open
    } else {
        CarStanding::Withdrawn
    }
}

/// Whether the lost run's change is already on main, as the conductor
/// read it from git. A sha that is an ancestor of main is the plain
/// case; a TWIN is the case the sha check missed (backlog 6a39558a,
/// 2026-10-18): item 0c77e4db's builder opened three branches for one
/// change, the third landed (#1247), and the conductor relaunched the
/// two lost gates of the other two — each with its park intent — so two
/// cars were parked for commits main already held and one boarded a red
/// train (b0e4ba33). Their shas were never ancestors of main; merging
/// either head into main wrote main's own tree (13190d8a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Landed {
    /// Neither: the change is not on main.
    No,
    /// The lost sha is an ancestor of main.
    Ancestor,
    /// Merging the lost sha into main writes main's own `tree`: its net
    /// change landed under another branch.
    InMainTree { tree: String },
    /// The content check could not be read; the cause. Never a verdict.
    Unknown(String),
}

/// The facts about one lost gate-run, as the conductor read them.
#[derive(Debug, Clone)]
pub struct Facts<'a> {
    /// The lost gate-run, as `GET /api/jobs/{id}` (or a `full=true`
    /// list row) returns it: id, status, `opened_at`, metadata, steps.
    pub lost: &'a Value,
    /// The branch's head on the forge now: `Ok(None)` when the branch is
    /// gone, `Err(cause)` when the forge could not be read. UNKNOWN is
    /// never "gone" (cd4c55ef), and never silent either (backlog
    /// f39eedf2): an unread head is answered [`Answer::Hold`], so each
    /// failed read is recorded on the run and a forge dark for the whole
    /// window is handed back naming its error rather than aging out.
    pub branch_head: Result<Option<&'a str>, &'a str>,
    /// Every OTHER gate-run for the branch, any status, same shape.
    pub others: &'a [Value],
    /// Is the lost run's change already on main — by its sha, or by its
    /// content under a twin branch — or could that not be read?
    pub landed: Landed,
    pub car: CarStanding,
    pub now: DateTime<Utc>,
}

/// What the machine does about a lost gate-run that is its to act on.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// Relaunch `branch@sha` as a car gate and stamp `marks` on the new
    /// gate-run: the park intent copied, plus [`RELAUNCH_OF`].
    Relaunch {
        branch: String,
        sha: String,
        marks: Value,
    },
    /// Hand it back: file ONE pipeline-failure item naming `first` (the
    /// run the machine relaunched, or tried to) and `second` (the run
    /// that was lost again, or the run the failed launch filed).
    FileFailure {
        branch: String,
        sha: String,
        first: String,
        second: String,
        why: String,
    },
    /// The machine looked and will not relaunch; the reason is stamped.
    Decline(String),
    /// The machine cannot read a fact its answer turns on — whether the
    /// change is already on main, or where the branch's head is: launch
    /// and decline nothing this pass, record the cause as a refusal (so a
    /// hold that never clears is handed back when the window closes), and
    /// ask again next pass.
    Hold(String),
}

/// The run's metadata as its resident submission reads it — the same
/// view [`crate::stranded`] reads park intent through.
fn source(run: &Value) -> Option<&Value> {
    crate::gate_reservation::source_metadata(run.get("metadata")?).ok()
}

fn text<'a>(md: &'a Value, key: &str) -> Option<&'a str> {
    md.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn id(run: &Value) -> &str {
    run.get("id").and_then(Value::as_str).unwrap_or_default()
}

fn opened_at(run: &Value) -> Option<DateTime<Utc>> {
    run.get("opened_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

fn verdict_step(run: &Value) -> Option<&Value> {
    crate::car::find_step(run, "record-verdict", "Record the gate verdict")
}

/// When the run was settled lost: the verdict step's completion.
fn lost_at(run: &Value) -> Option<DateTime<Utc>> {
    verdict_step(run)?
        .get("completed_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

/// PURE, CHEAP: is this gate-run one the machine owes a look at all — a
/// closed CAR gate settled `lost`, carrying a park intent, lost within
/// [`WINDOW_HOURS`], and not yet stamped? The conductor asks this of
/// every closed gate-run before it reads any other fact, so a pass over
/// a hundred runs costs reads only for the few that qualify.
pub fn is_candidate(run: &Value, now: DateTime<Utc>) -> bool {
    let Some(md) = source(run) else {
        return false;
    };
    run.get("status").and_then(Value::as_str) == Some("closed")
        && verdict_step(run)
            .and_then(|s| s.pointer("/metadata/verdict"))
            .and_then(Value::as_str)
            == Some("lost")
        && !crate::stranded::is_train_gate(md)
        && crate::stranded::park_intent(md)
        && text(md, "branch").is_some()
        && text(md, "sha").is_some()
        && match stamp_action(run) {
            // The machine's own unfinished act is answered whatever the
            // window says — it never ages out silently.
            Some(a) => is_unfinished(a),
            None => in_window(run, now) || refusals_said(run).is_some(),
        }
}

/// The stamp actions that are the machine's own unfinished act, owed an
/// answer whatever the window says. One predicate for both readers: the
/// candidate test above and the [`UNFINISHED`] marker [`stamp`] writes,
/// which is how the conductor's census finds them.
fn is_unfinished(action: &str) -> bool {
    action == LAUNCH_FAILED || action == ATTEMPTING || action == FILING
}

fn in_window(run: &Value, now: DateTime<Utc>) -> bool {
    lost_at(run).is_some_and(|t| (now - t).num_hours() < WINDOW_HOURS)
}

fn stamp_action(run: &Value) -> Option<&str> {
    run.pointer(&format!("/metadata/{STAMP}/action"))
        .and_then(Value::as_str)
}

/// PURE: the one answer for a lost gate-run. `None` when it is not a
/// candidate ([`is_candidate`]) or the machine already acted on it,
/// except a [`LAUNCH_FAILED`] stamp, which is answered with the hand-back.
pub fn answer(facts: &Facts<'_>) -> Option<Answer> {
    let run = facts.lost;
    if !is_candidate(run, facts.now) {
        return None;
    }
    let md = source(run)?;
    let lost_id = id(run).to_string();
    let branch = text(md, "branch")?.to_string();
    let sha = text(md, "sha")?.to_string();
    let short = &sha[..sha.len().min(8)];
    // The hand-back is already under way: its request is on the record,
    // and the answer is THAT request, whatever has moved since — a fresh
    // reading would name other runs or another cause, or decline over an
    // item that may already exist (backlog cedc8e69).
    if stamp_action(run) == Some(FILING) {
        let st = run.pointer(&format!("/metadata/{STAMP}"));
        let evidence = st.and_then(|s| s.pointer("/request/metadata/evidence"));
        let field = |k: &str| evidence.and_then(|e| e.get(k)).and_then(Value::as_str);
        let gate_run = |n: usize| {
            evidence
                .and_then(|e| e.pointer(&format!("/gate_runs/{n}")))
                .and_then(Value::as_str)
                .unwrap_or("(not recorded)")
                .to_string()
        };
        return Some(Answer::FileFailure {
            first: gate_run(0),
            second: gate_run(1),
            why: field("why")
                .or_else(|| st.and_then(|s| s.get("reason")).and_then(Value::as_str))
                .unwrap_or("(no cause recorded)")
                .to_string(),
            branch: field("branch").unwrap_or(&branch).to_string(),
            sha: field("sha").unwrap_or(&sha).to_string(),
        });
    }
    // The machine tried and could not start the relaunch: hand it back
    // whatever has moved since, because a launch failure is the
    // machine's failure and an agent must see it.
    if stamp_action(run) == Some(LAUNCH_FAILED) {
        let st = run.pointer(&format!("/metadata/{STAMP}"));
        let field = |k: &str| st.and_then(|s| s.get(k)).and_then(Value::as_str);
        return Some(Answer::FileFailure {
            first: lost_id,
            second: field("run")
                .unwrap_or("(no gate-run was filed)")
                .to_string(),
            why: format!(
                "the conductor tried to relaunch {branch}@{short} once and could not start \
                 it: {}",
                field("reason").unwrap_or("(no cause recorded)")
            ),
            branch,
            sha,
        });
    }
    let lost_opened = opened_at(run);
    let newer: Vec<&Value> = facts
        .others
        .iter()
        .filter(|o| id(o) != lost_id)
        .filter(|o| match (opened_at(o), lost_opened) {
            (Some(o), Some(l)) => o > l,
            // An undated run cannot be ordered; count it as newer, the
            // direction that launches nothing.
            _ => true,
        })
        .collect();
    let relaunch_of = |o: &Value| {
        source(o)
            .and_then(|m| text(m, RELAUNCH_OF))
            .map(str::to_string)
    };
    // AN ATTEMPT WITH NO RECORDED OUTCOME. The relaunch filed with its
    // marks is a relaunch (only the outcome stamp was lost, and the
    // decline below records it). Anything else — the new run's marks
    // refused so it was settled lost without `relaunch_of`, then the
    // `launch_failed` stamp refused too, or the conductor died mid-launch
    // — is the machine's failure, and a run it filed must not be read as
    // a newer gate that speaks for the branch.
    if stamp_action(run) == Some(ATTEMPTING)
        && !newer
            .iter()
            .any(|o| relaunch_of(o).as_deref() == Some(lost_id.as_str()))
    {
        let at = run
            .pointer(&format!("/metadata/{STAMP}/at"))
            .and_then(Value::as_str)
            .unwrap_or("?");
        return Some(Answer::FileFailure {
            first: lost_id,
            // The first run filed after the loss is the one the attempt made.
            second: newer
                .iter()
                .min_by_key(|o| opened_at(o))
                .map_or("(no gate-run was found)".to_string(), |o| id(o).to_string()),
            why: format!(
                "the conductor began relaunching {branch}@{short} at {at} and recorded no \
                 outcome — no gate-run carries relaunch_of naming the lost run{}",
                if newer.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; gate-runs filed for the branch since: {}",
                        newer.iter().map(|o| id(o)).collect::<Vec<_>>().join(", ")
                    )
                }
            ),
            branch,
            sha,
        });
    }
    // REFUSED UNTIL THE WINDOW CLOSED: every launch was refused before
    // anything was filed. Not a try, but not a silent end either.
    if !in_window(run, facts.now)
        && let Some(refusals) = refusals_said(run)
    {
        return Some(Answer::FileFailure {
            first: lost_id,
            second: "(none filed: every launch was refused before filing)".to_string(),
            why: format!(
                "the conductor could not relaunch {branch}@{short} within {WINDOW_HOURS}h of \
                 its loss — {refusals}"
            ),
            branch,
            sha,
        });
    }
    if let Some(marker) = crate::stranded::spent_reason(md) {
        return Some(Answer::Decline(format!(
            "the lost run is marked `{marker}` — spent, nothing to relaunch"
        )));
    }
    if let Some(r) = newer
        .iter()
        .find(|o| relaunch_of(o).as_deref() == Some(lost_id.as_str()))
    {
        return Some(Answer::Decline(format!("already relaunched as {}", id(r))));
    }
    if let Some(n) = newer.first() {
        return Some(Answer::Decline(format!(
            "a newer gate-run {} exists for {branch}; it speaks for the branch now",
            id(n)
        )));
    }
    match facts.branch_head {
        // Every answer below turns on the head (a moved branch declines
        // even a second loss), so an unread one decides none of them.
        Err(cause) => {
            return Some(Answer::Hold(format!(
                "the forge's head for {branch} could not be read ({cause}); whether it moved \
                 or is gone is unknown, so nothing is relaunched or declined until it reads"
            )));
        }
        Ok(None) => {
            return Some(Answer::Decline(format!(
                "{branch} no longer exists on the forge"
            )));
        }
        Ok(Some(head)) if head != sha => {
            return Some(Answer::Decline(format!(
                "{branch} moved from {short} to {} since the lost gate",
                &head[..head.len().min(8)]
            )));
        }
        Ok(Some(_)) => {}
    }
    if facts.landed == Landed::Ancestor {
        return Some(Answer::Decline(format!(
            "{short} already landed on main — nothing left to gate"
        )));
    }
    // A landed twin is declined like a landed sha, and for the same
    // reason: nothing is left to gate, so nothing may be parked (backlog
    // 6a39558a — faa27ef3 and 363c2d7d were parked this way).
    if let Landed::InMainTree { tree } = &facts.landed {
        return Some(Answer::Decline(format!(
            "{short}'s change is already on main — a landed twin: merging it into main \
             writes main's own tree {tree}; nothing left to gate or park"
        )));
    }
    match &facts.car {
        CarStanding::Withdrawn => {
            return Some(Answer::Decline(format!(
                "the car for {branch} was withdrawn (closed without landing)"
            )));
        }
        CarStanding::Landed => {
            return Some(Answer::Decline(format!("the car for {branch} landed")));
        }
        CarStanding::VouchesForHead { car } => {
            return Some(Answer::Decline(format!(
                "car {car} already carries a current green for {short}"
            )));
        }
        CarStanding::NoCar | CarStanding::Open => {}
    }
    // THE SECOND LOSS on this (branch, sha): this run is the machine's
    // relaunch, and it was lost too. One try was the whole budget.
    if let Some(first) = text(md, RELAUNCH_OF) {
        return Some(Answer::FileFailure {
            first: first.to_string(),
            why: format!(
                "the conductor relaunched {branch}@{short} once after gate-run {first} was \
                 lost, and the relaunch {lost_id} was lost too — two infrastructure deaths \
                 on one sha is a pattern for an agent, not a third try"
            ),
            second: lost_id,
            branch,
            sha,
        });
    }
    // Once per (branch, sha): a machine relaunch already spent on this
    // sha makes a later loss the agent's (it re-gated by hand).
    if let Some(spent) = facts.others.iter().find(|o| {
        relaunch_of(o).is_some() && source(o).and_then(|m| text(m, "sha")) == Some(sha.as_str())
    }) {
        return Some(Answer::Decline(format!(
            "the machine's one relaunch of {branch}@{short} was {} — this loss is the \
             re-gate's owner's",
            id(spent)
        )));
    }
    // UNKNOWN never parks: a relaunch carries the park intent, so it
    // waits until the content check reads. Held only here, at the one
    // answer that launches — every hand-back and decline above stands
    // without it.
    if let Landed::Unknown(cause) = &facts.landed {
        return Some(Answer::Hold(format!(
            "whether {branch}@{short}'s change is already on main could not be read ({cause}); \
             a relaunch would carry its park intent, so none is launched until it reads"
        )));
    }
    Some(Answer::Relaunch {
        marks: relaunch_marks(md, &lost_id),
        branch,
        sha,
    })
}

/// The marks a relaunch stamps on its new gate-run: every `park_*` key
/// and [`CARRIED_KEYS`] entry the lost run carried, plus [`RELAUNCH_OF`].
pub fn relaunch_marks(lost_source: &Value, lost_id: &str) -> Value {
    let mut m: Map<String, Value> = lost_source
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, v)| {
            (k.starts_with("park_") || CARRIED_KEYS.contains(&k.as_str())) && !v.is_null()
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    m.insert(RELAUNCH_OF.into(), json!(lost_id));
    Value::Object(m)
}

/// The [`STAMP`] value the conductor writes on the lost run, with its
/// [`UNFINISHED`] marker set or deleted to match.
pub fn stamp(
    action: &str,
    now: DateTime<Utc>,
    run: Option<&str>,
    item: Option<&str>,
    reason: Option<&str>,
) -> Value {
    let mut m = Map::new();
    m.insert("action".into(), json!(action));
    m.insert(
        "at".into(),
        json!(now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
    );
    for (k, v) in [("run", run), ("item", item), ("reason", reason)] {
        if let Some(v) = v {
            m.insert(k.into(), json!(v));
        }
    }
    // The marker rides in the same PATCH as the stamp, so the two can
    // never be read apart; null deletes a prior attempt's marker.
    let unfinished = if is_unfinished(action) {
        json!(true)
    } else {
        Value::Null
    };
    json!({ STAMP: Value::Object(m), UNFINISHED: unfinished })
}

/// The id of THE hand-back item for one lost gate-run, derived from that
/// run alone (backlog cedc8e69). The POST used to carry no id, so the
/// server minted one per send, and a send whose answer was lost — or
/// whose [`FAILURE_FILED`] stamp was refused — left the run eligible and
/// the next pass filed a second item. The jobs API admits an id once
/// (558396ff: the same body again is answered `already_admitted`, a
/// different one 409), so every pass, every restart and every concurrent
/// conductor names ONE packet. The lost run and not the answer's fields:
/// the hand-back is one per lost run, and a reading that moved must still
/// collide with the first.
pub fn handback_item_id(lost_id: &str) -> String {
    /// A fixed namespace, so every conductor derives the same id.
    const HANDBACK: uuid::Uuid = uuid::Uuid::from_u128(0xcedc_8e69_250f_4f4e_b8ea_ef46_ca58_fcbe);
    uuid::Uuid::new_v5(
        &HANDBACK,
        format!("lost-gate-handback:{lost_id}").as_bytes(),
    )
    .to_string()
}

/// The hand-back request already on the record — the [`FILING`] stamp's
/// `request`, exactly as it was first written — or `None` when no filing
/// is under way. A pass that finds one replays it verbatim.
pub fn filing_request(run: &Value) -> Option<&Value> {
    if stamp_action(run) != Some(FILING) {
        return None;
    }
    run.pointer(&format!("/metadata/{STAMP}/request"))
        .filter(|r| r.get("id").and_then(Value::as_str).is_some())
}

fn request_why(request: &Value) -> Option<&str> {
    request
        .pointer("/metadata/evidence/why")
        .and_then(Value::as_str)
}

/// The write BEFORE the hand-back's POST: [`FILING`], naming the item it
/// will be and carrying the whole request (the request receipt).
pub fn filing_stamp(now: DateTime<Utc>, request: &Value) -> Value {
    let item = request.get("id").and_then(Value::as_str);
    let mut s = stamp(FILING, now, None, item, request_why(request));
    s[STAMP]["request"] = request.clone();
    s
}

/// The write AFTER the item was admitted or recovered: [`FAILURE_FILED`]
/// naming it, with the request as sent and `result` — what the jobs API
/// answered, or what was read back — beside it, whole.
pub fn filed_stamp(now: DateTime<Utc>, request: &Value, item: &str, result: &Value) -> Value {
    let mut s = stamp(FAILURE_FILED, now, None, Some(item), request_why(request));
    s[STAMP]["request"] = request.clone();
    s[STAMP]["result"] = result.clone();
    s
}

/// PURE: is `item`, as `GET /api/jobs/{id}` returned it, THE hand-back
/// for `lost_id` — the derived id, a backlog-item, naming that lost run
/// among its gate-runs? The recovery read after an answer that could not
/// be read accepts nothing less: anything else is UNKNOWN, never filed.
pub fn is_original_handback(item: &Value, lost_id: &str) -> bool {
    item.get("id").and_then(Value::as_str) == Some(handback_item_id(lost_id).as_str())
        && item.get("kind").and_then(Value::as_str) == Some("backlog-item")
        && item
            .pointer("/metadata/evidence/gate_runs")
            .and_then(Value::as_array)
            .is_some_and(|runs| runs.iter().any(|r| r.as_str() == Some(lost_id)))
}

/// The ONE pipeline-failure item a hand-back files: both gate-runs, the
/// branch and sha, and what the machine saw — so the agent starts from
/// the reading, not from "lost". Under [`handback_item_id`] of `lost_id`.
pub fn failure_item_body(
    lost_id: &str,
    branch: &str,
    sha: &str,
    first: &str,
    second: &str,
    why: &str,
    owner: &str,
) -> Value {
    let short = &sha[..sha.len().min(8)];
    json!({
        "id": handback_item_id(lost_id),
        "kind": "backlog-item",
        "title": format!(
            "A lost car gate could not be relaunched by the machine: {branch}@{short}"
        ),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": owner,
        "status": "open",
        "tags": [],
        "priority": "standard",
        "metadata": {
            "input_channel": "pipeline-failure",
            "reporter": "conductor",
            "description": format!(
                "The conductor relaunches a lost car gate ONCE, on the same sha, with its \
                 park intent (backlog 565131e1). For {branch}@{sha} that one try is spent: \
                 {why}.\n\nGate-runs: {first} (the run the machine relaunched or tried to) \
                 and {second}. Read both receipts before re-gating: a second death on one \
                 sha is a pattern in the gate infrastructure, not in the branch. Re-gate by \
                 hand with the park flags (or `boss rerail <car> --finish`) once the cause \
                 is understood."
            ),
            "evidence": {
                "branch": branch,
                "sha": sha,
                "gate_runs": [first, second],
                "why": why,
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const LOST: &str = "11111111-0000-0000-0000-000000000000";

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-16T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn at(minutes_ago: i64) -> String {
        (now() - Duration::minutes(minutes_ago)).to_rfc3339()
    }

    /// A closed car gate settled lost 10 minutes ago, filed 30 minutes
    /// ago, carrying the full park intent a builder's gate carries.
    fn lost_run() -> Value {
        json!({
            "id": LOST,
            "kind": "gate-run",
            "status": "closed",
            "opened_at": at(30),
            "metadata": {
                "branch": "fix/a-thing",
                "sha": SHA,
                "runner": "infra/gate-runner/gate-runner.yaml",
                "park_summary": "does a thing",
                "park_excludes": "not that",
                "park_test": "ran it",
                "park_verified": "saw it",
                "park_backlog_item": "565131e1-e529-4bb0-9b22-731e879c6826",
                "park_probe": "echo ok",
                "park_expect": "ok",
                "agent_run": "25cbfd23-a713-428f-80a5-29750804a674",
                "gate_launched_at": at(29),
            },
            "steps": [{
                "spec_slug": "record-verdict",
                "status": "completed",
                "completed_at": at(10),
                "metadata": {"verdict": "lost", "receipt": "NO VERDICT WAS RECORDED"},
            }],
        })
    }

    fn with_md(mut run: Value, key: &str, v: Value) -> Value {
        run["metadata"][key] = v;
        run
    }

    fn other(id: &str, opened_min_ago: i64, md: Value) -> Value {
        let mut m = json!({"branch": "fix/a-thing", "sha": SHA});
        if let (Some(base), Some(extra)) = (m.as_object_mut(), md.as_object()) {
            base.extend(extra.clone());
        }
        json!({"id": id, "kind": "gate-run", "status": "open", "opened_at": at(opened_min_ago), "metadata": m})
    }

    fn facts<'a>(lost: &'a Value, others: &'a [Value]) -> Facts<'a> {
        Facts {
            lost,
            branch_head: Ok(Some(SHA)),
            others,
            landed: Landed::No,
            car: CarStanding::NoCar,
            now: now(),
        }
    }

    fn declined(a: Option<Answer>) -> String {
        match a {
            Some(Answer::Decline(why)) => why,
            other => panic!("expected a decline, got {other:?}"),
        }
    }

    // -- candidates -----------------------------------------------------

    #[test]
    fn a_lost_car_gate_with_park_intent_is_a_candidate() {
        assert!(is_candidate(&lost_run(), now()));
    }

    #[test]
    fn a_green_failed_or_open_run_is_not_a_candidate() {
        let mut green = lost_run();
        green["steps"][0]["metadata"]["verdict"] = json!("green");
        assert!(!is_candidate(&green, now()));
        let mut failed = lost_run();
        failed["steps"][0]["metadata"]["verdict"] = json!("failed");
        assert!(!is_candidate(&failed, now()));
        let mut open = lost_run();
        open["status"] = json!("open");
        assert!(!is_candidate(&open, now()));
    }

    /// Train gates relaunch through `train_gate::MAX_RELAUNCHES`; this
    /// module must never double-handle one.
    #[test]
    fn a_train_gate_is_never_a_candidate() {
        let run = with_md(lost_run(), "train_gate", json!(true));
        assert!(!is_candidate(&run, now()));
        assert_eq!(answer(&facts(&run, &[])), None);
    }

    /// A plain gate (no `--park-*`) has a human waiting on it; the
    /// machine has no car to deliver and leaves it alone.
    #[test]
    fn a_gate_without_park_intent_is_not_a_candidate() {
        let run = with_md(lost_run(), "park_summary", Value::Null);
        assert!(!is_candidate(&run, now()));
    }

    /// The first deploy must not relaunch a week of history.
    #[test]
    fn a_loss_older_than_the_window_is_not_a_candidate() {
        let mut run = lost_run();
        run["steps"][0]["completed_at"] = json!(at(WINDOW_HOURS * 60 + 1));
        assert!(!is_candidate(&run, now()));
        run["steps"][0]["completed_at"] = json!(at(WINDOW_HOURS * 60 - 1));
        assert!(is_candidate(&run, now()));
    }

    /// Once stamped, the machine has acted; no later pass looks again.
    #[test]
    fn a_stamped_run_is_not_a_candidate_unless_its_launch_failed() {
        for action in [RELAUNCHED, DECLINED, FAILURE_FILED] {
            let run = with_md(lost_run(), STAMP, json!({"action": action}));
            assert!(!is_candidate(&run, now()), "{action}");
            assert_eq!(answer(&facts(&run, &[])), None, "{action}");
        }
        let run = with_md(lost_run(), STAMP, json!({"action": LAUNCH_FAILED}));
        assert!(is_candidate(&run, now()));
    }

    // -- the relaunch ---------------------------------------------------

    #[test]
    fn a_first_loss_on_an_unmoved_branch_is_relaunched_with_its_park_intent() {
        let run = lost_run();
        let Some(Answer::Relaunch { branch, sha, marks }) = answer(&facts(&run, &[])) else {
            panic!("expected a relaunch");
        };
        assert_eq!(branch, "fix/a-thing");
        assert_eq!(sha, SHA);
        assert_eq!(marks[RELAUNCH_OF], LOST);
        for k in [
            "park_summary",
            "park_excludes",
            "park_test",
            "park_verified",
            "park_backlog_item",
            "park_probe",
            "park_expect",
            "agent_run",
        ] {
            assert_eq!(marks[k], run["metadata"][k], "{k}");
        }
        // The launch's own facts are the new run's, never copied.
        for k in ["branch", "sha", "runner", "gate_launched_at"] {
            assert!(marks.get(k).is_none(), "{k} must not be copied: {marks}");
        }
        // And the copied intent is still a park intent by the one rule.
        assert!(crate::stranded::park_intent(&marks));
    }

    /// The car's brake rides with its intent (486dde37).
    #[test]
    fn a_hold_beside_the_intent_is_carried() {
        let run = with_md(lost_run(), "hold", json!("waits for review"));
        let Some(Answer::Relaunch { marks, .. }) = answer(&facts(&run, &[])) else {
            panic!("expected a relaunch");
        };
        assert_eq!(marks["hold"], "waits for review");
    }

    #[test]
    fn a_parked_car_that_does_not_vouch_for_the_head_is_relaunched() {
        let run = lost_run();
        let f = Facts {
            car: CarStanding::Open,
            ..facts(&run, &[])
        };
        assert!(matches!(answer(&f), Some(Answer::Relaunch { .. })));
    }

    /// Only a NEWER gate-run speaks for the branch; an older plain one
    /// (no `relaunch_of`) is the history the loss already superseded.
    #[test]
    fn an_older_run_on_the_same_branch_without_a_relaunch_does_not_stop_it() {
        let run = lost_run();
        let others = [other("older", 120, json!({}))];
        assert!(matches!(
            answer(&facts(&run, &others)),
            Some(Answer::Relaunch { .. })
        ));
    }

    // -- declines -------------------------------------------------------

    #[test]
    fn a_moved_branch_is_declined() {
        let run = lost_run();
        let f = Facts {
            branch_head: Ok(Some("fedcba9876543210fedcba9876543210fedcba98")),
            ..facts(&run, &[])
        };
        assert!(declined(answer(&f)).contains("moved"));
    }

    #[test]
    fn a_deleted_branch_is_declined() {
        let run = lost_run();
        let f = Facts {
            branch_head: Ok(None),
            ..facts(&run, &[])
        };
        assert!(declined(answer(&f)).contains("no longer exists"));
    }

    /// Backlog f39eedf2: a head the forge would not answer is UNKNOWN —
    /// not "gone", not "moved", and not a relaunch either. It holds, the
    /// conductor records the hold as a refusal, and when the window
    /// closes on those refusals the loss is handed back naming the forge
    /// error rather than aging out with nothing on its record.
    #[test]
    fn an_unread_branch_head_holds_and_is_handed_back_when_the_window_closes() {
        let mut run = lost_run();
        let dark = "HTTP 502 Bad Gateway reading branch fix/a-thing";
        let f = Facts {
            branch_head: Err(dark),
            ..facts(&run, &[])
        };
        let why = match answer(&f) {
            Some(Answer::Hold(why)) => why,
            other => panic!("expected a hold, got {other:?}"),
        };
        assert!(why.contains(dark), "{why}");
        // A second loss on an unread head holds too: a moved branch would
        // decline it, so it is not filed until the head reads.
        let second = with_md(lost_run(), RELAUNCH_OF, json!("first-run"));
        assert!(matches!(
            answer(&Facts {
                branch_head: Err(dark),
                ..facts(&second, &[])
            }),
            Some(Answer::Hold(_))
        ));
        // The conductor records each hold as a refusal; past the window,
        // with the forge still dark, the hand-back names the forge error.
        run["metadata"][REFUSALS] = refusal_patch(&run, now(), &why)[REFUSALS].clone();
        run["steps"][0]["completed_at"] = json!(at(WINDOW_HOURS * 60 + 1));
        assert!(is_candidate(&run, now()));
        match answer(&Facts {
            branch_head: Err(dark),
            ..facts(&run, &[])
        }) {
            Some(Answer::FileFailure { first, why, .. }) => {
                assert_eq!(first, LOST);
                assert!(why.contains(dark), "{why}");
            }
            other => panic!("expected a hand-back, got {other:?}"),
        }
    }

    #[test]
    fn a_newer_gate_for_the_branch_is_declined_and_named() {
        let run = lost_run();
        let others = [other("newer-run", 5, json!({}))];
        let why = declined(answer(&facts(&run, &others)));
        assert!(why.contains("newer-run"), "{why}");
    }

    /// The stamp write after a relaunch can fail; the relaunch itself is
    /// then the record, and nothing is launched twice.
    #[test]
    fn a_relaunch_already_filed_is_not_filed_again() {
        let run = lost_run();
        let others = [other("the-relaunch", 5, json!({RELAUNCH_OF: LOST}))];
        let why = declined(answer(&facts(&run, &others)));
        assert!(why.contains("already relaunched as the-relaunch"), "{why}");
    }

    #[test]
    fn a_landed_sha_is_declined() {
        let run = lost_run();
        let f = Facts {
            landed: Landed::Ancestor,
            ..facts(&run, &[])
        };
        assert!(declined(answer(&f)).contains("landed"));
    }

    /// Backlog 6a39558a: the lost sha is no ancestor of main, but its
    /// change landed under a twin branch — merging it into main writes
    /// main's own tree. Relaunching it carries the park intent and files
    /// a car for nothing (faa27ef3, 363c2d7d); it is declined instead,
    /// naming the tree, and nothing is launched.
    #[test]
    fn a_change_already_on_main_under_a_twin_branch_is_not_relaunched() {
        let run = lost_run();
        let f = Facts {
            landed: Landed::InMainTree {
                tree: "13190d8a".into(),
            },
            ..facts(&run, &[])
        };
        let why = declined(answer(&f));
        assert!(why.contains("landed twin"), "{why}");
        assert!(why.contains("13190d8a"), "{why}");
    }

    /// The content check could not be read: UNKNOWN never parks. Nothing
    /// is launched, and the cause rides the hold.
    #[test]
    fn an_unreadable_landing_holds_and_launches_nothing() {
        let run = lost_run();
        let f = Facts {
            landed: Landed::Unknown("git merge-tree errored".into()),
            ..facts(&run, &[])
        };
        match answer(&f) {
            Some(Answer::Hold(why)) => assert!(why.contains("git merge-tree errored"), "{why}"),
            other => panic!("expected a hold, got {other:?}"),
        }
    }

    /// The control: a change genuinely not on main still relaunches with
    /// its whole park intent.
    #[test]
    fn an_unlanded_change_still_relaunches_with_its_park_intent() {
        let run = lost_run();
        let Some(Answer::Relaunch { marks, .. }) = answer(&facts(&run, &[])) else {
            panic!("expected a relaunch");
        };
        assert_eq!(
            marks["park_backlog_item"],
            run["metadata"]["park_backlog_item"]
        );
    }

    #[test]
    fn a_withdrawn_car_is_declined() {
        let run = lost_run();
        let f = Facts {
            car: CarStanding::Withdrawn,
            ..facts(&run, &[])
        };
        assert!(declined(answer(&f)).contains("withdrawn"));
        let f = Facts {
            car: CarStanding::Landed,
            ..facts(&run, &[])
        };
        assert!(declined(answer(&f)).contains("landed"));
    }

    #[test]
    fn a_car_already_vouching_for_the_head_is_declined() {
        let run = lost_run();
        let f = Facts {
            car: CarStanding::VouchesForHead {
                car: "car-9".into(),
            },
            ..facts(&run, &[])
        };
        assert!(declined(answer(&f)).contains("car-9"));
    }

    #[test]
    fn a_superseded_run_is_declined() {
        let run = with_md(lost_run(), "superseded", json!("landed on main"));
        assert!(declined(answer(&facts(&run, &[]))).contains("superseded"));
    }

    /// Once per (branch, sha): a HAND re-gate of a sha the machine
    /// already relaunched is the agent's, and its loss is not relaunched.
    #[test]
    fn a_hand_regate_after_the_machines_one_relaunch_is_declined() {
        let run = lost_run();
        let others = [other(
            "machine-relaunch",
            120,
            json!({RELAUNCH_OF: "first"}),
        )];
        let why = declined(answer(&facts(&run, &others)));
        assert!(why.contains("machine-relaunch"), "{why}");
    }

    // -- the hand-back --------------------------------------------------

    #[test]
    fn a_relaunch_lost_again_on_the_same_sha_files_one_failure_naming_both() {
        let run = with_md(lost_run(), RELAUNCH_OF, json!("first-run"));
        let others = [other("first-run", 120, json!({}))];
        match answer(&facts(&run, &others)) {
            Some(Answer::FileFailure {
                first,
                second,
                branch,
                sha,
                ..
            }) => {
                assert_eq!(first, "first-run");
                assert_eq!(second, LOST);
                assert_eq!(branch, "fix/a-thing");
                assert_eq!(sha, SHA);
            }
            other => panic!("expected a hand-back, got {other:?}"),
        }
    }

    /// A relaunch the machine could not start is handed back too — the
    /// machine tries once.
    #[test]
    fn a_failed_launch_is_handed_back_with_what_it_saw() {
        let run = with_md(
            lost_run(),
            STAMP,
            json!({"action": LAUNCH_FAILED, "run": "filed-run", "reason": "kubectl create refused"}),
        );
        match answer(&facts(&run, &[])) {
            Some(Answer::FileFailure {
                first, second, why, ..
            }) => {
                assert_eq!(first, LOST);
                assert_eq!(second, "filed-run");
                assert!(why.contains("kubectl create refused"), "{why}");
            }
            other => panic!("expected a hand-back, got {other:?}"),
        }
    }

    #[test]
    fn a_second_loss_on_a_moved_branch_is_declined_not_filed() {
        let run = with_md(lost_run(), RELAUNCH_OF, json!("first-run"));
        let f = Facts {
            branch_head: Ok(Some("fedcba9876543210fedcba9876543210fedcba98")),
            ..facts(&run, &[])
        };
        assert!(declined(answer(&f)).contains("moved"));
    }

    #[test]
    fn the_failure_item_is_a_pipeline_failure_naming_both_runs() {
        let body = failure_item_body(
            LOST,
            "fix/a-thing",
            SHA,
            "first-run",
            LOST,
            "lost twice",
            "emp-x",
        );
        assert_eq!(body["id"], json!(handback_item_id(LOST)));
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(body["owner_id"], "emp-x");
        assert_eq!(body["metadata"]["input_channel"], "pipeline-failure");
        assert_eq!(body["metadata"]["reporter"], "conductor");
        let desc = body["metadata"]["description"].as_str().unwrap();
        for s in ["first-run", LOST, "fix/a-thing", "lost twice"] {
            assert!(desc.contains(s), "{s} missing from {desc}");
        }
        assert_eq!(
            body["metadata"]["evidence"]["gate_runs"],
            json!(["first-run", LOST])
        );
    }

    // -- one hand-back, one item (backlog cedc8e69) ---------------------

    /// The id is the lost run's alone: every pass, restart and conductor
    /// derives the same one, and two lost runs never share one.
    #[test]
    fn the_handback_item_id_is_derived_from_the_lost_run_alone() {
        let id = handback_item_id(LOST);
        assert_eq!(id, handback_item_id(LOST));
        assert_ne!(id, handback_item_id("22222222-0000-0000-0000-000000000000"));
        assert!(uuid::Uuid::parse_str(&id).is_ok(), "{id}");
        // A different reading of the same loss still names the same item.
        let a = failure_item_body(LOST, "fix/a-thing", SHA, "x", LOST, "one cause", "emp-x");
        let b = failure_item_body(LOST, "fix/a-thing", SHA, LOST, "y", "another", "emp-y");
        assert_eq!(a["id"], b["id"]);
    }

    /// A filing under way is owed past the window, and answered with ITS
    /// request — the original gate-runs and cause — even when every fact
    /// has moved: a fresh reading here would decline over an item that
    /// may already exist, or file a second one.
    #[test]
    fn a_filing_under_way_replays_its_original_request_whatever_moved() {
        let request = failure_item_body(
            LOST,
            "fix/a-thing",
            SHA,
            "first-run",
            LOST,
            "the original cause",
            "emp-x",
        );
        let mut run = with_md(lost_run(), RELAUNCH_OF, json!("first-run"));
        let s = filing_stamp(now(), &request);
        assert_eq!(s[UNFINISHED], json!(true), "{s}");
        assert_eq!(s[STAMP]["item"], request["id"]);
        run["metadata"][STAMP] = s[STAMP].clone();
        run["metadata"][UNFINISHED] = json!(true);
        run["steps"][0]["completed_at"] = json!(at(WINDOW_HOURS * 60 + 30));
        assert!(is_candidate(&run, now()));
        assert_eq!(filing_request(&run), Some(&request));
        let others = [other("hand-regate", 1, json!({}))];
        let f = Facts {
            branch_head: Ok(Some("fedcba9876543210fedcba9876543210fedcba98")),
            landed: Landed::Ancestor,
            car: CarStanding::Withdrawn,
            ..facts(&run, &others)
        };
        assert_eq!(
            answer(&f),
            Some(Answer::FileFailure {
                branch: "fix/a-thing".into(),
                sha: SHA.into(),
                first: "first-run".into(),
                second: LOST.into(),
                why: "the original cause".into(),
            })
        );
        // No filing under way: no request to replay.
        assert_eq!(filing_request(&lost_run()), None);
    }

    /// The outcome carries both receipts whole and clears the marker.
    #[test]
    fn the_filed_stamp_keeps_the_request_and_the_result() {
        let request = failure_item_body(LOST, "fix/a-thing", SHA, "f", LOST, "cause", "emp-x");
        let id = handback_item_id(LOST);
        let result = json!({"id": id, "already_admitted": true});
        let s = filed_stamp(now(), &request, &id, &result);
        assert_eq!(s[STAMP]["action"], FAILURE_FILED);
        assert_eq!(s[STAMP]["item"], json!(id));
        assert_eq!(s[STAMP]["reason"], "cause");
        assert_eq!(s[STAMP]["request"], request);
        assert_eq!(s[STAMP]["result"], result);
        assert_eq!(s.as_object().unwrap().get(UNFINISHED), Some(&Value::Null));
    }

    /// The recovery read accepts the original and nothing else.
    #[test]
    fn only_the_original_handback_is_recovered() {
        let mut item = failure_item_body(LOST, "fix/a-thing", SHA, "f", LOST, "cause", "emp-x");
        assert!(is_original_handback(&item, LOST));
        assert!(!is_original_handback(&item, "f"), "another run's id");
        item["kind"] = json!("ops-request");
        assert!(!is_original_handback(&item, LOST));
        let mut other_runs = failure_item_body(LOST, "fix/a-thing", SHA, "f", "g", "c", "e");
        assert!(!is_original_handback(&other_runs, LOST), "names neither");
        other_runs["metadata"]["evidence"]["gate_runs"] = json!(["f", LOST]);
        other_runs["id"] = json!("someone-else");
        assert!(!is_original_handback(&other_runs, LOST), "another id");
    }

    // -- the stamp and the car ------------------------------------------

    #[test]
    fn the_stamp_carries_only_what_it_was_given() {
        let s = stamp(RELAUNCHED, now(), Some("new-run"), None, None);
        assert_eq!(s[STAMP]["action"], RELAUNCHED);
        assert_eq!(s[STAMP]["run"], "new-run");
        assert!(s[STAMP].get("item").is_none());
        assert_eq!(s[STAMP]["at"], "2026-10-16T20:00:00Z");
    }

    #[test]
    fn car_standing_reads_every_car_for_the_branch() {
        let car = |status: &str, md: Value| {
            let mut m = json!({"branch": "fix/a-thing"});
            if let (Some(b), Some(e)) = (m.as_object_mut(), md.as_object()) {
                b.extend(e.clone());
            }
            json!({"status": status, "metadata": m})
        };
        assert_eq!(car_standing(&[], "fix/a-thing"), CarStanding::NoCar);
        let elsewhere = json!({"status": "open", "metadata": {"branch": "fix/other"}});
        assert_eq!(
            car_standing(&[elsewhere], "fix/a-thing"),
            CarStanding::NoCar
        );
        assert_eq!(
            car_standing(&[car("open", json!({}))], "fix/a-thing"),
            CarStanding::Open
        );
        assert_eq!(
            car_standing(
                &[car("closed", json!({"abandoned": "true"}))],
                "fix/a-thing"
            ),
            CarStanding::Withdrawn
        );
        // A retired car beside a live successor: the successor stands.
        assert_eq!(
            car_standing(
                &[car("closed", json!({})), car("open", json!({}))],
                "fix/a-thing"
            ),
            CarStanding::Open
        );
    }

    // -- no silent end (coordinator review of this car) ------------------

    /// GAP 2. The conductor stamped its attempt, then the relaunch's
    /// marks write failed (so `file_and_start_gate` settled the new run
    /// lost WITHOUT `relaunch_of`) and the `launch_failed` stamp failed
    /// too. The orphan relaunch must not read as "a newer gate exists":
    /// an attempt with no recorded outcome is the machine's failure.
    #[test]
    fn an_attempt_with_no_outcome_is_handed_back_not_declined_as_newer() {
        let run = with_md(
            lost_run(),
            STAMP,
            json!({"action": ATTEMPTING, "at": at(8)}),
        );
        let others = [other("orphan-relaunch", 7, json!({}))];
        assert!(is_candidate(&run, now()));
        match answer(&facts(&run, &others)) {
            Some(Answer::FileFailure {
                first, second, why, ..
            }) => {
                assert_eq!(first, LOST);
                assert_eq!(second, "orphan-relaunch");
                assert!(why.contains("no outcome"), "{why}");
            }
            other => panic!("expected a hand-back, got {other:?}"),
        }
    }

    /// The same attempt whose relaunch DID file with its marks only lost
    /// the outcome stamp: that is a relaunch, recorded, not a failure.
    #[test]
    fn an_attempt_whose_relaunch_carries_relaunch_of_is_not_handed_back() {
        let run = with_md(
            lost_run(),
            STAMP,
            json!({"action": ATTEMPTING, "at": at(8)}),
        );
        let others = [other("the-relaunch", 7, json!({RELAUNCH_OF: LOST}))];
        let why = declined(answer(&facts(&run, &others)));
        assert!(why.contains("already relaunched as the-relaunch"), "{why}");
    }

    /// An attempt is answered whatever the window says: the machine's
    /// own unfinished act never ages out silently.
    #[test]
    fn an_attempt_past_the_window_is_still_handed_back() {
        let mut run = with_md(
            lost_run(),
            STAMP,
            json!({"action": ATTEMPTING, "at": at(9)}),
        );
        run["steps"][0]["completed_at"] = json!(at(WINDOW_HOURS * 60 + 30));
        assert!(matches!(
            answer(&facts(&run, &[])),
            Some(Answer::FileFailure { .. })
        ));
    }

    /// GAP 3. Launches refused before anything was filed (the gate
    /// bound) are retried inside the window, and when the window closes
    /// the loss is handed back naming the refusals — never just dropped.
    #[test]
    fn refusals_until_the_window_closes_are_handed_back_naming_them() {
        let mut run = lost_run();
        let mut p = refusal_patch(
            &run,
            now() - Duration::minutes(300),
            "2 gate(s) running of 2",
        );
        run["metadata"][REFUSALS] = p[REFUSALS].take();
        let p = refusal_patch(
            &run,
            now() - Duration::minutes(10),
            "train priority is UNKNOWN",
        );
        run["metadata"][REFUSALS] = p[REFUSALS].clone();
        // Inside the window: still a relaunch to try.
        assert!(matches!(
            answer(&facts(&run, &[])),
            Some(Answer::Relaunch { .. })
        ));
        // Past it: the hand-back, naming what refused it.
        run["steps"][0]["completed_at"] = json!(at(WINDOW_HOURS * 60 + 1));
        assert!(is_candidate(&run, now()));
        match answer(&facts(&run, &[])) {
            Some(Answer::FileFailure {
                first, second, why, ..
            }) => {
                assert_eq!(first, LOST);
                assert!(second.contains("none"), "{second}");
                for s in [
                    "2 refusal",
                    "2 gate(s) running of 2",
                    "train priority is UNKNOWN",
                ] {
                    assert!(why.contains(s), "{s} missing from {why}");
                }
            }
            other => panic!("expected a hand-back, got {other:?}"),
        }
    }

    /// A loss that was never attempted and never refused still ages out
    /// quietly: the window exists so the first deploy relaunches no
    /// history, and history is not the machine's failure.
    #[test]
    fn an_untouched_loss_past_the_window_stays_quiet() {
        let mut run = lost_run();
        run["steps"][0]["completed_at"] = json!(at(WINDOW_HOURS * 60 + 1));
        assert_eq!(answer(&facts(&run, &[])), None);
    }

    /// THE NARROW CENSUS (backlog de67e500). Beyond the window the
    /// conductor reads stamped runs only to find the machine's own
    /// unfinished acts, and it finds them by [`UNFINISHED`] rather than
    /// by every stamp ever written (re-read each pass, growing forever).
    /// So every stamp carries the marker exactly when [`is_candidate`]
    /// still owes it an answer past the window, and says null otherwise,
    /// which a metadata PATCH reads as "delete": the outcome that
    /// answers an attempt takes the marker with it.
    #[test]
    fn a_stamp_carries_the_unfinished_marker_exactly_when_it_is_still_owed() {
        let mut past = lost_run();
        past["steps"][0]["completed_at"] = json!(at(WINDOW_HOURS * 60 + 1));
        assert!(
            !is_candidate(&past, now()),
            "the fixture is past its window"
        );
        for action in [
            RELAUNCHED,
            DECLINED,
            FAILURE_FILED,
            LAUNCH_FAILED,
            ATTEMPTING,
            FILING,
        ] {
            let s = stamp(action, now(), None, None, None);
            let owed = is_candidate(&with_md(past.clone(), STAMP, s[STAMP].clone()), now());
            let marker = s.as_object().unwrap().get(UNFINISHED);
            if owed {
                assert_eq!(marker, Some(&json!(true)), "{action}: {s}");
            } else {
                assert_eq!(marker, Some(&Value::Null), "{action}: {s}");
            }
        }
        // Every owed action is owed (the loop above is not vacuous).
        assert!([LAUNCH_FAILED, ATTEMPTING, FILING].iter().all(|a| stamp(
            a,
            now(),
            None,
            None,
            None
        )[UNFINISHED]
            == json!(true)));
        // A refused launch clears the attempt AND its marker.
        let p = refusal_patch(&lost_run(), now(), "2 gate(s) running of 2");
        assert_eq!(p.as_object().unwrap().get(UNFINISHED), Some(&Value::Null));
    }

    /// The refusal record is bounded: a count, the first time, and the
    /// last few causes — one write per pass for six hours must not grow
    /// the packet without limit. It also clears an attempt stamp, so the
    /// refused try is not read as an attempt with no outcome.
    #[test]
    fn the_refusal_patch_counts_keeps_the_last_few_and_clears_the_attempt() {
        let mut run = with_md(lost_run(), STAMP, json!({"action": ATTEMPTING}));
        for i in 0..(REFUSALS_KEPT as i64 + 3) {
            let p = refusal_patch(
                &run,
                now() - Duration::minutes(60 - i),
                &format!("cause {i}"),
            );
            assert!(p[STAMP].is_null(), "{p}");
            run["metadata"][REFUSALS] = p[REFUSALS].clone();
        }
        let r = &run["metadata"][REFUSALS];
        assert_eq!(r["count"], REFUSALS_KEPT + 3);
        assert_eq!(r["last"].as_array().unwrap().len(), REFUSALS_KEPT);
        assert_eq!(
            r["first_at"],
            json!(
                (now() - Duration::minutes(60)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            )
        );
        assert_eq!(
            r["last"][REFUSALS_KEPT - 1]["cause"],
            format!("cause {}", REFUSALS_KEPT + 2)
        );
    }
}
