//! `maintenance.chore.file_reds` — a chore that closed red opens one
//! backlog-item per red route it recorded, or ONE for the night when
//! every red route shares a cause (the shell down, one HTTP status).
//!
//! The gap this closes (backlog ac3270c7, design 0e07ce64, 2026-09-18).
//! The nightly playground crawl records its verdict on its `run` step
//! through boss-chore.sh — `result=failed` and the check's own output,
//! one `RED <route> <kind>: <error>` line per surface that threw or
//! never painted — and its packet closes `failed`. That is the whole
//! of what happened: the chore records, and nothing opened. A red route
//! lived in a closed packet's step metadata, found by whoever thought
//! to open it, which on the chore's first rehearsal was the operator by
//! hand (`/it/registry/dispatcher` throwing on the playground's real
//! rule rows). The chore's own header says whose job the opening is:
//! "the daily judge's, not this run's". This is that judge, in the
//! shape `ops.judge` and `maintenance.sweep.judge` landed the same day
//! (#448, #452): fire on the close, read the recorded step, write the
//! consequence, note on the judged packet what was done.
//!
//! ## The rule's args
//!
//! - `step` — the slug of the step the chore's output was recorded on
//!   (`run` for every boss-chore.sh chore).
//! - `design` — the design id every filed item carries as
//!   `metadata.design`, so the queue reads the items as one thread.
//! - `area` — `metadata.area` on each item, the queue's grouping key.
//! - `no_red_failure_route` — optional bounded route key for ONE item
//!   when a failed close has no shaped RED line. Undeclared rules keep
//!   the original no-item behavior (backlog e4a9a9b3).
//! - `one_item_per` — optional, `route` (undeclared) or `finding`. Under
//!   `finding` step 4 keys each item on its RED line rather than its
//!   route (`red_finding`), for a chore whose findings all print under
//!   one route and each name a different line to act on: the boss-gcp
//!   deposit key audit, whose second unconfined line while the first
//!   line's item was open was named nowhere (backlog d1b03874). A finding
//!   is keyed on the key fingerprint it names, not its line or its prose
//!   ([`finding_key`], backlog 52534a15), and each tick is judged WHOLE:
//!   an open item whose finding it still prints is given the current
//!   words, and one it no longer prints is closed `stale` — or told, if a
//!   person has acted on it.
//! - `clean_field` / `clean_value` — optional, together, only under
//!   `finding`: the run-step word that says the chore found nothing, so a
//!   tick with no RED line closes every finding item of this rule
//!   (52534a15). Undeclared, a tick with no RED line closes nothing.
//!
//! Which chore kind and which outcome ride the rule's `when`; this
//! handler reads whatever closed and finds RED lines or does not.
//!
//! ## What it does
//!
//! On `jobs.job.closed` — or on a step's `step.done.<kind>`, whose
//! marker names the packet as `job_id` (the boss-gcp deposit key rule,
//! backlog dea2236f, fires there because only the step's marker carries
//! the metadata that tells a finding from a clean tick):
//!
//! 1. Read the closed chore. A packet already carrying
//!    `metadata.judged` was judged by an earlier delivery (JetStream is
//!    at-least-once): nothing more.
//! 2. Parse the RED lines off the `step`'s recorded `output`, grouped
//!    by route — a route that threw twice is one finding with two
//!    errors, and ONE item. No RED line (a red for another reason: the
//!    clone refused, the browser never started) files nothing and says
//!    so on the packet, so the closed chore still reads as judged.
//!    boss-chore.sh carries head 20 + tail 40 lines of the capture;
//!    the crawl prints its RED lines last, so up to ~38 red routes
//!    ride the packet and an omitted-lines marker means the pod log
//!    holds more.
//! 3. ONE CAUSE IS ONE ITEM (backlog c8c6b9a8). When every red route
//!    shares one cause — every one `no-shell` (the SPA shell painted for
//!    none of them), or every finding naming the same HTTP status — the
//!    reds are one finding, not N: file ONE item naming the cause
//!    (`red_cause`) and listing the routes (`red_routes`), deduped by
//!    the cause while one is open. Measured 2026-09-28: crawl 55bcf7fb
//!    ran 04:45:18-04:45:59Z inside converge a10cbaa4 (opened 04:41:01,
//!    rolled 4f9a588 onto the playground, closed 04:47:18); all 43
//!    routes read no-shell and this handler filed 43 items, closed stale
//!    by hand in 43 writes. A route-level item is right only when the
//!    shell rendered and the route alone failed — a mixed night, or a
//!    single red route, files per route as before.
//! 4. Otherwise dedup by route while one is open: every open
//!    backlog-item carrying `metadata.red_route` (the paged walk
//!    `ops.judge` reads the open board with, `common::open_jobs_of_kind`)
//!    — a route with an open item files nothing, whichever night raised
//!    it. A route whose item was CLOSED files again: a recurrence after
//!    a fix is a new fact, the same posture `estate.alarm` takes. File
//!    one backlog-item per remaining route, carrying the route, the
//!    kind(s), the error text copied not retyped and the design id.
//!    A rule declaring `one_item_per = "finding"` dedups the same way
//!    on each distinct RED line instead, so a second line under an
//!    open route is its own item and a repeat of a line files nothing.
//! 5. Every item's `source` is `{kind: packet, id: <chore id>}` — the
//!    structured shape `boss job file --source` records and the
//!    receiving yard groups by (`boss_jobs::origin`), so one night's
//!    items read as one group. It was the bare id string until
//!    c8c6b9a8, which the yard reads as "no recorded source".
//! 6. Note `judged` on the chore's packet naming what was filed and
//!    what was already open, and `red_verdict`: `shell down (N routes)`
//!    / `HTTP <status> on N routes` for a shared cause, `N routes red`
//!    for route-level reds — so the crawl's own packet says which kind
//!    of night it was without a reader re-deriving it from the lines.

use super::common::{
    RECOVERED_AT, api_client, complete_step, get_json, open_jobs_of_kind, owner_for_filing,
    recovery_note, row_or_refuse, withdrawal_fields, write_json,
};
use super::jobs_complete_linked_step::step_by_slug;
use super::ops_judge::JUDGED;
use super::verdict_streak::{ADMITTED_AT, Recovery, withdrawal};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use boss_jobs::channels::InputChannel;
use boss_jobs::opened_by::OPENED_BY_KEY;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;

/// The handler's registered name, and the `cleared_by` its closes carry.
const HANDLER: &str = "maintenance.chore.file_reds";

/// The line shape the crawl prints per red surface
/// (apps/web/tests/live/playground-crawl.spec.ts, pinned by
/// the_playground_is_crawled_nightly.rs): `RED <route> <kind>: <error>`.
pub(crate) const RED_PREFIX: &str = "RED ";
/// The dedup key a filed item carries — the route, and nothing else,
/// because "dedup by route while one is open" is the whole rule.
pub(crate) const RED_ROUTE: &str = "red_route";
/// The dedup key a finding-level item carries beside its route (rule arg
/// `one_item_per = "finding"`, backlog d1b03874): the RED line without
/// its `RED ` prefix — `<route> <kind>: <error>`, copied not retyped.
pub(crate) const RED_FINDING: &str = "red_finding";
/// The dedup key a shared-cause item carries instead of a route.
pub(crate) const RED_CAUSE: &str = "red_cause";
/// The routes a shared-cause item lists, in the order the crawl printed.
pub(crate) const RED_ROUTES: &str = "red_routes";
/// The chore's own verdict, noted beside `judged`.
pub(crate) const RED_VERDICT: &str = "red_verdict";
/// The finding kind the crawl prints when the shell never painted
/// (apps/web/tests/live/playground-crawl.spec.ts).
pub(crate) const NO_SHELL: &str = "no-shell";
/// The cause every route `no-shell` names.
pub(crate) const SHELL_DOWN: &str = "shell down";

/// One red route as the crawl reported it: every `(kind, error)` line
/// it printed for that route, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RedRoute {
    pub route: String,
    pub findings: Vec<(String, String)>,
}

/// PURE: the RED lines of a recorded output, grouped by route in
/// first-seen order. A line is `RED <route> <kind>: <error>` — route
/// and kind carry no whitespace, the error is the rest of the line.
/// A line that starts `RED ` but lacks that shape is not a finding.
pub(crate) fn red_routes(output: &str) -> Vec<RedRoute> {
    output
        .lines()
        .map(str::trim)
        .filter_map(|l| {
            let rest = l.strip_prefix(RED_PREFIX)?;
            let (route, rest) = rest.split_once(' ')?;
            let (kind, error) = rest.split_once(':')?;
            if route.is_empty() || kind.is_empty() || kind.contains(char::is_whitespace) {
                return None;
            }
            Some((
                route.to_string(),
                kind.to_string(),
                error.trim().to_string(),
            ))
        })
        .fold(
            Vec::new(),
            |mut acc: Vec<RedRoute>, (route, kind, error)| {
                match acc.iter_mut().find(|r| r.route == route) {
                    Some(r) => r.findings.push((kind, error)),
                    None => acc.push(RedRoute {
                        route,
                        findings: vec![(kind, error)],
                    }),
                }
                acc
            },
        )
}

/// PURE: the routes already carried by an open backlog-item — the
/// dedup set, over the open listing.
pub(crate) fn open_routes(open_jobs: &[serde_json::Value]) -> Vec<String> {
    open_jobs
        .iter()
        .filter_map(|j| j.get("metadata")?.get(RED_ROUTE)?.as_str())
        .map(str::to_string)
        .collect()
}

/// PURE: the HTTP status an error text names — `status of NNN` (what
/// Chromium logs for a failed resource) or `HTTP NNN` — as exactly three
/// digits, or none.
pub(crate) fn http_status(error: &str) -> Option<u16> {
    ["status of ", "HTTP "].iter().find_map(|marker| {
        error.match_indices(marker).find_map(|(at, _)| {
            let digits: String = error[at + marker.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if digits.len() == 3 {
                digits.parse().ok()
            } else {
                None
            }
        })
    })
}

/// PURE: the cause one finding names, if it names one a whole night can
/// share: the shell never painted, or the gateway answered one status.
fn cause_of(kind: &str, error: &str) -> Option<String> {
    if kind == NO_SHELL {
        Some(SHELL_DOWN.to_string())
    } else {
        http_status(error).map(|s| format!("HTTP {s}"))
    }
}

/// PURE: the one cause EVERY finding on EVERY red route shares, when
/// there are at least two red routes — one red route is a route, not a
/// cause. A finding that names no cause, or two causes on one night,
/// is route-level reds.
pub(crate) fn shared_cause(reds: &[RedRoute]) -> Option<String> {
    if reds.len() < 2 {
        return None;
    }
    let mut causes = reds
        .iter()
        .flat_map(|r| r.findings.iter())
        .map(|(kind, error)| cause_of(kind, error));
    let first = causes.next().flatten()?;
    causes
        .all(|c| c.as_deref() == Some(first.as_str()))
        .then_some(first)
}

/// PURE: the causes already carried by an open backlog-item.
pub(crate) fn open_causes(open_jobs: &[serde_json::Value]) -> Vec<String> {
    open_jobs
        .iter()
        .filter_map(|j| j.get("metadata")?.get(RED_CAUSE)?.as_str())
        .map(str::to_string)
        .collect()
}

/// PURE: where every item this handler files came from — the chore
/// packet, in the structured shape the receiving yard groups by
/// (`boss_jobs::origin::source_of`; backlog c8c6b9a8).
fn source_of_chore(chore_id: &str) -> serde_json::Value {
    json!({ "kind": "packet", "id": chore_id })
}

/// PURE: the chore's verdict for a shared cause.
fn cause_verdict(cause: &str, routes: usize) -> String {
    if cause == SHELL_DOWN {
        format!("{SHELL_DOWN} ({routes} routes)")
    } else {
        format!("{cause} on {routes} routes")
    }
}

/// PURE: the chore's verdict for route-level reds.
fn routes_verdict(routes: usize) -> String {
    if routes == 1 {
        "1 route red".to_string()
    } else {
        format!("{routes} routes red")
    }
}

/// PURE: the ONE backlog-item a shared cause becomes — the cause on the
/// title and as the dedup key, every route listed, every error copied.
pub(crate) fn cause_item_body(
    cause: &str,
    reds: &[RedRoute],
    chore_id: &str,
    chore_kind: &str,
    chore_title: &str,
    design: &str,
    area: &str,
    owner: &str,
    ctx: &InvocationContext,
) -> serde_json::Value {
    let routes: Vec<&str> = reds.iter().map(|r| r.route.as_str()).collect();
    let mut kinds: Vec<&str> = reds
        .iter()
        .flat_map(|r| r.findings.iter().map(|(k, _)| k.as_str()))
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    let errors: Vec<String> = reds
        .iter()
        .flat_map(|r| {
            r.findings
                .iter()
                .map(move |(k, e)| format!("{} {k}: {e}", r.route))
        })
        .collect();
    let short = &chore_id[..chore_id.len().min(8)];
    let n = routes.len();
    let why = if cause == SHELL_DOWN {
        "the SPA shell painted for none of them, so the instance, its gateway or a roll is the \
         finding — not any one route"
    } else {
        "every one answered the same status, so what serves them is the finding — not any one \
         route"
    };
    json!({
        "kind": "backlog-item",
        "title": format!("{chore_title} red: {cause} on {n} routes"),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": owner,
        "priority": "standard",
        "status": "open",
        "tags": [],
        "metadata": {
            "input_channel": super::common::lane_label(InputChannel::PipelineFailure),
            "area": area,
            "design": design,
            RED_CAUSE: cause,
            RED_ROUTES: routes,
            "red_kind": kinds.join(","),
            "red_error": errors.join(" | "),
            "source": source_of_chore(chore_id),
            "reporter": ctx.rule_name,
            "triggered_by_event_id": ctx.triggering_event_id,
            "detail": format!(
                "Filed by {} (backlog c8c6b9a8, design {design}): the {chore_kind} packet \
                 {short} closed red with {n} red routes sharing one cause, {cause}: {why}. \
                 ONE item for the cause, listing every route: {}. While it is open a second \
                 night with the same cause files nothing. The chore's run step holds the \
                 whole excerpt.",
                ctx.rule_name,
                routes.join(", ")
            ),
        },
    })
}

/// PURE: the backlog-item one red route becomes.
pub(crate) fn item_body(
    red: &RedRoute,
    chore_id: &str,
    chore_kind: &str,
    chore_title: &str,
    design: &str,
    area: &str,
    owner: &str,
    ctx: &InvocationContext,
) -> serde_json::Value {
    let mut kinds: Vec<&str> = red.findings.iter().map(|(k, _)| k.as_str()).collect();
    kinds.dedup();
    let kinds = kinds.join(",");
    let errors: Vec<String> = red
        .findings
        .iter()
        .map(|(k, e)| format!("{k}: {e}"))
        .collect();
    let short = &chore_id[..chore_id.len().min(8)];
    json!({
        "kind": "backlog-item",
        "title": format!("{chore_title} red: {} ({kinds})", red.route),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        // The platform owner as the registry answers it, or nobody for
        // the jobs API to resolve from the kind's owner_role (3c23662d).
        "owner_id": owner,
        "priority": "standard",
        "status": "open",
        "tags": [],
        "metadata": {
            "input_channel": super::common::lane_label(InputChannel::PipelineFailure),
            "area": area,
            "design": design,
            RED_ROUTE: red.route,
            "red_kind": kinds,
            "red_error": errors.join(" | "),
            "source": source_of_chore(chore_id),
            "reporter": ctx.rule_name,
            "triggered_by_event_id": ctx.triggering_event_id,
            "detail": format!(
                "Filed by {} (backlog ac3270c7, design {design}): the {chore_kind} packet \
                 {short} closed red with this route on its own RED line. {} The chore's run \
                 step holds the whole excerpt; the pod log holds the whole output. One item \
                 per route while it is open — a second red night for the same route files \
                 nothing until this one closes.",
                ctx.rule_name,
                errors.join(" | ")
            ),
        },
    })
}

/// PURE: the key fingerprint a finding's error text names first —
/// `SHA256:<base64>`, the form `ssh-keygen -l` prints and the boss-gcp
/// deposit key audit names every key by (infra/gcp/audit-deposit-keys.sh,
/// "A KEY IS WHAT ssh-keygen READS, COMPARED BY FINGERPRINT"). Every
/// finding that names a key names it before the forced command or the
/// comment it quotes, so the first one is the key the line is about.
pub(crate) fn key_fingerprint(error: &str) -> Option<&str> {
    const MARK: &str = "SHA256:";
    error.match_indices(MARK).find_map(|(at, _)| {
        let tail = &error[at + MARK.len()..];
        let len = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='))
            .unwrap_or(tail.len());
        (len > 0).then(|| &error[at..at + MARK.len() + len])
    })
}

/// PURE: the dedup key one finding carries under `one_item_per =
/// "finding"` (d1b03874).
///
/// THE KEY, NOT THE LINE (backlog 52534a15). It was the whole RED line,
/// and the audit leads every line with `<file>:<line>` and follows it with
/// prose: removing an EARLIER line of authorized_keys shifted every later
/// finding's number, and any car rewording the audit (26173f40's did)
/// changed every finding's text — each re-filed beside its stale item. A
/// finding that names a key fingerprint is keyed on the route, the kind and
/// that fingerprint: the class is what the chore printed it under, the
/// identity is the key, and neither moves when a line above it is removed
/// or the sentence is rewritten. Two lines carrying ONE key are one item,
/// because the act is one — re-place that key's authorized_keys_line whole
/// — and the item carries every line it names ([`RED_ERROR`]). A finding
/// that names no key (a line sshd's parser reads nothing from) has no
/// identity but its line, so it keeps the whole line; a shift re-files it,
/// and the stale one closes on the same tick.
pub(crate) fn finding_key(route: &str, kind: &str, error: &str) -> String {
    match key_fingerprint(error) {
        Some(fp) => format!("{route} {kind}: key {fp}"),
        None => format!("{route} {kind}: {error}"),
    }
}

/// The error text an item carries — every line of its finding, copied.
pub(crate) const RED_ERROR: &str = "red_error";

/// PURE: the finding key an open item stands for, re-derived from what it
/// carries. An item filed under rule v2 carries its whole RED line; read
/// back through [`red_routes`] and [`finding_key`] it is the fingerprint
/// key, so the first tick after v3 neither re-files it nor closes it.
pub(crate) fn item_finding_key(item: &serde_json::Value) -> Option<String> {
    let stored = item.get("metadata")?.get(RED_FINDING)?.as_str()?;
    Some(
        red_routes(&format!("{RED_PREFIX}{stored}"))
            .first()
            .and_then(|r| {
                r.findings
                    .first()
                    .map(|(kind, error)| finding_key(&r.route, kind, error))
            })
            .unwrap_or_else(|| stored.to_string()),
    )
}

/// PURE: an item already told its finding cleared ([`recovery_note`]).
/// It no longer stands for the finding: a recurrence files anew, the
/// posture `maintenance.verdict.streak` takes (backlog 3ff17da3).
fn told_recovered(item: &serde_json::Value) -> bool {
    item.get("metadata")
        .and_then(|m| m.get(RECOVERED_AT))
        .is_some_and(|v| !v.is_null())
}

/// PURE: the findings already carried by an open backlog-item that still
/// stands for its finding.
pub(crate) fn open_findings(open_jobs: &[serde_json::Value]) -> Vec<String> {
    open_jobs
        .iter()
        .filter(|j| !told_recovered(j))
        .filter_map(item_finding_key)
        .collect()
}

/// PURE: the keys an item this judge files under `one_item_per =
/// "finding"` carries while nobody has written on it — read off
/// [`finding_item_body`] rather than listed a second time, plus the two
/// admission stamps (CLAUDE.md §9a).
fn finding_own_keys(ctx: &InvocationContext) -> BTreeSet<String> {
    let body = finding_item_body("r", "k", "e", "c", "", "", "", "", "", ctx);
    body["metadata"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.keys().cloned())
        .chain([ADMITTED_AT.to_string(), OPENED_BY_KEY.to_string()])
        .collect()
}

/// PURE: what a tick that no longer prints an item's finding does to it.
///
/// ONLY ITS OWN ITEM: one this rule filed (`reporter`), keyed on a
/// finding, not already told it cleared, and whose key is not among
/// `present` — the keys this tick printed (none, on a clean one).
/// ONLY A LATER TICK: the tick opened after the item was filed, so a
/// redelivered old tick judges nothing. Then the shared judgement: closed
/// `stale` while nobody has acted on it, told otherwise
/// ([`verdict_streak::withdrawal`]).
fn cleared(
    item: &serde_json::Value,
    rule: &str,
    present: &[String],
    tick_opened_at: &str,
    own: &BTreeSet<String>,
) -> Recovery {
    let reporter = item
        .get("metadata")
        .and_then(|m| m.get("reporter"))
        .and_then(|v| v.as_str());
    let Some(key) = item_finding_key(item) else {
        return Recovery::Leave;
    };
    if reporter != Some(rule) || told_recovered(item) || present.contains(&key) {
        return Recovery::Leave;
    }
    let at = |t: &str| chrono::DateTime::parse_from_rfc3339(t).ok();
    match (at(tick_opened_at), at(str_field(item, "opened_at"))) {
        (Some(tick), Some(filed)) if tick > filed => withdrawal(item, own),
        _ => Recovery::Leave,
    }
}

/// PURE: the backlog-item one finding becomes under `one_item_per =
/// "finding"`: the route item's body for that one line, plus the line as
/// its dedup key, and a title that carries the start of the error — the
/// boss-gcp audit leads it with the file:line to remove, so two items
/// under one route read apart on the board (backlog d1b03874).
pub(crate) fn finding_item_body(
    route: &str,
    kind: &str,
    error: &str,
    chore_id: &str,
    chore_kind: &str,
    chore_title: &str,
    design: &str,
    area: &str,
    owner: &str,
    ctx: &InvocationContext,
) -> serde_json::Value {
    let red = RedRoute {
        route: route.to_string(),
        findings: vec![(kind.to_string(), error.to_string())],
    };
    let mut body = item_body(
        &red,
        chore_id,
        chore_kind,
        chore_title,
        design,
        area,
        owner,
        ctx,
    );
    let head: String = error.chars().take(140).collect();
    let more = if head.len() < error.len() { "…" } else { "" };
    let short = &chore_id[..chore_id.len().min(8)];
    body["title"] = json!(format!("{chore_title} red: {route} {kind}: {head}{more}"));
    body["metadata"][RED_FINDING] = json!(finding_key(route, kind, error));
    body["metadata"]["detail"] = json!(format!(
        "Filed by {} (backlog d1b03874, design {design}): the {chore_kind} packet {short} \
         recorded this finding on its own RED line: {kind}: {error}. One item per distinct \
         finding while it is open, because the rule declares one_item_per = finding: a \
         different line under the same route files its own item, and this same line again \
         files nothing until this item closes. The chore's run step holds the whole excerpt.",
        ctx.rule_name
    ));
    body
}

/// PURE: one tick's findings, one per KEY in first-seen order — the key,
/// the first line's route, kind and error (the item's title and detail),
/// and EVERY line under that key as `kind: error`, joined, which is what
/// the item carries as [`RED_ERROR`]. A key on two lines is one finding
/// naming both (backlog 52534a15).
pub(crate) fn finding_lines(reds: &[RedRoute]) -> Vec<(String, &str, &str, &str, String)> {
    let mut out: Vec<(String, &str, &str, &str, Vec<String>)> = Vec::new();
    for red in reds {
        for (kind, error) in &red.findings {
            let key = finding_key(&red.route, kind, error);
            let line = format!("{kind}: {error}");
            match out.iter_mut().find(|(k, ..)| *k == key) {
                Some((.., lines)) => {
                    if !lines.contains(&line) {
                        lines.push(line);
                    }
                }
                None => out.push((key, &red.route, kind, error, vec![line])),
            }
        }
    }
    out.into_iter()
        .map(|(key, route, kind, error, lines)| (key, route, kind, error, lines.join(" | ")))
        .collect()
}

/// What one judgement did to the finding items it no longer prints.
#[derive(Debug, Default)]
pub(crate) struct Cleared {
    closed: Vec<String>,
    told: Vec<String>,
}

impl Cleared {
    fn is_empty(&self) -> bool {
        self.closed.is_empty() && self.told.is_empty()
    }

    /// The judged note's words: how many, and which, by short id.
    fn text(&self) -> String {
        let mut parts = Vec::new();
        if !self.closed.is_empty() {
            parts.push(format!(
                "closed {} backlog-item(s) stale, the finding no longer printed [{}]",
                self.closed.len(),
                self.closed.join(", ")
            ));
        }
        if !self.told.is_empty() {
            parts.push(format!(
                "told {} backlog-item(s) their finding cleared, left open for whoever acted on \
                 them [{}]",
                self.told.len(),
                self.told.join(", ")
            ));
        }
        parts.join("; ")
    }
}

/// PURE: the evidence a cleared finding's close cites — the tick by id
/// and kind, what it read, and the finding's key and last-known words,
/// copied from the item.
fn cleared_evidence(
    chore: &serde_json::Value,
    step: &str,
    item: &serde_json::Value,
    reading: &str,
) -> String {
    let key = item_finding_key(item).unwrap_or_default();
    let words = item
        .get("metadata")
        .and_then(|m| m.get(RED_ERROR))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let id = str_field(chore, "id");
    format!(
        "Tick {} ({}) completed its `{step}` step and {reading}: the finding this item was \
         filed for — {key} — is no longer printed. Its last words, copied: {words}. A recurrence \
         files a new item rather than reopening this one (backlog 52534a15).",
        &id[..id.len().min(8)],
        str_field(chore, "kind"),
    )
}

pub struct ChoreFileReds {
    client: boss_core::machine_token::Client,
    jobs_base: String,
    /// Who the items this handler files are owned by — the platform
    /// owner through the port (backlog 3c23662d), resolved once per
    /// invocation by `common::owner_for_filing`; never a literal.
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl ChoreFileReds {
    pub fn new(
        jobs_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            owner,
        })
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

    async fn job(&self, id: &str, rule: &str) -> Result<serde_json::Value, HandlerError> {
        let job = get_json(
            &self.client,
            &format!("{}/api/jobs/{id}", self.base()),
            rule,
        )
        .await?;
        row_or_refuse(job, &format!("GET /api/jobs/{id}")).map_err(HandlerError::Downstream)
    }

    async fn annotate(
        &self,
        chore_id: &str,
        note: serde_json::Value,
        rule: &str,
    ) -> Result<(), HandlerError> {
        write_json(
            &self.client,
            reqwest::Method::PATCH,
            &format!("{}/api/jobs/{chore_id}/metadata", self.base()),
            &note,
            rule,
        )
        .await
    }

    /// Each open item still standing for a finding this tick printed is
    /// given the finding's words as they stand now, when they differ: a
    /// removed earlier line moves the number the item tells David to
    /// remove, and an item naming the old number names the wrong line.
    /// [`RED_ERROR`] is the judge's own key, so the write is not someone
    /// acting on the item.
    async fn refresh_findings(
        &self,
        open: &[serde_json::Value],
        candidates: &[(String, serde_json::Value)],
        rule: &str,
    ) -> Result<(), HandlerError> {
        for item in open.iter().filter(|j| !told_recovered(j)) {
            let Some(key) = item_finding_key(item) else {
                continue;
            };
            let Some((_, body)) = candidates.iter().find(|(k, _)| *k == key) else {
                continue;
            };
            let now = &body["metadata"][RED_ERROR];
            if item.get("metadata").and_then(|m| m.get(RED_ERROR)) == Some(now) {
                continue;
            }
            write_json(
                &self.client,
                reqwest::Method::PATCH,
                &format!(
                    "{}/api/jobs/{}/metadata",
                    self.base(),
                    str_field(item, "id")
                ),
                &json!({ RED_ERROR: now }),
                rule,
            )
            .await?;
        }
        Ok(())
    }

    /// The finding items of this rule that `chore` — a later tick — no
    /// longer prints (`present` holds the keys it did), each closed or
    /// told by [`cleared`]. Written BEFORE the chore's judged note, so a
    /// delivery that failed between retries the items rather than
    /// skipping them; idempotent, since a closed item is no longer open
    /// and a told one carries [`RECOVERED_AT`].
    async fn clear_findings(
        &self,
        chore: &serde_json::Value,
        step: &str,
        open: &[serde_json::Value],
        present: &[String],
        reading: &str,
        ctx: &InvocationContext,
    ) -> Result<Cleared, HandlerError> {
        let rule = ctx.rule_name.as_str();
        let own = finding_own_keys(ctx);
        let tick_opened_at = str_field(chore, "opened_at");
        let mut done = Cleared::default();
        for item in open {
            let decision = cleared(item, rule, present, tick_opened_at, &own);
            if decision == Recovery::Leave {
                continue;
            }
            let item_id = str_field(item, "id");
            let short = &item_id[..item_id.len().min(8)];
            let evidence = cleared_evidence(chore, step, item, reading);
            // When the finding cleared: the tick's own step stamp, else
            // the instant the event fired — never this process's clock.
            let at = match step_by_slug(chore, step)
                .and_then(|s| s.get("completed_at"))
                .and_then(|v| v.as_str())
            {
                Some(at) => at.to_string(),
                None => ctx.firing_instant()?.to_rfc3339(),
            };
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
                    done.closed.push(short.to_string());
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
                    done.told.push(short.to_string());
                }
                Recovery::Leave => {}
            }
        }
        if !done.is_empty() {
            tracing::info!(rule = %rule, chore = %str_field(chore, "id"), "finding items cleared: {}", done.text());
        }
        Ok(done)
    }

    /// A declared failed check still reaches its owner when it crashed
    /// before printing a RED line. Keep the captured evidence distinct
    /// from a diagnostic: no route, error kind or exit status is guessed.
    async fn file_unshaped_failure(
        &self,
        route: &str,
        chore: &serde_json::Value,
        step: &str,
        design: &str,
        area: &str,
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = &ctx.rule_name;
        let chore_id = str_field(chore, "id");
        let open = open_jobs_of_kind(&self.client, self.base(), "backlog-item", rule).await?;
        let already = open_routes(&open).iter().any(|r| r == route);
        let mut filed = Vec::new();
        if !already {
            let owner = owner_for_filing(self.owner.as_ref(), rule).await;
            let metadata = step_by_slug(chore, step).and_then(|s| s.get("metadata"));
            let output_value = metadata.and_then(|m| m.get("output"));
            let output = output_value.and_then(|v| v.as_str());
            let excerpt: Option<String> = output.map(|s| s.chars().take(4096).collect());
            let exit_value = metadata.and_then(|m| m.get("exit_status"));
            let exit = exit_value
                .and_then(|v| v.as_str())
                .filter(|s| s.len() <= 32 && s.parse::<i32>().is_ok());
            let body = json!({
                "kind": "backlog-item",
                "title": format!("{} failed without a RED finding", str_field(chore, "title")),
                "subject": {"subject_kind": "custom", "id": "bosspipeline"},
                "owner_id": owner, "priority": "standard", "status": "open", "tags": [],
                "metadata": {
                    "input_channel": super::common::lane_label(InputChannel::PipelineFailure),
                    "area": area, "design": design, RED_ROUTE: route,
                    "source": source_of_chore(chore_id), "reporter": rule,
                    "triggered_by_event_id": ctx.triggering_event_id,
                    "failure_step": step,
                    "failure_output": excerpt,
                    "failure_output_state": if output_value.is_none() {"missing"} else if output.is_some() {"recorded"} else {"not a string"},
                    "failure_output_truncated": output.is_some_and(|s| s.chars().count() > 4096),
                    "failure_exit_status": exit,
                    "failure_exit_status_state": if exit_value.is_none() {"missing"} else if exit.is_some() {"recorded"} else {"not an integer string"},
                    "detail": format!("The {} packet {chore_id} closed failed without a shaped RED finding on step {step}. The recorded output excerpt is bounded to 4096 characters; the source packet holds the recorded capture. No diagnostic cause is inferred. One item for the declared route {route} while it is open.", str_field(chore, "kind")),
                },
            });
            filed.push(
                super::common::post_json_minted_id(
                    &self.client,
                    &format!("{}/api/jobs", self.base()),
                    &body,
                    rule,
                )
                .await?,
            );
        }
        self.annotate(chore_id, json!({
            JUDGED: format!("{rule}: failed without a shaped RED finding; filed {} backlog-item(s) for {route}; already open: {already}", filed.len()),
            RED_VERDICT: "failed without a shaped RED finding", "filed": filed,
        }), rule).await
    }
}

fn str_field<'a>(job: &'a serde_json::Value, key: &str) -> &'a str {
    job.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

#[async_trait]
impl Handler for ChoreFileReds {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = arg_string(args, "step")?;
        let design = arg_string(args, "design")?;
        let area = arg_string(args, "area")?;
        let fallback = if args.iter().any(|(k, _)| k == "no_red_failure_route") {
            let route = arg_string(args, "no_red_failure_route")?;
            if route.is_empty() || route.len() > 128 || route.contains(char::is_whitespace) {
                return Err(HandlerError::Permanent("no_red_failure_route must be a nonempty route key of at most 128 bytes without whitespace".into()));
            }
            Some(route)
        } else {
            None
        };
        // Undeclared, an item per ROUTE, as every crawl-shaped rule wants;
        // `finding` is for a chore whose RED lines all share one route and
        // each name a different thing to act on (backlog d1b03874).
        let per_finding = if args.iter().any(|(k, _)| k == "one_item_per") {
            match arg_string(args, "one_item_per")? {
                "route" => false,
                "finding" => true,
                other => {
                    return Err(HandlerError::Permanent(format!(
                        "one_item_per must be \"route\" or \"finding\", not {other:?}"
                    )));
                }
            }
        } else {
            false
        };
        // The run-step word that says the chore found NOTHING, so a tick
        // with no RED line may close this rule's finding items (backlog
        // 52534a15). Both or neither; only under `finding`, whose items
        // are each one finding a later tick can stop printing.
        let clean = match (
            args.iter().any(|(k, _)| k == "clean_field"),
            args.iter().any(|(k, _)| k == "clean_value"),
        ) {
            (false, false) => None,
            (true, true) if per_finding => Some((
                arg_string(args, "clean_field")?,
                arg_string(args, "clean_value")?,
            )),
            _ => {
                return Err(HandlerError::Permanent(
                    "clean_field and clean_value are declared together, and only with \
                     one_item_per = \"finding\""
                        .into(),
                ));
            }
        };
        let rule = ctx.rule_name.as_str();

        // The close marker names the packet as `id`; a step's marker
        // (`step.done.<kind>`) names it as `job_id`, which is how the
        // boss-gcp deposit key rule reaches this judge from an ok run
        // (review 1fa0d093 of car 3cd92f7a, B2). A malformed marker is
        // not something a redelivery can fix, so it is a no-op, not an
        // error.
        let Some(chore_id) = ["id", "job_id"]
            .iter()
            .find_map(|k| ctx.event_payload.get(*k).and_then(|v| v.as_str()))
        else {
            return Ok(());
        };

        // 1. The closed chore; judged by an earlier delivery means the
        //    record is true.
        let chore = self.job(chore_id, rule).await?;
        if chore
            .get("metadata")
            .and_then(|m| m.get(JUDGED))
            .and_then(|v| v.as_str())
            .is_some()
        {
            return Ok(());
        }
        let chore_kind = str_field(&chore, "kind");
        let chore_title = str_field(&chore, "title");

        // 2. The RED lines, off the recorded output.
        let output = step_by_slug(&chore, step)
            .and_then(|s| s.get("metadata"))
            .and_then(|m| m.get("output"))
            .and_then(|o| o.as_str())
            .unwrap_or("");
        let reds = red_routes(output);
        if reds.is_empty() {
            // A CLEAN TICK closes what it no longer prints — everything of
            // this rule's — and writes nothing when there is nothing to
            // close: 47 clean ticks a day are not 47 notes (52534a15).
            if let Some((field, value)) = clean {
                let word = step_by_slug(&chore, step)
                    .and_then(|s| s.get("metadata"))
                    .and_then(|m| m.get(field))
                    .and_then(|v| v.as_str());
                if word == Some(value) {
                    let open =
                        open_jobs_of_kind(&self.client, self.base(), "backlog-item", rule).await?;
                    let reading = format!("reads {field}={value} and prints no finding");
                    let done = self
                        .clear_findings(&chore, step, &open, &[], &reading, ctx)
                        .await?;
                    if done.is_empty() {
                        return Ok(());
                    }
                    return self
                        .annotate(
                            chore_id,
                            json!({
                                JUDGED: format!("{rule}: this tick {reading}; {}", done.text()),
                            }),
                            rule,
                        )
                        .await;
                }
            }
            if let Some(route) = fallback.filter(|_| {
                ctx.event_payload.get("outcome").and_then(|v| v.as_str()) == Some("failed")
            }) {
                return self
                    .file_unshaped_failure(route, &chore, step, design, area, ctx)
                    .await;
            }
            self.annotate(
                chore_id,
                json!({
                    JUDGED: format!(
                        "{rule}: nothing filed — no `{RED_PREFIX}<route> <kind>: <error>` line \
                         on step `{step}`; a red for another reason (the clone, the browser) \
                         is read on this packet, not opened per route"
                    ),
                }),
                rule,
            )
            .await?;
            tracing::info!(rule = %rule, chore = %chore_id, "closed red with no RED line on step {step}; nothing filed");
            return Ok(());
        }

        let open = open_jobs_of_kind(&self.client, self.base(), "backlog-item", rule).await?;
        let owner = owner_for_filing(self.owner.as_ref(), rule).await;

        // 3. One cause, one item, deduped by the cause; 4. otherwise one
        //    item per route, deduped by the route. Each candidate is its
        //    dedup key and the body it files.
        let (verdict, shared, candidates): (String, bool, Vec<(String, serde_json::Value)>) =
            match shared_cause(&reds) {
                Some(cause) => {
                    let body = cause_item_body(
                        &cause,
                        &reds,
                        chore_id,
                        chore_kind,
                        chore_title,
                        design,
                        area,
                        &owner,
                        ctx,
                    );
                    (cause_verdict(&cause, reds.len()), true, vec![(cause, body)])
                }
                None if per_finding => (
                    routes_verdict(reds.len()),
                    false,
                    finding_lines(&reds)
                        .into_iter()
                        .map(|(key, route, kind, error, all)| {
                            let mut body = finding_item_body(
                                route,
                                kind,
                                error,
                                chore_id,
                                chore_kind,
                                chore_title,
                                design,
                                area,
                                &owner,
                                ctx,
                            );
                            body["metadata"][RED_ERROR] = json!(all);
                            (key, body)
                        })
                        .collect(),
                ),
                None => (
                    routes_verdict(reds.len()),
                    false,
                    reds.iter()
                        .map(|red| {
                            let body = item_body(
                                red,
                                chore_id,
                                chore_kind,
                                chore_title,
                                design,
                                area,
                                &owner,
                                ctx,
                            );
                            (red.route.clone(), body)
                        })
                        .collect(),
                ),
            };
        let already = if shared {
            open_causes(&open)
        } else if per_finding {
            open_findings(&open)
        } else {
            open_routes(&open)
        };

        let mut filed: Vec<(String, String)> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        for (key, body) in &candidates {
            // A line printed twice on one tick is one finding, so a key
            // this pass already filed or skipped is not judged again.
            if filed.iter().any(|(k, _)| k == key) || skipped.contains(key) {
                continue;
            }
            if already.iter().any(|k| k == key) {
                skipped.push(key.clone());
                continue;
            }
            let id = super::common::post_json_minted_id(
                &self.client,
                &format!("{}/api/jobs", self.base()),
                body,
                rule,
            )
            .await?;
            filed.push((key.clone(), id));
        }

        // 5b. Under `finding`, the tick is a whole judgement of the file:
        //     an open item whose finding it still prints is told the
        //     finding's words as they stand now (the line it names may
        //     have moved), and one whose finding it no longer prints is
        //     closed, or told (backlog 52534a15).
        let cleared_text = if per_finding && !shared {
            let present: Vec<String> = candidates.iter().map(|(k, _)| k.clone()).collect();
            self.refresh_findings(&open, &candidates, rule).await?;
            let reading = format!(
                "prints {} finding(s), none of them this item's",
                present.len()
            );
            let done = self
                .clear_findings(&chore, step, &open, &present, &reading, ctx)
                .await?;
            if done.is_empty() {
                String::new()
            } else {
                format!("; {}", done.text())
            }
        } else {
            String::new()
        };

        // 6. The note on the judged chore, with its verdict.
        let filed_ids: Vec<&str> = filed.iter().map(|(_, id)| id.as_str()).collect();
        let filed_text: Vec<String> = filed
            .iter()
            .map(|(key, id)| format!("{key} → {}", &id[..id.len().min(8)]))
            .collect();
        let shape = if shared {
            " share one cause, so ONE item lists them"
        } else {
            ""
        };
        self.annotate(
            chore_id,
            json!({
                JUDGED: format!(
                    "{rule}: {verdict} — {} red route(s) on step `{step}`{shape}; filed {} \
                     backlog-item(s) [{}]; already open [{}]{cleared_text}",
                    reds.len(),
                    filed.len(),
                    filed_text.join(", "),
                    skipped.join(", ")
                ),
                RED_VERDICT: verdict,
                "filed": filed_ids,
            }),
            rule,
        )
        .await?;
        tracing::info!(rule = %rule, chore = %chore_id, filed = filed.len(), already_open = skipped.len(), "{} red route(s) judged", reds.len());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::Path, extract::Query, routing::get};
    use std::collections::HashMap;
    use std::sync::Mutex;

    const CRAWL: &str = "22222222-2222-2222-2222-222222222222";
    const RULE: &str = "file-backlog-items-on-playground-crawl-red";
    const DESIGN: &str = "0e07ce64";

    /// The crawl's output as boss-chore.sh carries it onto the run step
    /// (the summary line, the reported console noise, then the RED
    /// lines the spec prints last).
    const RED_OUTPUT: &str = "\n> boss-web@0.0.0 test:live\n> playwright test -c playwright.live.config.ts\n\nRunning 1 test using 1 worker\n\ncrawled 55 routes at http://boss-gateway.boss-playground.svc.cluster.local: 3 red, 2 console.error\n  console.error [/it] Failed to load resource: the server responded with a status of 404 (Not Found)\n  console.error [/it/codebase] Failed to load resource: the server responded with a status of 403 (Forbidden)\nRED /it/registry/dispatcher pageerror: Cannot read properties of null (reading 'why') | at DispatcherRules.svelte:42\nRED /it/registry/dispatcher pageerror: Cannot read properties of null (reading 'why') | at DispatcherRules.svelte:42\nRED /ux/catalog/models/m-1 no-shell: Timed out 20000ms waiting for expect(locator).toBeVisible()\n  1) every catalogued route renders the instance as a guest\n";
    const NO_RED_OUTPUT: &str =
        "fatal: unable to access the forge: The requested URL returned error: 401\n";

    fn ctx() -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: RULE.into(),
            triggering_event_id: "evt-close-9".into(),
            triggering_topic: "jobs.job.closed".into(),
            // The close marker in the shape every emit site produces:
            // every key present, and NO step metadata.
            event_payload: json!({
                "id": CRAWL,
                "closed_on": "2026-09-19",
                "kind": "maintenance-playground-crawl",
                "outcome": "failed",
                "title": "Playground crawl",
                "subject_id": "infra/maintenance-playground-crawl",
                "parent_step_id": null,
            }),
        }
    }

    fn args() -> Vec<(String, Value)> {
        [("step", "run"), ("design", DESIGN), ("area", "web")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), Value::String(v.into())))
            .collect()
    }

    /// The closed chore, as boss-chore.sh completed its run step.
    fn crawl(output: &str) -> serde_json::Value {
        json!({
            "id": CRAWL,
            "kind": "maintenance-playground-crawl",
            "title": "Playground crawl",
            "status": "closed",
            "metadata": {},
            "steps": [
                { "id": "c-scheduled", "spec_slug": "scheduled", "status": "completed", "metadata": {} },
                { "id": "c-run", "spec_slug": "run", "status": "completed",
                  "metadata": { "result": "failed", "exit_status": "1", "output": output } },
                { "id": "c-failed", "spec_slug": "failed", "status": "completed", "metadata": {} },
            ],
        })
    }

    /// An item a previous night filed, as the jobs API lists it back.
    fn open_item(id: &str, route: &str) -> serde_json::Value {
        json!({
            "id": id,
            "kind": "backlog-item",
            "status": "open",
            "metadata": { "area": "web", "design": DESIGN, RED_ROUTE: route },
            "steps": [],
        })
    }

    type Writes = Arc<Mutex<Vec<(String, String, serde_json::Value)>>>;
    type Rows = Arc<Mutex<HashMap<String, serde_json::Value>>>;

    /// When the jobs API admits every row `POST /api/jobs` mints: one
    /// minute after the newest row it holds, so an item is filed after
    /// the tick that filed it and before the tick a test adds next.
    fn admitted_at(map: &HashMap<String, serde_json::Value>) -> String {
        let newest = map
            .values()
            .filter_map(|j| j["opened_at"].as_str())
            .filter_map(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .max()
            .unwrap_or_else(|| {
                chrono::DateTime::parse_from_rfc3339("2026-10-19T00:00:00+00:00").unwrap()
            });
        (newest + chrono::Duration::minutes(1)).to_rfc3339()
    }

    async fn mock_jobs(jobs: Vec<serde_json::Value>) -> (String, Writes) {
        let (base, writes, _) = mock_jobs_rows(jobs).await;
        (base, writes)
    }

    /// Stand-in for jobs-api, the shape `ops_judge`'s tests use: `GET
    /// /api/jobs/{id}` serves one, `GET /api/jobs?kind=&status=open`
    /// lists the open ones, `POST /api/jobs` mints an id and keeps the
    /// row open — admitted as the API admits it, with `opened_at`, the
    /// two admission stamps and a ready `triage` — `PATCH
    /// /api/jobs/{id}/metadata` merges, and the step merge door plus the
    /// completion PUT close an item whose `triage` completes `stale` — so
    /// a second delivery reads what the first wrote.
    async fn mock_jobs_rows(jobs: Vec<serde_json::Value>) -> (String, Writes, Rows) {
        let writes: Writes = Arc::new(Mutex::new(Vec::new()));
        let by_id: Rows = Arc::new(Mutex::new(
            jobs.into_iter()
                .map(|j| (j["id"].as_str().unwrap_or_default().to_string(), j))
                .collect(),
        ));
        let (g, l, pj, pm) = (by_id.clone(), by_id.clone(), by_id.clone(), by_id.clone());
        let (sm, sp, rows) = (by_id.clone(), by_id.clone(), by_id.clone());
        let (wj, wm, wsm, wsp) = (
            writes.clone(),
            writes.clone(),
            writes.clone(),
            writes.clone(),
        );
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = g.clone();
                    async move {
                        by_id
                            .lock()
                            .unwrap()
                            .get(&id)
                            .cloned()
                            .map(Json)
                            .ok_or(axum::http::StatusCode::NOT_FOUND)
                    }
                }),
            )
            .route(
                "/api/jobs",
                get(move |Query(q): Query<HashMap<String, String>>| {
                    let by_id = l.clone();
                    async move {
                        let rows: Vec<serde_json::Value> = by_id
                            .lock()
                            .unwrap()
                            .values()
                            .filter(|j| {
                                q.get("kind").is_none_or(|k| j["kind"] == json!(k))
                                    && q.get("status").is_none_or(|s| j["status"] == json!(s))
                            })
                            .cloned()
                            .collect();
                        Json(json!({ "data": rows, "total": rows.len() }))
                    }
                })
                .post(move |Json(body): Json<serde_json::Value>| {
                    let (w, by_id) = (wj.clone(), pj.clone());
                    async move {
                        w.lock()
                            .unwrap()
                            .push(("POST".into(), "/api/jobs".into(), body.clone()));
                        let mut map = by_id.lock().unwrap();
                        let id = format!("f0000000-0000-0000-0000-{:012}", map.len() + 1);
                        let at = admitted_at(&map);
                        let mut row = body;
                        row["id"] = json!(id);
                        row["opened_at"] = json!(at);
                        row["metadata"]["opened_at"] = json!(at);
                        row["metadata"]["opened_by"] = json!("automation:rule:test");
                        row["steps"] = json!([
                            { "id": format!("{id}-filed"), "spec_slug": "filed", "status": "completed", "metadata": {} },
                            { "id": format!("{id}-triage"), "spec_slug": "triage", "status": "ready", "metadata": {} },
                        ]);
                        map.insert(id.clone(), row);
                        (axum::http::StatusCode::CREATED, Json(json!({ "id": id })))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(
                    move |Path(id): Path<String>, Json(body): Json<serde_json::Value>| {
                        let (w, by_id) = (wm.clone(), pm.clone());
                        async move {
                            w.lock().unwrap().push((
                                "PATCH".into(),
                                format!("/api/jobs/{id}/metadata"),
                                body.clone(),
                            ));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id)
                                && let (Some(m), Some(b)) =
                                    (job["metadata"].as_object_mut(), body.as_object())
                            {
                                for (k, v) in b {
                                    m.insert(k.clone(), v.clone());
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
                        let (w, by_id) = (wsm.clone(), sm.clone());
                        async move {
                            w.lock().unwrap().push((
                                "PATCH".into(),
                                format!("/api/jobs/{id}/steps/{sid}/metadata"),
                                body.clone(),
                            ));
                            let mut map = by_id.lock().unwrap();
                            let step = map
                                .get_mut(&id)
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
                        let (w, by_id) = (wsp.clone(), sp.clone());
                        async move {
                            w.lock().unwrap().push((
                                "PUT".into(),
                                format!("/api/jobs/{id}/steps/{sid}"),
                                body.clone(),
                            ));
                            let mut map = by_id.lock().unwrap();
                            if let Some(job) = map.get_mut(&id) {
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

    fn handler(base: String) -> Arc<ChoreFileReds> {
        ChoreFileReds::with_client(
            crate::handlers::common::api_client(),
            base,
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    fn posts(w: &[(String, String, serde_json::Value)]) -> Vec<serde_json::Value> {
        w.iter()
            .filter(|(m, _, _)| m == "POST")
            .map(|(_, _, b)| b.clone())
            .collect()
    }

    // -- the pure parser -------------------------------------------------

    fn fallback_args() -> Vec<(String, Value)> {
        let mut a = args();
        a.push((
            "no_red_failure_route".into(),
            Value::String("check-failure".into()),
        ));
        a
    }

    #[test]
    fn only_the_break_glass_rule_declares_the_no_red_failure_route() {
        let root = boss_testing::repo_root();
        let rules = root.join("infra/dispatcher/rules");
        let mut declarations = Vec::new();
        for entry in std::fs::read_dir(rules).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|x| x.to_str()) != Some("toml") {
                continue;
            }
            let parsed: toml::Value =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            for rule in parsed["rule"].as_array().unwrap() {
                for action in rule["do"].as_array().unwrap() {
                    if action
                        .get("args")
                        .and_then(|x| x.get("no_red_failure_route"))
                        .is_some()
                    {
                        declarations.push(rule["name"].as_str().unwrap().to_string());
                        assert_eq!(
                            action["args"]["no_red_failure_route"].as_str(),
                            Some("\"break-glass-deposit\"")
                        );
                        assert_eq!(
                            rule["when"].as_str(),
                            Some(
                                "kind = \"maintenance-break-glass-deposit\" AND outcome = \"failed\""
                            )
                        );
                    }
                }
            }
        }
        assert_eq!(
            declarations,
            ["file-backlog-items-on-break-glass-deposit-red"]
        );
    }

    #[tokio::test]
    async fn a_declared_failed_check_without_red_files_one_owned_item() {
        let (base, writes) = mock_jobs(vec![crawl(NO_RED_OUTPUT)]).await;
        let h = handler(base);
        h.invoke(&fallback_args(), &ctx()).await.unwrap();
        h.invoke(&fallback_args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(filed.len(), 1, "{w:?}");
        assert_eq!(filed[0]["owner_id"], "emp-owner");
        assert_eq!(
            filed[0]["metadata"]["input_channel"],
            super::super::common::lane_label(InputChannel::PipelineFailure)
        );
        assert_eq!(filed[0]["metadata"][RED_ROUTE], "check-failure");
        assert_eq!(filed[0]["metadata"]["source"], source_of_chore(CRAWL));
        assert_eq!(filed[0]["metadata"]["failure_output"], NO_RED_OUTPUT);
        assert_eq!(filed[0]["metadata"]["failure_exit_status"], "1");
        assert_eq!(w.len(), 2, "redelivery writes nothing");
    }

    #[tokio::test]
    async fn the_authored_break_glass_rule_dispatches_and_dedups_two_crashed_days() {
        use boss_dispatcher::rules::{
            expr::NoHelpers,
            handler::{HandlerRegistry, dispatch},
            registry::{Registry, match_event},
        };
        let rule = std::fs::read_to_string(
            boss_testing::repo_root()
                .join("infra/dispatcher/rules/file-backlog-items-on-break-glass-deposit-red.toml"),
        )
        .unwrap();
        let registry = Registry::from_toml(&rule).unwrap();
        const DAY2: &str = "33333333-3333-3333-3333-333333333332";
        let mut first = crawl(NO_RED_OUTPUT);
        first["kind"] = json!("maintenance-break-glass-deposit");
        let mut second = first.clone();
        second["id"] = json!(DAY2);
        let (base, writes) = mock_jobs(vec![first, second]).await;
        let mut handlers = HandlerRegistry::new();
        handlers.register(handler(base));
        for id in [CRAWL, CRAWL, DAY2] {
            let payload =
                json!({"id": id, "kind": "maintenance-break-glass-deposit", "outcome": "failed"});
            let matched = match_event(&registry, "jobs.job.closed", &payload, &NoHelpers).matched;
            assert_eq!(matched.len(), 1);
            let result = dispatch(
                &matched,
                &handlers,
                "event-crash",
                "jobs.job.closed",
                &payload,
                None,
            )
            .await
            .unwrap();
            assert!(result[0].outcome.is_ok(), "{result:?}");
        }
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(
            filed.len(),
            1,
            "two failed days and redelivery are one open item: {w:?}"
        );
        assert_eq!(filed[0]["metadata"][RED_ROUTE], "break-glass-deposit");
        assert_eq!(filed[0]["metadata"]["design"], "835c0c9c");
        assert_eq!(filed[0]["metadata"]["area"], "credentials");
        assert_eq!(
            filed[0]["metadata"]["reporter"],
            "file-backlog-items-on-break-glass-deposit-red"
        );
        assert_eq!(filed[0]["owner_id"], "emp-owner");
        assert_eq!(w.len(), 3);
    }

    /// The boss-gcp deposit key rule fires on the converge's `run` step,
    /// not its close (review 1fa0d093 of car 3cd92f7a, B2): a finding
    /// rides an ok tick, and only `step.done.task` carries the step
    /// metadata that tells a finding tick from a clean one. That marker
    /// names the packet as `job_id`; the judge read only `id`, the close
    /// marker's key, and so filed nothing for it.
    #[tokio::test]
    async fn the_authored_deposit_key_rule_files_one_item_from_the_run_step_marker() {
        use boss_dispatcher::rules::{
            expr::NoHelpers,
            handler::{HandlerRegistry, dispatch},
            registry::{Registry, match_event},
        };
        const RULE_FILE: &str = "file-backlog-items-on-boss-gcp-deposit-key-unconfined";
        const TICK2: &str = "33333333-3333-3333-3333-333333333334";
        let rule = std::fs::read_to_string(
            boss_testing::repo_root().join(format!("infra/dispatcher/rules/{RULE_FILE}.toml")),
        )
        .unwrap();
        let registry = Registry::from_toml(&rule).unwrap();
        let red = "RED boss-gcp-converge/deposit-key-unconfined refused: deposit-key-audit: \
                   UNCONFINED /home/david/.ssh/authorized_keys:1 — the key ssh-ed25519 \
                   ...Q8OBmag8/TG5 is also on this line with NO forced command";
        let tick = |id: &str| {
            let mut job = crawl(red);
            job["id"] = json!(id);
            job["kind"] = json!("maintenance-boss-gcp-converge");
            job["title"] = json!("boss-gcp adopts its own units");
            job["steps"][1]["metadata"] = json!({
                "result": "ok", "deposit_key_verdict": "unconfined", "output": red,
            });
            job
        };
        let (base, writes) = mock_jobs(vec![tick(CRAWL), tick(TICK2)]).await;
        let mut handlers = HandlerRegistry::new();
        handlers.register(handler(base));
        for id in [CRAWL, CRAWL, TICK2] {
            let payload = json!({
                "job_id": id, "step_id": "c-run", "kind": "task",
                "subject_kind": "custom", "subject_id": "infra/maintenance-boss-gcp-converge",
                "workflow_kind": "maintenance-boss-gcp-converge",
                "completed_on": "2026-10-10", "notify_on_done": false, "spec_slug": "run",
                "metadata": {"result": "ok", "deposit_key_verdict": "unconfined", "output": red},
            });
            let matched = match_event(&registry, "step.done.task", &payload, &NoHelpers).matched;
            assert_eq!(matched.len(), 1);
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
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(
            filed.len(),
            1,
            "two finding ticks and a redelivery are one open item: {w:?}"
        );
        assert_eq!(
            filed[0]["metadata"][RED_ROUTE],
            "boss-gcp-converge/deposit-key-unconfined"
        );
        assert_eq!(filed[0]["metadata"]["area"], "credentials");
        assert_eq!(filed[0]["metadata"]["source"], source_of_chore(CRAWL));
        assert_eq!(filed[0]["owner_id"], "emp-owner");
    }

    /// A SECOND LINE WHILE THE FIRST IS OPEN IS NAMED (backlog d1b03874).
    /// Every deposit key finding prints under ONE route, so a judge that
    /// dedups by route alone filed the first converge's line and then
    /// nothing for a different line found while that item was open: the
    /// open item named one line of two, and removing a line is David's
    /// hand act, done from what the item says. The authored rule declares
    /// `one_item_per = "finding"`, so each distinct RED line is its own
    /// item: tick 1 finds line 1, tick 2 finds line 1 AND a new line 4,
    /// tick 3 is line 1 again — two items, each naming its own line, and
    /// the repeat (and a redelivery) files nothing.
    #[tokio::test]
    async fn the_authored_deposit_key_rule_names_a_second_line_found_while_the_first_is_open() {
        use boss_dispatcher::rules::{
            expr::NoHelpers,
            handler::{HandlerRegistry, dispatch},
            registry::{Registry, match_event},
        };
        const RULE_FILE: &str = "file-backlog-items-on-boss-gcp-deposit-key-unconfined";
        const TICK2: &str = "33333333-3333-3333-3333-333333333335";
        const TICK3: &str = "33333333-3333-3333-3333-333333333336";
        let rule = std::fs::read_to_string(
            boss_testing::repo_root().join(format!("infra/dispatcher/rules/{RULE_FILE}.toml")),
        )
        .unwrap();
        let registry = Registry::from_toml(&rule).unwrap();
        let line1 = "RED boss-gcp-converge/deposit-key-unconfined refused: deposit-key-audit: \
                     UNCONFINED /home/david/.ssh/authorized_keys:1 — the key ssh-ed25519 \
                     SHA256:aaaa is also on this line with NO forced command";
        let line4 = "RED boss-gcp-converge/deposit-key-unconfined refused: deposit-key-audit: \
                     UNCONFINED /home/david/.ssh/authorized_keys:4 — the key ssh-ed25519 \
                     SHA256:bbbb is forced to the receiver WITHOUT restrict";
        let both = format!("{line1}\n{line4}");
        let tick = |id: &str, red: &str| {
            let mut job = crawl(red);
            job["id"] = json!(id);
            job["kind"] = json!("maintenance-boss-gcp-converge");
            job["title"] = json!("boss-gcp adopts its own units");
            job["steps"][1]["metadata"] = json!({
                "result": "ok", "deposit_key_verdict": "unconfined", "output": red,
            });
            job
        };
        let (base, writes) = mock_jobs(vec![
            tick(CRAWL, line1),
            tick(TICK2, &both),
            tick(TICK3, line1),
        ])
        .await;
        let mut handlers = HandlerRegistry::new();
        handlers.register(handler(base));
        for (id, red) in [
            (CRAWL, line1),
            (CRAWL, line1),
            (TICK2, both.as_str()),
            (TICK3, line1),
        ] {
            let payload = json!({
                "job_id": id, "step_id": "c-run", "kind": "task",
                "subject_kind": "custom", "subject_id": "infra/maintenance-boss-gcp-converge",
                "workflow_kind": "maintenance-boss-gcp-converge",
                "completed_on": "2026-10-18", "notify_on_done": false, "spec_slug": "run",
                "metadata": {"result": "ok", "deposit_key_verdict": "unconfined", "output": red},
            });
            let matched = match_event(&registry, "step.done.task", &payload, &NoHelpers).matched;
            assert_eq!(matched.len(), 1);
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
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(
            filed.len(),
            2,
            "two distinct lines are two open items; a repeat and a redelivery file nothing: {w:?}"
        );
        for (item, fp, at) in [
            (&filed[0], "SHA256:aaaa", "authorized_keys:1"),
            (&filed[1], "SHA256:bbbb", "authorized_keys:4"),
        ] {
            // Keyed on the key it names, not the line (backlog 52534a15).
            assert_eq!(
                item["metadata"][RED_FINDING],
                format!("boss-gcp-converge/deposit-key-unconfined refused: key {fp}")
            );
            assert_eq!(
                item["metadata"][RED_ROUTE],
                "boss-gcp-converge/deposit-key-unconfined"
            );
            assert!(
                item["title"].as_str().unwrap().contains(at),
                "the title names the line to act on: {item}"
            );
            assert!(
                item["metadata"]["red_error"].as_str().unwrap().contains(at),
                "{item}"
            );
            assert_eq!(item["metadata"]["area"], "credentials");
        }
        assert_eq!(filed[1]["metadata"]["source"], source_of_chore(TICK2));
        let judged: Vec<&str> = w
            .iter()
            .filter(|(m, _, _)| m == "PATCH")
            .filter_map(|(_, _, b)| b[JUDGED].as_str())
            .collect();
        assert_eq!(judged.len(), 3, "each tick judged once: {judged:?}");
        assert!(
            judged[2].contains("filed 0 backlog-item(s)"),
            "the repeat of line 1 files nothing and says so: {}",
            judged[2]
        );
    }

    // -- a finding is its KEY, and a judgement that drops it closes it ---
    // (backlog 52534a15)

    const DEPOSIT_RULE: &str = "file-backlog-items-on-boss-gcp-deposit-key-unconfined";
    const FP_A: &str = "SHA256:Q8OBmag8TG5aAaAaAaAaAaAaAaAaAaAaAaAaAaA";
    const FP_B: &str = "SHA256:bBbBbBbBbBbBbBbBbBbBbBbBbBbBbBbBbBbBbBbBbBb";

    /// One `UNCONFINED` RED line as boss-gcp-converge.sh prints it, for the
    /// key `fp` at `authorized_keys:<n>`, with `why` as the audit's prose.
    fn deposit_red(n: u32, fp: &str, why: &str) -> String {
        format!(
            "RED boss-gcp-converge/deposit-key-unconfined refused: deposit-key-audit: \
             UNCONFINED /home/david/.ssh/authorized_keys:{n} — the key ED25519 {fp} {why}"
        )
    }

    /// A converge tick as its packet reads back: the verdict word and
    /// `output` on a completed `run` step, opened at `opened_at`.
    fn deposit_tick(id: &str, opened_at: &str, verdict: &str, output: &str) -> serde_json::Value {
        let mut job = crawl(output);
        job["id"] = json!(id);
        job["kind"] = json!("maintenance-boss-gcp-converge");
        job["title"] = json!("boss-gcp adopts its own units");
        job["status"] = json!("open");
        job["opened_at"] = json!(opened_at);
        job["steps"][1]["completed_at"] = json!(opened_at);
        job["steps"][1]["metadata"] = json!({
            "result": "ok", "deposit_key_verdict": verdict, "output": output,
        });
        job
    }

    /// Deliver `id`'s `run` marker through the AUTHORED rule file, as the
    /// dispatcher would; `false` when the rule does not select it.
    async fn deliver_deposit(base: &str, id: &str, verdict: &str, output: &str) -> bool {
        use boss_dispatcher::rules::{
            expr::NoHelpers,
            handler::{HandlerRegistry, dispatch},
            registry::{Registry, match_event},
        };
        let rule = std::fs::read_to_string(
            boss_testing::repo_root().join(format!("infra/dispatcher/rules/{DEPOSIT_RULE}.toml")),
        )
        .unwrap();
        let registry = Registry::from_toml(&rule).unwrap();
        let mut handlers = HandlerRegistry::new();
        handlers.register(handler(base.to_string()));
        let payload = json!({
            "job_id": id, "step_id": "c-run", "kind": "task",
            "subject_kind": "custom", "subject_id": "infra/maintenance-boss-gcp-converge",
            "workflow_kind": "maintenance-boss-gcp-converge",
            "completed_on": "2026-10-19", "notify_on_done": false, "spec_slug": "run",
            "metadata": {"result": "ok", "deposit_key_verdict": verdict, "output": output},
        });
        let matched = match_event(&registry, "step.done.task", &payload, &NoHelpers).matched;
        if matched.is_empty() {
            return false;
        }
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
        true
    }

    fn item_rows(rows: &Rows) -> Vec<serde_json::Value> {
        let mut items: Vec<serde_json::Value> = rows
            .lock()
            .unwrap()
            .values()
            .filter(|j| j["kind"] == "backlog-item")
            .cloned()
            .collect();
        items.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        items
    }

    /// The fingerprint is read off the error as ssh-keygen prints it, up to
    /// the first character a base64 fingerprint cannot hold; a finding that
    /// names no key keeps its whole line as its key.
    #[test]
    fn a_finding_is_keyed_on_the_first_fingerprint_it_names() {
        let e = format!("UNCONFINED /h/a:9 — the key ED25519 {FP_A} (me@x), forced to 'SHA256:zz'");
        assert_eq!(key_fingerprint(&e), Some(FP_A));
        assert_eq!(
            key_fingerprint("the key ED25519 SHA256:abc/+=, then"),
            Some("SHA256:abc/+=")
        );
        assert_eq!(key_fingerprint("reads no key from this line"), None);
        assert_eq!(key_fingerprint("SHA256: nothing after"), None);
        assert_eq!(finding_key("r", "k", &e), format!("r k: key {FP_A}"));
        assert_eq!(finding_key("r", "k", "no key here"), "r k: no key here");
    }

    const T1: &str = "44444444-0000-0000-0000-000000000001";
    const T2: &str = "44444444-0000-0000-0000-000000000002";
    const T3: &str = "44444444-0000-0000-0000-000000000003";

    /// (1) A SHIFTED LINE IS THE SAME FINDING. David removes an earlier
    /// line of authorized_keys, so key A's finding moves from line 4 to
    /// line 3: the open item still matches — nothing is filed beside it —
    /// and it now names line 3, the line to act on.
    #[tokio::test]
    async fn a_shifted_line_keeps_matching_its_open_item_and_names_the_new_line() {
        let why = "is forced to the receiver WITHOUT restrict.";
        let (at4, at3) = (deposit_red(4, FP_A, why), deposit_red(3, FP_A, why));
        let (base, writes, rows) = mock_jobs_rows(vec![deposit_tick(
            T1,
            "2026-10-19T06:00:00+00:00",
            "unconfined",
            &at4,
        )])
        .await;
        assert!(deliver_deposit(&base, T1, "unconfined", &at4).await);
        rows.lock().unwrap().insert(
            T2.into(),
            deposit_tick(T2, "2026-10-19T06:30:00+00:00", "unconfined", &at3),
        );
        assert!(deliver_deposit(&base, T2, "unconfined", &at3).await);
        let w = writes.lock().unwrap().clone();
        assert_eq!(
            posts(&w).len(),
            1,
            "a shifted line files nothing beside its item: {w:?}"
        );
        let items = item_rows(&rows);
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0]["status"], "open",
            "the finding still stands: {}",
            items[0]
        );
        assert_eq!(
            items[0]["metadata"][RED_FINDING],
            format!("boss-gcp-converge/deposit-key-unconfined refused: key {FP_A}"),
            "keyed on the key's fingerprint, never on file:line"
        );
        let error = items[0]["metadata"]["red_error"].as_str().unwrap();
        assert!(
            error.contains("authorized_keys:3") && !error.contains("authorized_keys:4"),
            "the item names the line as it stands now: {error}"
        );
    }

    /// (2) A REWORDED FINDING IS THE SAME FINDING. A later audit prints
    /// key A's finding in new prose (a car rewrote the text): it does not
    /// re-file beside the stale item.
    #[tokio::test]
    async fn a_reworded_finding_does_not_refile_beside_its_open_item() {
        let old = deposit_red(4, FP_A, "is forced to the receiver WITHOUT restrict.");
        let new = deposit_red(
            4,
            FP_A,
            "is forced to '/usr/local/libexec/boss/x' WITHOUT restrict, so sshd also gives it a pty.",
        );
        let (base, writes, rows) = mock_jobs_rows(vec![deposit_tick(
            T1,
            "2026-10-19T06:00:00+00:00",
            "unconfined",
            &old,
        )])
        .await;
        assert!(deliver_deposit(&base, T1, "unconfined", &old).await);
        rows.lock().unwrap().insert(
            T2.into(),
            deposit_tick(T2, "2026-10-19T06:30:00+00:00", "unconfined", &new),
        );
        assert!(deliver_deposit(&base, T2, "unconfined", &new).await);
        let w = writes.lock().unwrap().clone();
        assert_eq!(
            posts(&w).len(),
            1,
            "a reworded finding is not a new one: {w:?}"
        );
        let items = item_rows(&rows);
        assert_eq!(items[0]["status"], "open");
        assert!(
            items[0]["metadata"]["red_error"]
                .as_str()
                .unwrap()
                .contains("so sshd also gives it a pty"),
            "the item carries the audit's current words: {}",
            items[0]
        );
    }

    /// An item filed under rule v2 carries the whole RED line as its key;
    /// it is the same finding as the fingerprint key, so the first tick
    /// after this lands neither re-files it nor closes it.
    #[tokio::test]
    async fn an_item_keyed_on_the_whole_line_matches_its_fingerprint() {
        let line = deposit_red(4, FP_A, "is forced to the receiver WITHOUT restrict.");
        let (base, writes, rows) = mock_jobs_rows(vec![deposit_tick(
            T1,
            "2026-10-19T06:00:00+00:00",
            "unconfined",
            &line,
        )])
        .await;
        assert!(deliver_deposit(&base, T1, "unconfined", &line).await);
        // Rewrite the filed item's key to the v2 shape.
        let id = item_rows(&rows)[0]["id"].as_str().unwrap().to_string();
        rows.lock().unwrap().get_mut(&id).unwrap()["metadata"][RED_FINDING] =
            json!(line.strip_prefix(RED_PREFIX).unwrap());
        let shifted = deposit_red(3, FP_A, "is forced to the receiver WITHOUT restrict.");
        rows.lock().unwrap().insert(
            T2.into(),
            deposit_tick(T2, "2026-10-19T06:30:00+00:00", "unconfined", &shifted),
        );
        assert!(deliver_deposit(&base, T2, "unconfined", &shifted).await);
        let w = writes.lock().unwrap().clone();
        assert_eq!(posts(&w).len(), 1, "{w:?}");
        assert_eq!(item_rows(&rows)[0]["status"], "open");
    }

    /// (3) A CLEARED FINDING CLOSES ITS ITEM. Tick 2 prints key B only — a
    /// changed judgement: key A's item closes `stale` at triage, citing
    /// the tick that stopped printing it, and B files. Tick 3 reads
    /// `clean`: B's item closes too. Each close is the judge's own act,
    /// recorded with the reading that cleared it.
    #[tokio::test]
    async fn a_cleared_finding_closes_its_item_citing_the_tick_that_dropped_it() {
        let a = deposit_red(4, FP_A, "is forced to the receiver WITHOUT restrict.");
        let b = deposit_red(
            7,
            FP_B,
            "is a prepared deposit key on a line with NO forced command.",
        );
        let (base, writes, rows) = mock_jobs_rows(vec![deposit_tick(
            T1,
            "2026-10-19T06:00:00+00:00",
            "unconfined",
            &a,
        )])
        .await;
        assert!(deliver_deposit(&base, T1, "unconfined", &a).await);
        rows.lock().unwrap().insert(
            T2.into(),
            deposit_tick(T2, "2026-10-19T06:30:00+00:00", "unconfined", &b),
        );
        assert!(deliver_deposit(&base, T2, "unconfined", &b).await);
        let items = item_rows(&rows);
        assert_eq!(items.len(), 2, "{items:?}");
        let (item_a, item_b) = (&items[0], &items[1]);
        assert_eq!(
            item_a["status"], "closed",
            "A is no longer printed: {item_a}"
        );
        let triage = item_a["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["spec_slug"] == "triage")
            .unwrap();
        assert_eq!(triage["metadata"]["disposition"], "stale");
        assert_eq!(
            triage["metadata"]["cleared_by"],
            "maintenance.chore.file_reds"
        );
        let evidence = triage["metadata"]["evidence"].as_str().unwrap();
        assert!(
            evidence.contains(&T2[..8]) && evidence.contains(FP_A),
            "the close names the tick and the key: {evidence}"
        );
        assert_eq!(
            triage["metadata"]["recovered_at"],
            "2026-10-19T06:30:00+00:00"
        );
        assert_eq!(item_b["status"], "open");

        // A clean tick: B closes; a redelivery writes nothing more.
        rows.lock().unwrap().insert(
            T3.into(),
            deposit_tick(T3, "2026-10-19T07:00:00+00:00", "clean", ""),
        );
        assert!(deliver_deposit(&base, T3, "clean", "").await);
        let before = writes.lock().unwrap().len();
        assert!(deliver_deposit(&base, T3, "clean", "").await);
        assert_eq!(
            writes.lock().unwrap().len(),
            before,
            "a redelivery is a no-op"
        );
        assert!(
            item_rows(&rows).iter().all(|i| i["status"] == "closed"),
            "{:?}",
            item_rows(&rows)
        );
        let note = rows.lock().unwrap()[T3]["metadata"][JUDGED]
            .as_str()
            .unwrap_or("")
            .to_string();
        assert!(
            note.contains("closed 1"),
            "the clean tick says what it closed: {note}"
        );
    }

    /// An UNVERIFIED tick is no evidence either way: the rule does not
    /// hand it over, so nothing closes. A clean tick older than the item
    /// (a redelivery from before it was filed) closes nothing; and an
    /// item a person has written on is told, never closed from under them.
    #[tokio::test]
    async fn unverified_older_or_touched_closes_nothing() {
        let a = deposit_red(4, FP_A, "is forced to the receiver WITHOUT restrict.");
        let (base, writes, rows) = mock_jobs_rows(vec![
            deposit_tick(T1, "2026-10-19T06:00:00+00:00", "unconfined", &a),
            deposit_tick(T2, "2026-10-19T05:00:00+00:00", "clean", ""),
        ])
        .await;
        assert!(deliver_deposit(&base, T1, "unconfined", &a).await);
        assert!(
            !deliver_deposit(&base, T1, "unverified", "").await,
            "the rule does not select an unverified tick"
        );
        assert!(deliver_deposit(&base, T2, "clean", "").await);
        assert_eq!(
            item_rows(&rows)[0]["status"],
            "open",
            "an older tick closes nothing"
        );
        let id = item_rows(&rows)[0]["id"].as_str().unwrap().to_string();
        rows.lock().unwrap().get_mut(&id).unwrap()["metadata"]["owner_note"] =
            json!("removing it tonight");
        rows.lock().unwrap().insert(
            T3.into(),
            deposit_tick(T3, "2026-10-19T07:00:00+00:00", "clean", ""),
        );
        assert!(deliver_deposit(&base, T3, "clean", "").await);
        let item = &item_rows(&rows)[0];
        assert_eq!(
            item["status"], "open",
            "a person's item is told, not closed: {item}"
        );
        assert!(
            item["metadata"]["recovery"]
                .as_str()
                .unwrap_or("")
                .contains("RECOVERED")
        );
        let _ = writes;
    }

    /// The crawl keeps one item per ROUTE: a route already open files
    /// nothing even when tonight's error text differs (ac3270c7), and an
    /// unknown `one_item_per` is a rule mistake, refused permanently.
    #[tokio::test]
    async fn one_item_per_defaults_to_the_route_and_refuses_an_unknown_value() {
        let (base, writes) = mock_jobs(vec![
            crawl(RED_OUTPUT),
            open_item("old", "/it/registry/dispatcher"),
        ])
        .await;
        let h = handler(base);
        h.invoke(&args(), &ctx()).await.unwrap();
        let filed = posts(&writes.lock().unwrap());
        assert_eq!(filed.len(), 1, "{filed:?}");
        assert_eq!(filed[0]["metadata"][RED_ROUTE], "/ux/catalog/models/m-1");
        assert!(filed[0]["metadata"].get(RED_FINDING).is_none());
        let mut a = args();
        a.push(("one_item_per".into(), Value::String("line".into())));
        assert!(h.invoke(&a, &ctx()).await.unwrap_err().is_permanent());
    }

    /// Two identical finding lines on one tick are one finding, one item.
    #[tokio::test]
    async fn one_item_per_finding_files_a_repeated_line_on_one_tick_once() {
        let (base, writes) = mock_jobs(vec![crawl(RED_OUTPUT)]).await;
        let mut a = args();
        a.push(("one_item_per".into(), Value::String("finding".into())));
        handler(base).invoke(&a, &ctx()).await.unwrap();
        let filed = posts(&writes.lock().unwrap());
        assert_eq!(
            filed.len(),
            2,
            "the dispatcher route printed one error twice, so two distinct findings: {filed:?}"
        );
    }

    #[tokio::test]
    async fn missing_step_and_malformed_exit_remain_unknown_and_bad_declarations_refuse() {
        let mut missing = crawl("");
        missing["steps"] = json!([]);
        let (base, writes) = mock_jobs(vec![missing]).await;
        handler(base)
            .invoke(&fallback_args(), &ctx())
            .await
            .unwrap();
        let w = writes.lock().unwrap().clone();
        let f = posts(&w);
        assert_eq!(f[0]["metadata"]["failure_output_state"], "missing");
        assert_eq!(f[0]["metadata"]["failure_exit_status_state"], "missing");
        assert!(f[0]["metadata"]["failure_exit_status"].is_null());
        let mut bad = crawl("RED malformed\n");
        bad["steps"][1]["metadata"]["exit_status"] = json!({"wrong": 1});
        let (base, writes) = mock_jobs(vec![bad]).await;
        let h = handler(base);
        h.invoke(&fallback_args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        let f = posts(&w);
        assert_eq!(f[0]["metadata"]["failure_output"], "RED malformed\n");
        assert_eq!(
            f[0]["metadata"]["failure_exit_status_state"],
            "not an integer string"
        );
        assert!(f[0]["metadata"]["failure_exit_status"].is_null());
        for route in ["", "two words", &"x".repeat(129)] {
            let mut a = args();
            a.push(("no_red_failure_route".into(), Value::String(route.into())));
            assert!(h.invoke(&a, &ctx()).await.unwrap_err().is_permanent());
        }
    }

    #[tokio::test]
    async fn a_declared_failure_dedups_an_open_item_without_a_judged_note() {
        let (base, writes) = mock_jobs(vec![
            crawl(NO_RED_OUTPUT),
            open_item("old", "check-failure"),
        ])
        .await;
        handler(base)
            .invoke(&fallback_args(), &ctx())
            .await
            .unwrap();
        let w = writes.lock().unwrap().clone();
        assert!(posts(&w).is_empty());
        assert!(w[0].2[JUDGED].as_str().unwrap().contains("already open"));
    }

    #[tokio::test]
    async fn declared_fallback_keeps_positive_red_and_nonfailed_controls() {
        let (base, writes) = mock_jobs(vec![crawl(RED_OUTPUT)]).await;
        handler(base)
            .invoke(&fallback_args(), &ctx())
            .await
            .unwrap();
        assert_eq!(posts(&writes.lock().unwrap()).len(), 2);
        let (base, writes) = mock_jobs(vec![crawl(NO_RED_OUTPUT)]).await;
        let mut c = ctx();
        c.event_payload["outcome"] = json!("ok");
        handler(base).invoke(&fallback_args(), &c).await.unwrap();
        assert!(posts(&writes.lock().unwrap()).is_empty());
    }

    #[tokio::test]
    async fn absent_or_malformed_failure_output_is_named_not_invented_and_capture_is_bounded() {
        for output in [None, Some(json!(7)), Some(json!("x".repeat(6000)))] {
            let mut job = crawl("");
            job["steps"][1]["metadata"]
                .as_object_mut()
                .unwrap()
                .remove("output");
            if let Some(v) = output.clone() {
                job["steps"][1]["metadata"]["output"] = v;
            }
            let (base, writes) = mock_jobs(vec![job]).await;
            handler(base)
                .invoke(&fallback_args(), &ctx())
                .await
                .unwrap();
            let w = writes.lock().unwrap().clone();
            let f = posts(&w);
            assert_eq!(f.len(), 1);
            assert_eq!(
                f[0]["metadata"]["failure_output_state"],
                match output.as_ref() {
                    None => "missing",
                    Some(value) if value.is_string() => "recorded",
                    Some(_) => "not a string",
                }
            );
            assert!(f[0]["metadata"].get("red_error").is_none());
            if output.as_ref().is_some_and(|x| x.is_string()) {
                assert_eq!(
                    f[0]["metadata"]["failure_output"].as_str().unwrap().len(),
                    4096
                );
                assert_eq!(f[0]["metadata"]["failure_output_truncated"], true);
            }
        }
    }

    /// Three RED lines, two routes: the route that threw twice is ONE
    /// finding with both errors, in the order printed.
    #[test]
    fn red_lines_group_by_route_in_first_seen_order() {
        let reds = red_routes(RED_OUTPUT);
        assert_eq!(reds.len(), 2, "{reds:?}");
        assert_eq!(reds[0].route, "/it/registry/dispatcher");
        assert_eq!(reds[0].findings.len(), 2);
        assert_eq!(reds[0].findings[0].0, "pageerror");
        assert!(
            reds[0].findings[0]
                .1
                .starts_with("Cannot read properties of null (reading 'why')"),
            "the error text copied, not retyped: {:?}",
            reds[0].findings[0].1
        );
        assert_eq!(reds[1].route, "/ux/catalog/models/m-1");
        assert_eq!(
            reds[1].findings,
            vec![(
                "no-shell".to_string(),
                "Timed out 20000ms waiting for expect(locator).toBeVisible()".to_string()
            )]
        );
    }

    /// The summary line, the console.error lines and a clone refusal
    /// are not findings; neither is a `RED` line without the shape.
    #[test]
    fn only_shaped_red_lines_are_findings() {
        assert!(red_routes(NO_RED_OUTPUT).is_empty());
        assert!(red_routes("").is_empty());
        assert!(
            red_routes("REDACTED /x y: z\nRED\nRED /only-route\nRED /r kind with space: e\n")
                .is_empty()
        );
        // Indented (a reporter that nests test stdout) still counts.
        assert_eq!(red_routes("   RED /a pageerror: boom\n")[0].route, "/a");
        // An error with a colon of its own keeps it.
        assert_eq!(
            red_routes("RED /a pageerror: x: y\n")[0].findings[0].1,
            "x: y"
        );
    }

    /// The item names the route on its title and carries the route,
    /// kind, error, design id and the chore as evidence.
    #[test]
    fn the_item_carries_route_error_design_and_source() {
        let red = &red_routes(RED_OUTPUT)[0];
        let body = item_body(
            red,
            CRAWL,
            "maintenance-playground-crawl",
            "Playground crawl",
            DESIGN,
            "web",
            "emp-owner",
            &ctx(),
        );
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(
            body["title"],
            "Playground crawl red: /it/registry/dispatcher (pageerror)"
        );
        assert_eq!(body["priority"], "standard");
        assert_eq!(body["owner_id"], "emp-owner");
        let m = &body["metadata"];
        assert_eq!(m["area"], "web");
        assert_eq!(m["design"], DESIGN);
        assert_eq!(m[RED_ROUTE], "/it/registry/dispatcher");
        assert_eq!(m["red_kind"], "pageerror");
        assert!(
            m["red_error"]
                .as_str()
                .is_some_and(|e| e.starts_with("pageerror: Cannot read properties of null")),
            "{m}"
        );
        // THE SOURCE IS STRUCTURED (backlog c8c6b9a8): `{kind: packet,
        // id}`, the shape `boss job file --source` records and the
        // receiving yard groups by — a bare id string read as "no
        // recorded source" on all 43 items of 2026-09-28.
        assert_eq!(m["source"], json!({"kind": "packet", "id": CRAWL}));
        let reading = boss_jobs::origin::source_of(m);
        assert_eq!(
            reading.basis,
            boss_jobs::origin::SourceBasis::Recorded,
            "the receiving yard reads the source as recorded: {m}"
        );
        assert_eq!(reading.key.as_deref(), Some(&*format!("packet:{CRAWL}")));
        assert_eq!(m["reporter"], RULE);
        assert!(
            m["detail"].as_str().is_some_and(
                |d| d.contains("0e07ce64") && d.contains("Cannot read properties of null")
            ),
            "{m}"
        );
    }

    // -- the handler -----------------------------------------------------

    /// A red night files one item per route (two, from three lines) and
    /// notes on the chore what was filed.
    #[tokio::test]
    async fn a_red_crawl_files_one_item_per_route_and_notes_the_chore() {
        let (base, writes) = mock_jobs(vec![crawl(RED_OUTPUT)]).await;
        handler(base).invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(filed.len(), 2, "one item per ROUTE, not per line: {w:?}");
        let routes: Vec<&str> = filed
            .iter()
            .map(|f| f["metadata"][RED_ROUTE].as_str().unwrap())
            .collect();
        assert_eq!(
            routes,
            ["/it/registry/dispatcher", "/ux/catalog/models/m-1"]
        );
        assert!(
            filed.iter().all(|f| f["metadata"]["design"] == DESIGN),
            "every item carries the design id"
        );
        let note = w.last().unwrap();
        assert_eq!(
            (note.0.as_str(), note.1.as_str()),
            ("PATCH", &*format!("/api/jobs/{CRAWL}/metadata"))
        );
        let text = note.2[JUDGED].as_str().unwrap_or("");
        assert!(
            text.contains("2 red route(s)") && text.contains("filed 2 backlog-item(s)"),
            "{text}"
        );
        assert!(
            text.contains("/it/registry/dispatcher → f0000000"),
            "names each filed item by route: {text}"
        );
        assert_eq!(
            note.2[RED_VERDICT], "2 routes red",
            "the shell rendered and two routes failed on their own: that is `N routes red`, \
             never `shell down`"
        );
        assert_eq!(
            note.2["filed"],
            json!([
                "f0000000-0000-0000-0000-000000000002",
                "f0000000-0000-0000-0000-000000000003"
            ])
        );
    }

    /// A route with an open item files nothing for that route — dedup by
    /// route while one is open — and the note says which was skipped.
    #[tokio::test]
    async fn an_open_item_for_the_route_dedups_it() {
        let (base, writes) = mock_jobs(vec![
            crawl(RED_OUTPUT),
            open_item(
                "aaaaaaaa-0000-0000-0000-000000000001",
                "/it/registry/dispatcher",
            ),
        ])
        .await;
        handler(base).invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(filed.len(), 1, "{w:?}");
        assert_eq!(filed[0]["metadata"][RED_ROUTE], "/ux/catalog/models/m-1");
        let text = w.last().unwrap().2[JUDGED].as_str().unwrap_or("");
        assert!(
            text.contains("filed 1 backlog-item(s)")
                && text.contains("already open [/it/registry/dispatcher]"),
            "{text}"
        );
    }

    /// JetStream is at-least-once: the second delivery reads the note the
    /// first wrote and files nothing; a redelivery whose note never landed
    /// still files nothing, because the routes are open.
    #[tokio::test]
    async fn a_redelivery_files_nothing_twice() {
        let (base, writes) = mock_jobs(vec![crawl(RED_OUTPUT)]).await;
        let h = handler(base);
        h.invoke(&args(), &ctx()).await.unwrap();
        h.invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(posts(&w).len(), 2, "two routes, filed once: {w:?}");
        assert_eq!(
            w.len(),
            3,
            "two POSTs + one note; the redelivery wrote nothing: {w:?}"
        );
    }

    /// A red for another reason — the clone refused — has no RED line:
    /// nothing is filed, and the chore is still noted as judged so the
    /// closed packet does not read as unread.
    #[tokio::test]
    async fn a_red_without_red_lines_files_nothing_and_says_so() {
        let (base, writes) = mock_jobs(vec![crawl(NO_RED_OUTPUT)]).await;
        handler(base).invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 1, "one note, no item: {w:?}");
        assert_eq!(w[0].0, "PATCH");
        let text = w[0].2[JUDGED].as_str().unwrap_or("");
        assert!(text.contains("nothing filed"), "{text}");
        assert!(
            text.contains("no `RED <route> <kind>: <error>` line"),
            "{text}"
        );
    }

    // -- one cause, one item (backlog c8c6b9a8) ---------------------------

    /// The night of 2026-09-28 in miniature: train #768's converge rolled
    /// the playground mid-crawl and no route painted the shell. Each
    /// route's error names its own URL, so the texts differ; the KIND is
    /// the shared cause.
    const SHELL_DOWN_OUTPUT: &str = "crawled 4 routes at http://gw: 4 red, 0 console.error, 0 expected console.error\n\
RED / no-shell: page.goto: net::ERR_CONNECTION_REFUSED at http://gw/\n\
RED /it no-shell: Timed out 20000ms waiting for expect(locator).toBeVisible()\n\
RED /ux/views no-shell: page.goto: net::ERR_CONNECTION_REFUSED at http://gw/ux/views\n\
RED /ux/views no-shell: page.goto: net::ERR_CONNECTION_REFUSED at http://gw/ux/views\n\
RED /system/workflows no-shell: Timed out 20000ms waiting for expect(locator).toBeVisible()\n";

    /// Every route threw on the same answer from the gateway.
    const HTTP_502_OUTPUT: &str = "RED /a pageerror: Failed to load resource: the server responded with a status of 502 (Bad Gateway)\n\
RED /b pageerror: Failed to load resource: the server responded with a status of 502 (Bad Gateway) | at B.svelte:9\n\
RED /c pageerror: HTTP 502 from /api/c\n";

    #[test]
    fn a_shared_cause_is_every_red_route_no_shell_or_one_http_status() {
        assert_eq!(
            shared_cause(&red_routes(SHELL_DOWN_OUTPUT)).as_deref(),
            Some(SHELL_DOWN)
        );
        assert_eq!(
            shared_cause(&red_routes(HTTP_502_OUTPUT)).as_deref(),
            Some("HTTP 502")
        );
        // The shell rendered on the pageerror route: no shared cause.
        assert_eq!(shared_cause(&red_routes(RED_OUTPUT)), None);
        // One red route is a route, not a cause — even a no-shell one.
        assert_eq!(
            shared_cause(&red_routes("RED /x no-shell: Timed out\n")),
            None
        );
        // Two statuses are two causes.
        assert_eq!(
            shared_cause(&red_routes(
                "RED /a pageerror: status of 502\nRED /b pageerror: status of 503\n"
            )),
            None
        );
        // A pageerror with no status beside a no-shell route: no cause.
        assert_eq!(
            shared_cause(&red_routes(
                "RED /a no-shell: Timed out\nRED /b pageerror: boom\n"
            )),
            None
        );
    }

    #[test]
    fn an_http_status_is_read_off_the_error_text_or_not_at_all() {
        assert_eq!(
            http_status("the server responded with a status of 404 (Not Found)"),
            Some(404)
        );
        assert_eq!(http_status("HTTP 503 from /api/x"), Some(503));
        assert_eq!(
            http_status("status of 5020"),
            None,
            "four digits is not a status"
        );
        assert_eq!(http_status("HTTP/1.1 said nothing"), None);
        assert_eq!(http_status("Cannot read properties of null"), None);
    }

    /// Every route no-shell: ONE item naming the cause and listing every
    /// route, and the chore's verdict says `shell down`, not `4 routes red`.
    #[tokio::test]
    async fn every_route_no_shell_files_one_shell_down_item_listing_the_routes() {
        let (base, writes) = mock_jobs(vec![crawl(SHELL_DOWN_OUTPUT)]).await;
        handler(base).invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(filed.len(), 1, "one cause, one item — not four: {w:?}");
        let item = &filed[0];
        assert_eq!(
            item["title"],
            "Playground crawl red: shell down on 4 routes"
        );
        let m = &item["metadata"];
        assert_eq!(m[RED_CAUSE], SHELL_DOWN);
        assert_eq!(
            m[RED_ROUTES],
            json!(["/", "/it", "/ux/views", "/system/workflows"]),
            "every route listed, in the order the crawl printed them"
        );
        assert!(
            m.get(RED_ROUTE).is_none(),
            "a shared-cause item holds no single route, so it never dedups a later night's \
             route-level red: {m}"
        );
        assert_eq!(m["source"], json!({"kind": "packet", "id": CRAWL}));
        assert_eq!(m["design"], DESIGN);
        assert!(
            m["red_error"].as_str().is_some_and(
                |e| e.contains("/ux/views no-shell: page.goto: net::ERR_CONNECTION_REFUSED")
            ),
            "each route's error copied, not retyped: {m}"
        );
        let note = w.last().unwrap();
        assert_eq!(note.0, "PATCH");
        assert_eq!(note.2[RED_VERDICT], "shell down (4 routes)");
        let text = note.2[JUDGED].as_str().unwrap_or("");
        assert!(
            text.contains("shell down") && text.contains("filed 1 backlog-item(s)"),
            "{text}"
        );
        assert_eq!(
            note.2["filed"],
            json!(["f0000000-0000-0000-0000-000000000002"])
        );
    }

    /// The same HTTP status on every red route is one cause too.
    #[tokio::test]
    async fn one_http_status_on_every_red_route_files_one_item() {
        let (base, writes) = mock_jobs(vec![crawl(HTTP_502_OUTPUT)]).await;
        handler(base).invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(filed.len(), 1, "{w:?}");
        assert_eq!(
            filed[0]["title"],
            "Playground crawl red: HTTP 502 on 3 routes"
        );
        assert_eq!(filed[0]["metadata"][RED_CAUSE], "HTTP 502");
        assert_eq!(filed[0]["metadata"][RED_ROUTES], json!(["/a", "/b", "/c"]));
        assert_eq!(w.last().unwrap().2[RED_VERDICT], "HTTP 502 on 3 routes");
    }

    /// A shell-down item still open dedups the next shell-down night: the
    /// cause is the key, the way the route is for a route-level item.
    #[tokio::test]
    async fn an_open_item_for_the_cause_dedups_it() {
        let mut open = open_item("aaaaaaaa-0000-0000-0000-000000000009", "");
        open["metadata"] = json!({ "area": "web", "design": DESIGN, RED_CAUSE: SHELL_DOWN });
        let (base, writes) = mock_jobs(vec![crawl(SHELL_DOWN_OUTPUT), open]).await;
        handler(base).invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert!(posts(&w).is_empty(), "{w:?}");
        let note = &w.last().unwrap().2;
        assert_eq!(note[RED_VERDICT], "shell down (4 routes)");
        let text = note[JUDGED].as_str().unwrap_or("");
        assert!(
            text.contains("filed 0 backlog-item(s)") && text.contains("already open [shell down]"),
            "{text}"
        );
    }

    /// One no-shell route while the rest painted is that route's own red.
    #[tokio::test]
    async fn a_lone_no_shell_route_is_its_own_item() {
        let (base, writes) = mock_jobs(vec![crawl("RED /x/1 no-shell: Timed out 20000ms\n")]).await;
        handler(base).invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(filed.len(), 1, "{w:?}");
        assert_eq!(filed[0]["metadata"][RED_ROUTE], "/x/1");
        assert!(filed[0]["metadata"].get(RED_CAUSE).is_none());
        assert_eq!(w.last().unwrap().2[RED_VERDICT], "1 route red");
    }

    /// The recovery-sheet check (design 125d405d car 3, rule
    /// file-backlog-items-on-recovery-sheet-red): its manifest prints
    /// `RED recovery-sheet <half>: <last line>` when a half fails, so
    /// two failed days in a row — even failing in different halves — are
    /// ONE item keyed `recovery-sheet`, and the second day notes it
    /// already open. A check failing every day is loud once, not silent.
    #[tokio::test]
    async fn consecutive_failed_recovery_sheet_checks_file_one_item() {
        let check = |id: &str, red: &str| {
            json!({
                "id": id,
                "kind": "maintenance-recovery-sheet",
                "title": "Recovery sheet check",
                "status": "closed",
                "metadata": {},
                "steps": [
                    { "id": format!("{id}-run"), "spec_slug": "run", "status": "completed",
                      "metadata": { "result": "failed", "exit_status": "1", "output": red } },
                ],
            })
        };
        let day = |id: &str| InvocationContext {
            event_timestamp: None,
            rule_name: "file-backlog-items-on-recovery-sheet-red".into(),
            triggering_event_id: format!("evt-{id}"),
            triggering_topic: "jobs.job.closed".into(),
            event_payload: json!({
                "id": id, "closed_on": "2026-09-30", "kind": "maintenance-recovery-sheet",
                "outcome": "failed", "title": "Recovery sheet check",
                "subject_id": "infra/maintenance-recovery-sheet", "parent_step_id": null,
            }),
        };
        const DAY1: &str = "33333333-3333-3333-3333-333333333331";
        const DAY2: &str = "33333333-3333-3333-3333-333333333332";
        let (base, writes) = mock_jobs(vec![
            check(
                DAY1,
                "tree version x\nError: the recovery sheet did not print: no Chromium found\nRED recovery-sheet check: Error: the recovery sheet did not print: no Chromium found\n",
            ),
            check(
                DAY2,
                "fatal: Authentication failed\nRED recovery-sheet clone: fatal: Authentication failed\n",
            ),
        ])
        .await;
        let args: Vec<(String, Value)> = [
            ("step", "run"),
            ("design", "125d405d"),
            ("area", "recovery"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.into())))
        .collect();
        let h = handler(base);
        h.invoke(&args, &day(DAY1)).await.unwrap();
        h.invoke(&args, &day(DAY2)).await.unwrap();
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(filed.len(), 1, "two failed days, one item: {w:?}");
        assert_eq!(filed[0]["metadata"][RED_ROUTE], "recovery-sheet");
        assert_eq!(filed[0]["metadata"]["design"], "125d405d");
        assert_eq!(filed[0]["metadata"]["area"], "recovery");
        let second = w.last().unwrap();
        assert_eq!(second.1, format!("/api/jobs/{DAY2}/metadata"));
        assert!(
            second.2[JUDGED]
                .as_str()
                .is_some_and(|t| t.contains("already open [recovery-sheet]")),
            "{second:?}"
        );
    }

    /// Missing args are rule authoring: permanent, never retried.
    #[tokio::test]
    async fn missing_args_are_permanent() {
        let (base, _) = mock_jobs(vec![crawl(RED_OUTPUT)]).await;
        let err = handler(base)
            .invoke(&args()[..2], &ctx())
            .await
            .unwrap_err();
        assert!(err.is_permanent(), "{err}");
    }

    #[tokio::test]
    async fn an_overlong_integer_string_does_not_escape_the_failure_evidence_bound() {
        let mut job = crawl(NO_RED_OUTPUT);
        job["steps"][1]["metadata"]["exit_status"] = json!("0".repeat(6000));
        let (base, writes) = mock_jobs(vec![job]).await;
        handler(base)
            .invoke(&fallback_args(), &ctx())
            .await
            .unwrap();
        let w = writes.lock().unwrap().clone();
        assert!(posts(&w)[0]["metadata"]["failure_exit_status"].is_null());
    }
}
