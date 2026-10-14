//! THE TOP BOARD in `boss orient` — OUTRANKS REGULAR ORDER, NEEDS YOU
//! and NEXT UP (backlog 74569e94, design ea906603, car 3).
//!
//! The IT department map's top board shows what outranks regular order,
//! what needs the viewer, and what happens next. The first and third are
//! fields on `GET /api/yard/regions` (`outranks`, `next_up`), computed
//! once on the server; the second is `GET /api/jobs/assignments?for=me`,
//! the viewer's queue with the agents registry's aliases and the roles
//! they hold expanded on the server (Q2). This verb prints the SAME
//! payloads, so the terminal and the page cannot disagree about what is
//! urgent, whose it is, or when the next train boards.
//!
//! UNREAD IS SAID. A payload without the field (a server older than the
//! design) or a read that failed prints as unread with the reason —
//! never as "nothing outranks" or "nothing is coming", which are claims.
//!
//! Pure: the lines are functions of the payloads and the server's `now`,
//! so every shape is pinned without a server.

use chrono::{DateTime, Utc};
use serde_json::Value;

/// How many characters of a title a board line shows.
const TITLE_CHARS: usize = 60;

/// How many NEEDS YOU rows the board prints before pointing at MY WORK,
/// which lists them all.
const NEEDS_YOU_ROWS: usize = 5;

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("?")
}

fn clip(t: &str) -> String {
    t.chars()
        .take(TITLE_CHARS)
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn id8(id: &str) -> &str {
    &id[..id.len().min(8)]
}

/// A span as the board says it: `4d`, `6h52m`, `22m`, `now`.
pub(crate) fn span(minutes: i64) -> String {
    match minutes {
        m if m <= 0 => "now".to_string(),
        m if m < 60 => format!("{m}m"),
        m if m < 24 * 60 => match m % 60 {
            0 => format!("{}h", m / 60),
            r => format!("{}h{r}m", m / 60),
        },
        m => format!("{}d", m / (24 * 60)),
    }
}

/// The instant a payload was computed at — its own `now`, so countdowns
/// run on the server's clock; the caller's when the payload has none.
pub(crate) fn payload_now(map: &Value, fallback: DateTime<Utc>) -> DateTime<Utc> {
    map.get("now")
        .and_then(Value::as_str)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map_or(fallback, |t| t.with_timezone(&Utc))
}

/// WHETHER ANYONE IS ON A CLAIM, in words — ONE formatter for every
/// section that prints the server's `run` reading
/// (`boss_jobs::outranks::claim_run`): OUTRANKS off `/api/yard/regions`,
/// NEEDS YOU and MY WORK off `assignments?for=me` (backlog e2519eca item
/// 2, which found the queue silent while the board said STALLED). The
/// first value is whether the claim is stalled — the server's
/// `StepRun::stalled`, read off the state it serialised.
pub(crate) fn run_reading(run: &Value) -> (bool, String) {
    let ago = |m: Option<i64>| m.map_or_else(|| "?".to_string(), span);
    match s(run, "state") {
        "building" => (false, format!("run {}", id8(s(run, "run")))),
        // Briefed, inside the start bound its protocol declares (backlog
        // 57326714): not stalled yet.
        "starting" => (
            false,
            format!(
                "run {} starting: briefed {}, {} allowed",
                id8(s(run, "run")),
                match ago(run.get("age_minutes").and_then(Value::as_i64)).as_str() {
                    "now" => "just now".to_string(),
                    a => format!("{a} ago"),
                },
                ago(run.get("expect_minutes").and_then(Value::as_i64)),
            ),
        ),
        "never-started" => (
            true,
            format!(
                "STALLED: run {} briefed {} ago, never started",
                id8(s(run, "run")),
                ago(run.get("age_minutes").and_then(Value::as_i64))
            ),
        ),
        "no-run" => (true, "STALLED: claimed, no open run".to_string()),
        _ => (false, "run unread".to_string()),
    }
}

/// A row's `run` reading, where the server sent one.
pub(crate) fn row_run(row: &Value) -> Option<&Value> {
    row.get("run").filter(|run| run.is_object())
}

/// Whether a row (a board row's `at`, or a queue row) is a stalled claim.
pub(crate) fn stalled(row: &Value) -> bool {
    row_run(row).is_some_and(|run| run_reading(run).0)
}

/// `(inside)` with the row's run reading: inside the brackets when the
/// claim is worked or unread, after them, loud, when it is stalled. No
/// reading prints the brackets as they always were.
pub(crate) fn with_run(inside: &str, row: &Value) -> String {
    match row_run(row).map(run_reading) {
        None => format!("({inside})"),
        Some((true, words)) => format!("({inside}) {words}"),
        Some((false, words)) => format!("({inside}, {words})"),
    }
}

/// OUTRANKS REGULAR ORDER — every open packet whose priority drains
/// before standard, oldest first, where it stands.
pub(crate) fn outranks_lines(map: &Value) -> Vec<String> {
    let rows = match map.get("outranks") {
        Some(Value::Array(rows)) => rows,
        _ => {
            return vec![
                "  OUTRANKS REGULAR ORDER — unread: the regions read carried no `outranks` \
                 (a server older than design ea906603, or its read failed)"
                    .to_string(),
            ];
        }
    };
    if rows.is_empty() {
        return vec!["  OUTRANKS REGULAR ORDER — nothing outranks regular order".to_string()];
    }
    let count = |p: &str| rows.iter().filter(|r| s(r, "priority") == p).count();
    let tally: Vec<String> = ["emergency", "urgent"]
        .iter()
        .filter_map(|p| match count(p) {
            0 => None,
            n => Some(format!("{n} {p}")),
        })
        .collect();
    let stalled = rows
        .iter()
        .filter(|r| r.get("at").is_some_and(stalled))
        .count();
    let undispatchable = rows
        .iter()
        .filter(|r| {
            r.get("at")
                .and_then(|a| a.get("dispatch"))
                .is_some_and(|d| s(d, "state") == "undispatchable")
        })
        .count();
    let mut out = vec![format!(
        "  OUTRANKS REGULAR ORDER — {} ({}), oldest first{}{}",
        rows.len(),
        tally.join(", "),
        match stalled {
            0 => String::new(),
            n => format!(" — {n} STALLED: claimed, nobody on it"),
        },
        match undispatchable {
            0 => String::new(),
            n => format!(" — {n} UNDISPATCHABLE: ready, nothing can dispatch it"),
        }
    )];
    for r in rows {
        let age = r
            .get("age_minutes")
            .and_then(Value::as_i64)
            .map_or_else(|| "age ?".to_string(), span);
        let at = match r.get("at") {
            Some(a) if a.is_object() => {
                let slug = a
                    .get("slug")
                    .and_then(Value::as_str)
                    .unwrap_or(s(a, "title"));
                let who = a
                    .get("assignee_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unassigned");
                let status = s(a, "status");
                // WHETHER ANYONE IS ON IT (backlog 8e9d3723): the
                // server's reading of the open run on a claimed step. A
                // server that sends none prints the line as it always did.
                // WHETHER ANYTHING CAN DISPATCH IT (backlog e2519eca):
                // the server's reading of a ready step with no agent
                // block that no person holds, naming the row to fix.
                let dispatch = a.get("dispatch").filter(|d| d.is_object());
                match row_run(a) {
                    None => match dispatch {
                        None => format!("at {slug} ({status}, {who})"),
                        Some(d) if s(d, "state") == "undispatchable" => format!(
                            "at {slug} ({status}, {who}) UNDISPATCHABLE: {} v{} declares no \
                             agent block on {slug} and no person holds it",
                            s(d, "kind"),
                            d.get("version")
                                .and_then(Value::as_i64)
                                .map_or_else(|| "?".to_string(), |v| v.to_string()),
                        ),
                        Some(_) => format!("at {slug} ({status}, {who}, dispatch unread)"),
                    },
                    Some(_) => format!("at {slug} {}", with_run(&format!("{status}, {who}"), a)),
                }
            }
            _ => "no workable step".to_string(),
        };
        out.push(format!(
            "    {} {} {} {} — {age}, {at}",
            s(r, "priority"),
            s(r, "kind"),
            id8(s(r, "id")),
            clip(s(r, "title")),
        ));
    }
    out
}

/// NEEDS YOU — the viewer's queue from `assignments?for=me`: the count,
/// whom it was read for, and the first few rows, urgent first then
/// oldest. MY WORK, further down, lists every row.
pub(crate) fn needs_you_lines(read: &Result<Value, String>) -> Vec<String> {
    let body = match read {
        Err(why) => return vec![format!("  NEEDS YOU — unread: {why}")],
        Ok(body) => body,
    };
    let rows: Vec<&Value> = body
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let who = for_whom(body);
    if rows.is_empty() {
        return vec![format!(
            "  NEEDS YOU — nothing: no ready/active step is for {who}"
        )];
    }
    let rank = |r: &Value| match s(r, "priority") {
        "emergency" => 0,
        "urgent" => 1,
        _ => 2,
    };
    let mut sorted = rows.clone();
    // Stable: the server's order (admission) survives within a rank, and
    // a stalled claim leads its rank (backlog e2519eca item 2) — it is
    // work the queue says is in hand and nobody is doing.
    sorted.sort_by_key(|r| (rank(r), !stalled(r)));
    let urgent = rows.iter().filter(|r| rank(r) < 2).count();
    let stalled_n = rows.iter().filter(|r| stalled(r)).count();
    let mut out = vec![format!(
        "  NEEDS YOU — {} ready/active step(s) for {who}{}{}",
        rows.len(),
        match urgent {
            0 => String::new(),
            n => format!(", {n} outranking regular order"),
        },
        match stalled_n {
            0 => String::new(),
            n => format!(" — {n} STALLED: claimed, nobody on it"),
        }
    )];
    for r in sorted.iter().take(NEEDS_YOU_ROWS) {
        let flag = match rank(r) {
            2 => String::new(),
            _ => format!("{} ", s(r, "priority")),
        };
        out.push(format!(
            "    {flag}{} {} {} {} {}",
            s(r, "workflow"),
            r.pointer("/step/spec_slug")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            id8(s(r, "job_id")),
            clip(s(r, "job_title")),
            with_run(&format!("since {}", s(r, "opened_on")), r),
        ));
    }
    if rows.len() > NEEDS_YOU_ROWS {
        out.push(format!(
            "    … {} more — MY WORK below lists every one",
            rows.len() - NEEDS_YOU_ROWS
        ));
    }
    out
}

/// Whom a `for=me` answer was read for, from its own `for` block: the
/// ids, then the roles.
pub(crate) fn for_whom(body: &Value) -> String {
    let list = |key: &str| -> Vec<String> {
        body.pointer(&format!("/for/{key}"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    };
    let ids = list("ids");
    let roles = list("roles");
    let who = match ids.as_slice() {
        [] => "nobody".to_string(),
        [one] => one.clone(),
        [first, rest @ ..] => format!("{first} (+ {})", rest.join(", ")),
    };
    match roles.as_slice() {
        [] => who,
        roles => format!("{who} or a role they hold ({})", roles.join(", ")),
    }
}

/// The ids a `for=me` answer was read for — the identities MY WORK's
/// overdue alarms are matched against.
pub(crate) fn for_ids(body: &Value) -> Vec<String> {
    body.pointer("/for/ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// A `for=me` answer, or why it is not one. A server older than the
/// read ignores the unknown `for` and answers the no-selector empty
/// queue — a well-formed `total: 0` — so an answer without its `for`
/// block is refused as unread rather than printed as "nothing".
pub(crate) fn for_me_answer(read: anyhow::Result<Option<Value>>) -> Result<Value, String> {
    match read {
        Err(e) => Err(format!("GET /api/jobs/assignments?for=me: {e}")),
        Ok(None) => Err("GET /api/jobs/assignments?for=me answered nothing".to_string()),
        Ok(Some(body)) if body.get("for").is_some_and(Value::is_object) => Ok(body),
        Ok(Some(_)) => Err(
            "GET /api/jobs/assignments?for=me answered without its `for` block — a server \
             older than design ea906603, whose empty answer would be a guess"
                .to_string(),
        ),
    }
}

/// NEXT UP — the IT department's upcoming events, as the server folded
/// them: timed rows soonest first (UTC, and how long until), then the
/// rows with no time and why, then the sources it could not read.
pub(crate) fn next_up_lines(map: &Value, fallback_now: DateTime<Utc>) -> Vec<String> {
    let rows = match map.get("next_up") {
        Some(Value::Array(rows)) => rows,
        _ => {
            return vec![
                "  NEXT UP — unread: the regions read carried no `next_up` (a server older \
                 than design ea906603)"
                    .to_string(),
            ];
        }
    };
    if rows.is_empty() {
        return vec!["  NEXT UP — nothing scheduled or expected".to_string()];
    }
    let now = payload_now(map, fallback_now);
    let timed = rows
        .iter()
        .filter(|r| r.get("at").is_some_and(Value::is_string))
        .count();
    let mut out = vec![format!(
        "  NEXT UP — {timed} timed event(s), UTC, soonest first"
    )];
    for r in rows {
        let unread = r.get("unread").and_then(Value::as_str);
        let at = r
            .get("at")
            .and_then(Value::as_str)
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc));
        let tilde = if r.get("estimate").and_then(Value::as_bool) == Some(true) {
            "~"
        } else {
            " "
        };
        let when = match (unread, at) {
            (Some(_), _) => "      ?          ".to_string(),
            (None, None) => "      --:--      ".to_string(),
            (None, Some(t)) => {
                let clock = if t.date_naive() == now.date_naive() {
                    t.format("%H:%MZ").to_string()
                } else {
                    t.format("%a %H:%MZ").to_string()
                };
                format!(
                    "{tilde}{clock:>10} {:>6}",
                    format!("in {}", span((t - now).num_minutes()))
                )
            }
        };
        let tail = match unread {
            Some(why) => format!("UNREAD: {why}"),
            None => s(r, "basis").to_string(),
        };
        out.push(format!(
            "    {when}  {:<12} {} — {tail}",
            s(r, "kind"),
            s(r, "title"),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-27T17:08:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn spans_read_as_the_board_says_them() {
        assert_eq!(span(-3), "now");
        assert_eq!(span(22), "22m");
        assert_eq!(span(120), "2h");
        assert_eq!(span(412), "6h52m");
        assert_eq!(span(5 * 24 * 60 + 7), "5d");
    }

    #[test]
    fn outranks_prints_each_packet_oldest_first_and_says_none_in_words() {
        let map = json!({"outranks": [
            {"id": "4a56c797-aaaa", "kind": "backlog-item", "title": "Conductor merges on a red gate",
             "priority": "urgent", "age_minutes": 3 * 24 * 60,
             "at": {"slug": "triage", "title": "Measure the claim", "status": "ready", "assignee_id": null},
             "stations": ["backlog"]},
            {"id": "e1e2e3e4-bbbb", "kind": "incident", "title": "SoR dark", "priority": "emergency",
             "age_minutes": 12, "at": null, "stations": null}
        ]});
        let lines = outranks_lines(&map);
        assert_eq!(
            lines[0],
            "  OUTRANKS REGULAR ORDER — 2 (1 emergency, 1 urgent), oldest first"
        );
        assert_eq!(
            lines[1],
            "    urgent backlog-item 4a56c797 Conductor merges on a red gate — 3d, at triage (ready, unassigned)"
        );
        assert_eq!(
            lines[2],
            "    emergency incident e1e2e3e4 SoR dark — 12m, no workable step"
        );
        assert_eq!(
            outranks_lines(&json!({"outranks": []})),
            vec!["  OUTRANKS REGULAR ORDER — nothing outranks regular order"]
        );
        // An older server, or a failed read: unread, never "nothing".
        for map in [json!({}), json!({"outranks": null})] {
            let l = outranks_lines(&map);
            assert!(l[0].contains("unread"), "{l:?}");
        }
    }

    /// A CLAIMED STEP SAYS WHETHER ANYONE IS ON IT (backlog 8e9d3723):
    /// `at build (active, agent-claude)` was printed alike for an item a
    /// builder was on and for five whose claims had outlived their
    /// sessions, so every drain session read them as in hand. Each state
    /// the server sends reads differently, the two stalled ones say
    /// STALLED, and the header counts them; a server that sends no `run`
    /// prints the line as before.
    #[test]
    fn a_claimed_step_names_its_run_or_says_it_is_stalled() {
        let row = |id: &str, run: Value| {
            json!({"id": id, "kind": "backlog-item", "title": "An item", "priority": "urgent",
                   "age_minutes": 60,
                   "at": {"slug": "build", "title": "Build", "status": "active",
                          "assignee_id": "agent-claude", "run": run}, "stations": []})
        };
        let mut older = row("55555555-eeee", Value::Null);
        older["at"].as_object_mut().unwrap().remove("run");
        let map = json!({"outranks": [
            row("11111111-aaaa", json!({"state": "building", "run": "086d9793-92d8-402f"})),
            row("22222222-bbbb", json!({"state": "never-started", "run": "f58a081d-4ce8", "age_minutes": 37 * 60})),
            row("33333333-cccc", json!({"state": "no-run"})),
            row("44444444-dddd", json!({"state": "unread"})),
            older,
            // Inside its worker's start bound (backlog 57326714): not
            // stalled, and not counted as such.
            row("66666666-ffff", json!({"state": "starting", "run": "0ccf6dab-4bd0", "age_minutes": 5, "expect_minutes": 15})),
            row("77777777-abab", json!({"state": "starting", "run": "1a2b3c4d-5e6f", "age_minutes": 0, "expect_minutes": 15})),
        ]});
        let lines = outranks_lines(&map);
        assert_eq!(
            lines[0],
            "  OUTRANKS REGULAR ORDER — 7 (7 urgent), oldest first — 2 STALLED: claimed, nobody on it"
        );
        let tail = |l: &str| l.split(" — ").nth(1).unwrap_or_default().to_string();
        assert_eq!(
            tail(&lines[1]),
            "1h, at build (active, agent-claude, run 086d9793)"
        );
        assert_eq!(
            tail(&lines[2]),
            "1h, at build (active, agent-claude) STALLED: run f58a081d briefed 1d ago, never started"
        );
        assert_eq!(
            tail(&lines[3]),
            "1h, at build (active, agent-claude) STALLED: claimed, no open run"
        );
        assert_eq!(
            tail(&lines[4]),
            "1h, at build (active, agent-claude, run unread)"
        );
        assert_eq!(tail(&lines[5]), "1h, at build (active, agent-claude)");
        assert_eq!(
            tail(&lines[6]),
            "1h, at build (active, agent-claude, run 0ccf6dab starting: briefed 5m ago, 15m allowed)"
        );
        assert_eq!(
            tail(&lines[7]),
            "1h, at build (active, agent-claude, run 1a2b3c4d starting: briefed just now, 15m allowed)"
        );
    }

    /// A READY STEP NOTHING CAN DISPATCH SAYS SO (backlog e2519eca):
    /// ff309ce6 stood at `prove-delivery (ready, agent-claude)`, worded
    /// exactly like a step a dispatch would pick up, while `boss
    /// dispatch` refused it for want of an agent block. The line names
    /// the row to fix, the header counts it beside the stalled claims,
    /// an unread judgement says unread, and a server that sends no
    /// `dispatch` prints the line as before.
    #[test]
    fn a_ready_step_nothing_can_dispatch_is_named_undispatchable() {
        let row = |id: &str, slug: &str, status: &str, dispatch: Value| {
            json!({"id": id, "kind": "backlog-item", "title": "An item", "priority": "urgent",
                   "age_minutes": 60,
                   "at": {"slug": slug, "title": "Step", "status": status,
                          "assignee_id": "agent-claude", "run": null, "dispatch": dispatch},
                   "stations": []})
        };
        let mut claimed = row("44444444-dddd", "build", "active", Value::Null);
        claimed["at"]["run"] = json!({"state": "no-run"});
        let map = json!({"outranks": [
            row("11111111-aaaa", "prove-delivery", "ready",
                json!({"state": "undispatchable", "kind": "backlog-item", "version": 15})),
            row("22222222-bbbb", "triage", "ready", json!({"state": "unread"})),
            row("33333333-cccc", "build", "ready", Value::Null),
            claimed,
        ]});
        let lines = outranks_lines(&map);
        assert_eq!(
            lines[0],
            "  OUTRANKS REGULAR ORDER — 4 (4 urgent), oldest first — 1 STALLED: claimed, \
             nobody on it — 1 UNDISPATCHABLE: ready, nothing can dispatch it"
        );
        let tail = |l: &str| l.split(" — ").nth(1).unwrap_or_default().to_string();
        assert_eq!(
            tail(&lines[1]),
            "1h, at prove-delivery (ready, agent-claude) UNDISPATCHABLE: backlog-item v15 \
             declares no agent block on prove-delivery and no person holds it"
        );
        assert_eq!(
            tail(&lines[2]),
            "1h, at triage (ready, agent-claude, dispatch unread)"
        );
        assert_eq!(tail(&lines[3]), "1h, at build (ready, agent-claude)");
    }

    fn for_me(rows: Value) -> Value {
        json!({
            "data": rows,
            "total": 0,
            "for": {"ids": ["claude@algedonic.dev", "agent-claude"], "roles": ["platform-admin"]}
        })
    }

    fn step_row(prio: &str, workflow: &str, slug: &str, title: &str, opened: &str) -> Value {
        json!({"job_id": "74569e94-9c5f", "job_title": title, "workflow": workflow,
               "priority": prio, "opened_on": opened,
               "step": {"id": format!("{slug}-{opened}"), "spec_slug": slug}})
    }

    #[test]
    fn needs_you_leads_with_what_outranks_and_names_whom_it_read_for() {
        let rows: Vec<Value> = (0..6)
            .map(|i| {
                step_row(
                    "standard",
                    "backlog-item",
                    "triage",
                    &format!("item {i}"),
                    &format!("2026-09-2{i}"),
                )
            })
            .chain([step_row(
                "urgent",
                "ship-a-change",
                "sign-off",
                "merge-tenant-main",
                "2026-09-26",
            )])
            .collect();
        let lines = needs_you_lines(&Ok(for_me(json!(rows))));
        assert_eq!(
            lines[0],
            "  NEEDS YOU — 7 ready/active step(s) for claude@algedonic.dev (+ agent-claude) \
             or a role they hold (platform-admin), 1 outranking regular order"
        );
        assert_eq!(
            lines[1],
            "    urgent ship-a-change sign-off 74569e94 merge-tenant-main (since 2026-09-26)"
        );
        assert_eq!(
            lines[2],
            "    backlog-item triage 74569e94 item 0 (since 2026-09-20)"
        );
        assert_eq!(lines.len(), 1 + NEEDS_YOU_ROWS + 1, "{lines:#?}");
        assert!(lines.last().unwrap().contains("2 more"), "{lines:#?}");

        let empty = needs_you_lines(&Ok(for_me(json!([]))));
        assert!(empty[0].starts_with("  NEEDS YOU — nothing"), "{empty:?}");
        let unread = needs_you_lines(&Err("HTTP 503".into()));
        assert_eq!(unread, vec!["  NEEDS YOU — unread: HTTP 503"]);
    }

    /// NEEDS YOU SAYS WHETHER ANYONE IS ON A CLAIM (backlog e2519eca item
    /// 2), in OUTRANKS' own words: the server's `run` on a row prints as
    /// the board prints it, a stalled claim leads its rank and is counted
    /// in the header, an unread run says so, and a row the server sent no
    /// run for (a ready step, or an older server) prints as it always did.
    #[test]
    fn needs_you_names_a_claims_run_and_leads_with_the_stalled_ones() {
        let with = |mut row: Value, run: Value| {
            row["run"] = run;
            row
        };
        let rows = vec![
            with(
                step_row("urgent", "backlog-item", "build", "worked", "2026-10-01"),
                json!({"state": "building", "run": "086d9793-92d8-402f"}),
            ),
            step_row("urgent", "backlog-item", "triage", "waiting", "2026-10-02"),
            with(
                step_row("urgent", "backlog-item", "build", "gone", "2026-10-03"),
                json!({"state": "no-run"}),
            ),
            with(
                step_row("standard", "backlog-item", "build", "dark", "2026-10-04"),
                json!({"state": "unread"}),
            ),
        ];
        let lines = needs_you_lines(&Ok(for_me(json!(rows))));
        assert_eq!(
            lines,
            vec![
                "  NEEDS YOU — 4 ready/active step(s) for claude@algedonic.dev (+ agent-claude) \
                 or a role they hold (platform-admin), 3 outranking regular order — 1 STALLED: \
                 claimed, nobody on it"
                    .to_string(),
                "    urgent backlog-item build 74569e94 gone (since 2026-10-03) STALLED: claimed, \
                 no open run"
                    .to_string(),
                "    urgent backlog-item build 74569e94 worked (since 2026-10-01, run 086d9793)"
                    .to_string(),
                "    urgent backlog-item triage 74569e94 waiting (since 2026-10-02)".to_string(),
                "    backlog-item build 74569e94 dark (since 2026-10-04, run unread)".to_string(),
            ]
        );
    }

    /// An older server ignores `for=me` and answers the empty no-selector
    /// queue; without the `for` block that answer is refused as unread.
    #[test]
    fn an_answer_without_its_for_block_is_not_an_empty_queue() {
        let old = for_me_answer(Ok(Some(json!({"data": [], "total": 0}))));
        assert!(old.unwrap_err().contains("older than design ea906603"));
        assert!(for_me_answer(Ok(None)).is_err());
        let ok = for_me_answer(Ok(Some(for_me(json!([]))))).unwrap();
        assert_eq!(for_ids(&ok), vec!["claude@algedonic.dev", "agent-claude"]);
    }

    #[test]
    fn next_up_prints_times_countdowns_and_what_it_could_not_read() {
        let map = json!({
            "now": "2026-09-27T17:08:00Z",
            "next_up": [
                {"kind": "train-board", "title": "next train board, 3 cars waiting",
                 "at": "2026-09-27T17:30:00Z", "estimate": true,
                 "basis": "the depth rule's 30-minute cooldown clears", "source": "cadence", "unread": null},
                {"kind": "scheduled", "title": "publish-to-github", "at": "2026-09-28T00:00:00Z",
                 "estimate": false, "basis": "the dispatcher's schedule (daily)", "source": "d", "unread": null},
                {"kind": "in-transit", "title": "PR train — deploying, expected", "at": null,
                 "estimate": false, "basis": "too little history", "source": "t", "unread": null},
                {"kind": "scheduled", "title": "scheduled rules", "at": null, "estimate": false,
                 "basis": "", "source": "d", "unread": "connection refused"}
            ]
        });
        let lines = next_up_lines(&map, now());
        assert_eq!(lines[0], "  NEXT UP — 2 timed event(s), UTC, soonest first");
        assert_eq!(
            lines[1],
            "    ~    17:30Z in 22m  train-board  next train board, 3 cars waiting — the depth rule's 30-minute cooldown clears"
        );
        assert_eq!(
            lines[2],
            "     Mon 00:00Z in 6h52m  scheduled    publish-to-github — the dispatcher's schedule (daily)"
        );
        assert_eq!(
            lines[3],
            "          --:--        in-transit   PR train — deploying, expected — too little history"
        );
        assert_eq!(
            lines[4],
            "          ?            scheduled    scheduled rules — UNREAD: connection refused"
        );
        assert_eq!(
            next_up_lines(&json!({"next_up": []}), now()),
            vec!["  NEXT UP — nothing scheduled or expected"]
        );
        assert!(next_up_lines(&json!({}), now())[0].contains("unread"));
    }
}
