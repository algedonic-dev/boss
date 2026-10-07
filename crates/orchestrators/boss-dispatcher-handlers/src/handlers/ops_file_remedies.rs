//! `ops.file_remedies` — an estate finding files the passkey-gated
//! request of every verb that declares it remedies that finding.
//!
//! THE GENERALISATION OF CAR 1 (backlog 3df309bf). Car 1 was one rule
//! file, `file-reclaim-gcp-root-while-disk-tight-boss-gcp`, and it was
//! proven live: the boss-gcp below-floor reading filed reclaim-gcp-root
//! and David signed the rendered plan with his passkey, nothing typed.
//! But that rule spelled the pairing — this finding, that verb, on that
//! host — in its own `when` and `jobs.spawn` args, so the next remedy
//! would be the next hand-written rule, and the verb file (the authority
//! for what the runner will execute) knew nothing of the finding it
//! exists to relieve. Now the pairing is DATA ON THE VERB: a verb file
//! declares `remedies = ["<finding key>", …]`, and this one handler,
//! behind one rule, files for any finding any verb declares. Adding a
//! remedy is editing the verb file that is already reviewed as the
//! runner's allowlist; it touches no rule and no code.
//!
//! THE KEY IS THE ALARM'S KEY. A declaration names a finding exactly as
//! `estate.alarm` keys it — `disk_tight:boss-gcp`, `gone:w-2`,
//! `unit_unhealthy:forge/boss-conductor.service` — read off the
//! comparison by the alarm's own `hard_finding_keys`, so a declaration
//! and the alarm it answers cannot disagree about what a finding is
//! called. A declaration whose class the alarm never keys is refused by
//! [`remedy_index`] rather than left to wait forever.
//!
//! IT CAN ONLY FILE WHAT A PASSKEY RUNS. Filing grants nothing: the
//! request's `requires_approval` makes its approve step ready, the
//! runner renders the verb's read-only plan verb onto that step, and the
//! write runs only after a presence stamp by an employee the verb file
//! names in `approvers`, on those bytes (design 17835005; ops-runner.sh
//! `verify_approval`). [`remedy_index`] refuses to index a verb that
//! declares `remedies` without that whole shape — `requires_approval`,
//! a `plan_verb`, named `approvers`, and a final `plan_sha256` param.
//! Legacy string declarations carry no arguments. Argument-bearing
//! declarations are objects: `{"finding":"<key>","args":["<word>",…]}`.
//! Every word must satisfy the verb's own parameter schema; the runner
//! appends the signed hash itself. The registry chooses the target and
//! arguments: this handler infers neither size nor replica placement.
//! Thus a declaration on a verb
//! that would run on filing alone never reaches the queue. The pin
//! `every_declared_remedy_is_a_passkey_gated_verb` holds the tree to it,
//! so such a declaration is a red gate, not a refusal at 03:00.
//!
//! DEDUPED ON THE REQUEST'S OWN SUBJECT. String declarations retain
//! `<verb>@<host>`; object declarations add a deterministic finding UUID,
//! so one claim cannot hold another's request. Proposed argument changes
//! retain the finding's subject and cannot evade its open/decline guard.
//! `<verb>@<host>` is the subject
//! car 1 filed under, kept so the request David signed on the day this
//! landed still counts. Two guards, both carried over from car 1's
//! review (car 4d80316f):
//! - an OPEN request on that subject files nothing more, however old;
//! - a request OPENED within the rule's `refile_after_hours`, whatever
//!   became of it, files nothing more — because a remedy that ran need
//!   not clear its finding (the reclaim frees ~2.3 GB and leaves
//!   boss-gcp at ~16.4 GB against a 17 GB floor), and refiling on every
//!   comparison would be a passkey prompt a day.
//! - a request whose approver DECLINED its plan stops the asking: none
//!   is refiled until the estate observes the finding clear (its alarm
//!   closed after the decline, by the recover rule or with an outcome
//!   of `completed` or `stale` — backlog e5ff616f), because a decline is an
//!   answer and the next episode is a new question (review 63bafc11,
//!   LOW 2b; a person's close counts since backlog aa816dd4). While it
//!   holds, the open alarm says so (`remedy_held:<verb>@<host>`), and
//!   the request filed after it lifts names the close (`decline_lifted`).
//! The board is read WHOLE and judged on each row's admission instant,
//! not on the list's first row, which is ordered by the sim date (LOW
//! 2d); and the request's id is derived from the request it succeeds,
//! so two firings that read the same board post one packet, which the
//! jobs API admits once (LOW 2a; backlog b2f78bb9).
//! A verb-specific subject and not the host's: `custom/boss-gcp` carries
//! dozens of read-only requests a day and would read as "already asked"
//! forever.
//!
//! ONE READING, as car 1: the alarm integrates PERSIST_N readings
//! because it interrupts someone; this files a plan that is signed or
//! rejected on sight, and boss-gcp's host series is daily.
//!
//! WHAT IT LEAVES ALONE. A comparison with no hard finding (no read at
//! all); a finding no verb declares; and every other request on the
//! board. A malformed declaration is a permanent refusal naming the file
//! — it cannot reach a built tree past the pin, and a guess would be a
//! request filed for a verb the runner then refuses.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use boss_dispatcher::rules::expr::Value as ExprValue;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};

use super::common::{api_client, get_json, post_json, rows_or_refuse, write_json};
use super::estate_alarm::{HARD_CLASSES, hard_finding_keys, query_safe};

// `VERB_FILES: &[(&str, &str)]` — (name, file contents) for every
// `infra/ops/verbs/*.json`, generated by build.rs from the directory so
// the list is never authored here (CLAUDE.md §9a).
pub(crate) use super::ops_discover_remedies::discover_for_comparison;
include!(concat!(env!("OUT_DIR"), "/verb_files.rs"));

pub(crate) fn verb_files() -> &'static [(&'static str, &'static str)] {
    VERB_FILES
}

/// The final param, supplied only by the runner: the hash it
/// appends from the SIGNED plan (ops-runner.sh `verify_approval`).
const PLAN_SHA256: &str = "plan_sha256";

/// The page a board read asks for — the jobs API's own ceiling. A board
/// that outgrows it is refused by name ([`OpsFileRemedies::whole`]),
/// never judged on its first page.
const BOARD_PAGE: u32 = 1000;

/// One verb's declaration: it remedies `findings`, and runs on `host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Remedy {
    pub verb: String,
    pub host: String,
    pub findings: Vec<String>,
    pub args: Vec<String>,
    pub target: Option<String>,
}

impl Remedy {
    /// The request's own subject, and the dedup key.
    pub(crate) fn subject(&self) -> String {
        let base = format!("{}@{}", self.verb, self.host);
        match &self.target {
            Some(key) => format!("{base}#{}", remedy_request_id(key, None)),
            None => base,
        }
    }
}

/// PURE: every verb file that declares `remedies`, checked to be one a
/// machine may file — or every fault, each naming its file. A verb
/// without `remedies` is not a remedy and is not read further.
pub(crate) fn remedy_index(files: &[(&str, &str)]) -> Result<Vec<Remedy>, String> {
    let prefixes: BTreeSet<&str> = HARD_CLASSES.iter().map(|&(_, p)| p).collect();
    let mut index = Vec::new();
    let mut faults = Vec::new();
    for (name, text) in files {
        let spec: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                faults.push(format!("{name}.json is not JSON: {e}"));
                continue;
            }
        };
        let Some(declared) = spec.get("remedies") else {
            continue;
        };
        let mut fault = |why: String| faults.push(format!("{name}.json declares remedies, {why}"));
        let entries = match declared.as_array() {
            Some(a) if !a.is_empty() => a,
            _ => {
                fault("but `remedies` is not a non-empty list of finding keys".into());
                continue;
            }
        };
        let findings: Vec<String> = entries
            .iter()
            .filter_map(|v| {
                v.as_str()
                    .or_else(|| v.get("finding").and_then(Value::as_str))
                    .map(str::to_string)
            })
            .collect();
        if findings.len() != entries.len() {
            fault("but a `remedies` entry is not a string or an object with a finding key".into());
            continue;
        }
        let unkeyed: Vec<&String> = findings
            .iter()
            .filter(|k| {
                k.split_once(':')
                    .is_none_or(|(p, id)| id.is_empty() || !prefixes.contains(p))
            })
            .collect();
        if !unkeyed.is_empty() {
            fault(format!(
                "but {unkeyed:?} is not a finding estate.alarm keys (<class>:<id>, the class one \
                 of {prefixes:?}), so nothing could ever file it"
            ));
            continue;
        }
        if spec.get("requires_approval") != Some(&json!(true)) {
            fault(
                "but is not `requires_approval` — a machine files only a verb that runs under a \
                 passkey on its rendered plan (design 17835005)"
                    .into(),
            );
            continue;
        }
        let signed = spec
            .get("plan_verb")
            .and_then(Value::as_str)
            .is_some_and(|p| !p.is_empty())
            && spec
                .get("approvers")
                .and_then(Value::as_array)
                .is_some_and(|a| {
                    !a.is_empty() && a.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                });
        if !signed {
            fault("but names no `plan_verb` or no `approvers` to sign it".into());
            continue;
        }
        let params: Vec<&str> = spec
            .get("params")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|p| p.get("name").and_then(Value::as_str).unwrap_or(""))
            .collect();
        if params.last() != Some(&PLAN_SHA256)
            || params.iter().any(|name| name.is_empty())
            || params[..params.len().saturating_sub(1)].contains(&PLAN_SHA256)
        {
            fault(format!(
                "but its params are {params:?}: the final param must be `{PLAN_SHA256}`, which only the runner appends"
            ));
            continue;
        }
        if spec["params"]
            .as_array()
            .and_then(|p| p.last())
            .is_none_or(|p| {
                p.get("pattern").and_then(Value::as_str) != Some("^[0-9a-f]{64}$")
                    || p.get("optional").and_then(Value::as_bool) == Some(true)
                    || p.get("default").is_some()
            })
        {
            fault(
                "but plan_sha256 must require the signed plan's 64 lowercase hex digits, without a default".into(),
            );
            continue;
        }
        let hosts: Vec<&str> = spec
            .get("hosts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let [host] = hosts.as_slice() else {
            fault(format!(
                "but serves {hosts:?}: a remedy is filed for the ONE host its verb serves"
            ));
            continue;
        };
        if spec["hosts"].as_array().is_none_or(|a| a.len() != 1) {
            fault("but hosts is not exactly one string".into());
            continue;
        }
        if !query_safe(name) || !query_safe(host) {
            fault(format!("but {name}@{host} cannot ride a query as it is"));
            continue;
        }
        let arg_params = spec["params"]
            .as_array()
            .map(|p| &p[..p.len() - 1])
            .unwrap_or(&[]);
        let mut legacy = Vec::new();
        let mut targets = BTreeSet::new();
        for (entry, finding) in entries.iter().zip(findings) {
            if entry.is_string() {
                if !arg_params.is_empty() {
                    fault(format!(
                        "but params {params:?} require an object declaration with explicit args"
                    ));
                } else {
                    legacy.push(finding);
                }
                continue;
            }
            let Some(object) = entry.as_object() else {
                continue;
            };
            if object.len() != 2 || !object.contains_key("args") {
                fault(format!(
                    "but {finding} must contain exactly finding and args"
                ));
                continue;
            }
            if !targets.insert(finding.clone()) {
                fault(format!(
                    "but {finding} has more than one argument declaration; its target is ambiguous"
                ));
                continue;
            }
            let args = match declared_args(&entry["args"], arg_params) {
                Ok(args) => args,
                Err(error) => {
                    fault(format!("but {finding}: {error}"));
                    continue;
                }
            };
            index.push(Remedy {
                verb: (*name).into(),
                host: (*host).into(),
                findings: vec![finding.clone()],
                args,
                target: Some(finding),
            });
        }
        if !legacy.is_empty() {
            if legacy.iter().any(|key| targets.contains(key)) {
                fault("but a finding has both legacy and argument declarations".into());
            }
            index.push(Remedy {
                verb: (*name).into(),
                host: (*host).into(),
                findings: legacy,
                args: vec![],
                target: None,
            });
        }
    }
    if faults.is_empty() {
        Ok(index)
    } else {
        Err(faults.join("; "))
    }
}

/// Explicit registry words only: no expression execution, inferred defaults,
/// caller-supplied signed hash, or newline-delimited argv expansion.
/// Bounds are exact signed integers, like the CLI. Pattern declarations use
/// common ASCII regex syntax; engine extensions refuse instead of guessing.
pub(crate) fn declared_args(value: &Value, params: &[Value]) -> Result<Vec<String>, String> {
    let args = value.as_array().ok_or("args must be an array of strings")?;
    if args.len() != params.len() {
        return Err(format!(
            "args must supply exactly {} params before plan_sha256, got {}",
            params.len(),
            args.len()
        ));
    }
    args.iter()
        .zip(params)
        .map(|(value, param)| {
            let name = param["name"].as_str().ok_or("param has no name")?;
            let raw = value
                .as_str()
                .ok_or_else(|| format!("arg {name} must be a string"))?;
            if raw.chars().any(char::is_control) {
                return Err(format!("arg {name} carries a control character"));
            }
            if let Some(choices) = param.get("one_of") {
                let choices = choices
                    .as_array()
                    .ok_or_else(|| format!("arg {name} one_of is not a list"))?;
                if !choices.iter().any(|choice| choice.as_str() == Some(raw)) {
                    return Err(format!("arg {name} does not match one_of"));
                }
            } else {
                let pattern = param["pattern"]
                    .as_str()
                    .ok_or_else(|| format!("arg {name} has no pattern"))?;
                if !portable_pattern(pattern) {
                    return Err(format!("arg {name} pattern uses unsupported engine syntax"));
                }
                let regex =
                    regex::Regex::new(pattern).map_err(|e| format!("arg {name} pattern: {e}"))?;
                if !regex.is_match(raw) {
                    return Err(format!("arg {name} does not match {pattern}"));
                }
                if let Some(max) = param.get("max") {
                    let max = max
                        .as_i64()
                        .ok_or_else(|| format!("arg {name} max is not an integer"))?;
                    let number = raw
                        .parse::<i64>()
                        .map_err(|_| format!("arg {name} is not an integer for max"))?;
                    if number > max {
                        return Err(format!("arg {name} exceeds max {max}"));
                    }
                }
            }
            Ok(raw.to_string())
        })
        .collect()
}

/// Rust and the runner's jq/Oniguruma interpret some valid patterns differently.
/// Explicit remedies accept ASCII literals, ordinary classes/groups, repetition,
/// anchors and alternation, plus noncapturing groups and escaped punctuation.
/// Flags, named/Unicode classes, nested class operations and special escapes are
/// outside this subset. Repetition has one quantifier, optionally lazy, with
/// bounds no greater than the runner's 100000 limit (pinned through actual jq).
/// This restriction changes no generic runner semantics.
fn portable_pattern(pattern: &str) -> bool {
    if !pattern.is_ascii()
        || pattern.chars().any(char::is_control)
        || pattern.replace("(?:", "(").contains("(?")
        || ["&&", "--", "~~", "[:", "[.", "[="]
            .iter()
            .any(|syntax| pattern.contains(syntax))
    {
        return false;
    }
    let mut chars = pattern.chars();
    let mut in_class = false;
    let mut repeated = false;
    let mut lazy = false;
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                if chars
                    .next()
                    .is_none_or(|escaped| !".^$*+?{}[]()|\\-".contains(escaped))
                {
                    return false;
                }
                repeated = false;
            }
            '[' if in_class => return false,
            '[' => {
                in_class = true;
                repeated = false;
            }
            ']' => {
                in_class = false;
                repeated = false;
            }
            '{' if !in_class => {
                if repeated {
                    return false;
                }
                let (mut bound, mut digits, mut comma) = (0_u32, false, false);
                loop {
                    match chars.next() {
                        Some(digit @ '0'..='9') => {
                            bound = bound * 10 + (digit as u32 - '0' as u32);
                            if bound > 100000 {
                                return false;
                            }
                            digits = true;
                        }
                        Some(',') if digits && !comma => {
                            bound = 0;
                            digits = false;
                            comma = true;
                        }
                        Some('}') if digits || comma => break,
                        _ => return false,
                    }
                }
                repeated = true;
                lazy = false;
            }
            '*' | '+' | '?' if !in_class => {
                if repeated {
                    if ch != '?' || lazy {
                        return false;
                    }
                    lazy = true;
                } else {
                    repeated = true;
                    lazy = false;
                }
            }
            _ => repeated = false,
        }
    }
    true
}

/// PURE: each remedy whose declared findings this comparison carries,
/// with the keys it carries, sorted.
pub(crate) fn remedies_for<'a>(
    index: &'a [Remedy],
    keys: &BTreeSet<String>,
) -> Vec<(&'a Remedy, Vec<String>)> {
    index
        .iter()
        .filter_map(|r| {
            let mut hit: Vec<String> = r
                .findings
                .iter()
                .filter(|f| keys.contains(*f))
                .cloned()
                .collect();
            hit.sort();
            hit.dedup();
            (!hit.is_empty()).then_some((r, hit))
        })
        .collect()
}

/// What a remedy's board — every request ever filed on its subject,
/// read whole — says to do with a finding that persists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Board {
    /// A request on the subject is open: nothing more, however old.
    Open,
    /// One was opened inside the window, whatever became of it.
    Recent,
    /// The newest request's approver DECLINED its plan at `at`. Asking
    /// again waits for the estate to observe the finding clear
    /// ([`cleared_since`]).
    Declined { request: String, at: DateTime<Utc> },
    /// File, as the successor of `after` — the newest request on the
    /// subject, or none on an empty board ([`remedy_request_id`]).
    File { after: Option<String> },
}

/// A row's admission instant, or a refusal naming the row. Refused
/// rather than guessed: "not recent" would refile, "recent" would
/// retire the guard in silence.
fn opened(row: &Value) -> Result<DateTime<Utc>, String> {
    let raw = row.get("opened_at").and_then(Value::as_str).unwrap_or("");
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| {
            format!(
                "request {} carries no readable opened_at ({raw:?}: {e})",
                row.get("id").and_then(Value::as_str).unwrap_or("?")
            )
        })
}

/// PURE: when `row`'s approver declined its plan — its `approve` step
/// COMPLETED with a decision that is not `approved`, the shape Reject
/// leaves (ops-request.toml routes it to `refused`) — or `None` when
/// the request was not declined: approved, refused by the runner
/// (approve skipped), closed with nothing to do, or still open. A
/// decline whose instant cannot be read is refused, never guessed.
fn declined_at(row: &Value) -> Result<Option<DateTime<Utc>>, String> {
    let approve = row
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("approve"));
    let Some(approve) = approve else {
        return Ok(None);
    };
    let completed = approve.get("status").and_then(Value::as_str) == Some("completed");
    let approved = approve
        .get("metadata")
        .and_then(|m| m.get("decision"))
        .and_then(Value::as_str)
        == Some("approved");
    if !completed || approved {
        return Ok(None);
    }
    let raw = approve
        .get("completed_at")
        .and_then(Value::as_str)
        .unwrap_or("");
    DateTime::parse_from_rfc3339(raw)
        .map(|t| Some(t.with_timezone(&Utc)))
        .map_err(|e| {
            format!("a declined approve step carries no readable completed_at ({raw:?}: {e})")
        })
}

/// PURE: judge the WHOLE board (review 63bafc11, LOW 2d). The list is
/// ordered by `opened_on`, the SIM date, and the window is judged on
/// `opened_at`, the admission instant, so its first row was never
/// reliably the newest; every row is read and the newest is the one
/// admitted last (ties broken on the id, so two readers of one board
/// agree on it — the successor id below depends on that).
pub(crate) fn judge_board(rows: &[Value], now: DateTime<Utc>, hours: i64) -> Result<Board, String> {
    if rows
        .iter()
        .any(|r| r.get("status").and_then(Value::as_str) == Some("open"))
    {
        return Ok(Board::Open);
    }
    let mut dated = Vec::with_capacity(rows.len());
    for r in rows {
        dated.push((
            opened(r)?,
            r.get("id").and_then(Value::as_str).unwrap_or(""),
            r,
        ));
    }
    if dated
        .iter()
        .any(|(at, _, _)| now.signed_duration_since(*at) < chrono::Duration::hours(hours))
    {
        return Ok(Board::Recent);
    }
    let Some(&(_, id, newest)) = dated.iter().max_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1))) else {
        return Ok(Board::File { after: None });
    };
    Ok(match declined_at(newest)? {
        Some(at) => Board::Declined {
            request: id.to_string(),
            at,
        },
        None => Board::File {
            after: Some(id.to_string()),
        },
    })
}

/// Does alarm packet `a` carry one of `findings`?
fn carries(a: &Value, findings: &[String]) -> bool {
    a.get("metadata")
        .and_then(|m| m.get("estate_finding"))
        .and_then(Value::as_str)
        .is_some_and(|f| findings.iter().any(|x| x == f))
}

/// The outcomes an alarm's close records (backlog-item.toml's
/// terminals) that DO say the finding ended: `completed`, the work it
/// asked for landed, and `stale`, the live system no longer shows it —
/// the recover rule's own close. The others do not: `duplicate` points
/// at another packet still carrying it, and `declined` is "not acting
/// on it", the approver's own answer given twice. A close with no
/// readable outcome is not a clear either: the decline holds, and the
/// held note on the open alarm says why.
///
/// THE OUTCOME, NOT A DISPOSITION (backlog e5ff616f, LOW-B of review
/// cc0e1d8e). This read the steps' `disposition` against a list of
/// those that do not clear, and a triage routed to `design` — then
/// declined at the design review — carried none of them, so a declined
/// alarm counted as the finding ending. Every route ends at one
/// terminal, and the close stamps its outcome on the packet
/// (job_outcome.rs), so the outcome is the one field every route
/// agrees on.
const CLEARS: &[&str] = &["completed", "stale"];

/// PURE: the alarm packet on which the estate's record shows any of
/// `findings` clear since `at`, or `None`. One carrying the finding,
/// CLOSED after `at`, either by the machine's recover rule (`cleared_by`
/// = `estate.recover`) or with an outcome that says the finding ended
/// ([`CLEARS`]). A close before the decline is the episode the decline
/// answered.
///
/// A PERSON'S CLOSE COUNTS (backlog aa816dd4, LOW-2 of review
/// ea2ecfd4). Only the recover rule's close counted, so an alarm a
/// person closed after the decline — d3c7eada, closed when the car that
/// fixed boss-gcp's disk landed — never recorded the clear, and the
/// decline held into the NEXT occurrence of the finding: a new episode,
/// which is a new question, answered with the old no. The id is
/// returned so the request filed after it names the close that lifted
/// the decline ([`remedy_request`]).
pub(crate) fn cleared_since(
    alarms: &[Value],
    findings: &[String],
    at: DateTime<Utc>,
) -> Option<String> {
    alarms
        .iter()
        .filter(|a| a.get("status").and_then(Value::as_str) == Some("closed"))
        .filter(|a| carries(a, findings))
        .filter(|a| {
            a.get("metadata")
                .and_then(|m| m.get("closed_at"))
                .and_then(Value::as_str)
                .and_then(|c| DateTime::parse_from_rfc3339(c).ok())
                .is_some_and(|c| c.with_timezone(&Utc) > at)
        })
        .find(|a| {
            let by_recover = a
                .get("steps")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|s| s.get("metadata")?.get("cleared_by")?.as_str())
                .any(|c| c == super::estate_recover::CLEARED_BY);
            let ended = a
                .get("metadata")
                .and_then(|m| m.get("outcome"))
                .and_then(Value::as_str)
                .is_some_and(|o| CLEARS.contains(&o));
            by_recover || ended
        })
        .and_then(|a| a.get("id").and_then(Value::as_str).map(str::to_string))
}

/// The job-metadata key a held remedy is recorded under on its alarm —
/// one key per remedy subject, so two remedies held on one alarm never
/// overwrite each other through the top-level merge.
pub(crate) fn held_key(remedy: &Remedy) -> String {
    format!("remedy_held:{}", remedy.subject())
}

/// PURE: the annotations a held remedy owes the alarms it is held on —
/// `(alarm id, value)` for each OPEN alarm carrying one of `findings`
/// that does not already say exactly this (backlog aa816dd4, LOW-2: a
/// held remedy showed only in a log line, so the alarm read as though
/// nothing had been offered and nobody had answered). Deterministic in
/// its inputs, so a finding that persists across comparisons writes once.
pub(crate) fn held_annotations(
    alarms: &[Value],
    remedy: &Remedy,
    findings: &[String],
    request: &str,
    at: DateTime<Utc>,
) -> Vec<(String, Value)> {
    let key = held_key(remedy);
    let value = json!({
        "verb": remedy.verb,
        "host": remedy.host,
        "declined_request": request,
        "declined_at": at.to_rfc3339(),
        "note": format!(
            "{} on {} remedies this finding, and its approver declined the last request \
             ({request}); it is not filed again until this finding's alarm is closed — by \
             the recover rule, or by a person — after that decline",
            remedy.verb, remedy.host
        ),
    });
    alarms
        .iter()
        .filter(|a| a.get("status").and_then(Value::as_str) == Some("open"))
        .filter(|a| carries(a, findings))
        .filter(|a| a.get("metadata").and_then(|m| m.get(&key)) != Some(&value))
        .filter_map(|a| a.get("id").and_then(Value::as_str))
        .map(|id| (id.to_string(), value.clone()))
        .collect()
}

/// PURE: the id of the request filed on `subject` as the successor of
/// `after` (review 63bafc11, LOW 2a). The dedup reads the board and
/// then posts, and two firings that read the same board — two
/// comparisons handled at once, or a spool replay beside the live
/// delivery — both found nothing to stop them and both posted under
/// ids the server minted: one extra passkey prompt. Derived from the
/// board instead, both posts name ONE packet, and the jobs API admits
/// an id once (backlog 558396ff: a re-send of the same body is
/// answered `already_admitted`; one that differs is refused 409, which
/// NAKs, and the redelivery finds the request open). The predecessor
/// and not the delivery: two different comparisons that raced carry
/// different event ids and must still collide.
pub(crate) fn remedy_request_id(subject: &str, after: Option<&str>) -> String {
    /// A fixed namespace, so every dispatcher derives the same id.
    const REMEDY_REQUEST: uuid::Uuid =
        uuid::Uuid::from_u128(0x0b2f_78b9_0000_4000_8000_63ba_fc11_0002);
    uuid::Uuid::new_v5(
        &REMEDY_REQUEST,
        format!("ops-remedy:{subject}:after:{}", after.unwrap_or("none")).as_bytes(),
    )
    .to_string()
}

/// PURE: the request — the packet `boss ops <host> <verb>` files for an
/// approval verb (`host`, `verb`, `args: []`, `requires_approval`), plus
/// the finding it answers and the firing that filed it, under `id`
/// ([`remedy_request_id`]). A request filed after a decline lifted says
/// so: `lifted` is `(the declined request, the alarm whose close lifted
/// it)`, recorded as `decline_lifted` — the clear on the record, not
/// only in the dispatcher's judgement (backlog aa816dd4).
pub(crate) fn remedy_request(
    id: &str,
    remedy: &Remedy,
    findings: &[String],
    rule_name: &str,
    event_id: &str,
    topic: &str,
    lifted: Option<(&str, &str)>,
) -> Value {
    let named = findings.join(" ");
    let mut body = json!({
        "id": id,
        "kind": "ops-request",
        "title": format!("{} on {} — the estate reads {named}", remedy.verb, remedy.host),
        "subject": {"subject_kind": "custom", "id": remedy.subject()},
        "owner_id": format!("rule:{rule_name}"),
        "priority": "standard",
        "status": "open",
        "tags": ["dispatcher-spawned"],
        "metadata": {
            "host": remedy.host,
            "verb": remedy.verb,
            // An empty LIST, never absent: the runner refuses a request
            // whose args are not an array of exactly params - 1.
            "args": remedy.args,
            "requires_approval": true,
            "remedies": named,
            "spawned_by_rule": rule_name,
            "triggered_by_event_id": event_id,
            "triggered_by_topic": topic,
        },
    });
    if let Some((declined, alarm)) = lifted {
        body["metadata"]["decline_lifted"] = json!({
            "declined_request": declined,
            "cleared_alarm": alarm,
        });
    }
    body
}

pub struct OpsFileRemedies {
    pub(crate) client: boss_core::machine_token::Client,
    jobs_base: String,
}

impl OpsFileRemedies {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Self::with_client(api_client(), jobs_base)
    }

    pub fn with_client(
        client: boss_core::machine_token::Client,
        jobs_base: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }

    pub(crate) fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// Every row of a list read, or a refusal when the page is not the
    /// whole answer: a limit is not a filter, and a board judged on a
    /// page would refile beside a request it never saw.
    pub(crate) async fn whole(
        &self,
        url: &str,
        what: &str,
        rule: &str,
    ) -> Result<Vec<Value>, HandlerError> {
        let body = get_json(&self.client, url, rule).await?;
        let rows: Vec<Value> = rows_or_refuse(&body, what).map_err(HandlerError::Downstream)?;
        let total = body.get("total").and_then(Value::as_u64);
        if total.is_none_or(|t| (rows.len() as u64) < t) {
            return Err(HandlerError::Downstream(format!(
                "{what} answered {} of {total:?} rows; nothing is filed on a board read in part",
                rows.len()
            )));
        }
        Ok(rows)
    }

    /// Every ops-request ever filed on `remedy`'s subject, steps whole
    /// (an approve step's decision is step metadata). One read answers
    /// the open guard, the window and the decline (review 63bafc11,
    /// LOW 2d: it was two `limit=1` reads that took the list's first row
    /// for the newest). A remedy files at most one request a window, so
    /// this stays a page for years.
    async fn board(&self, remedy: &Remedy, rule: &str) -> Result<Vec<Value>, HandlerError> {
        let mut url = reqwest::Url::parse(&format!("{}/api/jobs", self.base()))
            .map_err(|e| HandlerError::Permanent(format!("ops.file_remedies jobs URL: {e}")))?;
        url.query_pairs_mut()
            .append_pair("kind", "ops-request")
            .append_pair("subject_id", &remedy.subject())
            .append_pair("full", "true")
            .append_pair("limit", &BOARD_PAGE.to_string());
        self.whole(url.as_str(), "GET /api/jobs (ops-request by subject)", rule)
            .await
    }

    /// The estate's alarm packets open now or closed since `at`, read
    /// the way `estate.alarm` reads its own (`closed_within`, steps
    /// whole) — the record a decline is judged against.
    pub(crate) async fn alarms_since(
        &self,
        at: DateTime<Utc>,
        now: DateTime<Utc>,
        rule: &str,
    ) -> Result<Vec<Value>, HandlerError> {
        let days = now.signed_duration_since(at).num_days().max(0) + 1;
        let url = format!(
            "{}/api/jobs?kind=backlog-item&closed_within={days}&metadata_has=estate_finding&full=true&limit={BOARD_PAGE}",
            self.base()
        );
        self.whole(
            &url,
            "GET /api/jobs (estate alarms since the decline)",
            rule,
        )
        .await
    }

    /// File `remedy` unless its board says not to. Answers whether it
    /// filed.
    async fn file_one(
        &self,
        remedy: &Remedy,
        findings: &[String],
        hours: i64,
        now: DateTime<Utc>,
        ctx: &InvocationContext,
    ) -> Result<bool, HandlerError> {
        let Some((body, _)) = self.prepare_one(remedy, findings, hours, now, ctx).await? else {
            return Ok(false);
        };
        post_json(
            &self.client,
            &format!("{}/api/jobs", self.base()),
            &body,
            &ctx.rule_name,
        )
        .await?;
        Ok(true)
    }

    /// Compute admission under the existing whole-board and decline guards.
    /// Discovery and its completion use the SAME final mutation gate, so an
    /// intermediate read cannot bypass a declined or already open request.
    pub(crate) async fn prepare_one(
        &self,
        remedy: &Remedy,
        findings: &[String],
        hours: i64,
        now: DateTime<Utc>,
        ctx: &InvocationContext,
    ) -> Result<Option<(Value, Option<String>)>, HandlerError> {
        let rows = self.board(remedy, &ctx.rule_name).await?;
        let (after, lifted) = match judge_board(&rows, now, hours)
            .map_err(HandlerError::Downstream)?
        {
            Board::Open | Board::Recent => return Ok(None),
            Board::File { after } => (after, None),
            // DECLINE-STOP-ASKING (review 63bafc11, LOW 2b). A plan its
            // approver declined was refiled every window while the
            // finding persisted — a weekly passkey prompt for something
            // already answered no. The decline on the record is the stop,
            // and the finding's alarm closing after it is what lifts it:
            // the next episode is a new question.
            Board::Declined { request, at } => {
                let alarms = self.alarms_since(at, now, &ctx.rule_name).await?;
                let Some(alarm) = cleared_since(&alarms, &remedy.findings, at) else {
                    tracing::info!(
                        remedy = %remedy.subject(),
                        declined = %request,
                        "ops.file_remedies: its approver declined the last request and the estate has not observed the finding clear since — not asking again"
                    );
                    // HELD SAYS SO ON THE ALARM (backlog aa816dd4, LOW-2):
                    // the reader of the alarm is the one who wonders why
                    // nothing was offered. Once per alarm and decline.
                    // NOT FATAL (backlog e5ff616f, LOW-A of review
                    // cc0e1d8e): the hold is already decided, and the note
                    // is observability. Propagating its failure retried
                    // the firing and could dead-letter every estate
                    // comparison over a note; logged, the next comparison
                    // writes it again, because an alarm not yet saying
                    // it is still owed it.
                    for (id, value) in held_annotations(&alarms, remedy, findings, &request, at) {
                        if let Err(e) = write_json(
                            &self.client,
                            reqwest::Method::PATCH,
                            &format!("{}/api/jobs/{id}/metadata", self.base()),
                            &json!({ held_key(remedy): value }),
                            &ctx.rule_name,
                        )
                        .await
                        {
                            tracing::warn!(
                                remedy = %remedy.subject(),
                                alarm = %id,
                                error = %e,
                                "ops.file_remedies: the held note on the alarm was not written — the remedy stays held; the next comparison writes it again"
                            );
                        }
                    }
                    return Ok(None);
                };
                (Some(request.clone()), Some((request, alarm)))
            }
        };
        let id = remedy_request_id(&remedy.subject(), after.as_deref());
        let body = remedy_request(
            &id,
            remedy,
            findings,
            &ctx.rule_name,
            &ctx.triggering_event_id,
            &ctx.triggering_topic,
            lifted.as_ref().map(|(r, a)| (r.as_str(), a.as_str())),
        );
        Ok(Some((body, after)))
    }
    async fn invoke_declared(
        &self,
        files: &[(&str, &str)],
        args: &[(String, ExprValue)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let hours = match arg(args, "refile_after_hours") {
            Some(ExprValue::Int(h)) if *h > 0 => *h,
            Some(ExprValue::Int(h)) => {
                return Err(HandlerError::Permanent(format!(
                    "ops.file_remedies: refile_after_hours must be a positive number of hours, \
                     got {h}"
                )));
            }
            Some(other) => {
                return Err(HandlerError::BadArgType {
                    arg: "refile_after_hours".into(),
                    expected: "integer hours",
                    got: other.kind(),
                });
            }
            None => return Err(HandlerError::MissingArg("refile_after_hours".into())),
        };
        let keys = hard_finding_keys(&ctx.event_payload);
        if keys.is_empty() {
            return Ok(());
        }
        let index = remedy_index(files)
            .map_err(|e| HandlerError::Permanent(format!("ops.file_remedies: {e}")))?;
        // The window is measured from the READING's own instant, not the
        // dispatcher's clock: a comparison replayed from the spool asks
        // what was open when it was read (no-wallclock, 2b03a2df).
        let now = ctx.firing_instant()?;
        // Best-effort across remedies, as estate.alarm is across
        // findings: one failed read or POST must not stop another verb's
        // request; every failure is surfaced at the end, so a transient
        // still NAKs for a retry the dedup makes idempotent.
        let mut errors = Vec::new();
        for (remedy, findings) in remedies_for(&index, &keys) {
            if let Err(e) = self.file_one(remedy, &findings, hours, now, ctx).await {
                if e.is_permanent() {
                    return Err(e);
                }
                errors.push(format!("{}: {e}", remedy.subject()));
            }
        }
        if let Err(e) = discover_for_comparison(self, files, hours, now, ctx).await {
            if e.is_permanent() {
                return Err(e);
            }
            errors.push(e.to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(HandlerError::Downstream(format!(
                "ops.file_remedies: {}",
                errors.join("; ")
            )))
        }
    }
}

#[async_trait]
impl Handler for OpsFileRemedies {
    fn name(&self) -> &'static str {
        "ops.file_remedies"
    }

    async fn invoke(
        &self,
        args: &[(String, ExprValue)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        self.invoke_declared(VERB_FILES, args, ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const RULE: &str = "file-the-remedy-a-verb-declares-for-an-estate-finding";

    /// A verb file in the approval shape, declaring `remedies`.
    fn approval_verb(remedies: Value) -> String {
        json!({
            "about": "MUTATING — test.", "hosts": ["boss-gcp"],
            "requires_approval": true, "plan_verb": "plan-x", "approvers": ["emp-david"],
            "argv": ["infra/gcp/x.sh", "{1}"],
            "params": [{"name": "plan_sha256", "pattern": "^[0-9a-f]{64}$"}],
            "remedies": remedies,
        })
        .to_string()
    }

    fn index_of(name: &str, text: &str) -> Result<Vec<Remedy>, String> {
        remedy_index(&[(name, text)])
    }

    /// The boss-gcp host comparison of 2026-09-28T10:25:01Z, verbatim
    /// from the system of record (the prose shortened) — the reading
    /// car 1 was pinned on.
    fn boss_gcp_below_its_floor() -> Value {
        json!({
            "counts": {"disk_tight": 1, "drift": 1, "not_ready": 0, "observed": 1,
                       "observed_not_declared": 0, "ops_credentials_absent": 1},
            "findings": {
                "disk_tight": [{"disk_gb": 47, "floor_gb": 17, "free_gb": 11, "id": "boss-gcp"}],
                "drift": [{"fields": {"memory_gb": {"declared": 15, "observed": 16}}, "id": "boss-gcp"}],
                "not_ready": [],
                "observed_not_declared": [],
                "ops_credentials_absent": [{"act": "place …", "id": "boss-gcp",
                                            "state": "not ready: talosconfig:absent kubeconfig:absent"}],
                "ops_credentials_unmeasured": []
            },
            "host": "boss-gcp",
            "observed_at": "2026-09-28T10:25:01Z",
            "observer": "boss-estate-observe-host",
            "scope": "host"
        })
    }

    // --- the declarations, over the tree as it stands ---------------

    /// The compiled-in set IS the directory, each file byte-equal — the
    /// staleness a shared target dir can hide (boss-cli pins the same).
    #[test]
    fn the_compiled_in_verbs_are_the_directory() {
        let dir = boss_testing::repo_root().join("infra/ops/verbs");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("infra/ops/verbs/ exists")
            .map(|e| e.expect("dir entry").path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .map(|p| {
                p.file_stem()
                    .expect("a stem")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        on_disk.sort();
        let compiled: Vec<String> = VERB_FILES.iter().map(|(n, _)| (*n).to_string()).collect();
        assert_eq!(
            compiled, on_disk,
            "VERB_FILES is not infra/ops/verbs/*.json — rebuild"
        );
        for (name, text) in VERB_FILES {
            assert_eq!(
                std::fs::read_to_string(dir.join(format!("{name}.json"))).expect("read"),
                *text,
                "{name}.json changed since this build — rebuild"
            );
        }
    }

    /// NEVER WITHOUT THE PASSKEY, as a property of the tree: every verb
    /// that declares a remedy indexes clean, so none is a verb that would
    /// run on filing alone.
    #[test]
    fn every_declared_remedy_is_a_passkey_gated_verb() {
        let index = remedy_index(VERB_FILES).unwrap_or_else(|e| panic!("{e}"));
        assert!(!index.is_empty(), "no verb declares a remedy");
    }

    /// Car 1's pairing, now declared on the verb: reclaim-gcp-root
    /// remedies disk_tight:boss-gcp on boss-gcp, under the subject car 1
    /// filed its request with — so the request signed on the day this
    /// landed still holds the dedup window.
    #[test]
    fn reclaim_gcp_root_declares_the_boss_gcp_disk_floor() {
        let index = remedy_index(VERB_FILES).unwrap_or_else(|e| panic!("{e}"));
        let reclaim = index
            .iter()
            .find(|r| r.verb == "reclaim-gcp-root")
            .expect("reclaim-gcp-root declares a remedy");
        assert_eq!(reclaim.host, "boss-gcp");
        assert_eq!(reclaim.findings, vec!["disk_tight:boss-gcp".to_string()]);
        assert_eq!(reclaim.subject(), "reclaim-gcp-root@boss-gcp");
    }

    // --- the index refuses what a machine must not file -------------

    #[test]
    fn a_verb_without_remedies_is_not_a_remedy() {
        let plain = json!({"about": "READ-ONLY", "hosts": ["forge"], "argv": ["df"], "params": []});
        assert_eq!(index_of("df", &plain.to_string()), Ok(vec![]));
    }

    #[test]
    fn a_remedy_on_a_verb_that_runs_without_a_passkey_is_refused() {
        let mut v: Value =
            serde_json::from_str(&approval_verb(json!(["disk_tight:forge"]))).unwrap();
        v.as_object_mut().unwrap().remove("requires_approval");
        let err = index_of("reclaim-disk", &v.to_string()).expect_err("no passkey, no index");
        assert!(
            err.contains("reclaim-disk.json") && err.contains("requires_approval"),
            "{err}"
        );

        v["requires_approval"] = json!(true);
        v.as_object_mut().unwrap().remove("approvers");
        let err = index_of("reclaim-disk", &v.to_string()).expect_err("nobody to sign");
        assert!(err.contains("approvers"), "{err}");
    }

    /// The machine files `args: []`, so a verb with any param before
    /// the hash cannot be filed by it — refused, not filed short.
    #[test]
    fn a_remedy_whose_verb_takes_a_param_the_machine_cannot_fill_is_refused() {
        let mut v: Value =
            serde_json::from_str(&approval_verb(json!(["disk_tight:forge"]))).unwrap();
        v["params"] = json!([{"name": "target", "pattern": "^[a-z]+$"},
                             {"name": "plan_sha256", "pattern": "^[0-9a-f]{64}$"}]);
        let err = index_of("wipe", &v.to_string()).expect_err("a param to fill");
        assert!(
            err.contains("plan_sha256") && err.contains("target"),
            "{err}"
        );
    }

    #[test]
    fn a_remedy_names_a_finding_the_alarm_keys_and_one_host() {
        for bad in [
            json!(["disk_low:boss-gcp"]),
            json!(["disk_tight"]),
            json!(["disk_tight:"]),
            json!([]),
            json!("disk_tight:boss-gcp"),
            json!([7]),
        ] {
            let err = index_of("x", &approval_verb(bad.clone())).expect_err("refused");
            assert!(err.contains("x.json declares remedies"), "{bad}: {err}");
        }
        let mut two_hosts: Value =
            serde_json::from_str(&approval_verb(json!(["disk_tight:forge"]))).unwrap();
        two_hosts["hosts"] = json!(["forge", "boss-gcp"]);
        let err = index_of("x", &two_hosts.to_string()).expect_err("which host?");
        assert!(err.contains("ONE host"), "{err}");
    }

    // --- matching and the request -----------------------------------

    #[test]
    fn a_comparison_matches_the_remedies_that_declare_its_keys() {
        let index = vec![
            Remedy {
                verb: "reclaim-gcp-root".into(),
                host: "boss-gcp".into(),
                findings: vec!["disk_tight:boss-gcp".into()],
                args: vec![],
                target: None,
            },
            Remedy {
                verb: "reclaim-disk".into(),
                host: "forge".into(),
                findings: vec!["disk_tight:forge".into()],
                args: vec![],
                target: None,
            },
        ];
        let keys = hard_finding_keys(&boss_gcp_below_its_floor());
        let hits = remedies_for(&index, &keys);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].0.verb, "reclaim-gcp-root");
        assert_eq!(hits[0].1, vec!["disk_tight:boss-gcp".to_string()]);

        let mut clean = boss_gcp_below_its_floor();
        clean["findings"]["disk_tight"] = json!([]);
        assert!(remedies_for(&index, &hard_finding_keys(&clean)).is_empty());
    }

    /// The request is the one `boss ops boss-gcp reclaim-gcp-root` files
    /// for an approval verb, and the one car 1's rule filed.
    #[test]
    fn the_request_is_passkey_gated_with_empty_args_under_the_verbs_subject() {
        let r = Remedy {
            verb: "reclaim-gcp-root".into(),
            host: "boss-gcp".into(),
            findings: vec!["disk_tight:boss-gcp".into()],
            args: vec![],
            target: None,
        };
        let body = remedy_request(
            "req-1",
            &r,
            &["disk_tight:boss-gcp".into()],
            RULE,
            "ev-1",
            "jobs.estate.compared",
            None,
        );
        assert_eq!(body["id"], "req-1", "the id is the caller's, derived");
        assert_eq!(body["kind"], "ops-request");
        assert_eq!(
            body["subject"],
            json!({"subject_kind": "custom", "id": "reclaim-gcp-root@boss-gcp"})
        );
        assert_eq!(body["owner_id"], format!("rule:{RULE}"));
        let md = &body["metadata"];
        assert_eq!(md["host"], "boss-gcp");
        assert_eq!(md["verb"], "reclaim-gcp-root");
        assert_eq!(md["args"], json!([]), "an empty LIST, never absent");
        assert_eq!(md["requires_approval"], json!(true));
        assert_eq!(md["remedies"], "disk_tight:boss-gcp");
        assert_eq!(md["spawned_by_rule"], RULE);
        let title = body["title"].as_str().unwrap();
        assert!(
            title.contains("reclaim-gcp-root") && title.contains("disk_tight:boss-gcp"),
            "{title}"
        );
    }

    #[test]
    fn a_request_opened_inside_the_window_is_recent_and_an_unreadable_one_refuses() {
        let now: DateTime<Utc> = "2026-09-29T12:00:00Z".parse().unwrap();
        let at = |s: &str| json!({"id": "r", "status": "closed", "opened_at": s});
        assert_eq!(
            judge_board(&[at("2026-09-28T12:00:00Z")], now, 168),
            Ok(Board::Recent)
        );
        assert_eq!(
            judge_board(&[at("2026-09-22T11:59:00Z")], now, 168),
            Ok(Board::File {
                after: Some("r".into())
            })
        );
        assert!(judge_board(&[json!({"id": "r", "status": "closed"})], now, 168).is_err());
    }

    // --- the handler against a jobs API stand-in --------------------

    type Posts = Arc<Mutex<Vec<Value>>>;

    /// A `GET /api/jobs` that answers from `board` filtered by kind,
    /// status, subject_id and metadata_has, in the order the real list
    /// answers — `opened_on` (the SIM date) descending, then the
    /// admission instant — and a POST that records every body it is
    /// sent and admits an id ONCE, answering a re-send of a known id
    /// `already_admitted` as the jobs API does (backlog 558396ff). The
    /// board does not grow with the posts: two firings against it read
    /// the same board, which is exactly two firings that raced.
    async fn mock_jobs(board: Vec<Value>) -> (String, Posts) {
        let (base, posts, _) = mock_jobs_patched(board).await;
        (base, posts)
    }

    /// [`mock_jobs`], plus a `PATCH /api/jobs/{id}/metadata` that
    /// records `{id, body}` for every annotation it is sent.
    async fn mock_jobs_patched(board: Vec<Value>) -> (String, Posts, Posts) {
        mock_jobs_answering_patches(board, axum::http::StatusCode::NO_CONTENT).await
    }

    /// [`mock_jobs_patched`], every annotation answered `patch_status`
    /// (still recorded, so a test can see it was attempted).
    async fn mock_jobs_answering_patches(
        board: Vec<Value>,
        patch_status: axum::http::StatusCode,
    ) -> (String, Posts, Posts) {
        use axum::extract::{Path, Query};
        use axum::routing::{get, patch};
        use axum::{Json, Router};
        let posts: Posts = Arc::new(Mutex::new(Vec::new()));
        let patches: Posts = Arc::new(Mutex::new(Vec::new()));
        let patch_log = patches.clone();
        let log = posts.clone();
        let admitted: Arc<Mutex<BTreeSet<String>>> = Arc::new(Mutex::new(BTreeSet::new()));
        let rows = Arc::new(board);
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |Query(q): Query<HashMap<String, String>>| {
                    let rows = rows.clone();
                    async move {
                        let mut hit: Vec<Value> = rows
                            .iter()
                            .filter(|j| {
                                q.get("kind").is_none_or(|k| j["kind"] == json!(k))
                                    && q.get("status").is_none_or(|s| j["status"] == json!(s))
                                    && q.get("subject_id")
                                        .is_none_or(|s| j["subject_id"] == json!(s))
                                    && q.get("metadata_has")
                                        .is_none_or(|k| j["metadata"].get(k).is_some())
                            })
                            .cloned()
                            .collect();
                        hit.sort_by(|a, b| {
                            b["opened_on"]
                                .as_str()
                                .cmp(&a["opened_on"].as_str())
                                .then_with(|| b["opened_at"].as_str().cmp(&a["opened_at"].as_str()))
                        });
                        let total = hit.len();
                        let limit = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(50);
                        hit.truncate(limit);
                        Json(json!({"data": hit, "total": total}))
                    }
                })
                .post(move |Json(body): Json<Value>| {
                    let log = log.clone();
                    let admitted = admitted.clone();
                    async move {
                        log.lock().unwrap().push(body.clone());
                        let id = body["id"].as_str().unwrap_or("req-new").to_string();
                        let fresh = admitted.lock().unwrap().insert(id.clone());
                        Json(json!({"id": id, "already_admitted": !fresh}))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                patch(move |Path(id): Path<String>, Json(body): Json<Value>| {
                    let log = patch_log.clone();
                    async move {
                        log.lock().unwrap().push(json!({"id": id, "body": body}));
                        patch_status
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), posts, patches)
    }

    /// The comparison event's own instant — the window's anchor.
    fn read_at() -> DateTime<Utc> {
        "2026-09-29T12:00:00Z".parse().expect("an instant")
    }

    fn ctx(payload: Value) -> InvocationContext {
        InvocationContext {
            event_timestamp: Some(read_at()),
            rule_name: RULE.into(),
            triggering_event_id: "b7c2d05a".into(),
            triggering_topic: "jobs.estate.compared".into(),
            event_payload: payload,
        }
    }

    fn args() -> Vec<(String, ExprValue)> {
        vec![("refile_after_hours".to_string(), ExprValue::Int(168))]
    }

    fn request(status: &str, opened_at: &str) -> Value {
        json!({"id": "r-old", "kind": "ops-request", "status": status,
               "subject_id": "reclaim-gcp-root@boss-gcp", "opened_at": opened_at,
               "opened_on": &opened_at[..10]})
    }

    /// `hours` before the reading, RFC 3339.
    fn before_reading(hours: i64) -> String {
        (read_at() - chrono::Duration::hours(hours)).to_rfc3339()
    }

    /// A closed request whose approver DECLINED its plan `hours` before
    /// the reading: the approve step completed with a decision that is
    /// not `approved`, as Reject leaves it (ops-request.toml `refused`).
    fn declined(id: &str, opened_hours: i64, declined_hours: i64) -> Value {
        let mut r = request("closed", &before_reading(opened_hours));
        r["id"] = json!(id);
        r["steps"] = json!([
            {"spec_slug": "approve", "status": "completed",
             "completed_at": before_reading(declined_hours),
             "metadata": {"decision": "rejected"}},
            {"spec_slug": "refused", "status": "completed", "metadata": {}}
        ]);
        r
    }

    /// The estate alarm for `finding`, closed `hours` before the reading
    /// — by the machine's recover rule when `by_recover`, else by a
    /// person's triage.
    fn alarm_closed(finding: &str, hours: i64, by_recover: bool) -> Value {
        alarm_closed_as(finding, hours, by_recover, "stale")
    }

    /// [`alarm_closed`], its triage carrying `disposition` and the packet
    /// the outcome backlog-item.toml's terminal for that route records:
    /// `build` ends `completed` (the item's car landed), and each
    /// withdrawal its own name.
    fn alarm_closed_as(finding: &str, hours: i64, by_recover: bool, disposition: &str) -> Value {
        let outcome = match disposition {
            "build" => "completed",
            "decline" => "declined",
            other => other,
        };
        alarm_closed_ending(finding, hours, by_recover, disposition, outcome)
    }

    /// [`alarm_closed_as`], its close recording `outcome` whatever the
    /// triage routed — a `design` route ends where the design review
    /// sends it.
    fn alarm_closed_ending(
        finding: &str,
        hours: i64,
        by_recover: bool,
        disposition: &str,
        outcome: &str,
    ) -> Value {
        let mut step_md = json!({"disposition": disposition});
        if by_recover {
            step_md["cleared_by"] = json!(super::super::estate_recover::CLEARED_BY);
        }
        let opened = before_reading(hours + 72);
        json!({"id": format!("alarm-{finding}-{hours}"), "kind": "backlog-item", "status": "closed",
               "opened_at": opened, "opened_on": opened[..10],
               "metadata": {"estate_finding": finding, "closed_at": before_reading(hours),
                            "outcome": outcome},
               "steps": [{"spec_slug": "triage", "status": "completed", "metadata": step_md}]})
    }

    /// The estate alarm for `finding`, still open.
    fn alarm_open(finding: &str) -> Value {
        let opened = before_reading(30);
        json!({"id": format!("alarm-{finding}-open"), "kind": "backlog-item", "status": "open",
               "opened_at": opened, "opened_on": opened[..10],
               "metadata": {"estate_finding": finding},
               "steps": [{"spec_slug": "triage", "status": "ready", "metadata": {}}]})
    }

    async fn fire(board: Vec<Value>, payload: Value) -> Vec<Value> {
        fire_patched(board, payload).await.0
    }

    /// [`fire`], answering the annotations it wrote too.
    async fn fire_patched(board: Vec<Value>, payload: Value) -> (Vec<Value>, Vec<Value>) {
        let (base, posts, patches) = mock_jobs_patched(board).await;
        OpsFileRemedies::with_client(api_client(), base)
            .invoke(&args(), &ctx(payload))
            .await
            .expect("the firing succeeds");
        let posts = posts.lock().unwrap().clone();
        let patches = patches.lock().unwrap().clone();
        (posts, patches)
    }

    /// END TO END over the SHIPPED declarations: the boss-gcp
    /// below-floor reading, no request on the board, files exactly
    /// reclaim-gcp-root's passkey-gated request.
    #[tokio::test]
    async fn the_boss_gcp_disk_floor_files_the_reclaim_its_verb_declares() {
        let posts = fire(vec![], boss_gcp_below_its_floor()).await;
        assert_eq!(posts.len(), 1, "{posts:?}");
        let md = &posts[0]["metadata"];
        assert_eq!(md["verb"], "reclaim-gcp-root");
        assert_eq!(md["host"], "boss-gcp");
        assert_eq!(md["args"], json!([]));
        assert_eq!(md["requires_approval"], json!(true));
        assert_eq!(posts[0]["subject"]["id"], "reclaim-gcp-root@boss-gcp");
    }

    fn argument_verb(claim: &str, size: &str) -> String {
        let mut spec: Value = serde_json::from_str(&approval_verb(json!([
            {"finding": format!("disk_tight:boss/{claim}"),
             "args": ["boss", claim, size]}
        ])))
        .unwrap();
        spec["params"] = json!([
            {"name": "namespace", "one_of": ["boss"]},
            {"name": "claim", "pattern": "^[a-z][a-z0-9-]*$"},
            {"name": "size", "pattern": "^[0-9]+$", "max": 80},
            {"name": "plan_sha256", "pattern": "^[0-9a-f]{64}$"}
        ]);
        spec.to_string()
    }

    #[tokio::test]
    async fn declared_arguments_reach_the_passkey_request_and_each_claim_has_its_own_board() {
        let text_a = argument_verb("data-a", "60");
        let text_b = argument_verb("data-b", "60");
        let a = remedy_index(&[("expand", &text_a)])
            .expect("declared validated args")
            .remove(0);
        let b = remedy_index(&[("expand", &text_b)])
            .expect("second claim")
            .remove(0);
        assert_ne!(a.subject(), b.subject());
        let mut open = request("open", "2026-01-01T00:00:00Z");
        open["subject_id"] = json!(a.subject());
        let (base, posts) = mock_jobs(vec![open]).await;
        let h = OpsFileRemedies::with_client(api_client(), base);
        let context = ctx(json!({}));
        assert!(
            !h.file_one(&a, &a.findings, 168, read_at(), &context)
                .await
                .unwrap()
        );
        assert!(
            h.file_one(&b, &b.findings, 168, read_at(), &context)
                .await
                .unwrap()
        );
        let posts = posts.lock().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(
            posts[0]["metadata"]["args"],
            json!(["boss", "data-b", "60"])
        );
        assert_eq!(posts[0]["metadata"]["requires_approval"], true);
        assert_eq!(posts[0]["subject"]["id"], b.subject());
    }

    #[test]
    fn a_changed_plan_argument_keeps_the_same_claim_identity_and_request_id() {
        let a = remedy_index(&[("expand", &argument_verb("data-a", "60"))])
            .unwrap()
            .remove(0);
        let b = remedy_index(&[("expand", &argument_verb("data-a", "70"))])
            .unwrap()
            .remove(0);
        assert_eq!(a.subject(), b.subject());
        assert_eq!(
            remedy_request_id(&a.subject(), None),
            remedy_request_id(&b.subject(), None)
        );
    }

    #[test]
    fn declared_arguments_are_refused_before_a_request_when_the_verb_cannot_accept_them() {
        let original: Value = serde_json::from_str(&argument_verb("data-a", "60")).unwrap();
        for bad in [
            json!(["elsewhere", "data-a", "60"]),
            json!(["boss", "../data-a", "60"]),
            json!(["boss", "data-a", "81"]),
            json!(["boss", "data-a", "invalid"]),
            json!(["boss", "data-a\n", "60"]),
            json!(["boss", "data-a\u{0000}", "60"]),
            json!(["boss", "data-a"]),
            json!(["boss", "data-a", "60", "forged-hash"]),
            json!(["boss", "data-a", 60]),
        ] {
            let mut spec = original.clone();
            spec["remedies"][0]["args"] = bad.clone();
            assert!(
                remedy_index(&[("expand", &spec.to_string())]).is_err(),
                "accepted {bad}"
            );
        }
    }

    #[test]
    fn argument_declarations_refuse_ambiguous_or_unsigned_authority() {
        let original: Value = serde_json::from_str(&argument_verb("data-a", "60")).unwrap();
        for field in ["approvers", "params", "remedies"] {
            let mut spec = original.clone();
            match field {
                "approvers" => spec[field] = json!([""]),
                "params" => spec[field][3]["name"] = json!("other_hash"),
                _ => {
                    let mut duplicate = spec["remedies"][0].clone();
                    duplicate["args"][2] = json!("70");
                    spec[field].as_array_mut().unwrap().push(duplicate);
                }
            }
            assert!(
                remedy_index(&[("expand", &spec.to_string())]).is_err(),
                "accepted {field}"
            );
        }
    }

    #[test]
    fn declared_integer_bounds_do_not_round_a_value_above_the_ceiling_down() {
        let mut spec: Value =
            serde_json::from_str(&argument_verb("data-a", "9007199254740993")).unwrap();
        spec["params"][2]["max"] = json!(9007199254740992_i64);
        assert!(remedy_index(&[("expand", &spec.to_string())]).is_err());
        spec["remedies"][0]["args"][2] = json!("9007199254740992");
        assert!(remedy_index(&[("expand", &spec.to_string())]).is_ok());
    }

    #[test]
    fn malformed_parameter_and_host_shapes_refuse_argument_declarations() {
        let original: Value = serde_json::from_str(&argument_verb("data-a", "60")).unwrap();
        for (pointer, bad) in [
            ("/hosts", json!(["boss-gcp", 123])),
            ("/params/1/name", json!("")),
            ("/params/3/pattern", json!(".*")),
            ("/params/2/max", json!("80")),
            ("/params/2/pattern", json!(123)),
            ("/params/2", Value::Null),
            ("/params", Value::Null),
            ("/approvers", json!(["emp-david", 123])),
        ] {
            let mut spec = original.clone();
            *spec.pointer_mut(pointer).unwrap() = bad.clone();
            assert!(
                remedy_index(&[("expand", &spec.to_string())]).is_err(),
                "accepted {pointer}: {bad}"
            );
        }
    }

    #[test]
    fn the_signed_hash_cannot_be_defaulted_or_optional_in_a_remedy() {
        for (key, value) in [
            ("optional", json!(true)),
            ("default", json!("0".repeat(64))),
        ] {
            let mut spec: Value = serde_json::from_str(&argument_verb("data-a", "60")).unwrap();
            spec["params"][3][key] = value;
            assert!(
                remedy_index(&[("expand", &spec.to_string())]).is_err(),
                "accepted hash {key}"
            );
        }
    }

    fn runner_decide_script(root: &std::path::Path) -> std::path::PathBuf {
        let source =
            std::fs::read_to_string(boss_testing::repo_root().join("infra/ops/ops-runner.sh"))
                .unwrap();
        let function = source
            .split_once("\ndecide() {\n")
            .unwrap()
            .1
            .split_once("\n}\n")
            .unwrap()
            .0;
        let script = root.join("decide.sh");
        std::fs::write(&script, format!("set -eu\nHOST_ID=boss-gcp\nVERBS_FILE=$1\ndecide() {{\n{function}\n}}\ndecide expand \"$2\" true\n")).unwrap();
        script
    }

    /// The runner uses jq/Oniguruma and cannot reuse a Rust validator.
    /// Run its actual pure decide function against the same registry data:
    /// no runner startup, credentials, HTTP, plan rendering or execution.
    #[test]
    fn argument_validation_agrees_with_the_runners_actual_decide_function() {
        use std::process::Command;
        let root = boss_testing::scratch_dir("remedy-decide-contract");
        let script = runner_decide_script(&root);
        let registry = root.join("verbs.json");
        for args in [
            json!(["boss", "data-a", "60"]),
            json!(["boss", "data-a", "80"]),
            json!(["boss", "data-a", "81"]),
            json!(["elsewhere", "data-a", "60"]),
            json!(["boss", "../data-a", "60"]),
            json!(["boss", "data-a\n", "60"]),
            json!(["boss", "data-a\u{0000}", "60"]),
            json!(["boss", "data-a", 60]),
            json!(["boss", "data-a"]),
            json!(["boss", "data-a", "60", "forged-hash"]),
        ] {
            let mut spec: Value = serde_json::from_str(&argument_verb("data-a", "60")).unwrap();
            spec["argv"] = json!(["echo", "{1}", "{2}", "{3}", "{4}"]);
            // Like the runner, one_of takes precedence over pattern/max.
            spec["params"][0]["pattern"] = json!("^will-not-match$");
            spec["params"][0]["max"] = json!(0);
            spec["remedies"][0]["args"] = args.clone();
            std::fs::write(
                &registry,
                json!({"verbs": {"expand": spec.clone()}}).to_string(),
            )
            .unwrap();
            let accepted = remedy_index(&[("expand", &spec.to_string())]).is_ok();
            let mut signed = args.as_array().unwrap().clone();
            signed.push(json!("0".repeat(64)));
            let output = Command::new("bash")
                .arg(&script)
                .arg(&registry)
                .arg(json!(signed).to_string())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "decide failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let decision: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                accepted,
                decision.get("refuse").is_none(),
                "{args}: {decision}"
            );
        }
        for (pattern, raw) in [
            ("^[a-z][a-z0-9-]*$", "data-a"),
            ("^(?:data|logs)-[0-9]{1,3}$", "logs-12"),
            (r"^data\.[a-z]+$", "data.log"),
            ("^(data|logs)_[^/]+$", "data_é"),
            (
                "^pvc-[0-9a-f-]{36}$",
                "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56",
            ),
        ] {
            let mut spec: Value = serde_json::from_str(&argument_verb(raw, "60")).unwrap();
            spec["params"][1]["pattern"] = json!(pattern);
            spec["argv"] = json!(["echo", "{1}", "{2}", "{3}", "{4}"]);
            assert!(remedy_index(&[("expand", &spec.to_string())]).is_ok());
            std::fs::write(&registry, json!({"verbs":{"expand":spec}}).to_string()).unwrap();
            let output = Command::new("bash")
                .arg(&script)
                .arg(&registry)
                .arg(json!(["boss", raw, "60", "0".repeat(64)]).to_string())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{pattern}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let decision: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(decision.get("refuse").is_none(), "{pattern}: {decision}");
        }
    }

    #[test]
    fn regex_engine_specific_patterns_cannot_authorize_a_remedy() {
        use std::process::Command;
        let root = boss_testing::scratch_dir("remedy-regex-engine-contract");
        let script = runner_decide_script(&root);
        let registry = root.join("verbs.json");
        for pattern in ["(?u)^safe$", "(?-u)^safe$", "(?U)^safe$", "(?R)^safe$"] {
            let mut spec: Value = serde_json::from_str(&argument_verb("safe", "60")).unwrap();
            spec["params"][1]["pattern"] = json!(pattern);
            spec["argv"] = json!(["echo", "{1}", "{2}", "{3}", "{4}"]);
            std::fs::write(
                &registry,
                json!({"verbs": {"expand": spec.clone()}}).to_string(),
            )
            .unwrap();
            let output = Command::new("bash")
                .arg(&script)
                .arg(&registry)
                .arg(json!(["boss", "safe", "60", "0".repeat(64)]).to_string())
                .output()
                .unwrap();
            assert!(
                !output.status.success()
                    || serde_json::from_slice::<Value>(&output.stdout)
                        .unwrap()
                        .get("refuse")
                        .is_some(),
                "runner unexpectedly accepted {pattern}"
            );
            assert!(
                remedy_index(&[("expand", &spec.to_string())]).is_err(),
                "admission accepted {pattern} that the actual runner rejects"
            );
        }
        for pattern in [
            r"^\p{Letter}+$",
            r"^\u{0061}$",
            "^[a&&b]$",
            "^[a[b]]$",
            "^[[:alpha:]]$",
            "(?i)^safe$",
            "^é$",
        ] {
            let params = [json!({"name":"target", "pattern":pattern})];
            let error = declared_args(&json!(["a"]), &params).unwrap_err();
            assert!(
                error.contains("unsupported engine syntax"),
                "unsupported portable-pattern syntax accepted: {pattern}"
            );
        }
    }

    #[test]
    fn repetition_bounds_agree_with_the_actual_runner() {
        use std::process::Command;
        let root = boss_testing::scratch_dir("remedy-repetition-contract");
        let script = runner_decide_script(&root);
        let registry = root.join("verbs.json");
        for (pattern, raw, want) in [
            ("^a{0,100000}$", "a", true),
            ("^a{0,100001}$", "a", false),
            ("^a{100001,}$", "a", false),
            ("^a{0,}$", "a", true),
            (r"^a\{0,100001\}$", "a{0,100001}", true),
            ("^[{0,100001}]+$", "1", true),
            ("^size100001$", "size100001", true),
        ] {
            let mut spec: Value = serde_json::from_str(&argument_verb(raw, "60")).unwrap();
            spec["params"][1]["pattern"] = json!(pattern);
            spec["argv"] = json!(["echo", "{1}", "{2}", "{3}", "{4}"]);
            let accepted = remedy_index(&[("expand", &spec.to_string())]).is_ok();
            std::fs::write(&registry, json!({"verbs":{"expand":spec}}).to_string()).unwrap();
            let output = Command::new("bash")
                .arg(&script)
                .arg(&registry)
                .arg(json!(["boss", raw, "60", "0".repeat(64)]).to_string())
                .output()
                .unwrap();
            if want {
                assert!(
                    output.status.success(),
                    "{pattern}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let decision: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert!(decision.get("refuse").is_none(), "{pattern}: {decision}");
            } else {
                assert!(
                    !output.status.success(),
                    "runner accepted oversized {pattern}"
                );
                assert!(
                    String::from_utf8_lossy(&output.stderr)
                        .contains("too big number for repeat range")
                );
            }
            assert_eq!(accepted, want, "admission of {pattern}");
        }
        for pattern in ["^a{100001}$", "^a{0,18446744073709551616}$"] {
            assert!(!portable_pattern(pattern), "oversized {pattern}");
        }
    }

    #[test]
    fn stacked_quantifiers_cannot_change_a_declared_words_match() {
        use std::process::Command;
        let root = boss_testing::scratch_dir("remedy-stacked-repeat-contract");
        let script = runner_decide_script(&root);
        let registry = root.join("verbs.json");
        for pattern in ["^a++a$", "^a*+a$", "^a{1,2}+a$"] {
            let mut spec: Value = serde_json::from_str(&argument_verb("aa", "60")).unwrap();
            spec["params"][1]["pattern"] = json!(pattern);
            spec["argv"] = json!(["echo", "{1}", "{2}", "{3}", "{4}"]);
            std::fs::write(
                &registry,
                json!({"verbs":{"expand":spec.clone()}}).to_string(),
            )
            .unwrap();
            let output = Command::new("bash")
                .arg(&script)
                .arg(&registry)
                .arg(json!(["boss", "aa", "60", "0".repeat(64)]).to_string())
                .output()
                .unwrap();
            assert!(output.status.success());
            let decision: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(decision.get("refuse").is_some(), "{pattern}: {decision}");
            assert!(
                remedy_index(&[("expand", &spec.to_string())]).is_err(),
                "admission differs from the runner for {pattern}"
            );
        }
        for (pattern, raw) in [(r"^a\+\+a$", "a++a"), ("^[+*?]+$", "+"), ("^a+?a$", "aa")] {
            let mut spec: Value = serde_json::from_str(&argument_verb(raw, "60")).unwrap();
            spec["params"][1]["pattern"] = json!(pattern);
            spec["argv"] = json!(["echo", "{1}", "{2}", "{3}", "{4}"]);
            assert!(
                remedy_index(&[("expand", &spec.to_string())]).is_ok(),
                "{pattern}"
            );
            std::fs::write(&registry, json!({"verbs":{"expand":spec}}).to_string()).unwrap();
            let output = Command::new("bash")
                .arg(&script)
                .arg(&registry)
                .arg(json!(["boss", raw, "60", "0".repeat(64)]).to_string())
                .output()
                .unwrap();
            assert!(output.status.success());
            let decision: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(decision.get("refuse").is_none(), "{pattern}: {decision}");
        }
    }

    #[tokio::test]
    async fn argument_remedies_racing_on_the_same_claim_post_one_id() {
        let r = remedy_index(&[("expand", &argument_verb("data-a", "60"))])
            .unwrap()
            .remove(0);
        let (base, posts) = mock_jobs(vec![]).await;
        let h = OpsFileRemedies::with_client(api_client(), base);
        let context_a = ctx(json!({}));
        let context_b = ctx(json!({}));
        let (a, b) = tokio::join!(
            h.file_one(&r, &r.findings, 168, read_at(), &context_a),
            h.file_one(&r, &r.findings, 168, read_at(), &context_b)
        );
        assert!(a.unwrap() && b.unwrap());
        let posts = posts.lock().unwrap();
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0]["id"], posts[1]["id"]);
        assert_eq!(
            posts[0]["metadata"]["args"],
            json!(["boss", "data-a", "60"])
        );
    }

    #[tokio::test]
    async fn a_tight_claim_files_native_read_only_discovery_before_any_approval_request() {
        let mut mutation: Value = serde_json::from_str(&argument_verb("data-a", "60")).unwrap();
        mutation["hosts"] = json!(["forge"]);
        mutation["plan_verb"] = json!("plan-expand");
        mutation.as_object_mut().unwrap().remove("remedies");
        mutation["discovery_remedies"] = json!([{
            "finding_class": "disk_tight",
            "scope": "instance-volumes",
            "discovery_verb": "discover-expansion",
            "arg_fields": ["namespace", "claim"]
        }]);
        let discovery = json!({
            "about": "READ-ONLY argument discovery",
            "capture": "separate-streams",
            "hosts": ["forge"], "argv": ["echo", "{1}", "{2}"],
            "params": mutation["params"].as_array().unwrap()[..2]
        });
        let mutation_text = mutation.to_string();
        let discovery_text = discovery.to_string();
        let plan = json!({"hosts":["forge"],"params":mutation["params"].as_array().unwrap()[..3]})
            .to_string();
        let (base, posts) = mock_jobs(vec![]).await;
        OpsFileRemedies::with_client(api_client(), base)
            .invoke_declared(
                &[
                    ("expand", &mutation_text),
                    ("discover-expansion", &discovery_text),
                    ("plan-expand", &plan),
                ],
                &args(),
                &ctx(
                    json!({"scope":"instance-volumes","findings": {"disk_tight": [{
                        "id": "boss/data-a", "namespace": "boss", "claim": "data-a"
                    }]}}),
                ),
            )
            .await
            .unwrap();
        let posted = posts.lock().unwrap();
        assert_eq!(
            posted.len(),
            1,
            "the finding must ask its existing native discovery port"
        );
        assert_eq!(posted[0]["metadata"]["verb"], "discover-expansion");
        assert_eq!(posted[0]["metadata"]["args"], json!(["boss", "data-a"]));
        assert_ne!(posted[0]["metadata"]["requires_approval"], json!(true));
        assert_eq!(posted[0]["metadata"]["remedies"], "disk_tight:boss/data-a");
    }

    #[tokio::test]
    async fn the_dispatch_loop_matches_claim_findings_and_validates_before_http() {
        let text = argument_verb("data-a", "60");
        let payload = json!({"findings": {"disk_tight": [
            {"id": "boss/data-a", "namespace": "boss", "claim": "data-a"}
        ]}});
        let (base, posts) = mock_jobs(vec![]).await;
        let h = OpsFileRemedies::with_client(api_client(), base);
        h.invoke_declared(&[("expand", &text)], &args(), &ctx(payload.clone()))
            .await
            .unwrap();
        assert_eq!(posts.lock().unwrap().len(), 1);
        assert_eq!(
            posts.lock().unwrap()[0]["metadata"]["args"],
            json!(["boss", "data-a", "60"])
        );
        // No server exists here: an invalid declaration refuses before a read.
        let invalid = argument_verb("data-a", "81");
        let h = OpsFileRemedies::with_client(api_client(), "http://127.0.0.1:1");
        let error = h
            .invoke_declared(&[("expand", &invalid)], &args(), &ctx(payload))
            .await
            .unwrap_err();
        assert!(error.is_permanent(), "{error:?}");
        assert!(error.to_string().contains("size"), "{error:?}");
    }

    #[tokio::test]
    async fn another_claim_clearing_does_not_lift_this_claims_decline() {
        let mut spec: Value = serde_json::from_str(&argument_verb("data-a", "60")).unwrap();
        spec["remedies"].as_array_mut().unwrap().push(json!({
            "finding": "disk_tight:boss/data-b", "args": ["boss", "data-b", "60"]
        }));
        let text = spec.to_string();
        let index = remedy_index(&[("expand", &text)]).unwrap();
        let mut declined_a = declined("declined-a", 220, 210);
        declined_a["subject_id"] = json!(index[0].subject());
        let mut open_alarm = alarm_open("disk_tight:boss/data-a");
        // Packet ids do not contain the finding's namespace separator.
        open_alarm["id"] = json!("alarm-a");
        let (base, posts, patches) = mock_jobs_patched(vec![
            declined_a,
            alarm_closed("disk_tight:boss/data-b", 100, true),
            open_alarm,
        ])
        .await;
        let h = OpsFileRemedies::with_client(api_client(), base);
        h.invoke_declared(
            &[("expand", &text)],
            &args(),
            &ctx(json!({
                "findings": {"disk_tight": [{"id": "boss/data-a"}, {"id": "boss/data-b"}]}
            })),
        )
        .await
        .unwrap();
        let posts = posts.lock().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(
            posts[0]["metadata"]["args"],
            json!(["boss", "data-b", "60"])
        );
        let patches = patches.lock().unwrap();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0]["id"], "alarm-a");
        assert_eq!(
            patches[0]["body"][held_key(&index[0])]["declined_request"],
            "declined-a"
        );
    }

    /// AT MOST ONE OPEN REQUEST per verb and host, however old.
    #[tokio::test]
    async fn an_open_request_files_nothing_more() {
        let posts = fire(
            vec![request("open", "2026-01-01T00:00:00Z")],
            boss_gcp_below_its_floor(),
        )
        .await;
        assert!(posts.is_empty(), "{posts:?}");
    }

    /// A request opened this week, whatever became of it, files nothing:
    /// the reclaim does not clear the finding (review of car 4d80316f).
    #[tokio::test]
    async fn a_request_opened_inside_the_window_is_not_refiled() {
        let recent = (read_at() - chrono::Duration::hours(20)).to_rfc3339();
        let posts = fire(vec![request("closed", &recent)], boss_gcp_below_its_floor()).await;
        assert!(posts.is_empty(), "{posts:?}");

        let old = (read_at() - chrono::Duration::hours(200)).to_rfc3339();
        let posts = fire(vec![request("closed", &old)], boss_gcp_below_its_floor()).await;
        assert_eq!(
            posts.len(),
            1,
            "past the window a persisting finding files again"
        );
    }

    /// TWO FIRINGS THAT RACE FILE ONE REQUEST (review 63bafc11, LOW 2a).
    /// The dedup was check-then-post: two comparisons handled at once
    /// — or a spool replay beside the live one — each read a board with
    /// nothing on it and each posted, and the jobs API minted two ids,
    /// so David met one extra passkey prompt. Now the request's id is
    /// derived from the board it was filed against, so both posts name
    /// the same packet and the jobs API admits it once. Modelled by two
    /// firings against one board that does not grow: both read before
    /// either's post landed.
    #[tokio::test]
    async fn two_firings_against_one_board_file_one_request() {
        let old = request("closed", &before_reading(200));
        let (base, posts) = mock_jobs(vec![old]).await;
        let h = OpsFileRemedies::with_client(api_client(), base);
        let (args_a, ctx_a) = (args(), ctx(boss_gcp_below_its_floor()));
        let (args_b, ctx_b) = (args(), ctx(boss_gcp_below_its_floor()));
        let (a, b) = tokio::join!(h.invoke(&args_a, &ctx_a), h.invoke(&args_b, &ctx_b));
        a.expect("the first firing succeeds");
        b.expect("the second firing succeeds");
        let posts = posts.lock().unwrap().clone();
        assert_eq!(posts.len(), 2, "both firings posted: {posts:?}");
        let ids: BTreeSet<&str> = posts.iter().filter_map(|p| p["id"].as_str()).collect();
        assert_eq!(
            ids.len(),
            1,
            "both posts name ONE packet, so the jobs API admits it once: {posts:?}"
        );
        let id = *ids.iter().next().unwrap();
        assert_eq!(
            id,
            remedy_request_id("reclaim-gcp-root@boss-gcp", Some("r-old"))
        );
        assert_ne!(
            id,
            remedy_request_id("reclaim-gcp-root@boss-gcp", None),
            "the successor of a request is not the first request on an empty board"
        );
    }

    /// THE NEWEST REQUEST IS THE ONE OPENED LAST, not the one the list
    /// puts first (review 63bafc11, LOW 2d). The list orders by
    /// `opened_on`, the SIM date, and the window is judged on
    /// `opened_at`, the admission instant; a `limit=1` read took the
    /// list's first row as the newest. A request admitted 20 hours ago
    /// under an earlier sim date sorted below one admitted 200 hours ago
    /// under a later one, and the old guard filed beside it.
    #[tokio::test]
    async fn the_window_reads_every_request_not_the_lists_first_row() {
        let mut stale_first = request("closed", &before_reading(200));
        stale_first["opened_on"] = json!("2026-10-05");
        let mut recent = request("closed", &before_reading(20));
        recent["id"] = json!("r-recent");
        recent["opened_on"] = json!("2026-09-20");
        let posts = fire(vec![stale_first, recent], boss_gcp_below_its_floor()).await;
        assert!(
            posts.is_empty(),
            "a request opened 20 hours ago holds the window, wherever the list sorts it: {posts:?}"
        );
    }

    /// A DECLINE STOPS THE ASKING (review 63bafc11, LOW 2b). David
    /// rejecting the plan closed the request refused, and a week later
    /// the persisting finding filed it again — a passkey prompt a week
    /// for something he said no to, with nothing on the record saying
    /// stop. The decline IS that record: the newest request on the
    /// subject declined by its approver files nothing more until the
    /// estate observes the finding clear — its alarm closed after the
    /// decline.
    #[tokio::test]
    async fn a_declined_remedy_is_not_asked_again_while_its_finding_persists() {
        let board = vec![
            declined("r-declined", 220, 210),
            // A person closing the alarm as a duplicate points at another
            // packet still carrying it, and closing it declined is the
            // same no again: neither is the finding clearing.
            alarm_closed_as("disk_tight:boss-gcp", 100, false, "duplicate"),
            alarm_closed_as("disk_tight:boss-gcp", 90, false, "decline"),
            // The machine clearing it BEFORE the decline is the episode
            // the decline answered.
            alarm_closed("disk_tight:boss-gcp", 215, true),
            // Another finding clearing says nothing of this one.
            alarm_closed("disk_tight:forge", 50, true),
        ];
        let posts = fire(board, boss_gcp_below_its_floor()).await;
        assert!(posts.is_empty(), "{posts:?}");
    }

    #[tokio::test]
    async fn a_declined_remedy_is_asked_again_once_the_estate_cleared_its_finding() {
        let board = vec![
            declined("r-declined", 220, 210),
            alarm_closed("disk_tight:boss-gcp", 100, true),
        ];
        let posts = fire(board, boss_gcp_below_its_floor()).await;
        assert_eq!(posts.len(), 1, "a new episode is asked again: {posts:?}");
        assert_eq!(
            posts[0]["id"],
            json!(remedy_request_id(
                "reclaim-gcp-root@boss-gcp",
                Some("r-declined")
            ))
        );
        assert_eq!(
            posts[0]["metadata"]["decline_lifted"],
            json!({"declined_request": "r-declined",
                   "cleared_alarm": "alarm-disk_tight:boss-gcp-100"}),
            "the request names the close that lifted the decline"
        );
    }

    /// A PERSON'S CLOSE LIFTS IT TOO (backlog aa816dd4, LOW-2 of review
    /// ea2ecfd4). d3c7eada, boss-gcp's disk alarm, was closed when the
    /// car that fixed it landed — not by the recover rule — and the
    /// decline held into the finding's next occurrence, a new episode
    /// answered with the old no.
    #[tokio::test]
    async fn a_declined_remedy_is_asked_again_once_a_person_closed_its_alarm() {
        let board = vec![
            declined("r-declined", 220, 210),
            alarm_closed_as("disk_tight:boss-gcp", 100, false, "build"),
            alarm_open("disk_tight:boss-gcp"),
        ];
        let (posts, patches) = fire_patched(board, boss_gcp_below_its_floor()).await;
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(
            posts[0]["metadata"]["decline_lifted"]["cleared_alarm"],
            "alarm-disk_tight:boss-gcp-100"
        );
        assert!(
            patches.is_empty(),
            "a lifted decline holds nothing: {patches:?}"
        );
    }

    /// A HELD REMEDY SAYS SO ON ITS ALARM (backlog aa816dd4, LOW-2): it
    /// showed only in a log line, so the alarm read as though nothing had
    /// been offered. Once: an alarm already saying exactly this is not
    /// written again on the next comparison.
    #[tokio::test]
    async fn a_held_remedy_says_so_on_its_open_alarm_once() {
        let board = vec![
            declined("r-declined", 220, 210),
            alarm_open("disk_tight:boss-gcp"),
            alarm_open("disk_tight:forge"),
        ];
        let (posts, patches) = fire_patched(board.clone(), boss_gcp_below_its_floor()).await;
        assert!(posts.is_empty(), "{posts:?}");
        assert_eq!(patches.len(), 1, "only this finding's alarm: {patches:?}");
        assert_eq!(patches[0]["id"], "alarm-disk_tight:boss-gcp-open");
        let held = &patches[0]["body"]["remedy_held:reclaim-gcp-root@boss-gcp"];
        assert_eq!(held["declined_request"], "r-declined", "{held}");
        assert_eq!(held["verb"], "reclaim-gcp-root", "{held}");
        assert!(
            held["note"]
                .as_str()
                .is_some_and(|n| n.contains("declined")),
            "{held}"
        );

        let mut said = board;
        said[1]["metadata"]["remedy_held:reclaim-gcp-root@boss-gcp"] = held.clone();
        let (_, again) = fire_patched(said, boss_gcp_below_its_floor()).await;
        assert!(again.is_empty(), "already said: {again:?}");
    }

    /// THE HELD NOTE IS NOT FATAL (backlog e5ff616f, LOW-A of review
    /// cc0e1d8e). The hold was decided before the note was written, and
    /// the note is observability: a refused PATCH failed the whole
    /// firing, so a persistently refused one retried and dead-lettered
    /// every estate comparison it rode on. Attempted, logged, and the
    /// firing still succeeds holding.
    #[tokio::test]
    async fn a_refused_held_note_does_not_fail_the_firing() {
        let board = vec![
            declined("r-declined", 220, 210),
            alarm_open("disk_tight:boss-gcp"),
        ];
        let (base, posts, patches) =
            mock_jobs_answering_patches(board, axum::http::StatusCode::INTERNAL_SERVER_ERROR).await;
        OpsFileRemedies::with_client(api_client(), base)
            .invoke(&args(), &ctx(boss_gcp_below_its_floor()))
            .await
            .expect("a refused note does not fail the firing");
        assert_eq!(patches.lock().unwrap().len(), 1, "the note was attempted");
        assert!(posts.lock().unwrap().is_empty(), "still held");
    }

    /// A DESIGN-ROUTED ALARM DECLINED AT REVIEW IS NOT A CLEAR (backlog
    /// e5ff616f, LOW-B of review cc0e1d8e). Its triage disposition is
    /// `design`, which no list of not-a-clear dispositions named, so the
    /// close counted as the finding ending — while the design review had
    /// answered "not acting on it", the same no as a triage `decline`.
    /// The close's recorded outcome is what says how the alarm ended.
    #[tokio::test]
    async fn an_alarm_declined_at_design_review_does_not_lift_a_decline() {
        let board = vec![
            declined("r-declined", 220, 210),
            alarm_closed_ending("disk_tight:boss-gcp", 100, false, "design", "declined"),
            alarm_open("disk_tight:boss-gcp"),
        ];
        let posts = fire(board, boss_gcp_below_its_floor()).await;
        assert!(posts.is_empty(), "{posts:?}");

        let board = vec![
            declined("r-declined", 220, 210),
            alarm_closed_ending("disk_tight:boss-gcp", 100, false, "design", "completed"),
        ];
        let posts = fire(board, boss_gcp_below_its_floor()).await;
        assert_eq!(posts.len(), 1, "a design that was built clears: {posts:?}");
    }

    /// The board, judged whole and pure: open first, then the window,
    /// then a decline; and a board it cannot read is refused, never
    /// guessed.
    #[test]
    fn the_board_is_judged_whole() {
        let now = read_at();
        let at = |h: i64| request("closed", &before_reading(h));
        assert_eq!(
            judge_board(&[at(300), request("open", &before_reading(400))], now, 168),
            Ok(Board::Open)
        );
        assert_eq!(judge_board(&[at(300), at(10)], now, 168), Ok(Board::Recent));
        assert_eq!(judge_board(&[], now, 168), Ok(Board::File { after: None }));
        assert_eq!(
            judge_board(&[at(300)], now, 168),
            Ok(Board::File {
                after: Some("r-old".into())
            })
        );
        let d = declined("r-declined", 220, 210);
        assert_eq!(
            judge_board(&[at(300), d], now, 168),
            Ok(Board::Declined {
                request: "r-declined".into(),
                at: (read_at() - chrono::Duration::hours(210)),
            })
        );
        // An approval runs; it is not a decline.
        let mut approved = declined("r-approved", 220, 210);
        approved["steps"][0]["metadata"]["decision"] = json!("approved");
        assert_eq!(
            judge_board(&[approved], now, 168),
            Ok(Board::File {
                after: Some("r-approved".into())
            })
        );
        // A refusal by the runner skipped the approve step: not a decline.
        let mut runner_refused = declined("r-runner", 220, 210);
        runner_refused["steps"][0]["status"] = json!("skipped");
        assert_eq!(
            judge_board(&[runner_refused], now, 168),
            Ok(Board::File {
                after: Some("r-runner".into())
            })
        );
        assert!(judge_board(&[json!({"id": "x", "status": "closed"})], now, 168).is_err());
    }

    /// A reading with no declared finding files nothing — the forge's
    /// own floor (no verb declares it), boss-gcp above its floor, and the
    /// unit series.
    #[tokio::test]
    async fn a_finding_no_verb_declares_files_nothing() {
        let mut forge = boss_gcp_below_its_floor();
        forge["host"] = json!("forge");
        forge["findings"]["disk_tight"][0]["id"] = json!("forge");
        let mut clean = boss_gcp_below_its_floor();
        clean["findings"]["disk_tight"] = json!([]);
        for payload in [forge, clean] {
            assert!(fire(vec![], payload).await.is_empty());
        }
    }

    #[tokio::test]
    async fn a_missing_window_is_an_authoring_fault() {
        let h = OpsFileRemedies::with_client(api_client(), "http://unused");
        let err = h
            .invoke(&[], &ctx(boss_gcp_below_its_floor()))
            .await
            .expect_err("no window");
        assert!(err.is_permanent(), "{err:?}");
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = OpsFileRemedies::with_client(api_client(), "http://unused");
        assert_eq!(h.name(), "ops.file_remedies");
        assert_eq!(
            crate::cascade::handler_emits()
                .get("ops.file_remedies")
                .cloned(),
            Some(vec!["jobs.job.created", "jobs.job.updated"]),
            "the cascade table knows what this handler emits"
        );
    }
}
