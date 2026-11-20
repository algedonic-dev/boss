//! Which workflow publishes need a person's passkey — ONE rule, judged by
//! the server on the LIVE registry (backlog a4b57aca).
//!
//! THE RULE (David, 2026-11-08: "only require passkey approval if the
//! current protocol requires a passkey anywhere ... an agent can't strip
//! a passkey requirement off a protocol anywhere"). A live row is
//! PASSKEY-PROTECTED iff
//!
//! 1. any of its steps declares `assurance_required = "presence"`, or
//! 2. it admits or fulfils a packet of a protected kind (R8 of the ask:
//!    an agent must not strip a passkey indirectly by re-wiring a parent
//!    away from a protected child), closed transitively.
//!
//! A publish over a protected live row is refused unless the request
//! names a `publish-a-protocol` packet whose approve step a named
//! approver signed with a passkey for these exact bytes over this live
//! version. Every other publish needs policy alone — a new kind (one that
//! never published), a plain row, and a candidate that ADDS presence
//! among them. The LIVE row is judged, never the candidate: judging the
//! candidate would let a candidate that removes the presence step
//! publish itself.
//!
//! RETIRED IS NOT NEW (review R2 of car 8715776d). A kind with no active
//! row is judged on the row it last published, its newest retired row
//! ([`baselines`]): otherwise Retire policy plus an unsigned publish strip
//! a passkey in two steps. Only a kind with no published row at all is
//! new. The same baseline stands in for a retired kind in the edges, so
//! a retired parent and the parent of a retired child keep theirs.
//!
//! THE EDGES CHANGE ONLY BY CAR (review R1). The dispatcher's HTTP rule
//! doors refuse a publish or retire that would remove an edge into a
//! protected kind (`boss_dispatcher::rules::authoring`, over
//! [`removed_protected_edges`]); the reviewed road for such a change is
//! a car carrying the rule's file through the dispatcher's boot seed.
//! Without that, retiring the rule is itself the strip, and the parent's
//! next publish would be judged on edges an agent had already cut.
//!
//! WHY THE SERVER. Until this module the passkey on a protocol publish
//! was checked only client-side, in `infra/gcp/publish-workflow.sh` over
//! `infra/lib/signed-publish.jq`; `POST /api/workflows/{kind}/publish`
//! asked policy and nothing else, so any actor holding Publish on
//! `workflow` could publish any row, a presence-carrying one included
//! (the boundary finding of 2026-11-08T22:45Z on the item). The client
//! checks stay as early refusals; this is the boundary.
//!
//! THE EDGES (clause 2) are read from the LIVE dispatcher rule registry,
//! `GET /api/dispatcher/rules` (the [`DispatcherRules`] port the
//! departments readiness read already uses), plus each live row's own
//! `on_complete_create` triggers. A rule draws an edge parent -> child:
//!
//! - `jobs.spawn`: the child is the `kind` arg when it is a string
//!   literal; the parent is every kind the rule's `when` names as
//!   `kind = "…"` / `workflow_kind = "…"`, or, when it names none and the
//!   rule fires on `step.done.<k>` / `step.ready.<k>`, every live row
//!   with a step of step-kind `<k>` (narrowed by a literal `spec_slug`
//!   and `metadata.ops_verb` when the `when` names them). A rule on any
//!   other event names no protocol as its parent (a clock, an outbox
//!   dead letter) and draws no edge.
//! - `jobs.complete_linked_step`: the parent is every live row with a
//!   step whose slug is among `steps`; the child every kind its `when`
//!   names.
//! - `jobs.complete_step_matching` / `jobs.complete_step_from_record`:
//!   the parent is the `kind` arg; the child every kind its `when` names.
//!
//! Over-approximating a parent is the safe side (a passkey asked where
//! none was strictly owed); an edge the rules do not spell as a literal
//! is NOT drawn, and those are named as residuals on the car rather than
//! guessed at: a dynamic `jobs.spawn` kind (`metadata.subworkflow`), a
//! fulfil rule whose `when` names no kind, and the admissions coded in
//! handlers rather than rules.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::registry::{WorkflowError, WorkflowSpec};

/// The verb whose `approvers` list says who may sign a protocol publish,
/// compiled in so the server and `publish-workflow.sh` read ONE list
/// (CLAUDE.md §9a; the `car.rs` precedent).
const PUBLISH_WORKFLOW_VERB: &str =
    include_str!("../../../../infra/ops/verbs/publish-workflow.json");

/// The verb file's path as refusals name it — the jq's `$verb_file`.
pub const VERB_FILE: &str = "infra/ops/verbs/publish-workflow.json";

/// The kind of the packet that carries a passkey-signed publish.
pub const PUBLISH_A_PROTOCOL: &str = "publish-a-protocol";

/// Who may sign a protocol publish: the verb file's `approvers`, a
/// non-empty list of employee ids. A file that does not parse, or names
/// none, answers an empty list, and an empty list refuses every
/// signature (no stamp can be by a member of it).
pub fn approvers() -> Vec<String> {
    serde_json::from_str::<Value>(PUBLISH_WORKFLOW_VERB)
        .ok()
        .and_then(|v| {
            v.get("approvers")?.as_array().map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// The first step of `spec` that declares presence, by slug.
pub fn presence_step(spec: &WorkflowSpec) -> Option<&str> {
    spec.steps
        .iter()
        .find(|s| s.assurance_required == Some(boss_core::job::Assurance::Presence))
        .map(|s| s.title.as_str())
}

/// Each kind's BASELINE among `rows` (every status): its active row, or,
/// for a kind with none active, its newest retired row — the row the kind
/// last published, since a row is retired only by being superseded or
/// retired from active. Drafts and rejected arms never went live and are
/// not baselines. A kind absent here never published, and is new.
pub fn baselines<'a>(rows: impl IntoIterator<Item = &'a WorkflowSpec>) -> Vec<WorkflowSpec> {
    use crate::registry::WorkflowStatus::{Active, Retired};
    let mut by_kind: BTreeMap<&str, &WorkflowSpec> = BTreeMap::new();
    for row in rows {
        if !matches!(row.status, Active | Retired) {
            continue;
        }
        let better = by_kind.get(row.kind.as_str()).is_none_or(|held| {
            (row.status == Active, row.version) > (held.status == Active, held.version)
        });
        if better {
            by_kind.insert(row.kind.as_str(), row);
        }
    }
    by_kind.into_values().cloned().collect()
}

/// The edges into a protected kind that `before` draws and `after` does
/// not, judged over the baseline `rows` — what an unsigned change to the
/// rule registry may not do (review R1 of car 8715776d). Compared as
/// `(parent, child)` pairs, so a pair another rule still draws is not
/// removed; protection is read off `before`, the registry being changed.
pub fn removed_protected_edges(
    before: &[Value],
    after: &[Value],
    rows: &[WorkflowSpec],
) -> Vec<(Edge, String)> {
    let drawn = edges(before, rows);
    let protected = protected_kinds(rows, &drawn);
    let kept: BTreeSet<(String, String)> = edges(after, rows)
        .into_iter()
        .map(|e| (e.parent, e.child))
        .collect();
    let mut seen = BTreeSet::new();
    drawn
        .into_iter()
        .filter(|e| !kept.contains(&(e.parent.clone(), e.child.clone())))
        .filter_map(|e| {
            let why = protected.get(&e.child)?.clone();
            seen.insert((e.parent.clone(), e.child.clone()))
                .then_some((e, why))
        })
        .collect()
}

/// One admission/fulfilment edge: a packet of `parent` admits or
/// fulfils a packet of `child`, by `via`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub parent: String,
    pub child: String,
    pub via: String,
}

fn literal(expr: &str) -> Option<String> {
    let t = expr.trim();
    (t.len() >= 2 && t.starts_with('"') && t.ends_with('"'))
        .then(|| t[1..t.len() - 1].to_string())
        .filter(|s| !s.is_empty() && !s.contains('"'))
}

/// Every `<key> = "<value>"` equality a `when` spells, for `key` exactly
/// (so `kind` does not match `workflow_kind` or `metadata.kind`).
fn equalities(when: &str, key: &str) -> BTreeSet<String> {
    let pattern = format!(r#"(?:^|[\s(]){}\s*=\s*"([^"]+)""#, regex::escape(key));
    match regex::Regex::new(&pattern) {
        Ok(re) => re
            .captures_iter(when)
            .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
            .collect(),
        Err(_) => BTreeSet::new(),
    }
}

fn kinds_named(when: &str) -> BTreeSet<String> {
    let mut out = equalities(when, "kind");
    out.extend(equalities(when, "workflow_kind"));
    out
}

fn arg<'a>(step: &'a Value, key: &str) -> Option<&'a str> {
    step.get("args")?.get(key)?.as_str()
}

/// The protocols a rule's trigger fires from (see the module doc).
fn trigger_parents(rule: &Value, rows: &[WorkflowSpec]) -> BTreeSet<String> {
    let when = rule.get("when").and_then(Value::as_str).unwrap_or("");
    let named = kinds_named(when);
    if !named.is_empty() {
        return named;
    }
    let event = rule.get("on_event").and_then(Value::as_str).unwrap_or("");
    let Some(step_kind) = event
        .strip_prefix("step.done.")
        .or_else(|| event.strip_prefix("step.ready."))
    else {
        return BTreeSet::new();
    };
    let slugs = equalities(when, "spec_slug");
    let verbs = equalities(when, "metadata.ops_verb");
    rows.iter()
        .filter(|row| {
            row.steps.iter().any(|s| {
                (step_kind == "*" || s.kind == step_kind)
                    && (slugs.is_empty() || slugs.contains(&s.title))
                    && (verbs.is_empty()
                        || s.metadata_defaults
                            .get("ops_verb")
                            .and_then(Value::as_str)
                            .is_some_and(|v| verbs.contains(v)))
            })
        })
        .map(|row| row.kind.clone())
        .collect()
}

/// Every edge the live rule registry and the live rows draw.
pub fn edges(rules: &[Value], rows: &[WorkflowSpec]) -> Vec<Edge> {
    let mut out = Vec::new();
    for row in rows {
        for trigger in &row.on_complete_create {
            out.push(Edge {
                parent: row.kind.clone(),
                child: trigger.kind.clone(),
                via: "its own on_complete_create".into(),
            });
        }
    }
    for rule in rules {
        let name = rule.get("name").and_then(Value::as_str).unwrap_or("?");
        let when = rule.get("when").and_then(Value::as_str).unwrap_or("");
        let via = format!("dispatcher rule {name}");
        let steps = rule.get("do").and_then(Value::as_array);
        for step in steps.into_iter().flatten() {
            let handler = step.get("handler").and_then(Value::as_str).unwrap_or("");
            let pairs: Vec<(String, String)> = match handler {
                "jobs.spawn" => match arg(step, "kind").and_then(literal) {
                    Some(child) => trigger_parents(rule, rows)
                        .into_iter()
                        .map(|p| (p, child.clone()))
                        .collect(),
                    None => Vec::new(),
                },
                "jobs.complete_linked_step" => {
                    let slugs: BTreeSet<String> = arg(step, "steps")
                        .and_then(literal)
                        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
                        .unwrap_or_default();
                    let parents: BTreeSet<String> = rows
                        .iter()
                        .filter(|r| r.steps.iter().any(|s| slugs.contains(&s.title)))
                        .map(|r| r.kind.clone())
                        .collect();
                    let children = kinds_named(when);
                    parents
                        .iter()
                        .flat_map(|p| children.iter().map(|c| (p.clone(), c.clone())))
                        .collect()
                }
                "jobs.complete_step_matching" | "jobs.complete_step_from_record" => {
                    match arg(step, "kind").and_then(literal) {
                        Some(parent) => kinds_named(when)
                            .into_iter()
                            .map(|c| (parent.clone(), c))
                            .collect(),
                        None => Vec::new(),
                    }
                }
                _ => Vec::new(),
            };
            out.extend(pairs.into_iter().map(|(parent, child)| Edge {
                parent,
                child,
                via: via.clone(),
            }));
        }
    }
    out
}

/// Every protected kind among `rows`, each with the reason in words —
/// the presence step it declares, or the chain to one.
pub fn protected_kinds(rows: &[WorkflowSpec], edges: &[Edge]) -> BTreeMap<String, String> {
    let mut protected: BTreeMap<String, String> = rows
        .iter()
        .filter_map(|r| {
            presence_step(r).map(|slug| {
                (
                    r.kind.clone(),
                    format!("its step {slug} declares assurance_required = presence"),
                )
            })
        })
        .collect();
    let live: BTreeSet<&str> = rows.iter().map(|r| r.kind.as_str()).collect();
    loop {
        let found: Vec<(String, String)> = edges
            .iter()
            .filter(|e| live.contains(e.parent.as_str()) && !protected.contains_key(&e.parent))
            .filter_map(|e| {
                protected.get(&e.child).map(|why| {
                    (
                        e.parent.clone(),
                        format!(
                            "it admits or fulfils {} packets through {}, and {} is protected: {why}",
                            e.child, e.via, e.child
                        ),
                    )
                })
            })
            .collect();
        if found.is_empty() {
            return protected;
        }
        for (k, why) in found {
            protected.entry(k).or_insert(why);
        }
    }
}

/// A publish-a-protocol signature the request named, already judged
/// against the packet ([`judge_signature`]) and the bytes it carried;
/// what remains is judged inside the registry's write ([`judge`]).
#[derive(Debug, Clone)]
pub struct Signed {
    pub packet: String,
    pub by: String,
    pub at: String,
    /// The live version the signed diff was rendered against.
    pub live_version: String,
    /// The signed bytes, read by the platform seed's own loader.
    pub spec: WorkflowSpec,
}

/// The fields a publish authors — everything but the registry's own
/// bookkeeping (`version`, `status`, `created_at`, `authoring_job_id`).
fn authored(spec: &WorkflowSpec) -> Value {
    let mut v = serde_json::to_value(spec).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        for k in ["version", "status", "created_at", "authoring_job_id"] {
            o.remove(k);
        }
    }
    v
}

/// The judgement, run by the registry adapter INSIDE the write that
/// flips the row (under the same lock that reads `active`), so the live
/// row it judges is the row the publish replaces. `active` is the
/// adapter's [`baselines`]: a kind with no active row is judged on the
/// row it last published.
///
/// `rules` is the dispatcher's enforced rule list, or why it could not
/// be read. A row protected by its own presence step needs no rules; a
/// row that might be protected only through an edge refuses 503 when the
/// edges cannot be read — an unread edge is not an absent one.
pub fn judge(
    kind: &str,
    active: &[WorkflowSpec],
    candidate: &WorkflowSpec,
    rules: Result<&[Value], &str>,
    signed: Option<&Signed>,
) -> Result<(), WorkflowError> {
    let live = active.iter().find(|s| s.kind == kind);
    if let Some(s) = signed {
        // A signature binds whether or not the row needed one: a request
        // that names a packet publishes only what that packet signed.
        let Some(live) = live else {
            return Err(WorkflowError::Conflict(format!(
                "publish-a-protocol {} signed a diff against live v{}, and {kind} has never \
                 published a row: what was signed is not what this publish would write — nothing published",
                s.packet, s.live_version
            )));
        };
        if s.live_version != live.version.to_string() {
            return Err(WorkflowError::Conflict(format!(
                "publish-a-protocol {} signed a diff against live v{}, and live {kind} is v{} \
                 now: what was signed is not what this publish would overwrite — nothing \
                 published",
                s.packet, s.live_version, live.version
            )));
        }
        if authored(candidate) != authored(&s.spec) {
            return Err(WorkflowError::Conflict(format!(
                "the {kind} draft this publish would promote (v{}) is not the row publish-a-protocol \
                 {} signed: its authored fields differ from the signed bytes — nothing published",
                candidate.version, s.packet
            )));
        }
        return Ok(());
    }
    let Some(protection) = protection(kind, active, rules)? else {
        return Ok(());
    };
    let live = protection.baseline;
    let retired = if live.status == crate::registry::WorkflowStatus::Retired {
        ", the row it last published (retired, and retiring a row does not shed its passkey)"
    } else {
        ""
    };
    Err(WorkflowError::Conflict(format!(
        "{kind} v{}{retired} is passkey-protected ({}), so it is published only with a \
         publish-a-protocol packet whose approve step a named approver signed with a passkey \
         for these exact bytes over this live version — this request names none (POST \
         /api/workflows/{kind}/publish with a body {{approval, spec_path, spec_toml}}); \
         nothing published",
        live.version, protection.why
    )))
}

/// Why an UNSIGNED publish of `kind` would be refused: the baseline row
/// it is judged on and the reason, in the judge's words.
#[derive(Debug, Clone, PartialEq)]
pub struct Protection<'a> {
    pub baseline: &'a WorkflowSpec,
    pub why: String,
}

/// THE UNSIGNED HALF OF [`judge`], on its own — what
/// `GET /api/workflows/{kind}/passkey` answers (backlog 5af64799), so a
/// client road can route a protected kind to its owner's passkey BEFORE
/// it creates a draft, by asking this rule rather than re-deriving it
/// (CLAUDE.md §9a). On 2026-11-18 publish-drift sent publish-to-github
/// — protected by presence on its approve step, named by no hold — down
/// the unattended road, and the draft it created was stranded by the
/// refusal [`judge`] gave the publish.
///
/// `Ok(None)` is a publish policy alone decides: a kind that never
/// published, or a baseline protected neither by its own presence step
/// nor through an edge. A baseline whose own step declares presence
/// needs no rules; any other needs them read, and an unread rule graph
/// is `CoverageUnavailable`, never "unprotected".
pub fn protection<'a>(
    kind: &str,
    baselines: &'a [WorkflowSpec],
    rules: Result<&[Value], &str>,
) -> Result<Option<Protection<'a>>, WorkflowError> {
    let Some(live) = baselines.iter().find(|s| s.kind == kind) else {
        // Never published: a new kind has nothing to protect.
        return Ok(None);
    };
    if let Some(slug) = presence_step(live) {
        return Ok(Some(Protection {
            baseline: live,
            why: format!("its step {slug} declares assurance_required = presence"),
        }));
    }
    let rules = rules.map_err(|e| {
        WorkflowError::CoverageUnavailable(format!(
            "whether live {kind} v{} admits or fulfils a passkey-protected protocol could not be \
             read ({e}), and an unread edge is not an absent one — nothing published",
            live.version
        ))
    })?;
    Ok(protected_kinds(baselines, &edges(rules, baselines))
        .remove(kind)
        .map(|why| Protection {
            baseline: live,
            why,
        }))
}

fn tostring(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// jq's `a // b`: `a` unless it is null, false or absent.
fn or<'a>(v: Option<&'a Value>, default: &'a Value) -> &'a Value {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => default,
        Some(x) => x,
    }
}

fn str_or(v: Option<&Value>, default: &str) -> String {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => default.to_string(),
        Some(x) => tostring(x),
    }
}

fn step_by_slug<'a>(packet: &'a Value, slug: &str) -> Option<&'a Value> {
    packet
        .get("steps")?
        .as_array()?
        .iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
}

/// What a signature judged good yields — the jq's `{by, at, live}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub by: String,
    pub at: String,
    pub live: String,
}

/// Is this publish-a-protocol packet a signature of exactly the publish
/// in front of the caller? A line-for-line port of
/// `infra/lib/signed-publish.jq`, over the packet as `GET /api/jobs/{id}`
/// answers it (the job's fields plus `steps`), refusing in the jq's own
/// words; `a_signature_is_judged_alike_in_jq_and_rust` (boss-jobs tests)
/// runs both over the same fixtures and holds them equal (CLAUDE.md §9a).
///
/// `shape` is the approve step's `step_shape_hash` as it stands now; the
/// server computes it with [`boss_core::job::step_shape_hash`], the
/// function `infra/lib/step-shape.sh` is pinned to.
#[allow(clippy::too_many_arguments)]
pub fn judge_signature(
    packet: &Value,
    shape: &str,
    approvers: &[String],
    verb_file: &str,
    registry: &str,
    name: &str,
    path: &str,
    sha: &str,
    step: &str,
) -> Result<Verdict, String> {
    let null = Value::Null;
    let empty_obj = Value::Object(Default::default());
    let id8: String = packet
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(8)
        .collect();
    let a = step_by_slug(packet, "approve");
    let p = step_by_slug(packet, step);
    let am = or(a.and_then(|a| a.get("metadata")), &empty_obj);
    let pm = packet.get("metadata").unwrap_or(&null);
    let reg = str_or(pm.get("registry"), "workflow");
    // `.row_name // .protocol_kind // ""` — a v1 packet names its row
    // only as `protocol_kind`.
    let row = str_or(
        Some(or(pm.get("row_name"), or(pm.get("protocol_kind"), &null))),
        "",
    );
    let arow = str_or(
        Some(or(am.get("row_name"), or(am.get("protocol_kind"), &null))),
        "",
    );
    let sign_offs: Vec<&Value> = a
        .and_then(|a| a.get("sign_offs"))
        .and_then(Value::as_array)
        .map(|v| v.iter().collect())
        .unwrap_or_default();
    let live_by: Vec<String> = sign_offs
        .iter()
        .filter(|s| s.get("voided_at").is_none_or(Value::is_null))
        .map(|s| str_or(s.get("authority_id"), "none"))
        .collect();
    let kind = packet.get("kind");
    if kind.and_then(Value::as_str) != Some(PUBLISH_A_PROTOCOL) {
        return Err(format!(
            "packet {id8} is a {}, not a publish-a-protocol",
            str_or(kind, "?")
        ));
    }
    if packet.get("status").and_then(Value::as_str) != Some("open") {
        return Err(format!(
            "packet {id8} is {}, not open",
            str_or(packet.get("status"), "?")
        ));
    }
    if reg != registry {
        return Err(format!(
            "packet {id8} asks for a {reg} row, not a {registry} row"
        ));
    }
    if row != name {
        let said = if row.is_empty() { "nothing" } else { &row };
        return Err(format!("packet {id8} asks to publish {said}, not {name}"));
    }
    let p_status = p.and_then(|p| p.get("status")).and_then(Value::as_str);
    if p.is_none() || matches!(p_status, Some("completed") | Some("skipped")) {
        return Err(format!(
            "its {step} step is {}: a signature is spent on one publish",
            str_or(p.and_then(|p| p.get("status")), "absent")
        ));
    }
    if a.and_then(|a| a.get("status")).and_then(Value::as_str) != Some("completed") {
        return Err(format!(
            "its approve step is {}, not completed: a ready publish step is a flag, not a signature",
            str_or(a.and_then(|a| a.get("status")), "absent")
        ));
    }
    if am.get("decision").and_then(Value::as_str) != Some("approved") {
        let d = or(am.get("decision"), &Value::String("none".into())).to_string();
        return Err(format!(
            "its approve step records decision {d}, not \"approved\""
        ));
    }
    let a = a.unwrap_or(&null);
    if a.get("assurance_required").and_then(Value::as_str) != Some("presence") {
        return Err(format!(
            "its approve step declares assurance {}, not presence, so completing it proves no passkey",
            str_or(a.get("assurance_required"), "none")
        ));
    }
    let required: Vec<String> = a
        .get("sign_offs_required")
        .and_then(Value::as_array)
        .map(|r| r.iter().map(tostring).collect())
        .unwrap_or_default();
    if required.is_empty() {
        return Err(
            "its approve step requires no sign-off, so nothing on it records who approved".into(),
        );
    }
    let strangers: BTreeSet<&String> = live_by.iter().filter(|b| !approvers.contains(b)).collect();
    if !strangers.is_empty() {
        return Err(format!(
            "its approve step carries a live sign-off by {}, who is not among the approvers \
             {verb_file} names ({})",
            strangers
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
            approvers.join(", ")
        ));
    }
    if str_or(am.get("registry"), "workflow") != registry {
        return Err(format!(
            "its approve step signs a {} row, not a {registry} row",
            am.get("registry")
                .map(tostring)
                .unwrap_or_else(|| "null".into())
        ));
    }
    let signed_path = str_or(am.get("spec_path"), "");
    if arow != name || signed_path != path {
        let said = if arow.is_empty() { "none" } else { &arow };
        return Err(format!(
            "its approve step signs {said} from {}, not {name} from {path}",
            str_or(am.get("spec_path"), "none")
        ));
    }
    if str_or(am.get("spec_sha256"), "") != sha {
        return Err(format!(
            "its approve step signs {path} at sha256 {}, and the bytes here hash to {sha}: these \
             are not the bytes that were signed",
            str_or(am.get("spec_sha256"), "none")
        ));
    }
    let live_version = str_or(am.get("live_version"), "");
    if live_version.is_empty() || !live_version.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!(
            "its approve step signs no live version ({}), so the diff it signed was against nothing",
            str_or(am.get("live_version"), "none")
        ));
    }
    let mut errs = Vec::new();
    let mut by = BTreeSet::new();
    let mut at = Vec::new();
    for r in &required {
        let mine: Vec<&&Value> = sign_offs
            .iter()
            .filter(|s| s.get("role").map(tostring).as_deref() == Some(r.as_str()))
            .collect();
        let Some(last) = mine.last() else {
            errs.push(format!("no {r} sign-off is stamped on its approve step"));
            continue;
        };
        let nonce_ok = last
            .get("presence_nonce")
            .and_then(Value::as_str)
            .is_some_and(|n| !n.is_empty());
        let authority = str_or(last.get("authority_id"), "nobody");
        if last.get("assurance").and_then(Value::as_str) != Some("presence") || !nonce_ok {
            errs.push(format!(
                "the newest {r} sign-off is {}-assured, not presence: no passkey signed it",
                str_or(last.get("assurance"), "session")
            ));
        } else if !approvers
            .iter()
            .any(|x| Some(x.as_str()) == last.get("authority_id").and_then(Value::as_str))
        {
            errs.push(format!(
                "the newest {r} sign-off is by {authority}, who is not among the approvers \
                 {verb_file} names"
            ));
        } else if last.get("shape_hash").and_then(Value::as_str) != Some(shape) {
            errs.push(format!(
                "the newest {r} sign-off is bound to shape {}, and the approve step now hashes to \
                 {shape}: what is on the step is not what was signed",
                str_or(last.get("shape_hash"), "none")
            ));
        } else if last.get("voided_at").is_some_and(|v| !v.is_null()) {
            errs.push(format!(
                "the newest {r} sign-off was voided at {}",
                last.get("voided_at").map(tostring).unwrap_or_default()
            ));
        } else {
            by.insert(authority);
            at.push(str_or(last.get("stamped_at"), ""));
        }
    }
    if !errs.is_empty() {
        return Err(errs.join("; "));
    }
    Ok(Verdict {
        by: by.into_iter().collect::<Vec<_>>().join(", "),
        at: at.into_iter().max().unwrap_or_default(),
        live: live_version,
    })
}
