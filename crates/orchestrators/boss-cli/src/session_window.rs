//! `boss session window` — a session's working window, as packet data
//! (backlog 37e3295c).
//!
//! WHY. How long a factory session works was a chat line. On
//! 2026-10-18 15:50Z the builder session was told "we are going to try
//! and run for 6 hours"; it wrote "Six-hour window closed at 21:45Z"
//! and idled until 23:05Z. On 2026-10-19 04:34Z it was told "keep the
//! factory churning for the next 8 hours", stopped dispatching at
//! 12:14Z, and went idle at 12:52Z with four urgent build nominations
//! ready on its id — while the conductor, told the same in its own
//! chat, had ended its loop at 12:02Z. Nothing in the record said
//! either window existed, so nothing could read that a session was
//! about to stop, and no one could extend it but by typing into the
//! right transcript.
//!
//! WHAT. The work-session packet's `metadata.window` (declared in
//! `infra/platform/workflows/work-session.toml`) is one of three:
//!
//! - absent — no window was set; it reads OPEN-ENDED. A number of hours
//!   said in chat does not end a session on its own.
//! - `{state: open}` — set open-ended, by someone, on the record.
//! - `{state: until, until}` — the session works until that instant.
//! - `{state: closed, closed_at, handoff}` — the session wound down,
//!   and left its handoff on the packet for whoever reads next.
//!
//! THE ONE WRITER is this verb, through the metadata merge door (read
//! back, the way `boss job patch` confirms): an extension is a write to
//! the packet, signed by whoever runs it. The merge door records each
//! write as a `jobs.job.updated` event carrying the post-merge row, so
//! a close and its handoff are in the audit log.
//!
//! THE READERS. The UserPromptSubmit hook (`infra/dev/hooks/
//! prompt-submit.sh`) prints [`window_line`] onto the session's own
//! context on every prompt, so the session takes its end from the
//! packet rather than from what it remembers being told; and
//! `boss orient` prints [`sessions_lines`] — every open session with
//! its window — so the conductor reads the other session's window from
//! the record rather than from that session's chat.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

/// The protocol whose packets carry a window.
pub(crate) const KIND: &str = "work-session";

/// The one metadata key the window lives under.
pub(crate) const WINDOW_KEY: &str = "window";

/// The open work-sessions, paged on `total` by the caller.
pub(crate) fn open_sessions_path(offset: usize) -> String {
    format!(
        "/api/jobs?kind={KIND}&status=open&limit={}&offset={offset}",
        crate::train::PAGE_LIMIT
    )
}

/// What a write asks for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Set {
    Until(DateTime<Utc>),
    Open,
    Close { handoff: String },
}

/// The flags, judged into at most one [`Set`]. `None` is a read.
/// Pure: `now` is handed in, so `--hours` is pinned without a clock.
pub(crate) fn parse_set(
    until: Option<&str>,
    hours: Option<f64>,
    open: bool,
    handoff: Option<String>,
    now: DateTime<Utc>,
) -> Result<Option<Set>> {
    let asked = [until.is_some(), hours.is_some(), open, handoff.is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    if asked > 1 {
        bail!("give ONE of --until, --hours, --open or --close: a window is one of them");
    }
    if let Some(text) = until {
        let at = DateTime::parse_from_rfc3339(text.trim())
            .with_context(|| format!("--until {text:?} is not an RFC3339 instant"))?
            .with_timezone(&Utc);
        if at <= now {
            bail!(
                "--until {} is not in the future (now {}): close the window instead, with a handoff",
                crate::sor_client::stamp(at),
                crate::sor_client::stamp(now)
            );
        }
        return Ok(Some(Set::Until(at)));
    }
    if let Some(h) = hours {
        if !h.is_finite() || h <= 0.0 {
            bail!("--hours {h} is not a positive number of hours");
        }
        let at = now + chrono::Duration::seconds((h * 3600.0).round() as i64);
        return Ok(Some(Set::Until(at)));
    }
    if open {
        return Ok(Some(Set::Open));
    }
    if let Some(text) = handoff {
        if text.trim().is_empty() {
            bail!("the handoff file is empty: a close says what it leaves for the next reader");
        }
        return Ok(Some(Set::Close {
            handoff: text.trim_end().to_string(),
        }));
    }
    Ok(None)
}

/// The window a write leaves on the packet. A close keeps what the
/// window it closes said (its `until`, who set it and why) and adds
/// when, by whom, and the handoff; a close of a window already closed
/// is refused, so a handoff is never overwritten in silence.
pub(crate) fn window_value(
    set: &Set,
    prior: Option<&Value>,
    actor: &str,
    why: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Value> {
    let at = crate::sor_client::stamp(now);
    let with_why = |mut v: Value| {
        if let Some(w) = why.map(str::trim).filter(|w| !w.is_empty()) {
            v["why"] = json!(w);
        }
        v
    };
    Ok(match set {
        Set::Until(until) => with_why(json!({
            "state": "until",
            "until": crate::sor_client::stamp(*until),
            "set_by": actor,
            "set_at": at,
        })),
        Set::Open => with_why(json!({"state": "open", "set_by": actor, "set_at": at})),
        Set::Close { handoff } => {
            let mut v = match prior {
                Some(p) if state(p) == Some("closed") => bail!(
                    "this window was already closed at {} by {} — set a new one before closing again",
                    text(p, "closed_at").unwrap_or("?"),
                    text(p, "closed_by").unwrap_or("?")
                ),
                Some(p @ Value::Object(_)) => p.clone(),
                _ => json!({}),
            };
            v["state"] = json!("closed");
            v["closed_at"] = json!(at);
            v["closed_by"] = json!(actor);
            v["handoff"] = json!(handoff);
            with_why(v)
        }
    })
}

fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn state(window: &Value) -> Option<&str> {
    text(window, "state")
}

/// `42m`, `3h05m`.
fn span(seconds: i64) -> String {
    let m = seconds.max(0) / 60;
    if m < 60 {
        format!("{m}m")
    } else {
        format!("{}h{:02}m", m / 60, m % 60)
    }
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// " — set by X at T (why)", for the lines below.
fn provenance(w: &Value) -> String {
    let mut out = String::new();
    if let Some(by) = text(w, "set_by") {
        out.push_str(&format!(" — set by {by}"));
        if let Some(at) = text(w, "set_at") {
            out.push_str(&format!(" at {at}"));
        }
    }
    if let Some(why) = text(w, "why") {
        out.push_str(&format!(" ({why})"));
    }
    out
}

/// The window, as one line, read off a work-session's metadata. What
/// the prompt hook prints onto the session's context, so every word of
/// it is the record's: an end the session has to remember being told is
/// the defect this line replaces.
pub(crate) fn window_line(id: &str, md: &Value, now: DateTime<Utc>) -> String {
    let id8 = short(id);
    let head = format!("work-session {id8} window:");
    let Some(w) = md.get(WINDOW_KEY).filter(|w| !w.is_null()) else {
        return format!(
            "{head} none set — OPEN-ENDED. A number of hours said in chat does not end this \
             session; a window is set on the packet with boss session window {id8}"
        );
    };
    match state(w) {
        Some("open") => format!("{head} OPEN-ENDED{}", provenance(w)),
        Some("until") => {
            let until = text(w, "until")
                .and_then(|u| DateTime::parse_from_rfc3339(u).ok())
                .map(|u| u.with_timezone(&Utc));
            match until {
                Some(u) if u > now => format!(
                    "{head} until {} (in {}){}",
                    crate::sor_client::stamp(u),
                    span((u - now).num_seconds()),
                    provenance(w)
                ),
                Some(u) => format!(
                    "{head} ENDED at {} ({} ago), not yet closed — close it with a handoff: \
                     boss session window {id8} --close --handoff-file <file>{}",
                    crate::sor_client::stamp(u),
                    span((now - u).num_seconds()),
                    provenance(w)
                ),
                None => format!(
                    "{head} UNREADABLE — state until with no RFC3339 until ({w}); read as \
                     open-ended until someone sets it again"
                ),
            }
        }
        Some("closed") => format!(
            "{head} CLOSED at {} by {} — the handoff is on the packet",
            text(w, "closed_at").unwrap_or("?"),
            text(w, "closed_by").unwrap_or("?")
        ),
        _ => format!(
            "{head} UNREADABLE — no known state ({w}); read as open-ended until someone sets it \
             again"
        ),
    }
}

/// The open work-session of `actor`: the newest by heartbeat among the
/// rows naming them as the session's `actor` or as who opened it — the
/// hook writes the identity file's id as `actor`, the door stamps the
/// signing actor as `opened_by`, and an alias makes those differ
/// (eaf1cfac: `claude-b@algedonic.dev` and `agent-claude-b`).
pub(crate) fn pick_session<'a>(rows: &'a [Value], actor: &str) -> Option<&'a Value> {
    rows.iter()
        .filter(|r| {
            r.get("kind")
                .and_then(Value::as_str)
                .is_none_or(|k| k == KIND)
        })
        .filter(|r| {
            let md = r.get("metadata").unwrap_or(&Value::Null);
            text(md, "actor") == Some(actor) || text(md, "opened_by") == Some(actor)
        })
        .max_by_key(|r| {
            let md = r.get("metadata").unwrap_or(&Value::Null);
            text(md, "last_active_at")
                .or_else(|| text(md, "started_at"))
                .unwrap_or("")
                .to_string()
        })
}

/// SESSIONS — every open work-session and its window, for `boss
/// orient`. A failed read says so; a vanished section would read
/// exactly like no session at all.
pub(crate) fn sessions_lines(rows: &Result<Vec<Value>, String>, now: DateTime<Utc>) -> Vec<String> {
    let rows = match rows {
        Ok(r) => r,
        Err(e) => return vec![format!("  SESSIONS — unread: {e}")],
    };
    let mut out = vec![format!(
        "  SESSIONS — {} open work-session(s), each with the window its packet carries \
         (set with boss session window)",
        rows.len()
    )];
    for r in rows {
        let id = r.get("id").and_then(Value::as_str).unwrap_or("?");
        let md = r.get("metadata").unwrap_or(&Value::Null);
        let who = text(md, "actor")
            .or_else(|| text(md, "opened_by"))
            .unwrap_or("?");
        let heard = text(md, "last_active_at")
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| {
                format!(
                    "last prompt {} ago",
                    span((now - t.with_timezone(&Utc)).num_seconds())
                )
            })
            .unwrap_or_else(|| "no prompt yet".to_string());
        out.push(format!("    {who}  {heard}"));
        out.push(format!("      {}", window_line(id, md, now)));
    }
    out
}

/// Read every open work-session, paged on `total`.
pub(crate) async fn open_sessions(http: &crate::machine_http::MachineClient) -> Result<Vec<Value>> {
    crate::train::list_all_pages(|offset| async move {
        crate::sor_client::api(
            http,
            reqwest::Method::GET,
            &open_sessions_path(offset),
            None,
        )
        .await
    })
    .await
}

/// The section `boss orient` prints.
pub(crate) async fn section(http: &crate::machine_http::MachineClient) -> Vec<String> {
    let rows = open_sessions(http).await.map_err(|e| e.to_string());
    sessions_lines(&rows, boss_clock_client::wall_now())
}

/// What the verb was asked.
pub(crate) struct Args {
    pub packet: Option<String>,
    pub actor: Option<String>,
    pub until: Option<String>,
    pub hours: Option<f64>,
    pub open: bool,
    pub handoff_file: Option<std::path::PathBuf>,
    pub why: Option<String>,
}

/// One packet, whole.
async fn read(http: &crate::machine_http::MachineClient, id: &str) -> Result<Value> {
    crate::sor_client::api(http, reqwest::Method::GET, &format!("/api/jobs/{id}"), None)
        .await?
        .context("the packet read answered no body")
}

/// `boss session window`: read, or set, a work-session's window.
pub async fn run(args: Args) -> Result<()> {
    let now = boss_clock_client::wall_now();
    let handoff = args
        .handoff_file
        .as_deref()
        .map(|p| std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display())))
        .transpose()?;
    let set = parse_set(args.until.as_deref(), args.hours, args.open, handoff, now)?;
    let http = crate::sor_client::machine_client()?;

    let id = match (&args.packet, &args.actor) {
        (Some(p), _) => crate::job::fetch_and_resolve(&http, p).await?,
        (None, given) => {
            let actor = match given {
                Some(a) => a.clone(),
                None => crate::identity::caller()
                    .map(|c| c.id)
                    .context("name the packet or --actor: nothing names the actor running this")?,
            };
            let rows = open_sessions(&http).await?;
            pick_session(&rows, &actor)
                .and_then(|r| r.get("id").and_then(Value::as_str))
                .map(str::to_string)
                .with_context(|| {
                    format!(
                        "no open {KIND} names {actor} as its actor or opener ({} open)",
                        rows.len()
                    )
                })?
        }
    };
    let job = read(&http, &id).await?;
    let kind = job.get("kind").and_then(Value::as_str).unwrap_or("?");
    if kind != KIND {
        bail!("{id} is a {kind}, not a {KIND}: only a session carries a working window");
    }
    let md = job.get("metadata").cloned().unwrap_or_else(|| json!({}));

    let Some(set) = set else {
        println!("{}", window_line(&id, &md, now));
        return Ok(());
    };
    let actor = crate::identity::caller()
        .map(|c| c.id)
        .context("a window write is signed by the actor running it: set BOSS_ACTOR")?;
    let value = window_value(&set, md.get(WINDOW_KEY), &actor, args.why.as_deref(), now)?;
    let sent = json!({ WINDOW_KEY: value });
    crate::sor_client::api(
        &http,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{id}/metadata"),
        Some(sent.clone()),
    )
    .await?;
    // The status code said yes; the packet is the authority.
    let after = read(&http, &id).await?;
    let now_md = after.get("metadata").cloned().unwrap_or_else(|| json!({}));
    let (_, took) = crate::job::confirm_patch(&now_md, &sent);
    if !took {
        bail!(
            "the merge door answered but {id} does not hold the window sent — nothing to trust: {}",
            now_md.get(WINDOW_KEY).unwrap_or(&Value::Null)
        );
    }
    println!("{}", window_line(&id, &now_md, now));
    println!("boss session window: {id} written by {actor} — confirmed by reading it back");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    const ID: &str = "eaf1cfac-80ee-46c5-9b7a-f52bc1947421";

    /// The packet 37e3295c measured: no window key at all. It reads
    /// OPEN-ENDED and says outright that a chat line does not end it —
    /// the session that read "8 hours" in chat had nothing on its
    /// packet to say otherwise.
    #[test]
    fn an_absent_window_reads_open_ended_and_says_chat_does_not_end_it() {
        let md = json!({"actor": "claude-b@algedonic.dev", "prompt_count": 493});
        let line = window_line(ID, &md, at("2026-10-19T12:14:00Z"));
        assert!(
            line.starts_with("work-session eaf1cfac window: none set — OPEN-ENDED"),
            "{line}"
        );
        assert!(line.contains("does not end this session"), "{line}");
    }

    #[test]
    fn an_until_window_reads_its_end_and_who_set_it() {
        let w = window_value(
            &Set::Until(at("2026-10-19T20:00:00Z")),
            None,
            "agent-claude",
            Some("David: keep the factory churning"),
            at("2026-10-19T12:00:00Z"),
        )
        .unwrap();
        let md = json!({ "window": w });
        let line = window_line(ID, &md, at("2026-10-19T12:14:00Z"));
        assert_eq!(
            line,
            "work-session eaf1cfac window: until 2026-10-19T20:00:00Z (in 7h46m) — set by \
             agent-claude at 2026-10-19T12:00:00Z (David: keep the factory churning)"
        );
    }

    /// Past its `until` and not closed: the line says ENDED and names
    /// the close, so the end lands on the record rather than in a chat.
    #[test]
    fn a_passed_window_says_ended_and_names_the_close() {
        let md = json!({"window": {"state": "until", "until": "2026-10-19T12:35:00Z",
                                   "set_by": "agent-claude", "set_at": "2026-10-19T04:35:00Z"}});
        let line = window_line(ID, &md, at("2026-10-19T12:52:00Z"));
        assert!(
            line.contains("ENDED at 2026-10-19T12:35:00Z (17m ago), not yet closed"),
            "{line}"
        );
        assert!(
            line.contains("boss session window eaf1cfac --close --handoff-file"),
            "{line}"
        );
    }

    #[test]
    fn an_open_window_reads_open_ended_with_its_provenance() {
        let w = window_value(
            &Set::Open,
            None,
            "emp-david",
            None,
            at("2026-10-19T23:05:00Z"),
        )
        .unwrap();
        assert_eq!(
            w,
            json!({"state": "open", "set_by": "emp-david", "set_at": "2026-10-19T23:05:00Z"})
        );
        let line = window_line(ID, &json!({ "window": w }), at("2026-10-19T23:06:00Z"));
        assert_eq!(
            line,
            "work-session eaf1cfac window: OPEN-ENDED — set by emp-david at 2026-10-19T23:05:00Z"
        );
    }

    /// A close keeps what it closed, adds when/who/handoff, and a
    /// second close is refused rather than overwriting the handoff.
    #[test]
    fn a_close_keeps_the_window_it_closes_and_carries_the_handoff() {
        let prior =
            json!({"state": "until", "until": "2026-10-19T12:35:00Z", "set_by": "agent-claude"});
        let closed = window_value(
            &Set::Close {
                handoff: "f17627b6 resumed; 870dbfe4 ready".into(),
            },
            Some(&prior),
            "agent-claude-b",
            None,
            at("2026-10-19T12:40:00Z"),
        )
        .unwrap();
        assert_eq!(closed["state"], "closed");
        assert_eq!(closed["until"], "2026-10-19T12:35:00Z");
        assert_eq!(closed["set_by"], "agent-claude");
        assert_eq!(closed["closed_by"], "agent-claude-b");
        assert_eq!(closed["closed_at"], "2026-10-19T12:40:00Z");
        assert_eq!(closed["handoff"], "f17627b6 resumed; 870dbfe4 ready");
        let line = window_line(
            ID,
            &json!({ "window": closed.clone() }),
            at("2026-10-19T13:00:00Z"),
        );
        assert_eq!(
            line,
            "work-session eaf1cfac window: CLOSED at 2026-10-19T12:40:00Z by agent-claude-b — \
             the handoff is on the packet"
        );
        let again = window_value(
            &Set::Close {
                handoff: "x".into(),
            },
            Some(&closed),
            "agent-claude-b",
            None,
            at("2026-10-19T13:00:00Z"),
        );
        assert!(again.unwrap_err().to_string().contains("already closed"));
    }

    #[test]
    fn the_flags_choose_one_window_and_refuse_a_past_or_empty_one() {
        let now = at("2026-10-19T04:34:00Z");
        assert_eq!(
            parse_set(None, None, false, None, now).unwrap(),
            None,
            "no flag is a read"
        );
        assert_eq!(
            parse_set(None, Some(8.0), false, None, now).unwrap(),
            Some(Set::Until(at("2026-10-19T12:34:00Z"))),
            "--hours is computed from the clock at write time, then stored as an instant"
        );
        assert_eq!(
            parse_set(Some("2026-10-19T20:00:00-07:00"), None, false, None, now).unwrap(),
            Some(Set::Until(at("2026-10-20T03:00:00Z")))
        );
        assert_eq!(
            parse_set(None, None, true, None, now).unwrap(),
            Some(Set::Open)
        );
        assert!(parse_set(Some("2026-10-19T04:00:00Z"), None, false, None, now).is_err());
        assert!(parse_set(None, Some(0.0), false, None, now).is_err());
        assert!(parse_set(None, None, false, Some("  \n".into()), now).is_err());
        assert!(
            parse_set(None, Some(2.0), true, None, now).is_err(),
            "two windows at once"
        );
    }

    /// The conductor reads the other session's window here: the actor
    /// is matched as the hook's `actor` or the door's `opened_by`, and
    /// the newest heartbeat wins over a stale twin.
    #[test]
    fn the_actors_session_is_found_by_either_name_and_the_newest_heartbeat() {
        let rows = vec![
            json!({"id": "old", "kind": KIND, "metadata": {"actor": "claude-b@algedonic.dev",
                   "opened_by": "agent-claude-b", "last_active_at": "2026-10-18T09:00:00Z"}}),
            json!({"id": "new", "kind": KIND, "metadata": {"actor": "claude-b@algedonic.dev",
                   "opened_by": "agent-claude-b", "last_active_at": "2026-10-19T15:18:20Z"}}),
            json!({"id": "a", "kind": KIND, "metadata": {"actor": "agent-claude",
                   "last_active_at": "2026-10-19T15:20:00Z"}}),
        ];
        assert_eq!(pick_session(&rows, "agent-claude-b").unwrap()["id"], "new");
        assert_eq!(
            pick_session(&rows, "claude-b@algedonic.dev").unwrap()["id"],
            "new"
        );
        assert_eq!(pick_session(&rows, "agent-claude").unwrap()["id"], "a");
        assert!(pick_session(&rows, "emp-david").is_none());
    }

    #[test]
    fn orient_lists_every_open_session_with_its_window_and_says_when_unread() {
        let rows = Ok(vec![json!({"id": ID, "kind": KIND, "metadata": {
            "actor": "claude-b@algedonic.dev", "last_active_at": "2026-10-19T15:10:00Z",
            "window": {"state": "open", "set_by": "agent-claude", "set_at": "2026-10-19T14:00:00Z"}}})]);
        let lines = sessions_lines(&rows, at("2026-10-19T15:20:00Z"));
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(
            lines[0].starts_with("  SESSIONS — 1 open work-session(s)"),
            "{lines:?}"
        );
        assert_eq!(lines[1], "    claude-b@algedonic.dev  last prompt 10m ago");
        assert!(
            lines[2].contains("window: OPEN-ENDED — set by agent-claude"),
            "{lines:?}"
        );
        let unread = sessions_lines(&Err("HTTP 503".into()), at("2026-10-19T15:20:00Z"));
        assert_eq!(unread, vec!["  SESSIONS — unread: HTTP 503".to_string()]);
    }
}
