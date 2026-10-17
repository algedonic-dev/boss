//! ONE UNSCOPED FULL GATE ON CURRENT MAIN, after each converge, off the
//! critical path — the pure half (backlog 77596188; design c6d314a9 Q2,
//! option (a), answered by David 2026-10-16: "Let's try option (a)").
//!
//! WHY. Nothing guaranteed a landed change's DEPENDENTS were tested.
//! Measured Oct 1-16: `infra/gate.sh --auto` tests only the crates whose
//! files changed (clippy compiles the dependents but never runs their
//! tests); 502 of 570 car greens and 347 of 348 train gates ran `--auto`;
//! in 118 of 252 trains the change's dependents were tested by neither
//! gate, and forge CI runs no Rust tests. A dependent broken that way
//! surfaced red on an UNRELATED car — about 6 of October's 10 red train
//! gates. A full gate on main after each converge turns that into a red
//! on main, attributed to the one or two trains since main was last
//! proven, instead of a mystery on somebody else's car.
//!
//! WHO ACTS. The conductor, which already holds the gates Role and owns
//! the one launch path (`launch_gate`). This module decides; the
//! conductor reads the facts, asks, and writes what it is told — the
//! shape `lost_gate` and `stranded` already have.
//!
//! WHAT IT IS NOT. Detection with attribution, not a merge gate: it
//! never cancels, holds or strikes a train. It changes no car's or
//! train's gate scope (option (b) is out of scope).
//!
//! THE RULES, each a test below:
//! - ONCE PER MAIN SHA, COALESCED: at most one main gate in flight;
//!   none launched for a sha already judged green or red, nor when a
//!   newer main is already green. Main moves only by trains, so a sha
//!   skipped while a gate ran is covered by the next one.
//! - AFTER CONVERGE: nothing launches while a merged train has not yet
//!   converged — the gate then tests what the cluster will serve.
//! - IT YIELDS. It runs inside the gate bound and goes LAST: every
//!   queued car gate, every waiting train, every dock claim stands ahead
//!   of it, and it launches only when a bay would still stand free after
//!   it ([`SPARE_BAYS`]). An unreadable claim is a yield, never a launch:
//!   a car's gate may guess in its own favour; this one is optional.
//! - ON RED: ONE urgent item naming the failing checks and the trains
//!   merged since the last green full gate on main. A later red in the
//!   same streak with the same failing checks files nothing new; it is
//!   stamped as part of the first item's streak.
//! - A LOST OR REFUSED main gate files nothing and is relaunched the next
//!   pass; lost [`LOST_LIMIT`] times on one sha, it files ONE item and
//!   the machine stops trying that sha (the shape of backlog 565131e1).

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

/// The marker the conductor stamps on a main gate-run: `true`. The ONE
/// definition of "this gate-run is main's full gate" — the car lanes
/// (stranded, garage, limbo, the landed-verdict burial) read it through
/// [`is_main_gate`] and never through the branch name.
pub const MARK: &str = "main_gate";

/// The branch a main gate gates.
pub const BRANCH: &str = "main";

/// The stamp the conductor writes on a CLOSED main gate-run once it has
/// answered it: `{action, at, item?, first?}`. Its presence ends the
/// question for that run.
pub const ANSWER: &str = "main_gate_answer";
/// [`ANSWER`] actions.
pub const RED_FILED: &str = "red_filed";
pub const RED_STREAK: &str = "red_streak";
pub const LOST_FILED: &str = "lost_filed";

/// How many infrastructure deaths one sha is given before the machine
/// hands it to an agent: the first is relaunched next pass, the second
/// is a pattern (565131e1's budget, for the same reason).
pub const LOST_LIMIT: usize = 2;

/// Bays a main gate leaves free when it launches. MEASURED 2026-10-16
/// over the 36 hours before this car: a full gate takes a median 47 min
/// (p90 60, 68 green `full` receipts in the newest 200 closed gate-runs)
/// against 34 for `--auto`; a bay stood idle 19% of the time in 19
/// windows, and TWO bays stood idle in 6 windows of 5+ minutes (the
/// longest 70) — about four a day against about six trains a day, which
/// coalescing covers. Taking the last free bay instead would put a
/// train's gate or the next car behind a 47-minute run it never asked
/// for; this gate is the one that can wait.
pub const SPARE_BAYS: usize = 1;

/// How many main gate-runs one read takes, newest first. Attribution
/// needs only back to the last green; a red whose last green is further
/// back than this says so rather than guessing.
pub const PAGE: usize = 50;

/// The marks a main gate-run is filed with.
pub fn marks() -> Value {
    json!({ MARK: true })
}

/// Is this gate-run main's full gate? Read through the resident
/// submission's view, as every other gate-run marker is.
pub fn is_main_gate(gate_run_metadata: &Value) -> bool {
    crate::gate_reservation::source_metadata(gate_run_metadata)
        .ok()
        .and_then(|md| md.get(MARK))
        .and_then(Value::as_bool)
        == Some(true)
}

fn md(run: &Value) -> Option<&Value> {
    crate::gate_reservation::source_metadata(run.get("metadata")?).ok()
}

fn text<'a>(v: Option<&'a Value>, key: &str) -> Option<&'a str> {
    v?.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn id(run: &Value) -> &str {
    run.get("id").and_then(Value::as_str).unwrap_or_default()
}

fn sha(run: &Value) -> &str {
    text(md(run), "sha").unwrap_or_default()
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

fn opened_at(run: &Value) -> Option<DateTime<Utc>> {
    run.get("opened_at")
        .and_then(Value::as_str)
        .or_else(|| text(run.get("metadata"), "opened_at"))
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

fn answered(run: &Value) -> Option<&Value> {
    run.pointer(&format!("/metadata/{ANSWER}"))
        .filter(|v| !v.is_null())
}

/// Two shas name the same commit when one is a prefix of the other (a
/// train's `merge_ref` is twelve characters; a gate-run's sha is full).
fn same_commit(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && (a.starts_with(b) || b.starts_with(a))
}

/// What a main gate-run says, read off its record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// Open: running (a main gate is never queued — it launches only
    /// into a free bay).
    InFlight,
    Green,
    /// The checks ran and these did not pass (empty when the receipt
    /// named none).
    Red(Vec<String>),
    /// It judged nothing about main: lost, refused, withdrawn, or closed
    /// with no verdict. The word recorded.
    Lost(String),
}

/// PURE: a main gate-run's standing. A refusal is read off the RECEIPT,
/// as `flake::prior` reads it: a run recorded under the old vocabulary
/// says `failed` for a gate that refused before any check ran.
pub fn standing(run: &Value) -> Standing {
    if run.get("status").and_then(Value::as_str) != Some("closed") {
        return Standing::InFlight;
    }
    let receipt = crate::flake::receipt(run);
    let refused = receipt
        .as_ref()
        .and_then(|r| r.get("verdict"))
        .and_then(Value::as_str)
        == Some(crate::flake::REFUSED);
    match crate::flake::verdict(run) {
        Some(crate::stranded::VERDICT_GREEN) => Standing::Green,
        Some("failed") if refused => Standing::Lost(crate::flake::REFUSED.to_string()),
        Some("failed") => Standing::Red(
            receipt
                .as_ref()
                .and_then(crate::flake::failing_checks)
                .unwrap_or_default(),
        ),
        Some(other) => Standing::Lost(other.to_string()),
        None => Standing::Lost("closed with no verdict".to_string()),
    }
}

/// The main gate-runs among `runs`, newest first.
fn main_gates(runs: &[Value]) -> Vec<&Value> {
    let mut gates: Vec<&Value> = runs
        .iter()
        .filter(|r| r.get("metadata").is_some_and(is_main_gate))
        .collect();
    gates.sort_by_key(|r| std::cmp::Reverse(opened_at(r)));
    gates
}

/// The gate bays as the conductor read them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bays {
    /// Gate Jobs running now, whoever filed them.
    pub live: usize,
    /// The bound in force (`gate_max_concurrent`).
    pub max: usize,
    /// Car gate-runs waiting for a slot.
    pub queued: usize,
    /// Waiting trains plus the dock's claims, or why they could not be
    /// read.
    pub claims: Result<usize, String>,
}

/// PURE: how many car gate-runs among the OPEN gate-runs wait for a slot
/// — a resident submission not yet carried by a native Job, or a place
/// in line ([`crate::yard::QUEUED_AT`], a blank stamp being no stamp).
/// The yard's own reading (`yard::queued_for_a_slot`), on the JSON rows
/// the conductor holds. Train and main gates never queue, and are not
/// counted if one ever did.
pub fn car_gates_queued(open: &[Value]) -> usize {
    open.iter()
        .filter_map(|g| g.get("metadata"))
        .filter(|md| !crate::stranded::is_train_gate(md) && !is_main_gate(md))
        .filter(|md| {
            if crate::gate_reservation::durable_presence(md) {
                crate::gate_reservation::waiting_for_native(md)
            } else {
                md.get(crate::yard::QUEUED_AT)
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.trim().is_empty())
            }
        })
        .count()
}

/// The facts one pass decides a launch on.
#[derive(Debug, Clone)]
pub struct LaunchFacts<'a> {
    /// Current main, as the conductor's freshly fetched clone reads it.
    pub main_sha: &'a str,
    /// The gate-runs read for branch `main`, any status; non-main gates
    /// among them are ignored.
    pub runs: &'a [Value],
    /// Shas of main gates that are strict descendants of `main_sha`.
    pub newer: &'a BTreeSet<String>,
    /// A train merged into main but not yet converged, named.
    pub in_transit: Option<&'a str>,
    pub bays: Bays,
}

/// What a pass does about main's full gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// Launch an unscoped gate of `main` at this sha, with [`marks`].
    Launch { sha: String },
    /// Nothing is owed: main at this sha is gated, being gated, or not
    /// ready to be.
    Skip(String),
    /// Something is owed and other work goes first.
    Yield(String),
}

/// PURE: the one launch decision.
pub fn launch(f: &LaunchFacts<'_>) -> Launch {
    let main = f.main_sha.trim();
    if main.is_empty() {
        return Launch::Skip("main's sha could not be read".into());
    }
    let gates = main_gates(f.runs);
    if let Some(g) = gates.iter().find(|g| standing(g) == Standing::InFlight) {
        return Launch::Skip(format!(
            "main gate {} is in flight on {}",
            short(id(g)),
            short(sha(g))
        ));
    }
    let here: Vec<&&Value> = gates.iter().filter(|g| same_commit(sha(g), main)).collect();
    if let Some(g) = here
        .iter()
        .find(|g| matches!(standing(g), Standing::Green | Standing::Red(_)))
    {
        return Launch::Skip(format!(
            "main {} is already judged by gate {}",
            short(main),
            short(id(g))
        ));
    }
    let lost = here
        .iter()
        .filter(|g| matches!(standing(g), Standing::Lost(_)))
        .count();
    if lost >= LOST_LIMIT {
        return Launch::Skip(format!(
            "main {} was lost {lost} times — handed back; the next train's sha is the next try",
            short(main)
        ));
    }
    if let Some(g) = gates
        .iter()
        .find(|g| f.newer.contains(sha(g)) && standing(g) == Standing::Green)
    {
        return Launch::Skip(format!(
            "a newer main {} is already green (gate {})",
            short(sha(g)),
            short(id(g))
        ));
    }
    if let Some(train) = f.in_transit {
        return Launch::Skip(format!(
            "{train} has merged and not yet converged — main is gated after the converge"
        ));
    }
    let b = &f.bays;
    let claims = match &b.claims {
        Ok(n) => *n,
        Err(why) => return Launch::Yield(format!("the claims ahead are unreadable ({why})")),
    };
    if b.queued > 0 {
        return Launch::Yield(format!(
            "{} car gate(s) are queued for a slot and go first",
            b.queued
        ));
    }
    if b.live + claims + 1 + SPARE_BAYS > b.max {
        return Launch::Yield(format!(
            "{} running and {claims} claim(s) ahead of a bound of {}: a main gate launches only \
             when {SPARE_BAYS} bay(s) would still stand free",
            b.live, b.max
        ));
    }
    Launch::Launch {
        sha: main.to_string(),
    }
}

/// What the machine owes a closed main gate-run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owed {
    /// File ONE urgent item for this red and stamp [`RED_FILED`] on it.
    Red {
        run: String,
        sha: String,
        checks: Vec<String>,
        /// The newest green main gate before this red: (gate-run, sha).
        /// `None` when none is among the runs read.
        green: Option<(String, String)>,
    },
    /// A later red in a streak an item already names, failing the same
    /// checks: stamp [`RED_STREAK`] naming that item, file nothing.
    Streak {
        run: String,
        item: String,
        first: String,
    },
    /// Lost [`LOST_LIMIT`] times on one sha: file ONE item and stamp
    /// [`LOST_FILED`] on `run` (the newest of `runs`).
    LostTwice {
        run: String,
        sha: String,
        runs: Vec<String>,
        why: Vec<String>,
    },
}

/// PURE: everything owed among the runs read, oldest first. A red whose
/// streak already holds an UNANSWERED red failing the same checks waits
/// a pass: that earlier red files this pass, and the next pass reads its
/// item off its stamp — so two reds in one read never file twice, and a
/// streak stamp always names a real item.
pub fn owed(runs: &[Value]) -> Vec<Owed> {
    let mut gates = main_gates(runs);
    gates.reverse(); // oldest first
    let mut out = Vec::new();
    for (i, g) in gates.iter().enumerate() {
        let Standing::Red(checks) = standing(g) else {
            continue;
        };
        if answered(g).is_some() {
            continue;
        }
        let prior = &gates[..i];
        let green = prior
            .iter()
            .rev()
            .find(|p| standing(p) == Standing::Green)
            .map(|p| (id(p).to_string(), sha(p).to_string()));
        // The streak: the reds after that green and before this one that
        // failed exactly these checks, oldest first.
        let sorted = |mut c: Vec<String>| {
            c.sort();
            c
        };
        let mine = sorted(checks.clone());
        let mut same: Vec<&&Value> = prior
            .iter()
            .rev()
            .take_while(|p| standing(p) != Standing::Green)
            .filter(|p| matches!(standing(p), Standing::Red(c) if sorted(c.clone()) == mine))
            .collect();
        same.reverse();
        if same.is_empty() {
            out.push(Owed::Red {
                run: id(g).to_string(),
                sha: sha(g).to_string(),
                checks,
                green,
            });
            continue;
        }
        let joined = same.iter().find_map(|p| {
            let a = answered(p)?;
            let item = text(Some(a), "item")?.to_string();
            let first = text(Some(a), "first").unwrap_or(id(p)).to_string();
            Some((item, first))
        });
        if let Some((item, first)) = joined {
            out.push(Owed::Streak {
                run: id(g).to_string(),
                item,
                first,
            });
        }
    }
    // Lost twice on one sha, unanswered: one hand-back per sha.
    let mut by_sha: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for g in &gates {
        if matches!(standing(g), Standing::Lost(_)) && !sha(g).is_empty() {
            by_sha.entry(sha(g)).or_default().push(g);
        }
    }
    for (s, lost) in by_sha {
        if lost.len() < LOST_LIMIT || lost.iter().any(|g| answered(g).is_some()) {
            continue;
        }
        let newest = lost.last().map(|g| id(g).to_string()).unwrap_or_default();
        out.push(Owed::LostTwice {
            run: newest,
            sha: s.to_string(),
            runs: lost.iter().map(|g| id(g).to_string()).collect(),
            why: lost
                .iter()
                .map(|g| match standing(g) {
                    Standing::Lost(w) => w,
                    _ => String::new(),
                })
                .collect(),
        });
    }
    out
}

/// The [`ANSWER`] stamp.
pub fn answer_stamp(
    action: &str,
    now: DateTime<Utc>,
    item: Option<&str>,
    first: Option<&str>,
) -> Value {
    let mut a = json!({
        "action": action,
        "at": now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    });
    if let Some(item) = item {
        a["item"] = json!(item);
    }
    if let Some(first) = first {
        a["first"] = json!(first);
    }
    json!({ ANSWER: a })
}

/// One train merged into main, as the red item names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainLine {
    pub id: String,
    pub title: String,
    pub merge_ref: String,
    /// The `boarded` note the collect step recorded: the cars aboard.
    pub boarded: String,
}

/// PURE: the trains whose merge is one of `commits` — the commits in
/// `<last green>..<red>` on main, which the conductor reads with git.
/// Main moves only by trains, so every one of those commits is a train's
/// merge; one that matches no train read is returned as a commit, so
/// the item says so rather than dropping it.
pub fn trains_in(trains: &[Value], commits: &[String]) -> (Vec<TrainLine>, Vec<String>) {
    let line = |t: &Value| {
        let step = |slug: &str, key: &str| {
            crate::car::find_step(t, slug, "")
                .and_then(|s| s.pointer(&format!("/metadata/{key}")))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        TrainLine {
            id: id(t).to_string(),
            title: t
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            merge_ref: step("merged", "merge_ref"),
            boarded: step("collect", "boarded"),
        }
    };
    let lines: Vec<TrainLine> = trains.iter().map(line).collect();
    let mut found = Vec::new();
    let mut unmatched = Vec::new();
    for c in commits {
        match lines.iter().find(|l| same_commit(&l.merge_ref, c)) {
            Some(l) => found.push(l.clone()),
            None => unmatched.push(c.clone()),
        }
    }
    (found, unmatched)
}

fn subject() -> Value {
    json!({"subject_kind": "custom", "id": "bosspipeline"})
}

/// PURE: the ONE urgent item a red main gate files.
#[allow(clippy::too_many_arguments)]
pub fn red_item_body(
    run: &str,
    sha: &str,
    checks: &[String],
    green: Option<&(String, String)>,
    trains: &[TrainLine],
    unmatched: &[String],
    owner: &str,
) -> Value {
    let named = if checks.is_empty() {
        "(the receipt named no check)".to_string()
    } else {
        checks.join(", ")
    };
    let since = match green {
        Some((gid, gsha)) => format!(
            "since the last green full gate on main ({}, gate-run {gid})",
            short(gsha)
        ),
        None => format!(
            "with no green full gate on main among the newest {PAGE} main gate-runs read — the \
             list below is bounded by the commits the conductor read, not by a known green"
        ),
    };
    let mut lines: Vec<String> = trains
        .iter()
        .map(|t| {
            format!(
                "- train {} {} merged {}: {}",
                short(&t.id),
                t.title,
                t.merge_ref,
                if t.boarded.is_empty() {
                    "(no boarded note)"
                } else {
                    &t.boarded
                }
            )
        })
        .collect();
    lines.extend(
        unmatched
            .iter()
            .map(|c| format!("- commit {} — no train read names this merge", short(c))),
    );
    if lines.is_empty() {
        lines.push("- (no merge between the two shas was read)".to_string());
    }
    json!({
        "kind": "backlog-item",
        "title": format!(
            "Main went red in a full gate at {}: {named} — {} train(s) since the last green",
            short(sha),
            trains.len()
        ),
        "subject": subject(),
        "owner_id": owner,
        "status": "open",
        "tags": [],
        "priority": "urgent",
        "metadata": {
            "input_channel": "pipeline-failure",
            "source": {"kind": "gate-run", "id": run},
            "area": "delivery",
            "reporter": "conductor",
            "description": format!(
                "The conductor's unscoped full gate on main (backlog 77596188) went RED at \
                 {sha} (gate-run {run}). Failing: {named}.\n\nThe trains merged {since}:\n\n{}\n\n\
                 Detection with attribution, not a merge gate: no train was cancelled or held. A \
                 dependent of one of these changes is the likeliest cause — read the gate-run's \
                 receipt, then the cars above. Later reds failing the same checks before main \
                 goes green again are stamped as this streak and file nothing new.",
                lines.join("\n")
            ),
            "evidence": {
                "gate_run": run,
                "sha": sha,
                "failing": checks,
                "last_green": green.map(|(g, s)| json!({"gate_run": g, "sha": s})),
                "trains": trains.iter().map(|t| json!({
                    "id": t.id, "title": t.title, "merge_ref": t.merge_ref, "boarded": t.boarded,
                })).collect::<Vec<_>>(),
                "unmatched_commits": unmatched,
            },
        },
    })
}

/// PURE: the item a sha lost [`LOST_LIMIT`] times files.
pub fn lost_item_body(sha: &str, runs: &[String], why: &[String], owner: &str) -> Value {
    let newest = runs.last().map(String::as_str).unwrap_or_default();
    json!({
        "kind": "backlog-item",
        "title": format!(
            "Main's full gate was lost {} times at {}: the machine stopped trying that sha",
            runs.len(),
            short(sha)
        ),
        "subject": subject(),
        "owner_id": owner,
        "status": "open",
        "tags": [],
        "priority": "standard",
        "metadata": {
            "input_channel": "pipeline-failure",
            "source": {"kind": "gate-run", "id": newest},
            "area": "delivery",
            "reporter": "conductor",
            "description": format!(
                "The conductor relaunches a lost main gate on the next pass (backlog 77596188). \
                 At {sha} it was lost {} times ({}), each an infrastructure death that judged \
                 nothing about main: {}. Two on one sha is a pattern in the gate \
                 infrastructure, not in main — read the receipts. The next train's sha is \
                 gated as usual.",
                runs.len(),
                runs.join(", "),
                why.join("; ")
            ),
            "evidence": {"sha": sha, "gate_runs": runs, "why": why},
        },
    })
}

/// PURE: how many of `trains` merged after main's newest green full gate
/// was opened — the trains it has not proven, as a reader counts them.
/// `None` with no green among `runs`. (The red item attributes by git
/// range instead; this is the display's count, off the record alone.)
pub fn merged_since(trains: &[Value], runs: &[Value]) -> Option<usize> {
    let green = main_gates(runs)
        .into_iter()
        .find(|g| standing(g) == Standing::Green)
        .and_then(opened_at)?;
    Some(
        trains
            .iter()
            .filter_map(|t| crate::car::find_step(t, "merged", "Merged into main"))
            .filter(|s| s.get("status").and_then(Value::as_str) == Some("completed"))
            .filter_map(|s| s.get("completed_at").and_then(Value::as_str))
            .filter_map(|s| DateTime::parse_from_rfc3339(s).ok())
            .filter(|t| t.with_timezone(&Utc) > green)
            .count(),
    )
}

/// PURE: the one line a reader is shown — when main was last proven by
/// a full gate, how long ago, and what main's newest gate is doing now.
/// `main_sha` is current main when the reader knows it; `merged_since`
/// counts the trains merged after the green's gate was opened. `total`
/// is the read's `.total`, so a green older than the page says so.
pub fn proven_line(
    runs: &[Value],
    total: Option<usize>,
    main_sha: Option<&str>,
    merged_since: Option<usize>,
    now: DateTime<Utc>,
) -> String {
    let gates = main_gates(runs);
    let newest = gates.first().map(|g| {
        let what = match standing(g) {
            Standing::InFlight => "in flight".to_string(),
            Standing::Green => "green".to_string(),
            Standing::Red(c) if c.is_empty() => "RED".to_string(),
            Standing::Red(c) => format!("RED ({})", c.join(", ")),
            Standing::Lost(w) => w,
        };
        format!(
            "newest main gate {} at {}: {what}",
            short(id(g)),
            short(sha(g))
        )
    });
    let green = gates
        .iter()
        .find(|g| standing(g) == Standing::Green)
        .map(|g| {
            let age = opened_at(g)
                .map(|t| {
                    let m = (now - t).num_minutes().max(0);
                    if m < 120 {
                        format!("{m} min ago")
                    } else {
                        format!("{}h ago", m / 60)
                    }
                })
                .unwrap_or_else(|| "at an unknown time".to_string());
            let since = merged_since
                .map(|n| format!(", {n} train(s) merged since"))
                .unwrap_or_default();
            format!("last proven full at {}, {age}{since}", short(sha(g)))
        });
    let read = gates.len();
    let head = match (green, total) {
        (Some(g), _) => g,
        (None, Some(t)) if t > runs.len() => {
            format!("not proven full in the newest {read} main gate-runs read (of {t})")
        }
        (None, _) if read == 0 => "never gated in full by the conductor".to_string(),
        (None, _) => format!("not proven full in any of the {read} main gate-run(s) on record"),
    };
    let current = match (main_sha, gates.first()) {
        (Some(m), Some(g)) if !same_commit(m, sha(g)) => {
            format!("; current main {} not yet gated in full", short(m))
        }
        (Some(m), None) => format!("; current main {}", short(m)),
        _ => String::new(),
    };
    let newest = newest.map(|n| format!("; {n}")).unwrap_or_default();
    format!("  MAIN — {head}{newest}{current}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OLD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const NEW: &str = "cccccccccccccccccccccccccccccccccccccccc";

    fn at(min: i64) -> String {
        (base() + chrono::Duration::minutes(min)).to_rfc3339()
    }

    fn base() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-16T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// A main gate-run: `verdict` None = open; `receipt` the JSON
    /// receipt string the runner writes.
    fn gate(id: &str, sha: &str, min: i64, verdict: Option<&str>, receipt: Option<Value>) -> Value {
        let mut steps = vec![];
        if let Some(v) = verdict {
            let mut m = json!({"verdict": v});
            if let Some(r) = receipt {
                m["receipt"] = json!(r.to_string());
            }
            steps.push(json!({"spec_slug": "record-verdict", "metadata": m}));
        }
        json!({
            "id": id,
            "status": if verdict.is_some() { "closed" } else { "open" },
            "opened_at": at(min),
            "metadata": {"branch": "main", "sha": sha, MARK: true},
            "steps": steps,
        })
    }

    fn red(id: &str, sha: &str, min: i64, checks: &[&str]) -> Value {
        let checks: Vec<Value> = checks
            .iter()
            .map(|c| json!({"name": c, "result": "fail"}))
            .chain([json!({"name": "fmt", "result": "pass"})])
            .collect();
        gate(
            id,
            sha,
            min,
            Some("failed"),
            Some(json!({"verdict": "failed", "checks": checks})),
        )
    }

    fn free() -> Bays {
        Bays {
            live: 0,
            max: 3,
            queued: 0,
            claims: Ok(0),
        }
    }

    fn decide(runs: &[Value], newer: &[&str], transit: Option<&str>, bays: Bays) -> Launch {
        let newer: BTreeSet<String> = newer.iter().map(|s| s.to_string()).collect();
        launch(&LaunchFacts {
            main_sha: MAIN,
            runs,
            newer: &newer,
            in_transit: transit,
            bays,
        })
    }

    #[test]
    fn the_marker_is_the_one_definition_and_the_branch_name_is_not() {
        assert!(is_main_gate(&json!({"branch": "main", MARK: true})));
        assert!(!is_main_gate(&json!({"branch": "main"})));
        assert!(!is_main_gate(&json!({"branch": "feat/x", MARK: false})));
        assert_eq!(marks(), json!({"main_gate": true}));
    }

    #[test]
    fn no_full_gate_yet_launches_on_current_main() {
        assert_eq!(
            decide(&[], &[], None, free()),
            Launch::Launch { sha: MAIN.into() }
        );
    }

    #[test]
    fn a_gate_on_an_older_main_is_no_reason_to_wait() {
        let runs = [gate("g1", OLD, 0, Some("green"), None)];
        assert_eq!(
            decide(&runs, &[], None, free()),
            Launch::Launch { sha: MAIN.into() }
        );
    }

    #[test]
    fn one_in_flight_coalesces_every_later_main() {
        let runs = [gate("g1", OLD, 0, None, None)];
        assert!(
            matches!(decide(&runs, &[], None, free()), Launch::Skip(s) if s.contains("in flight"))
        );
    }

    #[test]
    fn a_sha_already_judged_green_or_red_is_gated_once() {
        for v in ["green", "failed"] {
            let runs = [gate("g1", MAIN, 0, Some(v), Some(json!({"verdict": v})))];
            assert!(
                matches!(decide(&runs, &[], None, free()), Launch::Skip(s) if s.contains("already judged")),
                "{v}"
            );
        }
    }

    #[test]
    fn a_newer_main_already_green_covers_this_one() {
        let runs = [gate("g1", NEW, 0, Some("green"), None)];
        assert!(matches!(
            decide(&runs, &[NEW], None, free()),
            Launch::Skip(s) if s.contains("newer main")
        ));
        // Not newer: an unrelated green says nothing about this sha.
        assert_eq!(
            decide(&runs, &[], None, free()),
            Launch::Launch { sha: MAIN.into() }
        );
    }

    #[test]
    fn nothing_launches_before_the_converge() {
        assert!(matches!(
            decide(&[], &[], Some("train 1234abcd"), free()),
            Launch::Skip(s) if s.contains("not yet converged")
        ));
    }

    #[test]
    fn it_yields_to_every_queued_car_gate() {
        let bays = Bays {
            queued: 1,
            ..free()
        };
        assert!(matches!(decide(&[], &[], None, bays), Launch::Yield(s) if s.contains("queued")));
    }

    #[test]
    fn it_launches_only_leaving_a_bay_free() {
        // 1 running of 3: the main gate takes one, one stands free.
        let one = Bays { live: 1, ..free() };
        assert_eq!(
            decide(&[], &[], None, one),
            Launch::Launch { sha: MAIN.into() }
        );
        // 2 running of 3: it would take the last bay — yield.
        let two = Bays { live: 2, ..free() };
        assert!(matches!(decide(&[], &[], None, two), Launch::Yield(_)));
        // A waiting train or a dock claim counts as running.
        let claimed = Bays {
            live: 1,
            claims: Ok(1),
            ..free()
        };
        assert!(matches!(decide(&[], &[], None, claimed), Launch::Yield(_)));
    }

    #[test]
    fn the_queue_counts_car_gates_waiting_for_a_slot_and_nothing_running() {
        let open = [
            json!({"metadata": {"branch": "fix/a", "queued_at": "2026-10-16T21:00:00Z"}}),
            json!({"metadata": {"branch": "fix/b", "queued_at": ""}}),
            json!({"metadata": {"branch": "fix/c", "launched_at": "2026-10-16T21:00:00Z"}}),
            json!({"metadata": {"branch": "train/x", "train_gate": true, "queued_at": "x"}}),
        ];
        assert_eq!(car_gates_queued(&open), 1);
    }

    #[test]
    fn unreadable_claims_yield_rather_than_guess() {
        let bays = Bays {
            claims: Err("the dock did not answer".into()),
            ..free()
        };
        assert!(
            matches!(decide(&[], &[], None, bays), Launch::Yield(s) if s.contains("unreadable"))
        );
    }

    #[test]
    fn a_lost_gate_relaunches_next_pass_and_twice_hands_back() {
        let once = [gate("g1", MAIN, 0, Some("lost"), None)];
        assert_eq!(
            decide(&once, &[], None, free()),
            Launch::Launch { sha: MAIN.into() }
        );
        assert!(owed(&once).is_empty(), "one loss files nothing");
        // A refusal recorded under the old vocabulary is a loss too.
        let refused = gate(
            "g2",
            MAIN,
            10,
            Some("failed"),
            Some(json!({"verdict": "refused", "refused_because": "disk floor"})),
        );
        assert_eq!(standing(&refused), Standing::Lost("refused".into()));
        let twice = [once[0].clone(), refused];
        assert!(
            matches!(decide(&twice, &[], None, free()), Launch::Skip(s) if s.contains("lost 2 times"))
        );
        assert_eq!(
            owed(&twice),
            vec![Owed::LostTwice {
                run: "g2".into(),
                sha: MAIN.into(),
                runs: vec!["g1".into(), "g2".into()],
                why: vec!["lost".into(), "refused".into()],
            }]
        );
        // Once answered, never again.
        let mut answered = twice.clone();
        answered[1]["metadata"][ANSWER] = json!({"action": LOST_FILED, "item": "i1"});
        assert!(owed(&answered).is_empty());
    }

    #[test]
    fn a_red_files_one_item_naming_the_last_green() {
        let runs = [
            gate(
                "g0",
                OLD,
                0,
                Some("green"),
                Some(json!({"verdict": "green"})),
            ),
            red("g1", MAIN, 60, &["test"]),
        ];
        assert_eq!(
            owed(&runs),
            vec![Owed::Red {
                run: "g1".into(),
                sha: MAIN.into(),
                checks: vec!["test".into()],
                green: Some(("g0".into(), OLD.into())),
            }]
        );
    }

    #[test]
    fn a_red_with_no_green_on_record_says_so() {
        let runs = [red("g1", MAIN, 60, &["test"])];
        assert!(matches!(&owed(&runs)[..], [Owed::Red { green: None, .. }]));
    }

    #[test]
    fn a_streak_failing_the_same_checks_files_once() {
        let mut first = red("g1", OLD, 0, &["test", "clippy"]);
        first["metadata"][ANSWER] = json!({"action": RED_FILED, "item": "item-1"});
        let second = red("g2", MAIN, 60, &["clippy", "test"]);
        assert_eq!(
            owed(&[first.clone(), second.clone()]),
            vec![Owed::Streak {
                run: "g2".into(),
                item: "item-1".into(),
                first: "g1".into()
            }]
        );
        // A different failure in the streak is a new breakage: it files.
        let other = red("g3", NEW, 120, &["web-suite"]);
        assert!(matches!(
            &owed(&[first.clone(), other])[..],
            [Owed::Red { run, .. }] if run == "g3"
        ));
        // A green between them ends the streak.
        let green = gate("gg", OLD, 30, Some("green"), None);
        assert!(matches!(
            &owed(&[first, green, second])[..],
            [Owed::Red { run, .. }] if run == "g2"
        ));
    }

    #[test]
    fn two_unanswered_reds_in_one_read_file_once() {
        let runs = [red("g1", OLD, 0, &["test"]), red("g2", MAIN, 60, &["test"])];
        let got = owed(&runs);
        assert!(
            matches!(&got[..], [Owed::Red { run, .. }] if run == "g1"),
            "the second waits for the first's stamp: {got:?}"
        );
    }

    #[test]
    fn non_main_gates_are_never_read() {
        let mut car = red("c1", MAIN, 0, &["test"]);
        car["metadata"] = json!({"branch": "fix/a", "sha": MAIN});
        assert!(owed(std::slice::from_ref(&car)).is_empty());
        assert_eq!(
            decide(&[car], &[], None, free()),
            Launch::Launch { sha: MAIN.into() }
        );
    }

    #[test]
    fn trains_are_matched_by_their_merge_and_a_stray_commit_is_named() {
        let train = json!({
            "id": "t1111111-x", "title": "PR train 2026-10-16 20:36",
            "steps": [
                {"spec_slug": "collect", "metadata": {"boarded": "fix/a (0cce2dab)"}},
                {"spec_slug": "merged", "metadata": {"merge_ref": "aaaaaaaaaaaa"}},
            ]
        });
        let (found, stray) = trains_in(&[train], &[MAIN.into(), NEW.into()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].boarded, "fix/a (0cce2dab)");
        assert_eq!(stray, vec![NEW.to_string()]);
        let body = red_item_body(
            "g1",
            MAIN,
            &["test".into()],
            Some(&("g0".into(), OLD.into())),
            &found,
            &stray,
            "emp-x",
        );
        assert_eq!(body["priority"], "urgent");
        assert_eq!(body["metadata"]["input_channel"], "pipeline-failure");
        assert_eq!(
            body["metadata"]["source"],
            json!({"kind": "gate-run", "id": "g1"})
        );
        let d = body["metadata"]["description"].as_str().unwrap();
        assert!(
            d.contains("fix/a (0cce2dab)") && d.contains("Failing: test"),
            "{d}"
        );
        assert!(d.contains("commit cccccccc"), "{d}");
        assert!(d.contains("bbbbbbbb"), "the last green is named: {d}");
    }

    #[test]
    fn the_lost_item_names_both_runs() {
        let body = lost_item_body(
            MAIN,
            &["g1".into(), "g2".into()],
            &["lost".into(), "refused".into()],
            "o",
        );
        assert_eq!(body["metadata"]["source"]["id"], "g2");
        assert!(
            body["metadata"]["description"]
                .as_str()
                .unwrap()
                .contains("g1, g2")
        );
    }

    #[test]
    fn the_answer_stamp_carries_what_it_did() {
        let s = answer_stamp(RED_STREAK, base(), Some("item-1"), Some("g1"));
        assert_eq!(s[ANSWER]["action"], RED_STREAK);
        assert_eq!(s[ANSWER]["item"], "item-1");
        assert_eq!(s[ANSWER]["first"], "g1");
        assert_eq!(s[ANSWER]["at"], "2026-10-16T12:00:00Z");
    }

    #[test]
    fn merged_since_counts_trains_merged_after_the_green_opened() {
        let train = |min: i64| json!({"steps": [{"spec_slug": "merged", "status": "completed", "completed_at": at(min)}]});
        let unmerged = json!({"steps": [{"spec_slug": "merged", "status": "ready"}]});
        let runs = [gate("g0", OLD, 30, Some("green"), None)];
        let trains = [train(10), train(40), train(90), unmerged];
        assert_eq!(merged_since(&trains, &runs), Some(2));
        assert_eq!(merged_since(&trains, &[]), None);
    }

    #[test]
    fn the_line_says_when_main_was_last_proven_and_what_runs_now() {
        let runs = [
            gate(
                "g0aaaaaa",
                OLD,
                0,
                Some("green"),
                Some(json!({"verdict": "green"})),
            ),
            gate("g1bbbbbb", MAIN, 60, None, None),
        ];
        let now = base() + chrono::Duration::minutes(90);
        let line = proven_line(&runs, Some(2), Some(MAIN), Some(1), now);
        assert_eq!(
            line,
            "  MAIN — last proven full at bbbbbbbb, 90 min ago, 1 train(s) merged since; newest \
             main gate g1bbbbbb at aaaaaaaa: in flight"
        );
        let none = proven_line(&[], Some(0), Some(NEW), None, now);
        assert_eq!(
            none,
            "  MAIN — never gated in full by the conductor; current main cccccccc"
        );
        let cut = proven_line(
            &[red("r", MAIN, 0, &["test"])],
            Some(80),
            Some(NEW),
            None,
            now,
        );
        assert!(
            cut.contains("not proven full in the newest 1 main gate-runs read (of 80)"),
            "{cut}"
        );
        assert!(
            cut.contains("RED (test)") && cut.contains("current main cccccccc not yet gated"),
            "{cut}"
        );
    }
}
