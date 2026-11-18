//! AN OPEN ALARM UPDATES ITS ITEM (backlog f4b0ad41; design 25511015
//! Q4, rule 3, decided by David 2026-11-17).
//!
//! An alarm handler that would file a new item while an open item for
//! the same alarm and the same subject exists APPENDS the occurrence to
//! that item — its count, when it was last seen, and the new evidence —
//! instead of filing. A closed item is history: the next occurrence
//! files a new one, because a recurrence after a fix is a new fact.
//!
//! WHY ONE HELPER. Every handler in this crate that files an alarm
//! already refused a second item while one was open, each in its own
//! words: a set of open keys and a `continue`, a map and a `match`, and
//! eight hand-written relapse branches (`relapse_patch` merged into a
//! refresh, or sent alone, or forgotten). The same judgement in twenty
//! spellings drifts, and it had: `maintenance.chore.file_reds` filed a
//! SECOND open item for a finding whose first item had been told it
//! recovered, and `dispatcher-rule-seed` keyed its alarm on the rule's
//! VERSION, so a rule that drifted at v3 and again at v4 held two open
//! items about one rule. And none of them said, on the item, that the
//! condition had come back: a relapse rubbed out `RECOVERED` and left
//! no trace that it had ever happened, so the reader of a three-week-old
//! alarm could not tell one long episode from five short ones.
//!
//! WHAT AN OCCURRENCE IS. A handler's subject is what the alarm is
//! about (a host, a route, a rule, a control); the open item for it is
//! found by the handler's own dedup key ([`open_item`]). An
//! [`Occurrence`] is NEW on that item when either
//!
//! - the item carries `recovered_at` — the machine told it the
//!   condition had cleared and the condition is back (a relapse); or
//! - the handler names an EPISODE and the item's [`LAST_EPISODE`] is
//!   another one: a rule drifted at a new version, a route red on a new
//!   night's chore.
//!
//! Otherwise the condition the item already reports is simply still
//! holding, and that is the SAME occurrence: nothing is appended, so a
//! five-minute watch over a day-long condition writes nothing per tick.
//! A handler with no episode of its own passes none and records only
//! relapses. A redelivered firing names the episode it named the first
//! time, so the append is idempotent (correctness protocol, property 4).
//!
//! WHAT IS STRICTER STAYS STRICTER. Four handlers dedup over EVERY
//! status, one alarm per subject ever (`jobs.agent_step_overdue`,
//! `jobs.flight_overdue`, `jobs.urgent_item_watch`,
//! `jobs.receipt_overdue`); three file per request, a subject that
//! fails once (`ops.judge` twice, `jobs.complete_linked_step`). Those
//! never reach a second filing for an open subject, and are named with
//! their reason in [`STRICTER`], which the crate pin below reads.
//!
//! BOUNDED. The appended evidence is clipped to [`EVIDENCE_BYTES`] and
//! the log keeps the newest [`LOG_KEEP`] entries, replaced whole on each
//! append (the metadata door merges top-level keys), so an alarm that
//! recurs for a year costs the packet a fixed few kilobytes — pinned
//! against `boss_jobs::packet_budget::BUDGET`, the line past which a
//! write that grows a packet is refused.

use boss_dispatcher::rules::handler::HandlerError;
use serde_json::{Map, Value, json};

use super::common::{post_json, post_json_minted_id, relapse_patch, told_recovered, write_json};

/// How many times the item's condition has occurred: 1 when filed.
pub const OCCURRENCES: &str = "occurrences";
/// When the newest occurrence was seen (the firing's instant).
pub const LAST_SEEN: &str = "last_seen";
/// The episode the newest occurrence named, for a handler that has one.
pub const LAST_EPISODE: &str = "last_episode";
/// The appended occurrences, newest last, at most [`LOG_KEEP`].
pub const OCCURRENCE_LOG: &str = "occurrence_log";
/// How many appended occurrences the log keeps.
pub const LOG_KEEP: usize = 10;
/// The most evidence one log entry carries, in bytes.
pub const EVIDENCE_BYTES: usize = 1024;

/// The keys this module writes on an alarm. They are the filer's own,
/// so a judge that closes only an item "nobody has written on"
/// (`verdict_streak::withdrawal`) counts them as its own writes.
pub const OWN_KEYS: [&str; 4] = [OCCURRENCES, LAST_SEEN, LAST_EPISODE, OCCURRENCE_LOG];

/// The filing handlers whose dedup is STRICTER than one open item per
/// subject, by source file, with the reason — read by the crate pin
/// `every_alarm_filer_raises_through_the_one_helper`, so a new filer
/// either raises through [`raise`] or says here why it need not.
pub const STRICTER: [(&str, &str); 6] = [
    (
        "jobs_agent_step_overdue.rs",
        "one alarm per late step EVER: its dedup read takes every status",
    ),
    (
        "jobs_flight_overdue.rs",
        "one alarm per flight and case EVER: its dedup read takes every status",
    ),
    (
        "jobs_urgent_item_watch.rs",
        "one RESCOPE per item EVER: its dedup read takes every status",
    ),
    (
        "jobs_receipt_overdue.rs",
        "one alarm per agent-run EVER: its dedup read takes every status",
    ),
    (
        "ops_judge.rs",
        "one alarm per failed ops-request: the subject is the request, which fails once",
    ),
    (
        "jobs_complete_linked_step.rs",
        "one alert per failed request (`for_request`): the subject fails once",
    ),
];

/// One occurrence of an alarm's condition, as its handler read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    /// The firing's instant, RFC 3339 — never this process's clock
    /// (backlog 2b03a2df). `None` for a filer that has no firing (the
    /// boot seed): the write's own event stamp is then the only when,
    /// and [`LAST_SEEN`] is not invented.
    pub at: Option<String>,
    /// What the handler read, in a sentence or a line.
    pub evidence: String,
    /// The handler's own episode, when it has one (see the module doc).
    pub episode: Option<String>,
}

impl Occurrence {
    /// An occurrence of a handler with no episode: only a relapse is new.
    pub fn new(at: impl Into<String>, evidence: impl Into<String>) -> Self {
        Self {
            at: Some(at.into()),
            evidence: evidence.into(),
            episode: None,
        }
    }

    /// An occurrence read with no firing to stamp it.
    pub fn unstamped(evidence: impl Into<String>) -> Self {
        Self {
            at: None,
            evidence: evidence.into(),
            episode: None,
        }
    }

    /// The same, naming the episode it belongs to.
    pub fn in_episode(mut self, episode: impl Into<String>) -> Self {
        self.episode = Some(episode.into());
        self
    }
}

/// What [`raise`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Raised {
    /// No open item for the subject: a new one was filed, with the
    /// minted id when the caller asked for it.
    Filed(Option<String>),
    /// The open item took the occurrence (and any refresh).
    Appended(String),
    /// The open item already records this occurrence; only a refresh
    /// that changed something was written.
    Held(String),
}

/// PURE: is `row` open? A listing row always carries its status; a row
/// without one is read as open, as the dedup reads it came from were.
pub fn is_open(row: &Value) -> bool {
    row.get("status")
        .and_then(Value::as_str)
        .is_none_or(|s| s == "open")
}

/// PURE: THE open item an alarm of this kind holds for this subject —
/// the row whose `metadata.<key>` is `subject`. The one definition of
/// "already raised" every filer reads.
pub fn open_item<'a>(rows: &'a [Value], key: &str, subject: &str) -> Option<&'a Value> {
    rows.iter().find(|row| {
        is_open(row)
            && row
                .pointer(&format!("/metadata/{key}"))
                .and_then(Value::as_str)
                == Some(subject)
    })
}

/// PURE: is `occ` an occurrence `item` does not already record?
pub fn is_new_occurrence(item: &Value, occ: &Occurrence) -> bool {
    if told_recovered(item) {
        return true;
    }
    match &occ.episode {
        None => false,
        Some(episode) => {
            item.pointer(&format!("/metadata/{LAST_EPISODE}"))
                .and_then(Value::as_str)
                != Some(episode.as_str())
        }
    }
}

/// PURE: does `open` — the open item for a subject, if any — already
/// record every occurrence a raise in `episode` could append? A handler
/// whose condition is costly to read asks this first and skips the
/// reading: true means [`raise`] would write nothing new.
pub fn already_recorded(open: Option<&Value>, episode: Option<&str>) -> bool {
    open.is_some_and(|item| {
        let probe = Occurrence {
            at: None,
            evidence: String::new(),
            episode: episode.map(str::to_string),
        };
        !is_new_occurrence(item, &probe)
    })
}

/// PURE: `s` clipped to at most `max` bytes on a character boundary,
/// marked when it was clipped.
pub fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// PURE: the metadata merge that records `occ` on `item`: the count
/// (an item filed before this module counts its filing as one), when it
/// was seen, its episode, the bounded log — and, on a relapse, the
/// withdrawal of the RECOVERED note, which is now false.
pub fn occurrence_patch(item: &Value, occ: &Occurrence) -> Map<String, Value> {
    let metadata = item.get("metadata");
    let count = metadata
        .and_then(|m| m.get(OCCURRENCES))
        .and_then(Value::as_u64)
        .unwrap_or(1);
    let mut log: Vec<Value> = metadata
        .and_then(|m| m.get(OCCURRENCE_LOG))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut entry = json!({"evidence": clip(&occ.evidence, EVIDENCE_BYTES)});
    if let Some(at) = &occ.at {
        entry["at"] = json!(at);
    }
    if told_recovered(item) {
        entry["relapse"] = json!(true);
    }
    if let Some(episode) = &occ.episode {
        entry["episode"] = json!(episode);
    }
    log.push(entry);
    let keep = log.len().saturating_sub(LOG_KEEP);
    let log: Vec<Value> = log.into_iter().skip(keep).collect();

    let mut patch = Map::new();
    patch.insert(OCCURRENCES.into(), json!(count + 1));
    if let Some(at) = &occ.at {
        patch.insert(LAST_SEEN.into(), json!(at));
    }
    if let Some(episode) = &occ.episode {
        patch.insert(LAST_EPISODE.into(), json!(episode));
    }
    patch.insert(OCCURRENCE_LOG.into(), Value::Array(log));
    if told_recovered(item)
        && let Value::Object(relapse) = relapse_patch()
    {
        patch.extend(relapse);
    }
    patch
}

/// PURE: a new alarm's body with its first occurrence stamped on it.
pub fn first_occurrence(mut body: Value, occ: &Occurrence) -> Value {
    if let Some(m) = body.get_mut("metadata").and_then(Value::as_object_mut) {
        m.insert(OCCURRENCES.into(), json!(1));
        if let Some(at) = &occ.at {
            m.insert(LAST_SEEN.into(), json!(at));
        }
        if let Some(episode) = &occ.episode {
            m.insert(LAST_EPISODE.into(), json!(episode));
        }
    }
    body
}

/// What [`raise`] will write, decided without any I/O.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// POST this body.
    File(Value),
    /// PATCH this merge onto the open item `id`; `appended` says whether
    /// it carries a new occurrence or only a refresh.
    Patch {
        id: String,
        merge: Map<String, Value>,
        appended: bool,
    },
    /// Write nothing to the open item `id`.
    Hold(String),
}

/// PURE: the plan for one raise. `refresh` is the handler's own reading
/// to keep current on an open item (a measurement, the newest version);
/// keys whose value the item already holds are dropped, so a tick that
/// changed nothing writes nothing.
pub fn plan(
    open: Option<&Value>,
    body: Value,
    occ: &Occurrence,
    refresh: Map<String, Value>,
) -> Plan {
    let Some(item) = open else {
        return Plan::File(first_occurrence(body, occ));
    };
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let held = |k: &String, v: &Value| item.pointer(&format!("/metadata/{k}")) == Some(v);
    let mut merge: Map<String, Value> = refresh.into_iter().filter(|(k, v)| !held(k, v)).collect();
    let appended = is_new_occurrence(item, occ);
    if appended {
        merge.extend(occurrence_patch(item, occ));
    }
    if merge.is_empty() {
        Plan::Hold(id)
    } else {
        Plan::Patch {
            id,
            merge,
            appended,
        }
    }
}

/// File `body` for a subject with no open item, or append `occ` to
/// `open`, the item it has — never both, never a second item. `refresh`
/// as in [`plan`]; pass an empty map for none.
pub async fn raise(
    client: &boss_core::machine_token::Client,
    jobs_base: &str,
    open: Option<&Value>,
    body: Value,
    occ: &Occurrence,
    refresh: Map<String, Value>,
    rule: &str,
) -> Result<Raised, HandlerError> {
    execute(
        client,
        jobs_base,
        plan(open, body, occ, refresh),
        false,
        rule,
    )
    .await
}

/// [`raise`] for a caller that names the filed item: a new one is
/// POSTed through `post_json_minted_id`, and [`Raised::Filed`] carries
/// the id.
pub async fn raise_minted(
    client: &boss_core::machine_token::Client,
    jobs_base: &str,
    open: Option<&Value>,
    body: Value,
    occ: &Occurrence,
    refresh: Map<String, Value>,
    rule: &str,
) -> Result<Raised, HandlerError> {
    execute(
        client,
        jobs_base,
        plan(open, body, occ, refresh),
        true,
        rule,
    )
    .await
}

async fn execute(
    client: &boss_core::machine_token::Client,
    jobs_base: &str,
    plan: Plan,
    minted: bool,
    rule: &str,
) -> Result<Raised, HandlerError> {
    let base = jobs_base.trim_end_matches('/');
    match plan {
        Plan::File(body) => {
            let url = format!("{base}/api/jobs");
            if minted {
                Ok(Raised::Filed(Some(
                    post_json_minted_id(client, &url, &body, rule).await?,
                )))
            } else {
                post_json(client, &url, &body, rule).await?;
                Ok(Raised::Filed(None))
            }
        }
        Plan::Patch {
            id,
            merge,
            appended,
        } => {
            write_json(
                client,
                reqwest::Method::PATCH,
                &format!("{base}/api/jobs/{id}/metadata"),
                &Value::Object(merge),
                rule,
            )
            .await?;
            Ok(if appended {
                Raised::Appended(id)
            } else {
                Raised::Held(id)
            })
        }
        Plan::Hold(id) => Ok(Raised::Held(id)),
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::RECOVERED_AT;
    use super::*;

    const AT: &str = "2026-11-17T18:00:00+00:00";
    const LATER: &str = "2026-11-18T18:00:00+00:00";

    fn body() -> Value {
        json!({"kind": "backlog-item", "title": "ALARM: x", "status": "open",
               "metadata": {"alarm_key": "host-a", "detail": "the reading"}})
    }

    fn item(metadata: Value) -> Value {
        json!({"id": "item-1", "status": "open", "metadata": metadata})
    }

    #[test]
    fn the_open_item_is_found_by_its_key_and_subject_and_a_closed_one_is_not() {
        let rows = vec![
            json!({"id": "closed", "status": "closed", "metadata": {"alarm_key": "host-a"}}),
            json!({"id": "other", "status": "open", "metadata": {"alarm_key": "host-b"}}),
            json!({"id": "open", "status": "open", "metadata": {"alarm_key": "host-a"}}),
        ];
        assert_eq!(
            open_item(&rows, "alarm_key", "host-a").unwrap()["id"],
            "open"
        );
        assert!(open_item(&rows[..2], "alarm_key", "host-a").is_none());
        assert!(open_item(&rows, "other_key", "host-a").is_none());
    }

    #[test]
    fn with_no_open_item_the_alarm_is_filed_with_its_first_occurrence() {
        let occ = Occurrence::new(AT, "seen").in_episode("v3");
        let Plan::File(filed) = plan(None, body(), &occ, Map::new()) else {
            panic!("no open item files");
        };
        assert_eq!(filed["metadata"][OCCURRENCES], 1);
        assert_eq!(filed["metadata"][LAST_SEEN], AT);
        assert_eq!(filed["metadata"][LAST_EPISODE], "v3");
        assert_eq!(filed["metadata"]["detail"], "the reading");
    }

    /// THE RULE: a second occurrence while the item is open appends to
    /// it and files nothing.
    #[test]
    fn a_new_episode_on_an_open_item_appends_and_files_nothing() {
        let open = item(json!({"alarm_key": "host-a", OCCURRENCES: 1, LAST_EPISODE: "v3"}));
        let occ = Occurrence::new(LATER, "drifted again").in_episode("v4");
        let Plan::Patch {
            id,
            merge,
            appended,
        } = plan(Some(&open), body(), &occ, Map::new())
        else {
            panic!("an open item is appended to, never filed beside");
        };
        assert!(appended);
        assert_eq!(id, "item-1");
        assert_eq!(merge[OCCURRENCES], 2);
        assert_eq!(merge[LAST_SEEN], LATER);
        assert_eq!(merge[LAST_EPISODE], "v4");
        assert_eq!(
            merge[OCCURRENCE_LOG],
            json!([{"at": LATER, "evidence": "drifted again", "episode": "v4"}])
        );
    }

    /// The same episode again — a redelivery, or the next tick of a
    /// condition that never cleared — is the same occurrence.
    #[test]
    fn the_same_episode_again_writes_nothing() {
        let open = item(json!({"alarm_key": "host-a", OCCURRENCES: 2, LAST_EPISODE: "v4"}));
        let occ = Occurrence::new(LATER, "still drifted").in_episode("v4");
        assert_eq!(
            plan(Some(&open), body(), &occ, Map::new()),
            Plan::Hold("item-1".into())
        );
        let continuous = Occurrence::new(LATER, "still down");
        assert_eq!(
            plan(Some(&open), body(), &continuous, Map::new()),
            Plan::Hold("item-1".into())
        );
    }

    /// A relapse is an occurrence for every handler, episode or not: the
    /// item was told it recovered, and that note is withdrawn.
    #[test]
    fn a_relapse_appends_and_withdraws_the_recovered_note() {
        let open = item(json!({"alarm_key": "host-a", RECOVERED_AT: AT,
                               "recovered_by": "x", "recovery": "RECOVERED — ..."}));
        let occ = Occurrence::new(LATER, "down again");
        let Plan::Patch {
            merge, appended, ..
        } = plan(Some(&open), body(), &occ, Map::new())
        else {
            panic!("a relapse is written");
        };
        assert!(appended);
        assert_eq!(
            merge[OCCURRENCES], 2,
            "a pre-helper item counts its filing as one"
        );
        assert_eq!(merge[RECOVERED_AT], Value::Null);
        assert_eq!(merge["recovery"], Value::Null);
        assert_eq!(merge[OCCURRENCE_LOG][0]["relapse"], true);
    }

    /// A refresh writes only what changed, and is not an occurrence.
    #[test]
    fn a_refresh_writes_only_what_changed_and_counts_nothing() {
        let open = item(json!({"alarm_key": "host-a", "measured": 5}));
        let occ = Occurrence::new(LATER, "x");
        let same: Map<String, Value> = [("measured".to_string(), json!(5))].into_iter().collect();
        assert_eq!(
            plan(Some(&open), body(), &occ, same),
            Plan::Hold("item-1".into())
        );
        let moved: Map<String, Value> = [("measured".to_string(), json!(6))].into_iter().collect();
        let Plan::Patch {
            merge, appended, ..
        } = plan(Some(&open), body(), &occ, moved)
        else {
            panic!("a changed reading is refreshed");
        };
        assert!(!appended);
        assert_eq!(Value::Object(merge), json!({"measured": 6}));
    }

    /// BOUNDED UNDER THE PACKET BUDGET. The log keeps the newest
    /// entries and clips each one, so the most it can ever weigh is a
    /// fixed few kilobytes (11,076 bytes measured at the bound) — under
    /// two percent of the budget past which a write that grows a packet
    /// is refused.
    #[test]
    fn the_occurrence_log_is_bounded_far_under_the_packet_budget() {
        let mut open = item(json!({"alarm_key": "host-a"}));
        let huge = "é".repeat(10 * EVIDENCE_BYTES);
        for n in 0..(3 * LOG_KEEP) {
            let occ = Occurrence::new(
                format!("2026-11-17T18:{:02}:00+00:00", n % 60),
                huge.clone(),
            )
            .in_episode(format!("episode-{n}"));
            let patch = occurrence_patch(&open, &occ);
            for (k, v) in patch {
                open["metadata"][k] = v;
            }
        }
        let log = open["metadata"][OCCURRENCE_LOG].as_array().unwrap();
        assert_eq!(log.len(), LOG_KEEP);
        assert_eq!(
            log[LOG_KEEP - 1]["episode"],
            format!("episode-{}", 3 * LOG_KEEP - 1)
        );
        assert_eq!(open["metadata"][OCCURRENCES], 3 * LOG_KEEP as u64 + 1);
        for entry in log {
            assert!(entry["evidence"].as_str().unwrap().len() <= EVIDENCE_BYTES);
        }
        let weight = serde_json::to_vec(&open["metadata"]).unwrap().len();
        assert!(
            weight * 50 < boss_jobs::packet_budget::BUDGET,
            "the occurrence keys weigh {weight} bytes at their bound; the packet budget is {}",
            boss_jobs::packet_budget::BUDGET
        );
    }

    /// EVERY ALARM FILER RAISES THROUGH THE ONE HELPER (backlog f4b0ad41).
    ///
    /// The defect this module cures was twenty spellings of one
    /// judgement, so what lasts is that the twenty-first cannot be
    /// written beside them: a handler that files a backlog-item either
    /// names `alarm_item::` in its production code or stands in
    /// [`STRICTER`] with the reason its own dedup needs none. Read off
    /// the source, like the lane pin in `common.rs`, because the defect
    /// is a missing call — no run of the handlers that are right can
    /// show that a new one is wrong. A [`STRICTER`] entry that names no
    /// filer is refused too, so the exemption cannot outlive its reason.
    // crate pin — it reads this crate's own filing sites, so a car that
    // adds one learns it in `infra/gate.sh --lint`
    #[test]
    fn every_alarm_filer_raises_through_the_one_helper() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/handlers");
        let mut filers: Vec<String> = Vec::new();
        let mut bypassing: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(dir).expect("handlers dir") {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let src = std::fs::read_to_string(&path).expect("read handler");
            let production = match src.find("\nmod tests {") {
                Some(i) => &src[..i],
                None => &src[..],
            };
            let files = production.contains(r#""kind": "backlog-item""#)
                || production.contains(r#""kind":"backlog-item""#);
            if !files || name == "alarm_item.rs" {
                continue;
            }
            filers.push(name.clone());
            let stricter = STRICTER.iter().any(|(f, _)| *f == name);
            if !stricter && !production.contains("alarm_item::") {
                bypassing.push(name);
            }
        }
        assert!(
            bypassing.is_empty(),
            "these handlers file a backlog-item without raising through `alarm_item`, so an \
             occurrence while their alarm is open can file a second item, or go unrecorded: \
             {bypassing:?}. Raise through `alarm_item::raise` (or decide with \
             `alarm_item::plan`), or, if the handler's dedup is stricter than one open item \
             per subject, add it to `alarm_item::STRICTER` with the reason."
        );
        let stale: Vec<&str> = STRICTER
            .iter()
            .map(|(f, _)| *f)
            .filter(|f| !filers.iter().any(|n| n == f))
            .collect();
        assert!(
            stale.is_empty(),
            "these `alarm_item::STRICTER` entries name no handler that files: {stale:?}"
        );
        assert!(
            filers.len() >= STRICTER.len() + 10,
            "the scan found only {} filing handlers ({filers:?}) — a reshaped body must not \
             read as `all clear`",
            filers.len()
        );
    }

    #[test]
    fn clip_cuts_on_a_character_boundary_and_says_so() {
        assert_eq!(clip("short", 10), "short");
        let clipped = clip(&"é".repeat(10), 9);
        assert!(clipped.len() <= 9, "{clipped}");
        assert!(clipped.ends_with('…'));
    }

    /// The I/O half, end to end against a stand-in jobs API: the second
    /// occurrence PATCHes the open item and POSTs nothing; once that
    /// item is closed, the next occurrence files a new one.
    #[tokio::test]
    async fn a_second_occurrence_appends_and_a_closed_item_lets_a_new_one_be_filed() {
        let stub = super::super::listing_stub::serve(vec![]).await;
        let client = super::super::common::api_client();
        let first = Occurrence::new(AT, "first").in_episode("night-1");
        let filed = raise(&client, &stub.base, None, body(), &first, Map::new(), "r")
            .await
            .unwrap();
        assert_eq!(filed, Raised::Filed(None));

        let open = item(json!({"alarm_key": "host-a", OCCURRENCES: 1, LAST_EPISODE: "night-1"}));
        let second = Occurrence::new(LATER, "second").in_episode("night-2");
        let appended = raise(
            &client,
            &stub.base,
            Some(&open),
            body(),
            &second,
            Map::new(),
            "r",
        )
        .await
        .unwrap();
        assert_eq!(appended, Raised::Appended("item-1".into()));

        let rows = vec![json!({"id": "item-1", "status": "closed",
                               "metadata": {"alarm_key": "host-a"}})];
        let after_close = open_item(&rows, "alarm_key", "host-a");
        let third = Occurrence::new(LATER, "third").in_episode("night-3");
        let refiled = raise(
            &client,
            &stub.base,
            after_close,
            body(),
            &third,
            Map::new(),
            "r",
        )
        .await
        .unwrap();
        assert_eq!(refiled, Raised::Filed(None));

        let sent = stub.sent();
        let writes: Vec<&str> = sent.iter().map(|(w, _)| w.as_str()).collect();
        assert_eq!(
            writes,
            [
                "POST /api/jobs",
                "PATCH /api/jobs/item-1/metadata",
                "POST /api/jobs"
            ],
            "{sent:?}"
        );
        assert_eq!(sent[1].1[OCCURRENCES], 2);
        assert_eq!(sent[1].1[OCCURRENCE_LOG][0]["evidence"], "second");
    }
}
