//! `jobs.supersede_stale_unsigned` — an open packet whose sign-off is
//! still UNSIGNED, and whose own later reading names a newer tree than
//! the one its measure step recorded, is superseded by a fresh packet
//! (design 62f27539 C1, backlog 44caee88).
//!
//! THE CASE. Publish packet d91c2b7b measured forge tree d88cf723 on
//! 2026-11-08. David held its approval on 2026-11-10 until a tree
//! carrying the mirror Gate fix (c1c49677) was measured. The fix landed
//! on 2026-11-11, and `refresh-publish-drift-daily` wrote a reading of a
//! newer tree onto the packet every day after — 2766f12b on 2026-11-16,
//! which carries it — but nothing moved the packet: the rule that copies
//! a reading onto `measure` writes nothing once the step is complete,
//! the step door refuses metadata on a completed step, `approve` stays
//! bound to the first tree's shas, and `publish-to-github-daily` files
//! nothing while a publish packet is open (`NOT open_publish_exists`).
//! Publishing stalled for seven days, until the drain superseded the
//! packet by hand at 17:03Z with d2e189f5.
//!
//! THE TRANSITION. The protocol already has the terminal: `superseded`
//! fires on `superseded_by` and `supersession_translation` both set.
//! What was missing is the machine that sets them. Once a day, for each
//! open packet of the declared kind on the declared subject:
//!
//! - its `signed_step` (`approve`) is NOT completed — a packet David has
//!   signed runs to its own terminal, and this handler never touches it;
//! - its `measured_step` (`measure`) IS completed, with a non-empty
//!   `measured_sha` (`source_sha`) — a packet not yet measured is the
//!   copy rule's to measure, and has no tree to be stale against;
//! - its reading (`reading_sha`, `/drift_refresh/source_sha`) names a
//!   DIFFERENT, non-empty sha, taken (`reading_at`) strictly AFTER the
//!   measure step completed. Forge main only moves forward, so a later
//!   reading of it naming another sha is a newer tree. An unreadable
//!   stamp on either side is not judged newer: superseding is the act,
//!   and no evidence is not a reason to act.
//!
//! Such a packet is superseded by a fresh packet of the same kind,
//! subject and title, carrying the stale packet's `carry` metadata keys
//! (the ones the daily rule files with) and `supersedes`. Superseding an
//! unsigned packet publishes nothing and costs nothing; keeping it blocks
//! every later day (the design's own reason).
//!
//! ONE STALE PACKET, ONE SUCCESSOR, ON EVERY REDELIVERY. The fresh
//! packet's id is derived from the stale packet's id alone, so a firing
//! that filed the successor and died before the supersession PATCH files
//! the SAME packet when it runs again (the jobs API answers an id it
//! already admitted, 558396ff), and a newer packet already open on the
//! subject — that successor, or one a person filed — is named instead of
//! filing another. The two writes are ordered successor-first, so the
//! stale packet never names a packet that does not exist.
//!
//! WHY A RULE OF ITS OWN AND NOT `publish-to-github-daily`. That rule is
//! a scheduled `jobs.spawn` whose guard the silence sweep reads as the
//! dedup question it can ask for itself (`cadence_roster::parse_guard`);
//! a guard that also asked about staleness would be a condition on the
//! world, which the roster refuses (backlog fbcac8b4). So the daily
//! cadence stays one packet per mirror, and this daily rule is the
//! transition that frees it. Whichever fires first on a tick, exactly
//! one fresh packet results: the dedup guard sees the stale packet or
//! the successor open and files nothing.
//!
//! WHAT IT WRITES. The successor's POST, and one PATCH of the stale
//! packet's metadata (`superseded_by`, `supersession_translation`)
//! through the job merge door. The protocol's own `superseded` terminal
//! does the closing. No step is completed, claimed or moved here.

use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

use super::common::{
    api_client, get_json, open_jobs_of_kind, post_json_or_held, row_or_refuse, write_json,
};

/// The handler's registered name.
pub const HANDLER: &str = "jobs.supersede_stale_unsigned";

/// The fresh packet's metadata key naming the packet it replaces.
pub const SUPERSEDES_KEY: &str = "supersedes";

/// The rule's declaration — every protocol word it judges by is an arg,
/// so the slugs and fields stay the row's and the copy rule's, not this
/// file's.
#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub kind: String,
    pub subject: String,
    pub measured_step: String,
    pub measured_sha: String,
    pub signed_step: String,
    /// A JSON pointer into the packet's metadata: the newer reading's sha.
    pub reading_sha: String,
    /// A JSON pointer into the packet's metadata: when it was read.
    pub reading_at: String,
    /// Packet-metadata keys the successor carries over.
    pub carry: Vec<String>,
}

/// Parse the rule's args. Every one is a required non-empty string; the
/// two pointers must be pointers, and `carry` is comma-separated.
pub fn declaration(args: &[(String, Value)]) -> Result<Declaration, String> {
    let text = |name: &str| match arg(args, name) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        Some(other) => Err(format!(
            "`{name}` must be a non-empty string, got a {} ({other:?})",
            other.kind()
        )),
        None => Err(format!("`{name}` is not declared on the rule")),
    };
    let pointer = |name: &str| {
        let p = text(name)?;
        if p.starts_with('/') {
            Ok(p)
        } else {
            Err(format!(
                "`{name}` must be a JSON pointer into the packet's metadata, got {p:?}"
            ))
        }
    };
    Ok(Declaration {
        kind: text("kind")?,
        subject: text("subject")?,
        measured_step: text("measured_step")?,
        measured_sha: text("measured_sha")?,
        signed_step: text("signed_step")?,
        reading_sha: pointer("reading_sha")?,
        reading_at: pointer("reading_at")?,
        carry: text("carry")?
            .split(',')
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect(),
    })
}

/// The successor's id, derived from the stale packet's id alone: a
/// redelivered firing re-sends the same packet instead of a second one.
pub fn successor_id(stale: &str) -> String {
    /// A fixed namespace, so every dispatcher derives the same id.
    const SUCCESSOR: uuid::Uuid = uuid::Uuid::from_u128(0x44ca_ee88_62f2_4539_8c01_5bfe_25ed_0001);
    uuid::Uuid::new_v5(&SUCCESSOR, format!("superseded-by:{stale}").as_bytes()).to_string()
}

/// Who takes over from a stale packet.
#[derive(Debug, Clone, PartialEq)]
pub enum Successor {
    /// A newer packet already open on the subject — named, not filed.
    Open(String),
    /// None is open: file this (derived) id.
    File(String),
}

/// One stale, unsigned packet and what it was measured against.
#[derive(Debug, Clone, PartialEq)]
pub struct Stale {
    pub packet: String,
    pub measured: String,
    pub measured_done: DateTime<Utc>,
    pub newer: String,
    pub read_at: DateTime<Utc>,
    pub successor: Successor,
}

/// Why a packet was left alone — each the record's own answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// The sign-off is completed: the packet runs to its own terminal.
    Signed,
    /// No sign-off step on the packet — nothing proves it unsigned.
    NoSignOff,
    /// Not measured yet, or measured without a sha.
    Unmeasured,
    /// No reading, or one naming the measured tree.
    Current,
    /// A different sha, but not provably read after the measurement.
    NotNewer,
    Stale(Stale),
}

fn text(v: &Json, k: &str) -> String {
    v.get(k)
        .and_then(Json::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn instant(v: Option<&Json>) -> Option<DateTime<Utc>> {
    v.and_then(Json::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn step<'a>(job: &'a Json, slug: &str) -> Option<&'a Json> {
    job.get("steps")
        .and_then(Json::as_array)?
        .iter()
        .find(|s| s.get("spec_slug").and_then(Json::as_str) == Some(slug))
}

/// Judge one open packet. `open` is every open packet on the subject,
/// this one included, for the successor already standing. Pure.
pub fn judge(decl: &Declaration, job: &Json, open: &[Json]) -> Verdict {
    let Some(signed) = step(job, &decl.signed_step) else {
        return Verdict::NoSignOff;
    };
    if text(signed, "status") == "completed" {
        return Verdict::Signed;
    }
    let Some(measured) = step(job, &decl.measured_step) else {
        return Verdict::Unmeasured;
    };
    let sha = measured
        .get("metadata")
        .map(|m| text(m, &decl.measured_sha))
        .unwrap_or_default();
    let done = instant(measured.get("completed_at"));
    let (true, false, Some(measured_done)) = (
        text(measured, "status") == "completed",
        sha.is_empty(),
        done,
    ) else {
        return Verdict::Unmeasured;
    };
    let md = job.get("metadata").cloned().unwrap_or(Json::Null);
    let newer = md
        .pointer(&decl.reading_sha)
        .and_then(Json::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if newer.is_empty() || newer == sha {
        return Verdict::Current;
    }
    let Some(read_at) = instant(md.pointer(&decl.reading_at)).filter(|t| *t > measured_done) else {
        return Verdict::NotNewer;
    };
    let id = text(job, "id");
    let opened = instant(job.get("opened_at"));
    let standing = open
        .iter()
        .filter(|o| text(o, "id") != id)
        .filter_map(|o| Some((instant(o.get("opened_at"))?, text(o, "id"))))
        .filter(|(at, _)| opened.is_some_and(|mine| *at > mine))
        .max();
    Verdict::Stale(Stale {
        successor: match standing {
            Some((_, newer_id)) => Successor::Open(newer_id),
            None => Successor::File(successor_id(&id)),
        },
        packet: id,
        measured: sha,
        measured_done,
        newer,
        read_at,
    })
}

/// The successor's create body: the stale packet's request, re-filed.
pub fn successor_body(decl: &Declaration, stale: &Json, id: &str, ctx: &InvocationContext) -> Json {
    let stale_md = stale.get("metadata").cloned().unwrap_or(Json::Null);
    let mut metadata = serde_json::Map::new();
    for key in &decl.carry {
        if let Some(v) = stale_md.get(key) {
            metadata.insert(key.clone(), v.clone());
        }
    }
    metadata.insert(SUPERSEDES_KEY.into(), json!(text(stale, "id")));
    metadata.insert("spawned_by_rule".into(), json!(ctx.rule_name));
    metadata.insert(
        "triggered_by_event_id".into(),
        json!(ctx.triggering_event_id),
    );
    metadata.insert("triggered_by_topic".into(), json!(ctx.triggering_topic));
    let subject_kind = stale
        .pointer("/subject/subject_kind")
        .and_then(Json::as_str)
        .unwrap_or("custom");
    let title = match text(stale, "title") {
        t if t.is_empty() => format!("Superseding {}", text(stale, "id")),
        t => t,
    };
    json!({
        "id": id,
        "kind": decl.kind,
        "subject": {"subject_kind": subject_kind, "id": decl.subject},
        "title": title,
        "owner_id": format!("rule:{}", ctx.rule_name),
        "priority": stale.get("priority").and_then(Json::as_str).unwrap_or("standard"),
        "status": "open",
        "metadata": metadata,
        "tags": ["dispatcher-spawned"],
    })
}

/// The merge onto the stale packet that fires its `superseded` terminal.
pub fn supersession(decl: &Declaration, s: &Stale, successor: &str, rule: &str) -> Json {
    let translation = format!(
        "Superseded by publish packet {successor}, by the dispatcher rule {rule} (design \
         62f27539 C1, backlog 44caee88). This packet's `{m}` step recorded {ms} = {old} at \
         {done}; its reading {rp} names {new}, read at {at}, after that measurement — a newer \
         tree. Its `{signed}` sign-off was not completed, so nothing was signed or published \
         from {old}. The successor measures the newer tree.",
        m = decl.measured_step,
        ms = decl.measured_sha,
        old = s.measured,
        done = s.measured_done.to_rfc3339(),
        rp = decl.reading_sha,
        new = s.newer,
        at = s.read_at.to_rfc3339(),
        signed = decl.signed_step,
    );
    json!({
        "superseded_by": successor,
        "supersession_translation": translation,
    })
}

pub struct JobsSupersedeStaleUnsigned {
    client: boss_core::machine_token::Client,
    jobs_base: String,
}

impl JobsSupersedeStaleUnsigned {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// File the successor, or confirm the one a previous firing filed.
    async fn file(
        &self,
        decl: &Declaration,
        stale: &Json,
        id: &str,
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let body = successor_body(decl, stale, id, ctx);
        let url = format!("{}/api/jobs", self.base());
        if post_json_or_held(&self.client, &url, &body, rule).await? {
            return Ok(());
        }
        // 409: the id is held by an earlier send whose body differed (a
        // firing on another day). Observed, never assumed: the packet
        // must be this kind on this subject before anything names it.
        let held = get_json(&self.client, &format!("{url}/{id}"), rule).await?;
        held_is_the_successor(decl, held, id, &text(stale, "id"), &url)
    }
}

/// Is the packet that answered `GET {url}/{id}` the successor a
/// previous firing filed? The jobs API answers that read with the row
/// BARE, so it is read through `row_or_refuse` (backlog f2eac973): an
/// envelope or an error body is a bad answer, refused Downstream with
/// the read named, never unwrapped into a guess that a kind check
/// would then skip without a word. A row of another kind or subject is
/// refused for good — the id is held by something this rule must not
/// name.
pub fn held_is_the_successor(
    decl: &Declaration,
    held: Json,
    id: &str,
    stale: &str,
    url: &str,
) -> Result<(), HandlerError> {
    let held = row_or_refuse(held, &format!("GET {url}/{id}")).map_err(HandlerError::Downstream)?;
    if text(&held, "kind") == decl.kind
        && held.pointer("/subject/id").and_then(Json::as_str) == Some(decl.subject.as_str())
    {
        Ok(())
    } else {
        Err(HandlerError::Permanent(format!(
            "POST {url} refused the successor id {id} (409), and the packet holding it is \
             not a {} on {} — refusing to name it as {stale}'s successor",
            decl.kind, decl.subject,
        )))
    }
}

#[async_trait]
impl Handler for JobsSupersedeStaleUnsigned {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let decl = declaration(args).map_err(HandlerError::Permanent)?;
        let open: Vec<Json> = open_jobs_of_kind(&self.client, self.base(), &decl.kind, rule)
            .await?
            .into_iter()
            .filter(|j| j.pointer("/subject/id").and_then(Json::as_str) == Some(&decl.subject))
            .collect();
        let mut superseded = 0usize;
        for job in &open {
            let stale = match judge(&decl, job, &open) {
                Verdict::Stale(stale) => stale,
                verdict => {
                    tracing::info!(rule, packet = %text(job, "id"), ?verdict, "{HANDLER}: left alone");
                    continue;
                }
            };
            let successor = match &stale.successor {
                Successor::Open(id) => id.clone(),
                Successor::File(id) => {
                    self.file(&decl, job, id, ctx).await?;
                    id.clone()
                }
            };
            write_json(
                &self.client,
                reqwest::Method::PATCH,
                &format!("{}/api/jobs/{}/metadata", self.base(), stale.packet),
                &supersession(&decl, &stale, &successor, rule),
                rule,
            )
            .await?;
            tracing::info!(
                rule,
                packet = %stale.packet,
                successor = %successor,
                measured = %stale.measured,
                newer = %stale.newer,
                "{HANDLER}: superseded an unsigned packet measured on an older tree"
            );
            superseded += 1;
        }
        tracing::info!(
            rule,
            open = open.len(),
            superseded,
            "{HANDLER}: read the open {} packets on {}",
            decl.kind,
            decl.subject
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::listing_stub;
    use super::*;

    const OLD: &str = "d88cf723f8333985fc425ef7d39696ef620a7104";
    const NEW: &str = "2766f12b4821080f9da339b041ae93da8e8e1cba";
    const STALE: &str = "d91c2b7b-df28-4800-8a2f-e54018ab9f0c";
    const OPEN: &str = "/api/jobs?kind=publish-to-github&status=open";

    fn args() -> Vec<(String, Value)> {
        [
            ("kind", "publish-to-github"),
            ("subject", "github-mirror"),
            ("measured_step", "measure"),
            ("measured_sha", "source_sha"),
            ("signed_step", "approve"),
            ("reading_sha", "/drift_refresh/source_sha"),
            ("reading_at", "/drift_refresh/measured_at"),
            ("carry", "target,area"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
        .collect()
    }

    fn decl() -> Declaration {
        declaration(&args()).unwrap()
    }

    fn ctx() -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: "a-stale-unsigned-publish-is-superseded-daily".into(),
            triggering_event_id: "clock-day:2026-11-16".into(),
            triggering_topic: "clock.day".into(),
            event_payload: json!({"_day": "2026-11-16"}),
        }
    }

    /// d91c2b7b as the record held it on 2026-11-16 before the hand
    /// supersession: measured d88cf723 at 2026-11-09 00:03:55Z, approve
    /// unsigned, and that morning's reading naming 2766f12b.
    fn packet(id: &str, approve: &str, reading: Option<(&str, &str)>) -> Json {
        let mut md = json!({
            "target": "origin/main",
            "area": "platform",
            "decided_2026_11_10": "a hold, which the successor must not inherit",
        });
        if let Some((sha, at)) = reading {
            md["drift_refresh"] = json!({"source_sha": sha, "measured_at": at});
        }
        json!({
            "id": id,
            "kind": "publish-to-github",
            "title": "Publish to the public GitHub mirror",
            "priority": "standard",
            "status": "open",
            "opened_at": "2026-11-09T00:03:40.664202Z",
            "subject": {"subject_kind": "custom", "id": "github-mirror"},
            "metadata": md,
            "steps": [
                {"id": format!("{id}-measure"), "spec_slug": "measure", "status": "completed",
                 "completed_at": "2026-11-09T00:03:55.020315Z",
                 "metadata": {"source_sha": OLD, "scanned_sha": OLD}},
                {"id": format!("{id}-review"), "spec_slug": "review", "status": "ready",
                 "assignee_id": "agent-claude"},
                {"id": format!("{id}-approve"), "spec_slug": "approve", "status": approve,
                 "metadata": {"source_sha": OLD, "scanned_sha": OLD}},
            ],
        })
    }

    fn held() -> Json {
        packet(STALE, "pending", Some((NEW, "2026-11-16T00:06:13Z")))
    }

    #[test]
    fn an_unsigned_packet_whose_reading_names_a_newer_tree_is_stale() {
        let job = held();
        let Verdict::Stale(s) = judge(&decl(), &job, std::slice::from_ref(&job)) else {
            panic!(
                "d91c2b7b on 2026-11-16 was stale: {:?}",
                judge(&decl(), &job, &[])
            );
        };
        assert_eq!(s.packet, STALE);
        assert_eq!(s.measured, OLD);
        assert_eq!(s.newer, NEW);
        assert_eq!(s.successor, Successor::File(successor_id(STALE)));
    }

    /// Once David has signed, the packet runs to its own terminal.
    #[test]
    fn a_signed_packet_is_never_superseded() {
        let job = packet(STALE, "completed", Some((NEW, "2026-11-16T00:06:13Z")));
        assert_eq!(judge(&decl(), &job, &[]), Verdict::Signed);
    }

    #[test]
    fn a_reading_of_the_measured_tree_or_no_reading_is_current() {
        let same = packet(STALE, "ready", Some((OLD, "2026-11-16T00:06:13Z")));
        assert_eq!(judge(&decl(), &same, &[]), Verdict::Current);
        let none = packet(STALE, "ready", None);
        assert_eq!(judge(&decl(), &none, &[]), Verdict::Current);
    }

    /// A different sha read BEFORE the measurement, or at an instant
    /// that does not parse, is no evidence of a newer tree.
    #[test]
    fn a_reading_not_taken_after_the_measurement_is_not_newer() {
        let before = packet(STALE, "ready", Some((NEW, "2026-11-09T00:03:54Z")));
        assert_eq!(judge(&decl(), &before, &[]), Verdict::NotNewer);
        let unreadable = packet(STALE, "ready", Some((NEW, "yesterday")));
        assert_eq!(judge(&decl(), &unreadable, &[]), Verdict::NotNewer);
    }

    /// A packet not yet measured has no tree to be stale against: the
    /// copy rule measures it.
    #[test]
    fn an_unmeasured_packet_is_left_to_the_copy_rule() {
        let mut job = held();
        job["steps"][0]["status"] = json!("ready");
        job["steps"][0]["completed_at"] = Json::Null;
        assert_eq!(judge(&decl(), &job, &[]), Verdict::Unmeasured);
        let mut no_sha = held();
        no_sha["steps"][0]["metadata"] = json!({});
        assert_eq!(judge(&decl(), &no_sha, &[]), Verdict::Unmeasured);
    }

    #[test]
    fn a_packet_without_its_sign_off_step_is_not_proven_unsigned() {
        let mut job = held();
        job["steps"].as_array_mut().unwrap().pop();
        assert_eq!(judge(&decl(), &job, &[]), Verdict::NoSignOff);
    }

    /// A newer packet already open — the successor a previous firing
    /// filed, or one a person filed — is named, never a second filed.
    #[test]
    fn a_newer_open_packet_is_named_as_the_successor() {
        let job = held();
        let mut newer = packet("d2e189f5", "pending", None);
        newer["opened_at"] = json!("2026-11-16T17:03:20Z");
        let Verdict::Stale(s) = judge(&decl(), &job, &[job.clone(), newer]) else {
            panic!("still stale");
        };
        assert_eq!(s.successor, Successor::Open("d2e189f5".into()));
    }

    /// The 409 path reads the packet holding the successor id through
    /// `row_or_refuse` (backlog f2eac973): the bare row of the right
    /// kind and subject is the successor; an envelope or an error body
    /// is a bad answer, refused Downstream with the read named, never
    /// unwrapped into a guess; a row of another kind is refused for good.
    #[test]
    fn the_packet_holding_the_successor_id_is_read_bare_or_refused() {
        let url = "http://jobs/api/jobs";
        let id = successor_id(STALE);
        let row = packet(&id, "pending", None);
        assert!(held_is_the_successor(&decl(), row.clone(), &id, STALE, url).is_ok());

        for body in [
            json!({ "data": row.clone() }),
            json!({ "error": "forbidden" }),
        ] {
            match held_is_the_successor(&decl(), body.clone(), &id, STALE, url) {
                Err(HandlerError::Downstream(why)) => {
                    assert!(why.contains(&format!("GET {url}/{id}")), "{why}");
                    assert!(why.contains("answered no row"), "{why}");
                }
                other => panic!("must refuse {body} as a bad answer: {other:?}"),
            }
        }

        let mut other = row;
        other["kind"] = json!("backlog-item");
        match held_is_the_successor(&decl(), other, &id, STALE, url) {
            Err(HandlerError::Permanent(why)) => assert!(why.contains("409"), "{why}"),
            other => panic!("another kind's packet is not the successor: {other:?}"),
        }
    }

    #[test]
    fn the_successor_id_is_derived_from_the_stale_packet_alone() {
        assert_eq!(successor_id(STALE), successor_id(STALE));
        assert_ne!(successor_id(STALE), successor_id("another"));
        assert!(uuid::Uuid::parse_str(&successor_id(STALE)).is_ok());
    }

    /// The successor is the stale packet's request re-filed: kind,
    /// subject, title and the carried keys, plus `supersedes` — and none
    /// of the stale packet's holds or readings.
    #[test]
    fn the_successor_carries_the_request_and_not_the_hold() {
        let body = successor_body(&decl(), &held(), "new-id", &ctx());
        assert_eq!(body["id"], "new-id");
        assert_eq!(body["kind"], "publish-to-github");
        assert_eq!(body["subject"]["id"], "github-mirror");
        assert_eq!(body["title"], "Publish to the public GitHub mirror");
        assert_eq!(body["metadata"]["target"], "origin/main");
        assert_eq!(body["metadata"]["area"], "platform");
        assert_eq!(body["metadata"][SUPERSEDES_KEY], STALE);
        assert!(body["metadata"].get("decided_2026_11_10").is_none());
        assert!(body["metadata"].get("drift_refresh").is_none());
    }

    #[test]
    fn a_missing_or_malformed_arg_refuses_by_name() {
        for name in ["kind", "measured_step", "signed_step", "carry"] {
            let a: Vec<_> = args().into_iter().filter(|(k, _)| k != name).collect();
            assert!(declaration(&a).unwrap_err().contains(name), "{name}");
        }
        let mut a = args();
        a.retain(|(k, _)| k != "reading_sha");
        a.push((
            "reading_sha".into(),
            Value::String("drift_refresh.source_sha".into()),
        ));
        assert!(declaration(&a).unwrap_err().contains("pointer"));
    }

    /// End to end: the successor is filed BEFORE the stale packet names
    /// it, and the merge sets both keys the `superseded` terminal reads,
    /// the translation naming both shas.
    #[tokio::test]
    async fn a_stale_packet_files_its_successor_then_is_superseded() {
        let stub = listing_stub::serve(vec![(OPEN, json!({"data": [held()], "total": 1}))]).await;
        JobsSupersedeStaleUnsigned::new(stub.base.clone())
            .invoke(&args(), &ctx())
            .await
            .unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert_eq!(sent[0].0, "POST /api/jobs");
        assert_eq!(sent[0].1["id"], successor_id(STALE));
        assert_eq!(sent[1].0, format!("PATCH /api/jobs/{STALE}/metadata"));
        assert_eq!(sent[1].1["superseded_by"], successor_id(STALE));
        let t = sent[1].1["supersession_translation"].as_str().unwrap();
        assert!(t.contains(OLD) && t.contains(NEW), "{t}");
    }

    /// A signed packet and a current one: nothing is written at all.
    #[tokio::test]
    async fn a_signed_or_current_board_writes_nothing() {
        let signed = packet(STALE, "completed", Some((NEW, "2026-11-16T00:06:13Z")));
        let current = packet("other", "ready", Some((OLD, "2026-11-16T00:06:13Z")));
        let stub =
            listing_stub::serve(vec![(OPEN, json!({"data": [signed, current], "total": 2}))]).await;
        JobsSupersedeStaleUnsigned::new(stub.base.clone())
            .invoke(&args(), &ctx())
            .await
            .unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    #[tokio::test]
    async fn a_board_with_no_data_array_refuses_by_name() {
        let stub = listing_stub::serve(vec![(OPEN, listing_stub::no_data_array())]).await;
        listing_stub::assert_refused_by_name(
            JobsSupersedeStaleUnsigned::new(stub.base.clone())
                .invoke(&args(), &ctx())
                .await,
            "GET /api/jobs",
        );
        assert!(stub.writes().is_empty());
    }

    /// The shipped rule: daily, its args parse, it judges the publish
    /// kind on the mirror the daily rule files for, and the reading it
    /// compares is the one the copy rule copies into the measured sha —
    /// so "newer than what was measured" compares like with like.
    #[test]
    fn the_shipped_rule_judges_the_publish_packet_by_the_copy_rules_own_reading() {
        let raw =
            boss_dispatcher::rules::registry::parse_raw_path(boss_testing::dispatcher_rules_dir())
                .expect("parse the shipped rule registry");
        let args_of = |name: &str| -> Vec<(String, Value)> {
            let rule = raw
                .rules
                .iter()
                .find(|r| r.name == name)
                .unwrap_or_else(|| panic!("rule {name} ships"));
            rule.do_steps
                .iter()
                .flat_map(|d| d.args.iter())
                .map(|(k, v)| {
                    (
                        k.clone(),
                        Value::String(v.trim().trim_matches('"').to_string()),
                    )
                })
                .collect()
        };
        let rule = raw
            .rules
            .iter()
            .find(|r| r.do_steps.iter().any(|d| d.handler == HANDLER))
            .expect("a shipped rule invokes this handler");
        let d = declaration(&args_of(&rule.name)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            d.kind,
            boss_dispatcher::rules::helpers_inventory::PUBLISH_JOB_KIND
        );

        let daily = args_of("publish-to-github-daily");
        let daily_arg = |k: &str| {
            daily
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("publish-to-github-daily has no `{k}`"))
        };
        assert_eq!(daily_arg("subject"), Value::String(d.subject.clone()));
        for key in &d.carry {
            daily_arg(&format!("metadata.{key}"));
        }

        // The copy rule's `record` is a quoted JSON map field -> pointer.
        let copy = args_of("publish-measure-lands-the-drift-reading");
        let Some((_, Value::String(record))) = copy.iter().find(|(k, _)| k == "record") else {
            panic!("the copy rule declares `record`");
        };
        let record: Json = serde_json::from_str(&record.replace("\\\"", "\""))
            .unwrap_or_else(|e| panic!("record parses: {e}: {record}"));
        assert_eq!(
            record[d.measured_sha.as_str()].as_str(),
            Some(d.reading_sha.as_str()),
            "the measured sha is copied from the very reading this rule compares against"
        );
        let Some((_, Value::String(at))) = copy.iter().find(|(k, _)| k == "measured_at") else {
            panic!("the copy rule declares `measured_at`");
        };
        assert_eq!(at, &d.reading_at);
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = JobsSupersedeStaleUnsigned::new("http://unused");
        assert_eq!(h.name(), HANDLER);
        assert!(
            crate::cascade::handler_emits().contains_key(HANDLER),
            "the cascade table knows what this handler emits"
        );
    }
}
