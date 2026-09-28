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
    let mut out = vec![format!(
        "  OUTRANKS REGULAR ORDER — {} ({}), oldest first",
        rows.len(),
        tally.join(", ")
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
                match a.get("assignee_id").and_then(Value::as_str) {
                    Some(who) => format!("at {slug} ({}, {who})", s(a, "status")),
                    None => format!("at {slug} ({}, unassigned)", s(a, "status")),
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
    // Stable: the server's order (admission) survives within a rank.
    sorted.sort_by_key(|r| rank(r));
    let urgent = rows.iter().filter(|r| rank(r) < 2).count();
    let mut out = vec![format!(
        "  NEEDS YOU — {} ready/active step(s) for {who}{}",
        rows.len(),
        match urgent {
            0 => String::new(),
            n => format!(", {n} outranking regular order"),
        }
    )];
    for r in sorted.iter().take(NEEDS_YOU_ROWS) {
        let flag = match rank(r) {
            2 => String::new(),
            _ => format!("{} ", s(r, "priority")),
        };
        out.push(format!(
            "    {flag}{} {} {} {} (since {})",
            s(r, "workflow"),
            r.pointer("/step/spec_slug")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            id8(s(r, "job_id")),
            clip(s(r, "job_title")),
            s(r, "opened_on"),
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
