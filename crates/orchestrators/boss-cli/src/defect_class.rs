//! ONE ITEM PER DEFECT CLASS FOR REVIEW FOLLOW-UPS (backlog f4b0ad41;
//! design 25511015 Q4, rule 2, decided by David 2026-11-17).
//!
//! A follow-up from a review or a builder report that names the defect
//! class of an item already OPEN is appended to that item, as evidence,
//! instead of being filed as a new one. `boss job file` is the door:
//! it reads the open backlog for the class before it POSTs, and when an
//! item holds it the filing becomes an entry in that item's
//! [`FOLLOW_UP_LOG`] and the verb says so, naming the item. A closed
//! item is history: the next follow-up of its class files a new one.
//!
//! THE CHOICE IS BOUND TO THE WRITE (review f9707c00 of car ee9d6bca).
//! An item chosen open can close before the append lands, and a plain
//! metadata merge annotates a closed packet without complaint — so the
//! follow-up was buried on history and reported as joined. The append
//! now goes through the conditional metadata door, which lands it only
//! while the row is open and still reads the class, count and log it was
//! judged on ([`command`]); anything else takes nothing, and the filing
//! chooses again ([`join`]), filing anew when no open item holds the
//! class. The same expectation refuses a second append judged on the
//! same count, so two follow-ups racing for one item no longer lose one.
//!
//! WHY A NAMED CLASS AND NOT A DERIVED KEY. The rule asked which key
//! would have merged the follow-ups. Measured 2026-11-18 on the system of
//! record (every backlog-item read whole: 1,284 open, 3,999 of 4,300
//! closed; the design's own list of 77 was a scratch file no longer on
//! the pod, so it was re-derived from its stated rule — open titles
//! reading as review residue — and 54 still match):
//!
//! - Those 54 are each a BUNDLE, one per reviewed car ("X follow-ups:
//!   a, b and c"). No two shared a defect on reading, and no key merged
//!   a pair that should merge: area put 33 of them into 13 groups of
//!   unrelated findings, a three-word title stem merged 1, the source car
//!   merged none.
//! - The 169 review-finding items filed 2026-11-04..18 held five
//!   near-duplicate title pairs; four were one defect filed twice. Area
//!   kept 2 of those 4 together, because the other two were filed under
//!   different areas for the same defect (`cluster` and `infra/cluster`
//!   for w-3's database label; `stations` and `jobs` for the live
//!   compatibility readers) — while merging 124 of the 169 unrelated
//!   items. A title stem kept only the one pair filed with an identical
//!   title. The source car merged 27, all distinct defects from one
//!   review.
//!
//! So no key derivable from what a filing carries today finds a defect
//! class: two people word one defect differently, and file it under
//! different areas. The class is NAMED by the filer — a slug, recorded
//! as [`DEFECT_CLASS_KEY`] — and the key is the class alone, not area
//! plus class, because area disagreed in half the true pairs. The door
//! makes it a requirement rather than a convention: a backlog-item in a
//! follow-up lane ([`CLASS_IS_REQUIRED_OF`]) is refused without one, and
//! the refusal names the read that lists the classes already open.
//!
//! BOUNDED. The log keeps the newest [`LOG_KEEP`] entries, replaced whole
//! on each append (the metadata door merges top-level keys), and
//! [`FOLLOW_UPS`] counts every one; each append is its own metadata
//! event in the log, carrying the entry whole, so an entry that ages out
//! of the packet is still on the record.

use anyhow::{Context, Result, bail};
use boss_jobs::conditional_metadata::{Command, Expected, Receipt};
use serde_json::{Value, json};

use crate::channels::InputChannel;

/// Where a filing records the defect class it names.
pub(crate) const DEFECT_CLASS_KEY: &str = "defect_class";
/// How many follow-ups have been appended to the item: absent until the
/// first, which writes 1.
pub(crate) const FOLLOW_UPS: &str = "follow_ups";
/// The appended follow-ups, newest last, at most [`LOG_KEEP`].
pub(crate) const FOLLOW_UP_LOG: &str = "follow_up_log";
/// How many appended follow-ups the item keeps on the packet.
pub(crate) const LOG_KEEP: usize = 20;

/// The lanes a follow-up comes in through: a reviewer's finding, and a
/// builder's report of what it found mid-car (analyst-rules.md names
/// discovery-while-working as that lane). A backlog-item in one of these
/// is refused without a class.
pub(crate) const CLASS_IS_REQUIRED_OF: [InputChannel; 2] =
    [InputChannel::Review, InputChannel::Discovery];

/// How a filer finds the classes already open before naming one.
pub(crate) const LIST_OPEN_CLASSES: &str = "boss job list --kind backlog-item --has defect_class";

/// A class is a grouping key: lowercase words joined by `-`, at least
/// two of them, because a one-word class (`bug`, `jobs`) is an area, and
/// an area merged 124 unrelated findings when it was measured.
fn check(class: &str) -> Result<()> {
    let words: Vec<&str> = class.split('-').collect();
    let shaped = (3..=80).contains(&class.len())
        && words.len() >= 2
        && words.iter().all(|w| {
            !w.is_empty()
                && w.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
        && class.starts_with(|c: char| c.is_ascii_lowercase());
    if !shaped {
        bail!(
            "--defect-class names a class of defect in two or more lowercase words joined by - \
             (e.g. probe-reads-unconverged-as-failed), 3-80 characters — got {class:?}"
        );
    }
    Ok(())
}

/// The class this filing names, or a refusal — before any socket.
/// `lane` is the input lane the filing records (flag or metadata).
pub(crate) fn requested(
    kind: &str,
    lane: Option<&str>,
    flag: Option<&str>,
    md: &Option<Value>,
) -> Result<Option<String>> {
    let in_md = match md
        .as_ref()
        .and_then(|m| m.get(DEFECT_CLASS_KEY))
        .filter(|v| !v.is_null())
    {
        Some(Value::String(s)) => Some(s.clone()),
        Some(other) => bail!("the metadata's `{DEFECT_CLASS_KEY}` is not a string: {other}"),
        None => None,
    };
    if let (Some(f), Some(said)) = (flag, &in_md)
        && f != said
    {
        bail!(
            "--defect-class {f} disagrees with the metadata's {DEFECT_CLASS_KEY} {said} — say it once"
        );
    }
    let class = flag.map(str::to_string).or(in_md);
    match &class {
        Some(c) => {
            if kind != "backlog-item" {
                bail!("a defect class groups backlog items; a {kind} carries none");
            }
            check(c)?;
        }
        None => {
            let required = kind == "backlog-item"
                && lane
                    .and_then(InputChannel::parse)
                    .is_some_and(|l| CLASS_IS_REQUIRED_OF.contains(&l));
            if required {
                bail!(
                    "a {} {kind} is a follow-up, and a follow-up joins the open item of its \
                     defect class rather than opening another (backlog f4b0ad41): pass \
                     --defect-class <class>, reusing one an open item already names \
                     ({LIST_OPEN_CLASSES}) or naming a new one",
                    lane.unwrap_or("?")
                );
            }
        }
    }
    Ok(class)
}

/// The read that finds a class's open items.
pub(crate) fn open_query(class: &str) -> String {
    let doc = json!({ DEFECT_CLASS_KEY: class }).to_string();
    format!(
        "/api/jobs?kind=backlog-item&status=open&limit=500&metadata={}",
        percent_encoding::utf8_percent_encode(&doc, crate::job::QUERY_VALUE)
    )
}

/// The id of the OPEN item that holds `class`, the oldest when several
/// do (the older item is the one that stays, as a duplicate triage
/// keeps it), or `None`. `get` reads one API path — the seam the tests
/// go through.
///
/// The rows are read, not the total: a server that ignored `metadata=`
/// would answer every open item there is, and the first of those is not
/// this class's item. A page smaller than its total is refused rather
/// than judged, because a limit is not a filter.
pub(crate) async fn open_item<G, Fut>(get: G, class: &str) -> Result<Option<String>>
where
    G: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    let body = get(open_query(class)).await?;
    let total = body
        .as_ref()
        .and_then(|b| b.get("total"))
        .and_then(Value::as_u64);
    let rows = crate::train::rows(body)?;
    if let Some(t) = total
        && t > rows.len() as u64
    {
        bail!(
            "the open items of defect class {class} answered {} of {t} rows — refusing to judge \
             a page as the whole class",
            rows.len()
        );
    }
    let mut holding: Vec<&Value> = rows
        .iter()
        .filter(|r| {
            r.get("metadata")
                .and_then(|m| m.get(DEFECT_CLASS_KEY))
                .and_then(Value::as_str)
                == Some(class)
                && r.get("status").and_then(Value::as_str) != Some("closed")
        })
        .collect();
    let at = |r: &Value| {
        (
            r.get("opened_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            r.get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        )
    };
    holding.sort_by_key(|r| at(r));
    Ok(holding
        .first()
        .and_then(|r| r.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string))
}

/// One follow-up as it is appended: the filing whole — its title, its
/// priority and subject when given, and every metadata key it carried —
/// copied, not summarised, because an entry is the only place on the
/// packet the follow-up's own words live.
pub(crate) fn entry(
    title: &str,
    priority: Option<&str>,
    subject_id: Option<&str>,
    md: &Value,
) -> Value {
    let mut e = json!({ "title": title, "metadata": md });
    if let Some(p) = priority {
        e["priority"] = json!(p);
    }
    if let Some(s) = subject_id {
        e["subject_id"] = json!(s);
    }
    e
}

/// The metadata patch that appends `entry` to `item` — pure, so the
/// count and the bound are pinned without a socket.
pub(crate) fn append_patch(item: &Value, entry: Value) -> Value {
    let md = item.get("metadata").unwrap_or(&Value::Null);
    let count = md.get(FOLLOW_UPS).and_then(Value::as_u64).unwrap_or(0);
    let mut log: Vec<Value> = md
        .get(FOLLOW_UP_LOG)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    log.push(entry);
    let skip = log.len().saturating_sub(LOG_KEEP);
    json!({ FOLLOW_UPS: count + 1, FOLLOW_UP_LOG: log.split_off(skip) })
}

/// How many times a filing chooses its class's item again after the one
/// it chose stopped holding the class before the write, before it stops.
pub(crate) const ATTEMPTS: usize = 3;

/// What an append to a chosen item came to.
#[derive(Debug)]
pub(crate) enum Appended {
    /// It landed on the item while the item was OPEN and named the class:
    /// the report the verb prints in place of "filed".
    Joined(String),
    /// The item no longer holds the class open — it closed, was reclassed,
    /// or moved under the write — and NOTHING was written on it. Why.
    Gone(String),
}

/// Why `item`, as read, can no longer take a follow-up of `class` — or
/// `None` while it is open and names the class.
pub(crate) fn no_longer_holds(item: &Value, class: &str) -> Option<String> {
    let status = item.get("status").and_then(Value::as_str).unwrap_or("?");
    if status != "open" {
        return Some(format!("it is {status}"));
    }
    let named = item
        .get("metadata")
        .and_then(|m| m.get(DEFECT_CLASS_KEY))
        .and_then(Value::as_str);
    (named != Some(class)).then(|| format!("it now names defect class {}", named.unwrap_or("none")))
}

/// The conditional command that appends `entry` to `item` AS READ. The
/// door lands it only while the row is OPEN (its own rule,
/// `Command::matches`) and still reads the class, the count and the log
/// this append was judged on — all under the write's lock — so a close,
/// a reclass or another append between the read and the write refuses it
/// with nothing written (review f9707c00 of car ee9d6bca: a second
/// unguarded read only narrows that window; this shuts it).
pub(crate) fn command(item: &Value, class: &str, entry: Value) -> Command {
    let md = item.get("metadata");
    let as_read = |key: &str| match md.and_then(|m| m.get(key)) {
        Some(value) => Expected {
            present: true,
            value: value.clone(),
        },
        None => Expected {
            present: false,
            value: Value::Null,
        },
    };
    let expected = [
        (
            DEFECT_CLASS_KEY.to_string(),
            Expected {
                present: true,
                value: json!(class),
            },
        ),
        (FOLLOW_UPS.to_string(), as_read(FOLLOW_UPS)),
        (FOLLOW_UP_LOG.to_string(), as_read(FOLLOW_UP_LOG)),
    ]
    .into_iter()
    .collect();
    let patch = match append_patch(item, entry) {
        Value::Object(patch) => patch,
        _ => serde_json::Map::new(),
    };
    Command {
        contract_version: 2,
        idempotency_key: uuid::Uuid::new_v4(),
        expected,
        patch,
        immutable_keys: Vec::new(),
        guarded_keys: Vec::new(),
    }
}

/// Join the open item of `class`: `Some(report)` when the follow-up
/// landed on it, `None` when no open item holds the class and the caller
/// files a new one. An item that stops holding the class between its
/// choice and the write took nothing, so the class is chosen again — at
/// most [`ATTEMPTS`] times, then the filing stops and says why.
pub(crate) async fn join(
    wire: &crate::steps::Wire,
    class: &str,
    entry: Value,
) -> Result<Option<String>> {
    let mut gone = Vec::new();
    for _ in 0..ATTEMPTS {
        let get = |path: String| async move { wire.call(reqwest::Method::GET, &path, None).await };
        let Some(item) = open_item(get, class).await? else {
            return Ok(None);
        };
        match append(wire, &item, class, entry.clone()).await? {
            Appended::Joined(report) => return Ok(Some(report)),
            Appended::Gone(why) => gone.push(format!("{item}: {why}")),
        }
    }
    bail!(
        "the open item of defect class {class} moved under {ATTEMPTS} appends in a row — NOT \
         filed, and nothing written:\n  {}",
        gone.join("\n  ")
    )
}

/// Append one follow-up to the item `id`, read afresh: [`Appended::Gone`]
/// when it no longer holds the class open, else [`write`].
pub(crate) async fn append(
    wire: &crate::steps::Wire,
    id: &str,
    class: &str,
    entry: Value,
) -> Result<Appended> {
    // Read the item afresh, not the list row: the count and the log are
    // judged on the packet as it stands now.
    let item = wire
        .call(reqwest::Method::GET, &format!("/api/jobs/{id}"), None)
        .await?
        .with_context(|| format!("the open item {id} of class {class} answered no packet"))?;
    if let Some(why) = no_longer_holds(&item, class) {
        return Ok(Appended::Gone(why));
    }
    write(wire, id, class, &item, entry).await
}

/// Write the append judged on `item` through the conditional door
/// ([`command`]) and confirm it from the server's receipt — the row as
/// the write left it, read under the same lock, not a later GET that a
/// close could already have moved.
pub(crate) async fn write(
    wire: &crate::steps::Wire,
    id: &str,
    class: &str,
    item: &Value,
    entry: Value,
) -> Result<Appended> {
    let asked_priority = entry
        .get("priority")
        .and_then(Value::as_str)
        .map(str::to_string);
    let command = command(item, class, entry);
    let path = format!("/api/jobs/{id}/metadata/conditional");
    let (status, body) = wire
        .call_answering(
            reqwest::Method::POST,
            &path,
            Some(serde_json::to_value(&command)?),
        )
        .await?;
    let said = String::from_utf8_lossy(&body);
    if status == reqwest::StatusCode::CONFLICT {
        return Ok(Appended::Gone(format!(
            "it moved under the write, which took nothing: {}",
            said.trim()
        )));
    }
    if !status.is_success() {
        bail!("POST {path} -> {status}: {}", said.trim());
    }
    let receipt: Receipt =
        serde_json::from_slice(&body).context("the conditional append answered no receipt")?;
    if receipt.command != command || receipt.job_id.to_string() != id {
        bail!("the conditional append's receipt is not this command's on {id}");
    }
    let now = serde_json::to_value(&receipt.job)?;
    if let Some(why) = no_longer_holds(&now, class) {
        bail!("the receipt shows {id} no longer holds {class} ({why}) — NOT filed either");
    }
    let patch = Value::Object(command.patch.clone());
    let (report, all_took) =
        crate::job::confirm_patch(now.get("metadata").unwrap_or(&Value::Null), &patch);
    if !all_took {
        bail!("the follow-up did not land on {id} as sent — NOT filed either:\n{report}");
    }
    let n = patch.get(FOLLOW_UPS).and_then(Value::as_u64).unwrap_or(0);
    let mut out = format!(
        "boss job: NOT filed — appended as follow-up {n} to {id} \"{}\", the open item of \
         defect class {class} (backlog f4b0ad41) — confirmed by the write's receipt",
        now.get("title").and_then(Value::as_str).unwrap_or("?"),
    );
    let held = now.get("priority").and_then(Value::as_str).unwrap_or("?");
    if let Some(p) = asked_priority
        && p != held
    {
        out.push_str(&format!(
            "\n  the follow-up asked {p}; the item is {held} — `boss job priority {id} {p} \
             --why …` moves it"
        ));
    }
    Ok(Appended::Joined(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn say(r: Result<impl std::fmt::Debug>) -> String {
        format!("{:#}", r.unwrap_err())
    }

    #[test]
    fn a_follow_up_lane_is_refused_without_a_class_and_the_refusal_names_the_flag_and_the_read() {
        for lane in ["review-finding", "discovery-while-working"] {
            let said = say(requested("backlog-item", Some(lane), None, &None));
            assert!(
                said.contains("--defect-class") && said.contains(LIST_OPEN_CLASSES),
                "{lane}: {said}"
            );
        }
        // Other lanes, and other kinds, may name none.
        for lane in ["roadmap", "pipeline-failure", "telemetry/monitoring"] {
            assert_eq!(
                requested("backlog-item", Some(lane), None, &None).unwrap(),
                None
            );
        }
        assert_eq!(
            requested("agent-run", Some("review-finding"), None, &None).unwrap(),
            None
        );
    }

    #[test]
    fn the_class_is_said_once_by_flag_or_metadata_and_is_a_slug_of_two_words() {
        let md = Some(json!({ DEFECT_CLASS_KEY: "probe-reads-unconverged-as-failed" }));
        assert_eq!(
            requested("backlog-item", Some("review-finding"), None, &md).unwrap(),
            Some("probe-reads-unconverged-as-failed".into())
        );
        assert_eq!(
            requested(
                "backlog-item",
                Some("roadmap"),
                Some("probe-reads-unconverged-as-failed"),
                &md
            )
            .unwrap(),
            Some("probe-reads-unconverged-as-failed".into())
        );
        assert!(
            say(requested(
                "backlog-item",
                Some("roadmap"),
                Some("another-class"),
                &md
            ))
            .contains("say it once")
        );
        for bad in [
            "bug",
            "jobs",
            "Probe-reads",
            "two words",
            "-lead",
            "trail-",
            "a--b",
            "x",
        ] {
            assert!(
                requested("backlog-item", Some("roadmap"), Some(bad), &None).is_err(),
                "{bad:?} is not a class"
            );
        }
        assert!(say(requested("gate-run", None, Some("some-class"), &None)).contains("backlog"));
        assert!(
            say(requested(
                "backlog-item",
                None,
                None,
                &Some(json!({ DEFECT_CLASS_KEY: 7 }))
            ))
            .contains("not a string")
        );
    }

    fn reader(
        map: &BTreeMap<String, Value>,
    ) -> impl Fn(String) -> std::future::Ready<Result<Option<Value>>> + '_ {
        move |path: String| {
            std::future::ready(match map.get(&path) {
                Some(v) => Ok(Some(v.clone())),
                None => Err(anyhow::anyhow!("unexpected read {path}")),
            })
        }
    }

    fn row(id: &str, class: &str, opened_at: &str) -> Value {
        json!({"id": id, "status": "open", "opened_at": opened_at,
               "metadata": { DEFECT_CLASS_KEY: class }})
    }

    #[tokio::test]
    async fn the_open_item_is_the_oldest_row_that_carries_the_class_and_a_short_page_refuses() {
        let c = "probe-reads-unconverged-as-failed";
        let map: BTreeMap<String, Value> = [
            (
                open_query(c),
                json!({"data": [row("b", c, "2026-11-12T00:00:00Z"),
                                 row("a", c, "2026-11-10T00:00:00Z")], "total": 2}),
            ),
            (open_query("no-such-class"), json!({"data": [], "total": 0})),
            // A server that ignored the filter answers an item of another class.
            (
                open_query("ignored-filter"),
                json!({"data": [row("z", "another-class", "2026-01-01T00:00:00Z")], "total": 1}),
            ),
            (
                open_query("short-page"),
                json!({"data": [row("y", "short-page", "2026-01-01T00:00:00Z")], "total": 501}),
            ),
        ]
        .into_iter()
        .collect();
        assert_eq!(open_item(reader(&map), c).await.unwrap(), Some("a".into()));
        assert_eq!(
            open_item(reader(&map), "no-such-class").await.unwrap(),
            None
        );
        assert_eq!(
            open_item(reader(&map), "ignored-filter").await.unwrap(),
            None
        );
        assert!(say(open_item(reader(&map), "short-page").await).contains("501"));
    }

    #[test]
    fn an_append_counts_every_follow_up_and_keeps_the_newest_entries() {
        let first = append_patch(&json!({"metadata": {}}), json!({"title": "t1"}));
        assert_eq!(first[FOLLOW_UPS], 1);
        assert_eq!(first[FOLLOW_UP_LOG], json!([{"title": "t1"}]));
        let mut item = json!({"metadata": {}});
        for i in 0..(LOG_KEEP + 3) {
            let p = append_patch(&item, json!({ "title": format!("t{i}") }));
            item["metadata"][FOLLOW_UPS] = p[FOLLOW_UPS].clone();
            item["metadata"][FOLLOW_UP_LOG] = p[FOLLOW_UP_LOG].clone();
        }
        let log = item["metadata"][FOLLOW_UP_LOG].as_array().unwrap();
        assert_eq!(item["metadata"][FOLLOW_UPS], LOG_KEEP as u64 + 3);
        assert_eq!(log.len(), LOG_KEEP);
        assert_eq!(log[0]["title"], "t3", "the oldest aged out");
        assert_eq!(log[LOG_KEEP - 1]["title"], format!("t{}", LOG_KEEP + 2));
    }

    #[test]
    fn the_append_expects_the_class_count_and_log_it_was_judged_on() {
        let c = "probe-reads-unconverged-as-failed";
        let first = command(
            &json!({"status": "open", "metadata": { DEFECT_CLASS_KEY: c }}),
            c,
            json!({"title": "t1"}),
        );
        assert!(first.validate().is_ok());
        assert_eq!(first.expected[DEFECT_CLASS_KEY].value, json!(c));
        assert!(first.expected[DEFECT_CLASS_KEY].present);
        assert!(!first.expected[FOLLOW_UPS].present);
        assert!(!first.expected[FOLLOW_UP_LOG].present);
        assert_eq!(first.patch[FOLLOW_UPS], 1);
        let read = json!({"status": "open", "metadata": {
            DEFECT_CLASS_KEY: c, FOLLOW_UPS: 1, FOLLOW_UP_LOG: [{"title": "t1"}]}});
        let next = command(&read, c, json!({"title": "t2"}));
        assert_eq!(next.expected[FOLLOW_UPS].value, 1);
        assert_eq!(next.expected[FOLLOW_UP_LOG].value, json!([{"title": "t1"}]));
        assert_eq!(next.patch[FOLLOW_UPS], 2);
        assert_ne!(first.idempotency_key, next.idempotency_key);
    }

    #[test]
    fn an_item_holds_its_class_only_while_open_and_named() {
        let c = "probe-reads-unconverged-as-failed";
        let at = |status: &str, class: &str| {
            no_longer_holds(
                &json!({"status": status, "metadata": { DEFECT_CLASS_KEY: class }}),
                c,
            )
        };
        assert_eq!(at("open", c), None);
        assert!(at("closed", c).unwrap().contains("closed"));
        assert!(at("cancelled", c).unwrap().contains("cancelled"));
        assert!(
            at("open", "another-class")
                .unwrap()
                .contains("another-class")
        );
    }

    #[test]
    fn an_entry_copies_the_filing_whole() {
        let md = json!({"description": "d", "area": "jobs", "input_channel": "review-finding"});
        let e = entry("t", Some("urgent"), None, &md);
        assert_eq!(
            e,
            json!({"title": "t", "priority": "urgent", "metadata": md})
        );
    }
}
