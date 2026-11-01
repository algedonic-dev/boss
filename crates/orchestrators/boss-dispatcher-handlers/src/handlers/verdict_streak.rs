//! `maintenance.verdict.streak` — a maintenance chore whose recorded
//! verdict has read the same word on its newest N completed runs files
//! ONE backlog-item naming the packet and the verdict's own line.
//!
//! The gap this closes (backlog ada4d019; review ee6589a9 N1 of car
//! 3cd92f7a). boss-gcp's converge records `deposit_key_verdict` on its
//! `run` step as `clean`, `unconfined` or `unverified`, every half hour.
//! `file-backlog-items-on-boss-gcp-deposit-key-unconfined` fires on
//! `unconfined` only, and rightly: an audit that could not judge the file
//! (a file it could not read, a NUL byte, the converge's 60 s bound) has
//! no finding to file. But nothing else read the word, so an audit that
//! can never judge the file read `unverified` every half hour, forever,
//! and nobody was told — "a check nobody reads is a check that is not
//! running" (CLAUDE.md, Diagnosis). One known way in, measured by that
//! car's builder: the audit's bash option split is quadratic, so a
//! receiver line with a ~200 KB option field outruns the 60 s bound.
//!
//! ONE TICK IS WEATHER, N IN A ROW IS A CONDITION — the estate alarm's
//! rule (`estate_alarm::PERSIST_N`), for the same reason: a home that
//! stalled once, or a converge killed mid-audit, is not a blind audit.
//! The rule declares N; the handler counts.
//!
//! THE STATE IS THE PACKETS. Nothing is held across ticks in this
//! process: on each firing the handler reads the newest page of the
//! firing packet's kind (`GET /api/jobs?kind=` is newest first — the
//! `ORDER BY opened_on DESC, created_at DESC` both stores run) and judges
//! the run steps there. A packet whose step has not completed (the tick
//! still in flight, or one that never reached it) is not a run and is
//! passed over; a COMPLETED step that reads anything else — `clean`,
//! `unconfined`, or no verdict at all — breaks the streak. So any clean
//! tick between unverified ones resets the count, and N-1 files nothing.
//!
//! ## The rule's args
//!
//! - `step` — the slug of the step carrying the verdict (`run`).
//! - `field` / `value` — the step metadata key and the word that, held
//!   on N runs in a row, is the condition.
//! - `n` — how many consecutive completed runs (an integer, 1..=48).
//! - `evidence` — the step metadata key whose text the item copies from
//!   the firing run, so the reader sees the verdict's own line, copied
//!   not retyped.
//! - `route` — the dedup key the item carries as `red_route`, the key the
//!   chore judge dedups on, so one open item stands for the condition.
//! - `title`, `design`, `area` — the item's title prefix and its
//!   `metadata.design` / `metadata.area`.
//!
//! ## What it does
//!
//! 1. Read the firing packet (`job_id` on a step marker, `id` on a
//!    close). A packet already carrying [`STREAK_NOTE`] was judged by an
//!    earlier delivery: nothing more.
//! 2. If its completed step records a word OTHER than `value`, it is the
//!    run that ends a streak: judge each open item of the route
//!    ([`recovery`]) and stop. See "The streak's end" below.
//! 3. Read the newest `2n` packets of its kind and judge the streak
//!    ([`streak`]). The firing packet must be in the window it judges;
//!    a newer completed run judges itself.
//! 4. On a held streak, file ONE backlog-item unless an open one carries
//!    the same `red_route` and does not say it recovered. A route whose
//!    item was CLOSED, or recovered, files again: a recurrence is a new
//!    fact, the chore judge's posture.
//! 5. Note [`STREAK_NOTE`] on the firing packet saying what was judged —
//!    `k of n`, which run broke it, or what was filed — so every
//!    unverified tick's own packet says it was read.
//!
//! ## The streak's end (backlog 3ff17da3)
//!
//! Until v2 of the rule only an unverified tick reached this handler, so
//! the run that BROKE a streak never did, and nothing closed the item:
//! the audit could read clean for weeks under it, and when it went blind
//! again the new streak was folded in as "already open; nothing filed" —
//! a weeks-old item naming the old streak, and no new fact for the
//! reader (review 9c86843a of car 3d96add5; the same class as 52534a15).
//! Now the rule hands over every tick that recorded a verdict, and a run
//! reading otherwise ends the streak. For each open item this judge filed
//! for the route, filed BEFORE that run opened:
//! - nobody has acted on it — no metadata key but the ones it was filed
//!   with, its `triage` step unrouted — so its `triage` completes `stale`,
//!   the evidence citing the run by id, its word, and its own audit line;
//! - someone has, so it is TOLD, through the [`recovery_note`] every
//!   machine alarm uses, and stays theirs to close. Carrying
//!   [`RECOVERED_AT`], it no longer holds a later streak.
//!
//! What was done is noted on the run; a run that found nothing writes
//! nothing.

use super::chore_file_reds::RED_ROUTE;
use super::common::{
    RECOVERED_AT, TRIAGE_SLUG, api_client, complete_step, get_json, jobs_where, owner_for_filing,
    recovery_note, row_or_refuse, rows_or_refuse, withdrawal_fields, write_json,
};
use super::jobs_complete_linked_step::step_by_slug;
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg, arg_string};
use boss_jobs::channels::InputChannel;
use boss_jobs::opened_by::OPENED_BY_KEY;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;

/// The handler's registered name, and the `cleared_by` its closes carry.
const HANDLER: &str = "maintenance.verdict.streak";
/// The note on the firing packet: what this handler judged there.
pub(crate) const STREAK_NOTE: &str = "verdict_streak";
/// The metadata key admission stamps on every packet it creates
/// (boss-jobs `http/jobs.rs`, the create handler), beside
/// [`OPENED_BY_KEY`]: an item carries both before anyone has touched it.
pub(crate) const ADMITTED_AT: &str = "opened_at";
/// The largest `n` a rule may declare: a day of half-hour ticks. The
/// read is `2n` rows with their steps whole, so the bound is the read's.
pub(crate) const MAX_N: i64 = 48;
/// How much of the evidence line the item copies. The converge's own
/// line is `UNVERIFIED: …: <the audit's last line>`, and the audit's last
/// line can carry a receiver line it choked on; the run step keeps the
/// whole of it.
const EVIDENCE_CHARS: usize = 2000;

/// What the newest completed runs say, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Streak {
    /// The newest `n` completed runs ALL read the value: their ids.
    Held(Vec<String>),
    /// `run` consecutive completed runs read it; `broken_by` is the
    /// newest completed run that read anything else, or `None` when
    /// there were simply fewer than `n` completed runs to read.
    Short {
        run: usize,
        broken_by: Option<String>,
    },
}

/// PURE: the streak of `field = value` on `step` over `rows`, which are
/// packets newest first. Only a packet whose step is `completed` is a
/// run; the newest `n` runs are judged and the rest are not read.
pub(crate) fn streak(
    rows: &[serde_json::Value],
    step: &str,
    field: &str,
    value: &str,
    n: usize,
) -> Streak {
    let completed = |row: &&serde_json::Value| {
        step_by_slug(row, step)
            .and_then(|s| s.get("status"))
            .and_then(|v| v.as_str())
            == Some("completed")
    };
    let reads = |row: &serde_json::Value| {
        step_by_slug(row, step)
            .and_then(|s| s.get("metadata"))
            .and_then(|m| m.get(field))
            .and_then(|v| v.as_str())
            == Some(value)
    };
    let id = |row: &serde_json::Value| str_field(row, "id").to_string();
    let window: Vec<&serde_json::Value> = rows.iter().filter(completed).take(n).collect();
    match window.iter().position(|row| !reads(row)) {
        Some(at) => Streak::Short {
            run: at,
            broken_by: Some(id(window[at])),
        },
        None if window.len() == n => Streak::Held(window.iter().map(|r| id(r)).collect()),
        None => Streak::Short {
            run: window.len(),
            broken_by: None,
        },
    }
}

fn str_field<'a>(job: &'a serde_json::Value, key: &str) -> &'a str {
    job.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

/// The rule's args, read once.
struct Args<'a> {
    step: &'a str,
    field: &'a str,
    value: &'a str,
    n: usize,
    evidence: &'a str,
    route: &'a str,
    title: &'a str,
    design: &'a str,
    area: &'a str,
}

fn read_args(args: &[(String, Value)]) -> Result<Args<'_>, HandlerError> {
    let n = match arg(args, "n") {
        Some(Value::Int(n)) if (1..=MAX_N).contains(n) => *n as usize,
        Some(Value::Int(n)) => {
            return Err(HandlerError::Permanent(format!(
                "n must be 1..={MAX_N} consecutive runs, not {n}"
            )));
        }
        Some(other) => {
            return Err(HandlerError::BadArgType {
                arg: "n".into(),
                expected: "int",
                got: other.kind(),
            });
        }
        None => return Err(HandlerError::MissingArg("n".into())),
    };
    let route = arg_string(args, "route")?;
    if route.is_empty() || route.len() > 128 || route.contains(char::is_whitespace) {
        return Err(HandlerError::Permanent(
            "route must be a nonempty key of at most 128 bytes without whitespace".into(),
        ));
    }
    Ok(Args {
        step: arg_string(args, "step")?,
        field: arg_string(args, "field")?,
        value: arg_string(args, "value")?,
        n,
        evidence: arg_string(args, "evidence")?,
        route,
        title: arg_string(args, "title")?,
        design: arg_string(args, "design")?,
        area: arg_string(args, "area")?,
    })
}

/// PURE: the ONE backlog-item a held streak becomes.
fn item_body(
    a: &Args<'_>,
    packet: &serde_json::Value,
    ids: &[String],
    owner: &str,
    ctx: &InvocationContext,
) -> serde_json::Value {
    let packet_id = str_field(packet, "id");
    let kind = str_field(packet, "kind");
    let evidence: String = step_by_slug(packet, a.step)
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get(a.evidence))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .chars()
        .take(EVIDENCE_CHARS)
        .collect();
    let (field, value, n) = (a.field, a.value, a.n);
    let runs: Vec<&str> = ids.iter().map(|i| short(i)).collect();
    json!({
        "kind": "backlog-item",
        "title": format!(
            "{}: {field}={value} on {n} consecutive {kind} runs (newest {})",
            a.title,
            short(packet_id)
        ),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        // The platform owner as the registry answers it (3c23662d).
        "owner_id": owner,
        "priority": "standard",
        "status": "open",
        "tags": [],
        "metadata": {
            "input_channel": super::common::lane_label(InputChannel::Telemetry),
            "area": a.area,
            "design": a.design,
            RED_ROUTE: a.route,
            "streak_field": field,
            "streak_value": value,
            "streak_n": n,
            "streak_packets": ids,
            "streak_evidence": evidence,
            "source": {"kind": "packet", "id": packet_id},
            "reporter": ctx.rule_name,
            "triggered_by_event_id": ctx.triggering_event_id,
            "detail": format!(
                "Filed by {} (backlog ada4d019, design {}): the newest {n} completed {kind} \
                 packets all record {field}={value} on their `{}` step, newest first: {}. The \
                 newest run's {}, copied: {evidence}. One reading is weather; {n} in a row is \
                 a check that is not running. While this item is open a further run files \
                 nothing. The first completed run that reads otherwise ends the streak: it \
                 closes this item `stale`, citing itself, while nobody has acted on it, and \
                 otherwise says here that it recovered; a later streak files a new item \
                 (backlog 3ff17da3).",
                ctx.rule_name,
                a.design,
                a.step,
                runs.join(", "),
                a.evidence,
            ),
        },
    })
}

/// PURE: the word the firing packet's COMPLETED step records under
/// `field`, or none — a step still in flight, or a run that recorded no
/// verdict, is not evidence of either side of a streak.
fn run_word<'a>(packet: &'a serde_json::Value, step: &str, field: &str) -> Option<&'a str> {
    let s = step_by_slug(packet, step)?;
    if s.get("status").and_then(|v| v.as_str()) != Some("completed") {
        return None;
    }
    s.get("metadata")?
        .get(field)?
        .as_str()
        .filter(|w| !w.is_empty())
}

/// PURE: an item already says its condition ended — this judge noted it
/// ([`recovery_note`]) when it could not close it. Such an item is no
/// longer the streak's: a further run neither tells it again nor is
/// folded into it.
fn recovered(item: &serde_json::Value) -> bool {
    item.get("metadata")
        .and_then(|m| m.get(RECOVERED_AT))
        .is_some_and(|v| !v.is_null())
}

/// PURE: the keys an item this judge filed carries while nobody has
/// written on it — its own body's, read off [`item_body`] rather than
/// listed a second time (CLAUDE.md §9a), and the two admission stamps.
fn own_keys(a: &Args<'_>, ctx: &InvocationContext) -> BTreeSet<String> {
    let body = item_body(a, &serde_json::Value::Null, &[], "", ctx);
    body["metadata"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.keys().cloned())
        .chain([ADMITTED_AT.to_string(), OPENED_BY_KEY.to_string()])
        .collect()
}

/// What a run that reads otherwise does to one open item of the route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Recovery {
    /// Nobody has acted on it since the judge filed it: complete its
    /// `triage` step `stale`, the terminal "the claim no longer holds".
    Close { step_id: String },
    /// Someone has: say on it that the condition ended, close nothing.
    Note { why_open: String },
    /// Not this streak's to judge: another route or judge, an item that
    /// already says it recovered, or a run no newer than the item.
    Leave,
}

/// PURE: what the run opened at `run_opened_at` — which reads otherwise —
/// does to `item`, an open backlog-item.
///
/// ONLY ITS OWN ITEM. The route, field and value must be the ones this
/// judge files with, so a hand-filed item that borrowed the route, or
/// another judge's, is left alone.
///
/// ONLY A LATER RUN. The run must have OPENED after the item was filed. A
/// redelivered tick from before the streak was judged says nothing about
/// whether the condition the item names has ended; an order that cannot
/// be read (a timestamp missing or malformed) closes nothing.
///
/// NEVER FROM UNDER A PERSON (backlog 3ff17da3). A closed packet does not
/// reopen (boss-jobs `job_outcome.rs`), so what a person can do to an
/// open item is write on it or route it. Any metadata key beyond
/// [`own_keys`] is someone writing on it; a `triage` step that is active,
/// completed or skipped is someone holding or routing it. Either way the
/// judge only TELLS the item its condition ended — the
/// [`super::common::Retraction::Annotate`] shape — and the person decides.
/// This is stricter than [`super::common::retraction`], which completes a
/// routed item's ready `build` step: that is right for an alarm, whose
/// route a person chose knowing it was a reading, and it is not what this
/// item's packet asked for.
fn recovery(
    item: &serde_json::Value,
    a: &Args<'_>,
    own: &BTreeSet<String>,
    run_opened_at: &str,
) -> Recovery {
    let meta = item.get("metadata");
    let says = |k: &str| meta.and_then(|m| m.get(k)).and_then(|v| v.as_str());
    if says(RED_ROUTE) != Some(a.route)
        || says("streak_field") != Some(a.field)
        || says("streak_value") != Some(a.value)
        || recovered(item)
    {
        return Recovery::Leave;
    }
    let at = |t: &str| chrono::DateTime::parse_from_rfc3339(t).ok();
    match (at(run_opened_at), at(str_field(item, "opened_at"))) {
        (Some(run), Some(filed)) if run > filed => {}
        _ => return Recovery::Leave,
    }
    withdrawal(item, own)
}

/// PURE: how a judge withdraws an item of its OWN whose condition a later
/// run no longer shows — closed `stale` at `triage` while nobody has
/// acted on it, told otherwise. The tail of [`recovery`], shared with the
/// chore judge's finding items (`chore_file_reds`, backlog 52534a15) so
/// "never from under a person" has one definition (CLAUDE.md §9a). The
/// caller has already decided the item is its own and the run is later.
pub(crate) fn withdrawal(item: &serde_json::Value, own: &BTreeSet<String>) -> Recovery {
    let meta = item.get("metadata");
    let written: Vec<&str> = meta
        .and_then(|m| m.as_object())
        .into_iter()
        .flat_map(|m| m.keys())
        .map(String::as_str)
        .filter(|k| !own.contains(*k))
        .collect();
    if !written.is_empty() {
        return Recovery::Note {
            why_open: format!(
                "someone wrote on it after the judge filed it ({}), and the judge closes only \
                 an item nobody has acted on",
                written.join(", ")
            ),
        };
    }
    let triage = item
        .get("steps")
        .and_then(|s| s.as_array())
        .into_iter()
        .flatten()
        .find(|s| s.get("spec_slug").and_then(|v| v.as_str()) == Some(TRIAGE_SLUG));
    let Some(triage) = triage else {
        return Recovery::Note {
            why_open: "it has no `triage` step to close at".into(),
        };
    };
    match triage.get("status").and_then(|v| v.as_str()) {
        Some("completed") | Some("skipped") => Recovery::Note {
            why_open: format!(
                "triage routed it to `{}`, and the judge closes only an item nobody has acted on",
                triage
                    .pointer("/metadata/disposition")
                    .and_then(|v| v.as_str())
                    .unwrap_or("an unrecorded route")
            ),
        },
        Some("active") => Recovery::Note {
            why_open: "its `triage` step is active — someone holds it".into(),
        },
        _ => match triage.get("id").and_then(|v| v.as_str()) {
            Some(id) => Recovery::Close {
                step_id: id.to_string(),
            },
            None => Recovery::Note {
                why_open: "its `triage` step carries no id to close at".into(),
            },
        },
    }
}

/// PURE: the evidence a recovery cites — the run that read otherwise, by
/// id and word, and its own audit line copied, never a time alone.
fn recovery_evidence(
    a: &Args<'_>,
    run: &serde_json::Value,
    word: &str,
    item: &serde_json::Value,
) -> String {
    let streak: Vec<&str> = item
        .pointer("/metadata/streak_packets")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .map(short)
        .collect();
    let line: String = step_by_slug(run, a.step)
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get(a.evidence))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .chars()
        .take(EVIDENCE_CHARS)
        .collect();
    let (field, value, n) = (a.field, a.value, a.n);
    format!(
        "Run {} ({}) completed its `{}` step reading {field}={word}, not {value}, after this \
         item was filed from the streak {}: the condition it names — {field}={value} on {n} \
         consecutive runs — has ended. The run's {}, copied: {line}. A later streak files a \
         new item rather than reopening this one (backlog 3ff17da3).",
        short(str_field(run, "id")),
        str_field(run, "kind"),
        a.step,
        streak.join(", "),
        a.evidence,
    )
}

/// When the condition ended: the recovering run step's own `completed_at`,
/// else the instant the event fired — never the dispatcher's clock
/// ([`InvocationContext::firing_instant`], eabc5943).
fn recovered_instant(
    run: &serde_json::Value,
    step: &str,
    ctx: &InvocationContext,
) -> Result<String, HandlerError> {
    match step_by_slug(run, step)
        .and_then(|s| s.get("completed_at"))
        .and_then(|v| v.as_str())
    {
        Some(at) => Ok(at.to_string()),
        None => Ok(ctx.firing_instant()?.to_rfc3339()),
    }
}

pub struct VerdictStreak {
    client: boss_core::machine_token::Client,
    jobs_base: String,
    /// Who the item is owned by — the platform owner through the port
    /// (backlog 3c23662d), resolved per filing; never a literal.
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl VerdictStreak {
    pub fn new(
        jobs_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Self::with_client(api_client(), jobs_base, owner)
    }

    /// Tests point the client at a local stand-in for jobs-api.
    pub fn with_client(
        client: boss_core::machine_token::Client,
        jobs_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
            owner,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// The open backlog-items carrying `route`. Read by key
    /// (`metadata_has`) rather than walking the whole open backlog — 685
    /// packets on 2026-10-19, with their steps whole — because since v2 of
    /// the rule this runs on every half-hour tick, not only a blind one.
    async fn open_items_of(
        &self,
        route: &str,
        rule: &str,
    ) -> Result<Vec<serde_json::Value>, HandlerError> {
        let open = jobs_where(
            &self.client,
            self.base(),
            &format!("kind=backlog-item&status=open&metadata_has={RED_ROUTE}"),
            rule,
        )
        .await?;
        Ok(open
            .into_iter()
            .filter(|j| {
                j.get("metadata")
                    .and_then(|m| m.get(RED_ROUTE))
                    .and_then(|v| v.as_str())
                    == Some(route)
            })
            .collect())
    }

    /// The streak's end: `run` completed reading `word`, not the value.
    /// Each open item of the route is judged by [`recovery`]; what was
    /// done is noted on the run, and a run that found nothing to do
    /// writes nothing — 47 clean ticks a day are not 47 writes.
    ///
    /// Idempotent under redelivery: a closed item is no longer open, and
    /// a noted one carries [`RECOVERED_AT`], so a second delivery finds
    /// nothing; the item is written BEFORE the run's note, so a delivery
    /// that failed between the two retries the item, never skips it.
    async fn recover(
        &self,
        a: &Args<'_>,
        run: &serde_json::Value,
        word: &str,
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let open = self.open_items_of(a.route, rule).await?;
        let own = own_keys(a, ctx);
        let mut done: Vec<String> = Vec::new();
        for item in &open {
            let item_id = str_field(item, "id");
            let decision = recovery(item, a, &own, str_field(run, "opened_at"));
            if decision == Recovery::Leave {
                continue;
            }
            let evidence = recovery_evidence(a, run, word, item);
            let at = recovered_instant(run, a.step, ctx)?;
            match decision {
                Recovery::Close { step_id } => {
                    complete_step(
                        &self.client,
                        self.base(),
                        item_id,
                        &step_id,
                        withdrawal_fields(&evidence, HANDLER, &at),
                        rule,
                    )
                    .await?;
                    done.push(format!("closed backlog-item {} stale", short(item_id)));
                }
                Recovery::Note { why_open } => {
                    write_json(
                        &self.client,
                        reqwest::Method::PATCH,
                        &format!("{}/api/jobs/{item_id}/metadata", self.base()),
                        &serde_json::Value::Object(recovery_note(
                            &evidence, HANDLER, &at, &why_open,
                        )),
                        rule,
                    )
                    .await?;
                    done.push(format!(
                        "told backlog-item {} it recovered and left it open: {why_open}",
                        short(item_id)
                    ));
                }
                Recovery::Leave => {}
            }
        }
        if done.is_empty() {
            return Ok(());
        }
        let (field, value) = (a.field, a.value);
        tracing::info!(rule = %rule, packet = %str_field(run, "id"), "{field}={word} ends a {value} streak: {}", done.join("; "));
        self.note(
            str_field(run, "id"),
            format!(
                "{rule}: this run reads {field}={word}, not {value}, which ends the streak; {}",
                done.join("; ")
            ),
            rule,
        )
        .await
    }

    async fn note(&self, id: &str, text: String, rule: &str) -> Result<(), HandlerError> {
        write_json(
            &self.client,
            reqwest::Method::PATCH,
            &format!("{}/api/jobs/{id}/metadata", self.base()),
            &json!({ STREAK_NOTE: text }),
            rule,
        )
        .await
    }
}

#[async_trait]
impl Handler for VerdictStreak {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let a = read_args(args)?;
        let rule = ctx.rule_name.as_str();
        // A malformed marker is not something a redelivery can fix.
        let Some(packet_id) = ["job_id", "id"]
            .iter()
            .find_map(|k| ctx.event_payload.get(*k).and_then(|v| v.as_str()))
        else {
            return Ok(());
        };

        // 1. The firing packet; noted by an earlier delivery means the
        //    record is true.
        let packet = get_json(
            &self.client,
            &format!("{}/api/jobs/{packet_id}", self.base()),
            rule,
        )
        .await?;
        let packet = row_or_refuse(packet, &format!("GET /api/jobs/{packet_id}"))
            .map_err(HandlerError::Downstream)?;
        if packet
            .get("metadata")
            .and_then(|m| m.get(STREAK_NOTE))
            .is_some()
        {
            return Ok(());
        }
        let kind = str_field(&packet, "kind");

        // 2. A run that reads otherwise ends the streak, and its item
        //    with it (backlog 3ff17da3).
        if let Some(word) = run_word(&packet, a.step, a.field)
            && word != a.value
        {
            return self.recover(&a, &packet, word, ctx).await;
        }

        // 3. The newest runs of the same kind, steps whole. `2n` rows
        //    leave room for packets that are not runs (one in flight).
        let listing = get_json(
            &self.client,
            &format!(
                "{}/api/jobs?kind={kind}&full=true&limit={}",
                self.base(),
                2 * a.n
            ),
            rule,
        )
        .await?;
        let rows: Vec<serde_json::Value> =
            rows_or_refuse(&listing, &format!("GET /api/jobs?kind={kind}"))
                .map_err(HandlerError::Downstream)?;
        let (field, value, n) = (a.field, a.value, a.n);
        let ids = match streak(&rows, a.step, field, value, n) {
            Streak::Held(ids) if ids.iter().any(|i| i == packet_id) => ids,
            Streak::Held(ids) => {
                let newest = ids.first().map(String::as_str).unwrap_or("");
                return self
                    .note(
                        packet_id,
                        format!(
                            "{rule}: nothing filed — a newer completed run ({}) judges the \
                             newest {n}, and this packet is not among them",
                            short(newest)
                        ),
                        rule,
                    )
                    .await;
            }
            Streak::Short { run, broken_by } => {
                let why = match broken_by {
                    Some(id) => format!("run {} reads otherwise", short(&id)),
                    None => "fewer completed runs than that to read".to_string(),
                };
                tracing::info!(rule = %rule, packet = %packet_id, run, n, "{field}={value} streak short; nothing filed");
                return self
                    .note(
                        packet_id,
                        format!(
                            "{rule}: {run} of {n} newest completed {kind} runs read \
                             {field}={value} ({why}); nothing filed"
                        ),
                        rule,
                    )
                    .await;
            }
        };

        // 4. One item while it is open — and while it still stands for
        //    a streak: an item that says it recovered is not this one's.
        let open = self.open_items_of(a.route, rule).await?;
        let filed = if open.iter().any(|item| !recovered(item)) {
            None
        } else {
            let owner = owner_for_filing(self.owner.as_ref(), rule).await;
            let body = item_body(&a, &packet, &ids, &owner, ctx);
            Some(
                super::common::post_json_minted_id(
                    &self.client,
                    &format!("{}/api/jobs", self.base()),
                    &body,
                    rule,
                )
                .await?,
            )
        };

        // 5. The note on the firing packet.
        let what = match &filed {
            Some(id) => format!("filed backlog-item {} for {}", short(id), a.route),
            None => format!("an item for {} is already open; nothing filed", a.route),
        };
        tracing::info!(rule = %rule, packet = %packet_id, n, filed = filed.is_some(), "{field}={value} on {n} consecutive runs");
        self.note(
            packet_id,
            format!("{rule}: {n} of {n} newest completed {kind} runs read {field}={value}; {what}"),
            rule,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::Path, extract::Query, routing::get};
    use std::collections::HashMap;
    use std::sync::Mutex;

    const KIND: &str = "maintenance-boss-gcp-converge";
    const RULE_FILE: &str = "file-backlog-items-on-boss-gcp-deposit-key-unverified";
    const AUDIT: &str = "UNVERIFIED: the audit did not reach a verdict (status 124): \
                         deposit-key-audit: reading /home/david/.ssh/authorized_keys";

    const CLEAN_AUDIT: &str = "deposit-key-audit: clean — 4 line(s) in \
                               /home/david/.ssh/authorized_keys force a root door";

    fn id(i: usize) -> String {
        format!("33333333-3333-3333-3333-{i:012}")
    }

    /// When tick `i` opened: `i` half hours before noon, so `ticks()`
    /// lists them newest first. A NEGATIVE offset is a later tick.
    fn opened(half_hours_before_noon: i64) -> String {
        (chrono::DateTime::parse_from_rfc3339("2026-10-18T12:00:00Z").unwrap()
            - chrono::Duration::minutes(30 * half_hours_before_noon))
        .to_rfc3339()
    }

    /// A converge packet as the jobs API lists it with `full=true`: the
    /// run step completed carrying `verdict`, or still in flight on `None`.
    fn tick(i: usize, verdict: Option<&str>) -> serde_json::Value {
        tick_at(i, i as i64, verdict)
    }

    /// Tick `i`, opened `at` half hours before noon.
    fn tick_at(i: usize, at: i64, verdict: Option<&str>) -> serde_json::Value {
        let audit = if verdict == Some("unverified") {
            AUDIT
        } else {
            CLEAN_AUDIT
        };
        let run = match verdict {
            Some(v) => {
                json!({ "id": format!("s-run-{i}"), "spec_slug": "run", "status": "completed",
                "completed_at": opened(at),
                "metadata": { "result": "ok", "deposit_key_verdict": v, "deposit_key_audit": audit } })
            }
            None => json!({ "id": format!("s-run-{i}"), "spec_slug": "run", "status": "active",
                "metadata": {} }),
        };
        json!({
            "id": id(i), "kind": KIND, "status": if verdict.is_some() { "closed" } else { "open" },
            "opened_at": opened(at),
            "title": "boss-gcp adopts its own units — 2026-10-18",
            "metadata": {},
            "steps": [
                { "id": format!("s-sched-{i}"), "spec_slug": "scheduled", "status": "completed", "metadata": {} },
                run,
            ],
        })
    }

    /// Newest first, as the listing serves them: `verdicts[0]` is newest.
    fn ticks(verdicts: &[Option<&str>]) -> Vec<serde_json::Value> {
        verdicts
            .iter()
            .enumerate()
            .map(|(i, v)| tick(i + 1, *v))
            .collect()
    }

    fn judge(verdicts: &[Option<&str>]) -> Streak {
        streak(
            &ticks(verdicts),
            "run",
            "deposit_key_verdict",
            "unverified",
            3,
        )
    }

    const U: Option<&str> = Some("unverified");
    const C: Option<&str> = Some("clean");

    // -- the pure count: N fires, N-1 does not, a clean tick resets ------

    #[test]
    fn n_unverified_runs_in_a_row_hold_the_streak() {
        assert_eq!(judge(&[U, U, U]), Streak::Held(vec![id(1), id(2), id(3)]));
        // Older runs past the window are not read, whatever they say.
        assert_eq!(
            judge(&[U, U, U, C, C]),
            Streak::Held(vec![id(1), id(2), id(3)])
        );
    }

    #[test]
    fn n_minus_one_unverified_runs_are_short() {
        assert_eq!(
            judge(&[U, U]),
            Streak::Short {
                run: 2,
                broken_by: None
            }
        );
        assert_eq!(
            judge(&[U, U, C]),
            Streak::Short {
                run: 2,
                broken_by: Some(id(3))
            }
        );
    }

    #[test]
    fn any_clean_run_between_resets_the_count() {
        assert_eq!(
            judge(&[U, C, U, U, U]),
            Streak::Short {
                run: 1,
                broken_by: Some(id(2))
            }
        );
        assert_eq!(
            judge(&[U, U, Some("unconfined"), U]),
            Streak::Short {
                run: 2,
                broken_by: Some(id(3))
            }
        );
    }

    #[test]
    fn a_run_with_no_verdict_breaks_the_streak_and_one_in_flight_is_passed_over() {
        let mut rows = ticks(&[U, U, U]);
        rows[1]["steps"][1]["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("deposit_key_verdict");
        assert_eq!(
            streak(&rows, "run", "deposit_key_verdict", "unverified", 3),
            Streak::Short {
                run: 1,
                broken_by: Some(id(2))
            },
            "a completed run that recorded no verdict is not evidence of one"
        );
        // The next tick already opened, its run not yet done: not a run.
        assert_eq!(
            judge(&[None, U, U, U]),
            Streak::Held(vec![id(2), id(3), id(4)])
        );
    }

    // -- the handler, through the authored rule --------------------------

    type Writes = Arc<Mutex<Vec<(String, String, serde_json::Value)>>>;
    type Rows = Arc<Mutex<Vec<serde_json::Value>>>;

    /// What admission does to a filed item that the handler's body does
    /// not: stamps `opened_at` (top level and in metadata, jobs.rs) and
    /// `opened_by` (opened_by.rs), and opens the workflow's steps — here
    /// only the `triage` step a machine withdraws at. It is filed a
    /// minute after the newest row it can see, as a real filing follows
    /// the run that fired it.
    fn admit(mut row: serde_json::Value, id: &str, all: &[serde_json::Value]) -> serde_json::Value {
        let newest = all
            .iter()
            .filter_map(|j| j["opened_at"].as_str())
            .filter_map(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .max()
            .unwrap_or_else(|| chrono::DateTime::parse_from_rfc3339(&opened(0)).unwrap());
        let at = (newest + chrono::Duration::minutes(1)).to_rfc3339();
        row["id"] = json!(id);
        row["opened_at"] = json!(at);
        row["metadata"]["opened_at"] = json!(at);
        row["metadata"]["opened_by"] = json!(format!("automation:rule:{RULE_FILE}"));
        row["steps"] = json!([
            { "id": format!("{id}-filed"), "spec_slug": "filed", "status": "completed", "metadata": {} },
            { "id": format!("{id}-triage"), "spec_slug": "triage", "status": "ready", "metadata": {} },
        ]);
        row
    }

    /// Stand-in for jobs-api: `GET /api/jobs/{id}`, `GET /api/jobs` newest
    /// first honouring `kind`, `status`, `metadata_has` and `limit`, `POST
    /// /api/jobs` minting an id and admitting the row open ([`admit`]),
    /// `PATCH …/metadata` merging, and the step merge door plus the
    /// completion PUT — a `triage` completed `stale` closes the item, as
    /// the backlog-item workflow routes it — so a second delivery reads
    /// what the first wrote.
    async fn mock_jobs(jobs: Vec<serde_json::Value>) -> (String, Writes, Rows) {
        let writes: Writes = Arc::new(Mutex::new(Vec::new()));
        let rows: Rows = Arc::new(Mutex::new(jobs));
        let (g, l, p, m) = (rows.clone(), rows.clone(), rows.clone(), rows.clone());
        let (sm, sp) = (rows.clone(), rows.clone());
        let (wp, wm) = (writes.clone(), writes.clone());
        let (wsm, wsp) = (writes.clone(), writes.clone());
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let rows = g.clone();
                    async move {
                        rows.lock()
                            .unwrap()
                            .iter()
                            .find(|j| j["id"] == json!(id))
                            .cloned()
                            .map(Json)
                            .ok_or(axum::http::StatusCode::NOT_FOUND)
                    }
                }),
            )
            .route(
                "/api/jobs",
                get(move |Query(q): Query<HashMap<String, String>>| {
                    let rows = l.clone();
                    async move {
                        let matched: Vec<serde_json::Value> = rows
                            .lock()
                            .unwrap()
                            .iter()
                            .filter(|j| {
                                q.get("kind").is_none_or(|k| j["kind"] == json!(k))
                                    && q.get("status").is_none_or(|s| j["status"] == json!(s))
                                    && q.get("metadata_has")
                                        .is_none_or(|key| j["metadata"].get(key).is_some())
                            })
                            .cloned()
                            .collect();
                        let limit = q
                            .get("limit")
                            .and_then(|l| l.parse().ok())
                            .unwrap_or(usize::MAX);
                        let page: Vec<_> = matched.iter().take(limit).cloned().collect();
                        Json(json!({ "data": page, "total": matched.len() }))
                    }
                })
                .post(move |Json(body): Json<serde_json::Value>| {
                    let (w, rows) = (wp.clone(), p.clone());
                    async move {
                        w.lock()
                            .unwrap()
                            .push(("POST".into(), "/api/jobs".into(), body.clone()));
                        let mut all = rows.lock().unwrap();
                        let id = format!("f0000000-0000-0000-0000-{:012}", all.len() + 1);
                        let row = admit(body, &id, &all);
                        all.push(row);
                        (axum::http::StatusCode::CREATED, Json(json!({ "id": id })))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(
                    move |Path(id): Path<String>, Json(body): Json<serde_json::Value>| {
                        let (w, rows) = (wm.clone(), m.clone());
                        async move {
                            w.lock().unwrap().push((
                                "PATCH".into(),
                                format!("/api/jobs/{id}/metadata"),
                                body.clone(),
                            ));
                            if let Some(job) = rows
                                .lock()
                                .unwrap()
                                .iter_mut()
                                .find(|j| j["id"] == json!(id))
                                && let (Some(meta), Some(b)) =
                                    (job["metadata"].as_object_mut(), body.as_object())
                            {
                                for (k, v) in b {
                                    meta.insert(k.clone(), v.clone());
                                }
                            }
                            axum::http::StatusCode::NO_CONTENT
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}/metadata",
                axum::routing::patch(
                    move |Path((id, sid)): Path<(String, String)>,
                          Json(body): Json<serde_json::Value>| {
                        let (w, rows) = (wsm.clone(), sm.clone());
                        async move {
                            w.lock().unwrap().push((
                                "PATCH".into(),
                                format!("/api/jobs/{id}/steps/{sid}/metadata"),
                                body.clone(),
                            ));
                            let mut all = rows.lock().unwrap();
                            let step = all
                                .iter_mut()
                                .find(|j| j["id"] == json!(id))
                                .and_then(|j| j["steps"].as_array_mut())
                                .and_then(|s| s.iter_mut().find(|s| s["id"] == json!(sid)));
                            if let (Some(step), Some(b)) = (step, body.as_object()) {
                                for (k, v) in b {
                                    step["metadata"][k] = v.clone();
                                }
                            }
                            axum::http::StatusCode::NO_CONTENT
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                axum::routing::put(
                    move |Path((id, sid)): Path<(String, String)>,
                          Json(body): Json<serde_json::Value>| {
                        let (w, rows) = (wsp.clone(), sp.clone());
                        async move {
                            w.lock().unwrap().push((
                                "PUT".into(),
                                format!("/api/jobs/{id}/steps/{sid}"),
                                body.clone(),
                            ));
                            let mut all = rows.lock().unwrap();
                            if let Some(job) = all.iter_mut().find(|j| j["id"] == json!(id)) {
                                let stale = job["steps"]
                                    .as_array_mut()
                                    .and_then(|s| s.iter_mut().find(|s| s["id"] == json!(sid)))
                                    .map(|step| {
                                        step["status"] = body["status"].clone();
                                        step["metadata"]["disposition"] == json!("stale")
                                    })
                                    .unwrap_or(false);
                                if stale && body["status"] == json!("completed") {
                                    job["status"] = json!("closed");
                                }
                            }
                            axum::http::StatusCode::NO_CONTENT
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), writes, rows)
    }

    /// Deliver the newest tick's `run` marker through the AUTHORED rule
    /// file and the registered handler, `times` times.
    async fn deliver(base: &str, packet: &str, verdict: &str, times: usize) {
        use boss_dispatcher::rules::{
            expr::NoHelpers,
            handler::{HandlerRegistry, dispatch},
            registry::{Registry, match_event},
        };
        let rule = std::fs::read_to_string(
            boss_testing::repo_root().join(format!("infra/dispatcher/rules/{RULE_FILE}.toml")),
        )
        .unwrap();
        let registry = Registry::from_toml(&rule).unwrap();
        let mut handlers = HandlerRegistry::new();
        handlers.register(VerdictStreak::with_client(
            api_client(),
            base,
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        ));
        let payload = json!({
            "job_id": packet, "step_id": "s-run", "kind": "task",
            "subject_kind": "custom", "subject_id": "infra/maintenance-boss-gcp-converge",
            "workflow_kind": KIND, "completed_on": "2026-10-18", "notify_on_done": false,
            "spec_slug": "run",
            "metadata": {"result": "ok", "deposit_key_verdict": verdict, "deposit_key_audit": AUDIT},
        });
        for _ in 0..times {
            let matched = match_event(&registry, "step.done.task", &payload, &NoHelpers).matched;
            assert_eq!(matched.len(), 1, "the authored rule selects the tick");
            let result = dispatch(
                &matched,
                &handlers,
                "event-step",
                "step.done.task",
                &payload,
                None,
            )
            .await
            .unwrap();
            assert!(result[0].outcome.is_ok(), "{result:?}");
        }
    }

    fn posts(w: &[(String, String, serde_json::Value)]) -> Vec<serde_json::Value> {
        w.iter()
            .filter(|(m, _, _)| m == "POST")
            .map(|(_, _, b)| b.clone())
            .collect()
    }

    fn note_on(w: &[(String, String, serde_json::Value)], packet: &str) -> String {
        w.iter()
            .filter(|(m, p, _)| m == "PATCH" && p == &format!("/api/jobs/{packet}/metadata"))
            .filter_map(|(_, _, b)| b[STREAK_NOTE].as_str().map(str::to_string))
            .next_back()
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn the_nth_unverified_tick_files_one_item_naming_the_packet_and_the_audit_line() {
        let (base, writes, _) = mock_jobs(ticks(&[U, U, U, C])).await;
        // The newest tick, delivered twice (JetStream is at-least-once).
        deliver(&base, &id(1), "unverified", 2).await;
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(filed.len(), 1, "one item for the streak: {w:?}");
        let item = &filed[0];
        assert_eq!(item["kind"], "backlog-item");
        assert_eq!(item["owner_id"], "emp-owner");
        let meta = &item["metadata"];
        assert_eq!(meta[RED_ROUTE], "boss-gcp-converge/deposit-key-unverified");
        assert_eq!(meta["area"], "credentials");
        assert_eq!(meta["streak_n"], 3);
        assert_eq!(meta["streak_packets"], json!([id(1), id(2), id(3)]));
        assert_eq!(
            meta["streak_evidence"], AUDIT,
            "the audit's own line, copied"
        );
        assert_eq!(meta["source"], json!({"kind": "packet", "id": id(1)}));
        assert_eq!(meta["input_channel"], "telemetry/monitoring");
        let title = item["title"].as_str().unwrap();
        assert!(
            title.contains("deposit_key_verdict=unverified on 3 consecutive"),
            "{title}"
        );
        assert!(
            title.contains(&id(1)[..8]),
            "the title names the packet: {title}"
        );
        assert!(
            note_on(&w, &id(1)).contains("3 of 3"),
            "the packet says it was read: {w:?}"
        );
    }

    #[tokio::test]
    async fn n_minus_one_unverified_ticks_file_nothing_and_say_so() {
        let (base, writes, _) = mock_jobs(ticks(&[U, U, C, U, U, U])).await;
        deliver(&base, &id(1), "unverified", 1).await;
        let w = writes.lock().unwrap().clone();
        assert!(posts(&w).is_empty(), "two in a row is weather: {w:?}");
        let note = note_on(&w, &id(1));
        assert!(note.contains("2 of 3"), "{note}");
        assert!(
            note.contains(&id(3)[..8]),
            "the note names the run that broke it: {note}"
        );
    }

    #[tokio::test]
    async fn an_open_item_for_the_route_holds_the_next_unverified_tick() {
        let mut rows = ticks(&[U, U, U, U]);
        rows.push(json!({
            "id": "a0000000-0000-0000-0000-000000000001", "kind": "backlog-item",
            "status": "open", "steps": [],
            "metadata": { RED_ROUTE: "boss-gcp-converge/deposit-key-unverified" },
        }));
        let (base, writes, _) = mock_jobs(rows).await;
        deliver(&base, &id(1), "unverified", 1).await;
        let w = writes.lock().unwrap().clone();
        assert!(posts(&w).is_empty(), "one item while it is open: {w:?}");
        assert!(note_on(&w, &id(1)).contains("already open"), "{w:?}");
    }

    // -- the streak's end: a run that reads otherwise (backlog 3ff17da3) --

    /// Put a newer tick at the head of the listing, as the next half hour
    /// would.
    fn arrive(rows: &Rows, tick: serde_json::Value) {
        rows.lock().unwrap().insert(0, tick);
    }

    fn row(rows: &Rows, id: &str) -> serde_json::Value {
        rows.lock()
            .unwrap()
            .iter()
            .find(|j| j["id"] == json!(id))
            .cloned()
            .unwrap()
    }

    fn triage_of(item: &serde_json::Value) -> serde_json::Value {
        item["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["spec_slug"] == "triage")
            .cloned()
            .unwrap()
    }

    /// Three unverified ticks file X; it is X's id.
    async fn file_one(base: &str, writes: &Writes, rows: &Rows) -> String {
        deliver(base, &id(1), "unverified", 1).await;
        assert_eq!(posts(&writes.lock().unwrap()).len(), 1);
        rows.lock()
            .unwrap()
            .iter()
            .find(|j| j["kind"] == "backlog-item")
            .and_then(|j| j["id"].as_str().map(str::to_string))
            .unwrap()
    }

    #[tokio::test]
    async fn the_run_that_reads_clean_closes_the_untouched_item_stale_citing_itself() {
        let (base, writes, rows) = mock_jobs(ticks(&[U, U, U])).await;
        let x = file_one(&base, &writes, &rows).await;
        assert_eq!(row(&rows, &x)["status"], "open");

        // The audit recovers: the next half hour reads clean. Delivered
        // twice — JetStream is at-least-once.
        arrive(&rows, tick_at(10, -1, C));
        deliver(&base, &id(10), "clean", 2).await;

        let item = row(&rows, &x);
        assert_eq!(item["status"], "closed", "{item}");
        let triage = triage_of(&item);
        assert_eq!(triage["status"], "completed");
        let meta = &triage["metadata"];
        assert_eq!(meta["disposition"], "stale");
        assert_eq!(meta["cleared_by"], "maintenance.verdict.streak");
        assert_eq!(meta[RECOVERED_AT], json!(opened(-1)), "the run's own time");
        let evidence = meta["evidence"].as_str().unwrap();
        for want in [&id(10)[..8], "deposit_key_verdict=clean", CLEAN_AUDIT] {
            assert!(
                evidence.contains(want),
                "the close cites the run that recovered ({want}): {evidence}"
            );
        }
        let w = writes.lock().unwrap().clone();
        let puts = w.iter().filter(|(m, _, _)| m == "PUT").count();
        assert_eq!(puts, 1, "closed once, however often it is delivered: {w:?}");
        assert!(
            note_on(&w, &id(10)).contains(&x[..8]),
            "the run says what it closed: {w:?}"
        );
    }

    #[tokio::test]
    async fn a_later_streak_files_a_new_item_rather_than_reviving_the_closed_one() {
        let (base, writes, rows) = mock_jobs(ticks(&[U, U, U])).await;
        let x = file_one(&base, &writes, &rows).await;
        arrive(&rows, tick_at(10, -1, C));
        deliver(&base, &id(10), "clean", 1).await;
        // Blind again, three half hours running.
        for (i, at) in [(11, -2), (12, -3), (13, -4)] {
            arrive(&rows, tick_at(i, at, U));
            deliver(&base, &id(i), "unverified", 1).await;
        }
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(filed.len(), 2, "the recurrence is a new fact: {w:?}");
        assert_eq!(
            filed[1]["metadata"]["streak_packets"],
            json!([id(13), id(12), id(11)]),
            "the new item names the new streak"
        );
        assert_eq!(row(&rows, &x)["status"], "closed", "X stays closed");
    }

    #[tokio::test]
    async fn an_item_a_person_annotated_is_told_it_recovered_and_left_open() {
        let (base, writes, rows) = mock_jobs(ticks(&[U, U, U])).await;
        let x = file_one(&base, &writes, &rows).await;
        // A person wrote on it after the judge filed it.
        rows.lock()
            .unwrap()
            .iter_mut()
            .find(|j| j["id"] == json!(x))
            .unwrap()["metadata"]["coordinator_cycle_801"] = json!("looking at the audit bound");
        arrive(&rows, tick_at(10, -1, C));
        deliver(&base, &id(10), "clean", 2).await;

        let item = row(&rows, &x);
        assert_eq!(item["status"], "open", "never closed from under a person");
        assert_eq!(triage_of(&item)["status"], "ready");
        assert_eq!(item["metadata"][RECOVERED_AT], json!(opened(-1)));
        let recovery = item["metadata"]["recovery"].as_str().unwrap();
        assert!(recovery.contains(&id(10)[..8]), "{recovery}");
        assert!(recovery.contains("coordinator_cycle_801"), "{recovery}");
        let w = writes.lock().unwrap().clone();
        let noted = w
            .iter()
            .filter(|(m, p, _)| m == "PATCH" && p == &format!("/api/jobs/{x}/metadata"))
            .count();
        assert_eq!(noted, 1, "told once: {w:?}");

        // Its condition is over, so it no longer holds a new streak.
        for (i, at) in [(11, -2), (12, -3), (13, -4)] {
            arrive(&rows, tick_at(i, at, U));
            deliver(&base, &id(i), "unverified", 1).await;
        }
        let w = writes.lock().unwrap().clone();
        assert_eq!(posts(&w).len(), 2, "a new streak, a new item: {w:?}");
    }

    #[tokio::test]
    async fn an_item_a_person_triaged_is_told_and_left_open() {
        let (base, writes, rows) = mock_jobs(ticks(&[U, U, U])).await;
        let x = file_one(&base, &writes, &rows).await;
        {
            let mut all = rows.lock().unwrap();
            let item = all.iter_mut().find(|j| j["id"] == json!(x)).unwrap();
            item["steps"][1]["status"] = json!("completed");
            item["steps"][1]["metadata"]["disposition"] = json!("build");
        }
        arrive(&rows, tick_at(10, -1, C));
        deliver(&base, &id(10), "clean", 1).await;
        let item = row(&rows, &x);
        assert_eq!(item["status"], "open");
        let recovery = item["metadata"]["recovery"].as_str().unwrap();
        assert!(recovery.contains("build"), "{recovery}");
        let w = writes.lock().unwrap().clone();
        assert!(!w.iter().any(|(m, _, _)| m == "PUT"), "{w:?}");
    }

    #[tokio::test]
    async fn a_clean_run_older_than_the_item_closes_nothing() {
        // A redelivered tick from before the streak was judged is not
        // evidence that the condition the item names has ended.
        let mut rows = ticks(&[U, U, U]);
        rows.push(tick_at(10, 4, C));
        let (base, writes, rows) = mock_jobs(rows).await;
        let x = file_one(&base, &writes, &rows).await;
        deliver(&base, &id(10), "clean", 1).await;
        assert_eq!(row(&rows, &x)["status"], "open");
        let w = writes.lock().unwrap().clone();
        assert!(note_on(&w, &id(10)).is_empty(), "{w:?}");
    }

    #[tokio::test]
    async fn a_clean_run_with_nothing_open_writes_nothing() {
        let (base, writes, _) = mock_jobs(ticks(&[C, U, U])).await;
        deliver(&base, &id(1), "clean", 1).await;
        let w = writes.lock().unwrap().clone();
        assert!(
            w.is_empty(),
            "48 clean ticks a day are not 48 writes: {w:?}"
        );
    }

    #[tokio::test]
    async fn an_open_item_of_another_route_or_judge_is_not_touched() {
        let mut rows = ticks(&[U, U, U]);
        rows.push(json!({
            "id": "a0000000-0000-0000-0000-000000000009", "kind": "backlog-item",
            "status": "open", "opened_at": opened(5),
            "steps": [{ "id": "t", "spec_slug": "triage", "status": "ready", "metadata": {} }],
            "metadata": { RED_ROUTE: "boss-gcp-converge/deposit-key-unconfined" },
        }));
        let (base, writes, rows) = mock_jobs(rows).await;
        arrive(&rows, tick_at(10, -1, C));
        deliver(&base, &id(10), "clean", 1).await;
        let w = writes.lock().unwrap().clone();
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn n_outside_its_bound_or_untyped_is_refused_permanently() {
        let base: Vec<(String, Value)> = [
            ("step", "run"),
            ("field", "f"),
            ("value", "v"),
            ("evidence", "e"),
            ("route", "r/x"),
            ("title", "t"),
            ("design", "d"),
            ("area", "a"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.into())))
        .collect();
        for n in [
            Value::Int(0),
            Value::Int(MAX_N + 1),
            Value::String("3".into()),
        ] {
            let mut args = base.clone();
            args.push(("n".into(), n.clone()));
            let err = read_args(&args).err().expect("refused");
            assert!(err.is_permanent(), "{n:?}: {err}");
        }
        let mut args = base.clone();
        args.push(("n".into(), Value::Int(3)));
        assert!(read_args(&args).is_ok());
    }
}
