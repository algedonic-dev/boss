//! Registry-declared argument discovery stays a native read request. Its
//! complete byte receipt may propose explicit arguments, never authority.
//! Both stages re-read the final mutation's existing admission guards.
//!
//! WHEN NO GROWTH FITS (design ada8f698, option A — David, 2026-10-07) the
//! native read may answer with a different proposal: the one replica move
//! after which the volume can grow, as the declared `no_growth_plan_verb`'s
//! own plan. From that this files ONLY the read-only plan request and a
//! note on the finding's open alarm — never the passkey-gated move, which
//! stays a person's to file. A native refusal (two short disks, a
//! destination that does not admit, an unhealthy volume, an unread
//! cluster) is exit 78 or 1: it proposes nothing, and stays this
//! handler's loud permanent error beside the alarm `estate.alarm` files.

use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use rsa::sha2::{Digest, Sha256};
use serde_json::{Value, json};

use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};

use super::common::{api_client, get_json, post_json, write_json};
use super::estate_alarm::HARD_CLASSES;
use super::ops_file_remedies::{
    OpsFileRemedies, Remedy, carries, cleared_since, declared_args, remedy_index,
    remedy_request_id, verb_files,
};

#[derive(Clone, Debug)]
struct Discovery {
    mutation: String,
    host: String,
    class: String,
    scope: String,
    verb: String,
    fields: Vec<String>,
    inputs: Vec<Value>,
    outputs: Vec<Value>,
    plan_verb: String,
    no_growth: Option<NoGrowth>,
}

/// What the discovery may propose INSTEAD when no growth fits (design
/// ada8f698, option A): one read-only plan verb, named by the registry
/// beside the discovery (`no_growth_plan_verb`), which exactly one
/// passkey-gated verb names as its own plan. This handler files only the
/// plan request; `mutation` is recorded so the reader knows which request
/// is theirs to file.
#[derive(Clone, Debug)]
struct NoGrowth {
    plan_verb: String,
    outputs: Vec<Value>,
    mutation: String,
}

fn fault(why: impl Into<String>) -> HandlerError {
    HandlerError::Permanent(format!("ops.discover_remedies: {}", why.into()))
}

fn index(files: &[(&str, &str)]) -> Result<Vec<Discovery>, HandlerError> {
    let rows: Vec<(&str, Value)> = files
        .iter()
        .map(|(name, text)| {
            serde_json::from_str(text)
                .map(|value| (*name, value))
                .map_err(|e| fault(format!("{name}.json: {e}")))
        })
        .collect::<Result<_, _>>()?;
    let mut result = Vec::new();
    for (name, spec) in &rows {
        let Some(entries) = spec.get("discovery_remedies") else {
            continue;
        };
        let entries = entries
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| {
                fault(format!(
                    "{name}.json discovery_remedies must be a nonempty list"
                ))
            })?;
        let mut classes = BTreeSet::new();
        for entry in entries {
            let object = entry
                .as_object()
                .filter(|o| o.len() == 4 || (o.len() == 5 && o.contains_key("no_growth_plan_verb")))
                .ok_or_else(|| {
                    fault(format!(
                        "{name}.json discovery entry must have exactly finding_class, scope, \
                         discovery_verb, arg_fields and, optionally, no_growth_plan_verb"
                    ))
                })?;
            let scope = object
                .get("scope")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| fault(format!("{name}.json discovery has no observation scope")))?;
            let class = object
                .get("finding_class")
                .and_then(Value::as_str)
                .filter(|c| HARD_CLASSES.iter().any(|(_, prefix)| prefix == c))
                .ok_or_else(|| {
                    fault(format!(
                        "{name}.json discovery finding_class is not a hard finding"
                    ))
                })?;
            if !classes.insert(class) {
                return Err(fault(format!(
                    "{name}.json has ambiguous discovery for {class}"
                )));
            }
            let discovery_verb = object
                .get("discovery_verb")
                .and_then(Value::as_str)
                .ok_or_else(|| fault(format!("{name}.json discovery_verb is not a name")))?;
            let discovery = rows
                .iter()
                .find(|(n, _)| *n == discovery_verb)
                .map(|(_, s)| s)
                .ok_or_else(|| {
                    fault(format!(
                        "{name}.json discovery verb {discovery_verb} is not in the registry"
                    ))
                })?;
            let fields = object
                .get("arg_fields")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
                .ok_or_else(|| fault(format!("{name}.json arg_fields is not a nonempty list")))?
                .iter()
                .map(|v| {
                    v.as_str()
                        .filter(|s| {
                            !s.is_empty()
                                && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                        })
                        .map(str::to_string)
                        .ok_or_else(|| {
                            fault(format!(
                                "{name}.json arg_fields must name direct finding fields"
                            ))
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if fields.iter().collect::<BTreeSet<_>>().len() != fields.len() {
                return Err(fault(format!("{name}.json has duplicate arg_fields")));
            }
            let hosts = spec
                .get("hosts")
                .and_then(Value::as_array)
                .filter(|a| a.len() == 1)
                .ok_or_else(|| fault(format!("{name}.json needs one host")))?;
            let host = hosts[0]
                .as_str()
                .ok_or_else(|| fault(format!("{name}.json host is not a string")))?;
            if discovery.get("hosts") != spec.get("hosts")
                || !discovery
                    .get("about")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.starts_with("READ-ONLY"))
                || discovery
                    .get("requires_approval")
                    .is_some_and(|v| v != &json!(false))
                || discovery.get("capture") != Some(&json!("separate-streams"))
            {
                return Err(fault(format!(
                    "{name}.json discovery must be same-host, read-only and capture separate-streams"
                )));
            }
            let inputs = discovery
                .get("params")
                .and_then(Value::as_array)
                .filter(|a| a.len() == fields.len())
                .ok_or_else(|| {
                    fault(format!(
                        "{name}.json arg_fields must supply every discovery param"
                    ))
                })?
                .clone();
            let params = spec
                .get("params")
                .and_then(Value::as_array)
                .filter(|a| a.len() > fields.len())
                .ok_or_else(|| {
                    fault(format!(
                        "{name}.json must resolve an argument before signed hash"
                    ))
                })?;
            if params[..fields.len()] != inputs {
                return Err(fault(format!(
                    "{name}.json discovery target params must equal the mutation's initial params"
                )));
            }
            // Run the SAME approval validator on a synthetic finding whose
            // args are supplied at runtime. Empty declarations cannot waive it.
            let mut approval = spec.clone();
            approval["remedies"] = json!([format!("{class}:validation")]);
            let mut approval_params = params.last().cloned().into_iter().collect::<Vec<_>>();
            approval["params"] = json!(std::mem::take(&mut approval_params));
            remedy_index(&[(name, &approval.to_string())]).map_err(fault)?;
            let plan_verb = spec["plan_verb"]
                .as_str()
                .ok_or_else(|| fault("approval has no plan verb"))?;
            let plan = rows
                .iter()
                .find(|(n, _)| *n == plan_verb)
                .map(|(_, s)| s)
                .ok_or_else(|| fault(format!("{name}.json plan verb {plan_verb} is absent")))?;
            if plan.get("hosts") != spec.get("hosts")
                || plan
                    .get("requires_approval")
                    .is_some_and(|v| v != &json!(false))
                || plan
                    .get("params")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    != Some(&params[..params.len() - 1])
            {
                return Err(fault(format!(
                    "{name}.json plan must take the mutation's exact unsigned params on the same host"
                )));
            }
            // THE NO-GROWTH FALLBACK (design ada8f698, option A). What it
            // names must be a plan and nothing more: a read-only verb on
            // the same host, which is not this mutation's own plan, and
            // which exactly ONE passkey-gated verb with named approvers
            // owns as its plan, taking that plan's words and then the
            // signed hash. So a proposal filed from it can lead only to a
            // request a person files and a passkey signs.
            let no_growth = match object.get("no_growth_plan_verb") {
                None => None,
                Some(value) => {
                    let fallback = value.as_str().filter(|v| *v != plan_verb).ok_or_else(|| {
                        fault(format!(
                            "{name}.json no_growth_plan_verb must name a plan verb other than its own"
                        ))
                    })?;
                    let fallback_plan = rows
                        .iter()
                        .find(|(n, _)| *n == fallback)
                        .map(|(_, s)| s)
                        .ok_or_else(|| {
                        fault(format!(
                            "{name}.json no_growth_plan_verb {fallback} is not in the registry"
                        ))
                    })?;
                    let outputs = fallback_plan
                        .get("params")
                        .and_then(Value::as_array)
                        .filter(|a| !a.is_empty())
                        .ok_or_else(|| {
                            fault(format!(
                                "{fallback}.json declares no words a proposal can fill"
                            ))
                        })?
                        .clone();
                    if fallback_plan.get("hosts") != spec.get("hosts")
                        || !fallback_plan
                            .get("about")
                            .and_then(Value::as_str)
                            .is_some_and(|s| s.starts_with("READ-ONLY"))
                        || fallback_plan
                            .get("requires_approval")
                            .is_some_and(|v| v != &json!(false))
                    {
                        return Err(fault(format!(
                            "{name}.json no_growth_plan_verb {fallback} must be a same-host read-only plan"
                        )));
                    }
                    let owners: Vec<&str> = rows
                        .iter()
                        .filter(|(_, s)| {
                            s.get("plan_verb").and_then(Value::as_str) == Some(fallback)
                        })
                        .map(|(n, _)| *n)
                        .collect();
                    let [owner] = owners.as_slice() else {
                        return Err(fault(format!(
                            "{name}.json no_growth_plan_verb {fallback} must be the plan of exactly one verb, found {owners:?}"
                        )));
                    };
                    let owner_spec = rows
                        .iter()
                        .find(|(n, _)| n == owner)
                        .map(|(_, s)| s)
                        .ok_or_else(|| fault("the owning verb left the registry"))?;
                    let owner_params = owner_spec
                        .get("params")
                        .and_then(Value::as_array)
                        .ok_or_else(|| fault(format!("{owner}.json has no params")))?;
                    if owner_spec.get("requires_approval") != Some(&json!(true))
                        || owner_spec.get("hosts") != spec.get("hosts")
                        || !owner_spec
                            .get("approvers")
                            .and_then(Value::as_array)
                            .is_some_and(|a| {
                                !a.is_empty()
                                    && a.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                            })
                        || owner_params.len() != outputs.len() + 1
                        || owner_params[..outputs.len()] != outputs[..]
                        || owner_params.last().and_then(|p| p.get("name"))
                            != Some(&json!("plan_sha256"))
                    {
                        return Err(fault(format!(
                            "{name}.json no_growth_plan_verb {fallback} must be the plan of a same-host \
                             passkey-gated verb with named approvers that takes its words and then the signed hash"
                        )));
                    }
                    Some(NoGrowth {
                        plan_verb: fallback.into(),
                        outputs,
                        mutation: (*owner).into(),
                    })
                }
            };
            result.push(Discovery {
                mutation: (*name).into(),
                host: host.into(),
                class: class.into(),
                scope: scope.into(),
                verb: discovery_verb.into(),
                fields,
                inputs,
                outputs: params[..params.len() - 1].to_vec(),
                plan_verb: plan_verb.into(),
                no_growth,
            });
        }
    }
    Ok(result)
}

fn target(declaration: &Discovery, finding: &str, args: Vec<String>) -> Remedy {
    Remedy {
        verb: declaration.mutation.clone(),
        host: declaration.host.clone(),
        findings: vec![finding.into()],
        args,
        target: Some(finding.into()),
    }
}

pub(crate) async fn discover_for_comparison(
    handler: &OpsFileRemedies,
    files: &[(&str, &str)],
    hours: i64,
    now: DateTime<Utc>,
    ctx: &InvocationContext,
) -> Result<(), HandlerError> {
    let mut targets = Vec::new();
    for declaration in index(files)? {
        if ctx.event_payload["scope"] != declaration.scope {
            continue;
        }
        let Some(rows) = ctx
            .event_payload
            .pointer(&format!("/findings/{}", declaration.class))
        else {
            continue;
        };
        let rows = rows
            .as_array()
            .ok_or_else(|| fault("finding population is not an array"))?;
        let mut seen = BTreeSet::new();
        for row in rows {
            let id = row["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| fault("discovery finding has no identity"))?;
            if !seen.insert(id) {
                return Err(fault("duplicate discovery finding identities"));
            }
            let finding = format!("{}:{id}", declaration.class);
            let values: Vec<Value> = declaration
                .fields
                .iter()
                .map(|field| row[field].clone())
                .collect();
            let inputs = declared_args(&json!(values), &declaration.inputs).map_err(fault)?;
            if id != inputs.join("/") {
                return Err(fault(
                    "finding identity does not conserve its declared target fields",
                ));
            }
            targets.push((declaration.clone(), finding, inputs));
        }
    }
    // Validate the COMPLETE finding population before any side effect. A
    // duplicate or malformed tail cannot leave a partially admitted answer.
    for (declaration, finding, inputs) in targets {
        // The final target has no inferred size. Its subject and guards
        // depend only on the finding, not arguments which discovery may change.
        let final_target = target(&declaration, &finding, vec![]);
        let Some((final_body, _)) = handler
            .prepare_one(&final_target, &final_target.findings, hours, now, ctx)
            .await?
        else {
            continue;
        };
        let final_id = final_body["id"]
            .as_str()
            .ok_or_else(|| fault("admission has no final identity"))?;
        let mut read_target = final_target.clone();
        read_target.verb = declaration.verb.clone();
        read_target.args = inputs.clone();
        // Each allowed final successor has ONE discovery episode. A replay
        // from an older episode cannot become a fresh successor later.
        read_target.target = Some(format!("{finding}:successor:{final_id}"));
        let Some((mut body, after)) = handler
            .prepare_one(&read_target, &read_target.findings, hours, now, ctx)
            .await?
        else {
            continue;
        };
        body["metadata"]
            .as_object_mut()
            .ok_or_else(|| fault("request metadata is not an object"))?
            .remove("requires_approval");
        body["metadata"]["discovery_remedy"] = json!({
            "mutation": declaration.mutation, "finding": finding, "inputs": inputs,
            "final_request_id": final_id, "refile_after_hours": hours,
            "discovery_subject": read_target.subject(),
            "discovery_after": after,
            "comparison_event": ctx.triggering_event_id,
            "comparison_at": now.to_rfc3339(),
        });
        post_json(
            &handler.client,
            &format!("{}/api/jobs", handler.base()),
            &body,
            &ctx.rule_name,
        )
        .await?;
    }
    Ok(())
}

// Parse the owning native proposal as a record, not a JSON map. Serde's
// derived record deserializer refuses repeated fields (also escaped-equivalent
// names) before a later value can erase an earlier one. Unknown fields and
// omitted fields refuse too; the raw conserved stdout remains the evidence.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeProposal {
    verb: String,
    args: Vec<String>,
    plan: String,
    plan_sha256: String,
    /// Only a no-growth proposal carries it, and must: its `args` are a
    /// volume and a replica, so the claim discovery was asked about is
    /// conserved here instead of in the first words.
    #[serde(default)]
    target: Option<Vec<String>>,
}

/// The no-growth fallback this proposal answers under, if it names that
/// fallback's plan verb.
fn no_growth_of<'a>(declaration: &'a Discovery, proposal: &Value) -> Option<&'a NoGrowth> {
    declaration
        .no_growth
        .as_ref()
        .filter(|n| proposal["verb"] == n.plan_verb.as_str())
}

fn decoded_proposal(
    step: &Value,
    declaration: &Discovery,
    inputs: &[String],
) -> Result<Value, HandlerError> {
    let md = &step["metadata"];
    if step["status"] != "completed"
        || md["disposition"] != "answered"
        || md["exit_code"] != "0"
        || md["runner_host"] != declaration.host
        || !md["streams_unread"].is_null()
    {
        return Err(fault(
            "discovery is not a successful completed same-host answer",
        ));
    }
    let stream = &md["streams"]["stdout"];
    let encoded = stream["base64"]
        .as_str()
        .ok_or_else(|| fault("discovery has no stdout byte record"))?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|e| fault(format!("stdout base64: {e}")))?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    if stream["complete"] != true
        || stream["bytes"].as_u64() != Some(bytes.len() as u64)
        || stream["retained_bytes"].as_u64() != Some(bytes.len() as u64)
        || stream["sha256"] != hash
    {
        return Err(fault(
            "discovery stdout is incomplete or contradicts its byte receipt",
        ));
    }
    let native: NativeProposal = serde_json::from_slice(&bytes).map_err(|e| {
        fault(format!(
            "stdout is not one unambiguous complete JSON proposal: {e}"
        ))
    })?;
    let mut proposal = json!({
        "verb": native.verb, "args": native.args,
        "plan": native.plan, "plan_sha256": native.plan_sha256,
    });
    match (no_growth_of(declaration, &proposal), native.target) {
        // The explicit-size proposal: its first words ARE the claim.
        (None, None) => {
            if proposal["verb"] != declaration.plan_verb.as_str() {
                return Err(fault(
                    "proposal plan verb differs from its registry declaration",
                ));
            }
            let args = declared_args(&proposal["args"], &declaration.outputs).map_err(fault)?;
            if !args.starts_with(inputs) {
                return Err(fault("proposal changes the discovery target"));
            }
        }
        (None, Some(_)) => {
            return Err(fault(
                "only a no-growth proposal carries a separate target; this one names another verb",
            ));
        }
        // The no-growth proposal: the declared fallback plan's own words,
        // and the claim discovery was asked about, conserved exactly.
        (Some(fallback), Some(target)) => {
            declared_args(&proposal["args"], &fallback.outputs).map_err(fault)?;
            if target != inputs {
                return Err(fault("proposal changes the discovery target"));
            }
            proposal["target"] = json!(target);
        }
        (Some(_), None) => {
            return Err(fault(
                "a no-growth proposal does not say which claim it answers",
            ));
        }
    }
    let plan = proposal["plan"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| fault("proposal plan is absent"))?;
    if proposal["plan_sha256"] != format!("{:x}", Sha256::digest(plan.as_bytes())) {
        return Err(fault("proposal plan bytes do not equal its hash"));
    }
    Ok(proposal)
}

/// The native reader orders this scoped series newest first. Read its two
/// newest rows to refuse tied/contradictory maxima, rather than treating a
/// capped page as the complete history. Missing coverage remains unknown.
fn finding_is_current(
    page: &Value,
    declaration: &Discovery,
    finding: &str,
    since: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<bool, HandlerError> {
    let total = page["total"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or_else(|| fault("current comparison population is unavailable"))?;
    let rows = page["data"]
        .as_array()
        .filter(|rows| rows.len() as u64 == total.min(2))
        .ok_or_else(|| fault("current comparison latest-page coverage differs from its total"))?;
    let mut previous = None;
    let mut seen = BTreeSet::new();
    for row in rows {
        let id = row["event_id"]
            .as_str()
            .filter(|id| uuid::Uuid::parse_str(id).is_ok())
            .ok_or_else(|| fault("current comparison has no native identity"))?;
        let at = row["timestamp"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|s| s.with_timezone(&Utc))
            .ok_or_else(|| fault("current comparison timestamp is unavailable"))?;
        if !seen.insert(id)
            || row["kind"] != "jobs.estate.compared"
            || row["source"] != "jobs"
            || row["payload"]["scope"] != declaration.scope
            || at > now
            || previous.is_some_and(|p| at >= p)
        {
            return Err(fault(
                "current comparisons have ambiguous order, identity, scope or chronology",
            ));
        }
        previous = Some(at);
    }
    let latest = &rows[0];
    let at = DateTime::parse_from_rfc3339(
        latest["timestamp"]
            .as_str()
            .ok_or_else(|| fault("latest comparison has no timestamp"))?,
    )
    .map_err(|_| fault("latest timestamp cannot be parsed"))?
    .with_timezone(&Utc);
    if at < since {
        return Err(fault(
            "current comparison predates the source discovery episode",
        ));
    }
    let findings = latest["payload"]["findings"][&declaration.class]
        .as_array()
        .ok_or_else(|| fault("latest comparison does not measure the declared finding class"))?;
    let mut identities = BTreeSet::new();
    let mut matched = false;
    for row in findings {
        let id = row["id"]
            .as_str()
            .ok_or_else(|| fault("latest finding has no identity"))?;
        let values: Vec<Value> = declaration
            .fields
            .iter()
            .map(|field| row[field].clone())
            .collect();
        let args = declared_args(&json!(values), &declaration.inputs).map_err(fault)?;
        if !identities.insert(id) || id != args.join("/") {
            return Err(fault("latest finding target population is ambiguous"));
        }
        matched |= finding == format!("{}:{id}", declaration.class);
    }
    Ok(matched)
}

pub struct OpsDiscoveredRemedies {
    handler: Arc<OpsFileRemedies>,
}

impl OpsDiscoveredRemedies {
    pub fn new(base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            handler: OpsFileRemedies::with_client(api_client(), base),
        })
    }

    async fn complete(
        &self,
        files: &[(&str, &str)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let job_id = ctx.event_payload["job_id"]
            .as_str()
            .ok_or_else(|| fault("completion has no job_id"))?;
        uuid::Uuid::parse_str(job_id)
            .map_err(|_| fault("completion job_id is not a native UUID"))?;
        let job = get_json(
            &self.handler.client,
            &format!("{}/api/jobs/{job_id}", self.handler.base()),
            &ctx.rule_name,
        )
        .await?;
        if job["id"] != job_id || job["kind"] != "ops-request" {
            return Err(fault("completion packet identity differs"));
        }
        let Some(link) = job.pointer("/metadata/discovery_remedy") else {
            return Ok(());
        };
        let declarations = index(files)?;
        let declaration = declarations
            .iter()
            .find(|d| job["metadata"]["verb"] == d.verb && link["mutation"] == d.mutation)
            .ok_or_else(|| fault("discovery completion has no current registry relationship"))?;
        if job["metadata"]["host"] != declaration.host
            || job["metadata"]["requires_approval"] == true
        {
            return Err(fault(
                "discovery packet is not the declared read-only host request",
            ));
        }
        let finding = link["finding"]
            .as_str()
            .filter(|f| f.starts_with(&format!("{}:", declaration.class)))
            .ok_or_else(|| fault("discovery finding class differs"))?;
        let inputs = declared_args(&link["inputs"], &declaration.inputs).map_err(fault)?;
        if finding != format!("{}:{}", declaration.class, inputs.join("/")) {
            return Err(fault(
                "discovery finding does not conserve its native target inputs",
            ));
        }
        if job["metadata"]["args"] != json!(inputs) {
            return Err(fault("discovery input provenance differs"));
        }
        let step_id = ctx.event_payload["step_id"]
            .as_str()
            .ok_or_else(|| fault("completion has no step_id"))?;
        let steps = job["steps"]
            .as_array()
            .ok_or_else(|| fault("discovery steps are unavailable"))?;
        let matching: Vec<_> = steps
            .iter()
            .filter(|s| s["spec_slug"] == "execute" && s["id"] == step_id)
            .collect();
        let [step] = matching.as_slice() else {
            return Err(fault("completion is not exactly the discovery execute"));
        };
        let proposal = decoded_proposal(step, declaration, &inputs)?;
        let since = link["comparison_at"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|s| s.with_timezone(&Utc))
            .ok_or_else(|| fault("discovery source comparison time is unavailable"))?;
        let now = ctx.firing_instant()?;
        let mut url =
            reqwest::Url::parse(&format!("{}/api/estate/comparisons", self.handler.base()))
                .map_err(|e| fault(format!("current comparison URL: {e}")))?;
        url.query_pairs_mut()
            .append_pair("scope", &declaration.scope)
            .append_pair("limit", "2");
        let latest = get_json(&self.handler.client, url.as_str(), &ctx.rule_name).await?;
        if !finding_is_current(&latest, declaration, finding, since, now)? {
            tracing::info!(
                discovery = job_id,
                finding,
                "discovery retained: current comparison no longer finds this remedy necessary"
            );
            return Ok(());
        }
        let alarms = self
            .handler
            .alarms_since(since, now, &ctx.rule_name)
            .await?;
        if cleared_since(&alarms, &[finding.to_string()], since).is_some() {
            return Err(fault(
                "discovery belongs to an episode the recorded alarm has since cleared",
            ));
        }
        let hours = link["refile_after_hours"]
            .as_i64()
            .filter(|n| *n > 0 && chrono::Duration::try_hours(*n).is_some())
            .ok_or_else(|| fault("discovery window is invalid"))?;
        // A no-growth proposal has no size to give the expansion: the
        // final request's guards are read on the finding alone, exactly as
        // the comparison read them before it asked.
        let no_growth = no_growth_of(declaration, &proposal);
        let final_args = match no_growth {
            Some(_) => vec![],
            None => declared_args(&proposal["args"], &declaration.outputs).map_err(fault)?,
        };
        let final_target = target(declaration, finding, final_args);
        let Some((mut body, _)) = self
            .handler
            .prepare_one(
                &final_target,
                &final_target.findings,
                hours,
                ctx.firing_instant()?,
                ctx,
            )
            .await?
        else {
            return Ok(());
        };
        if body["id"] != link["final_request_id"] {
            return Err(fault(
                "discovery belongs to an obsolete final request episode",
            ));
        }
        let mut read_target = final_target.clone();
        read_target.verb = declaration.verb.clone();
        let final_id = body["id"]
            .as_str()
            .ok_or_else(|| fault("final request has no ID"))?;
        read_target.target = Some(format!("{finding}:successor:{final_id}"));
        let after = match link.get("discovery_after") {
            Some(Value::Null) => None,
            Some(Value::String(id)) if uuid::Uuid::parse_str(id).is_ok() => Some(id.as_str()),
            _ => {
                return Err(fault(
                    "discovery predecessor is not a native optional identity",
                ));
            }
        };
        if link["discovery_subject"] != read_target.subject()
            || job["subject"]["id"] != read_target.subject()
            || job["id"] != remedy_request_id(&read_target.subject(), after)
        {
            return Err(fault(
                "discovery episode identity is not its deterministic admission",
            ));
        }
        if let Some(fallback) = no_growth {
            // NO GROWTH, ONE DECISIVE MOVE (design ada8f698, option A).
            // The expansion request is NOT filed — there is no size to
            // sign — and neither is the move: what is filed is the
            // read-only plan the native read proposed, once per final
            // episode, so the plan a person would sign is on the record
            // beside the evidence that named it. Filing the passkey-gated
            // move stays a person's act.
            let plan_target = Remedy {
                verb: fallback.plan_verb.clone(),
                host: declaration.host.clone(),
                findings: vec![finding.into()],
                args: declared_args(&proposal["args"], &fallback.outputs).map_err(fault)?,
                target: Some(format!("{finding}:no-growth-plan:{final_id}")),
            };
            let Some((mut plan_body, _)) = self
                .handler
                .prepare_one(&plan_target, &plan_target.findings, hours, now, ctx)
                .await?
            else {
                return Ok(());
            };
            let metadata = plan_body["metadata"]
                .as_object_mut()
                .ok_or_else(|| fault("request metadata is not an object"))?;
            metadata.remove("requires_approval");
            metadata.insert(
                "discovered_proposal".into(),
                json!({
                    "request": job_id, "step": step_id,
                    "stdout": step["metadata"]["streams"]["stdout"],
                    "comparison_event": link["comparison_event"], "proposal": proposal,
                    "proposes": fallback.mutation,
                }),
            );
            post_json(
                &self.handler.client,
                &format!("{}/api/jobs", self.handler.base()),
                &plan_body,
                &ctx.rule_name,
            )
            .await?;
            // SAID ON THE ALARM, where the person deciding reads: which
            // request holds the plan, and which verb is theirs to file.
            // Deterministic, so a redelivery writes nothing new; and NOT
            // FATAL, as the held note is not (backlog e5ff616f) — the plan
            // request stands whether or not this lands.
            let key = format!("remedy_proposed:{}", plan_target.subject());
            let note = json!({
                "verb": fallback.mutation, "plan_verb": fallback.plan_verb,
                "host": declaration.host, "args": plan_target.args,
                "plan_request": plan_body["id"], "plan_sha256": proposal["plan_sha256"],
                "discovery": job_id,
                "note": format!(
                    "No growth fits this claim, and the native read found exactly one replica move \
                     after which every remaining replica disk admits growth. Its plan is rendered \
                     read-only by request {}; nothing was moved. To act on it, file {} on {} with \
                     these words — it runs only on the plan a passkey signs.",
                    plan_body["id"].as_str().unwrap_or("unknown"),
                    fallback.mutation,
                    declaration.host
                ),
            });
            for alarm in alarms.iter().filter(|a| {
                a["status"] == "open"
                    && carries(a, &plan_target.findings)
                    && a["metadata"].get(&key) != Some(&note)
            }) {
                let Some(id) = alarm["id"].as_str() else {
                    continue;
                };
                if let Err(e) = write_json(
                    &self.handler.client,
                    reqwest::Method::PATCH,
                    &format!("{}/api/jobs/{id}/metadata", self.handler.base()),
                    &json!({ key.clone(): note }),
                    &ctx.rule_name,
                )
                .await
                {
                    tracing::warn!(
                        alarm = %id,
                        error = %e,
                        "ops.discover_remedies: the proposed-move note on the alarm was not written — the plan request stands"
                    );
                }
            }
            return Ok(());
        }
        body["metadata"]["discovered_proposal"] = json!({
            "request": job_id, "step": step_id, "stdout": step["metadata"]["streams"]["stdout"],
            "comparison_event": link["comparison_event"], "proposal": proposal,
        });
        post_json(
            &self.handler.client,
            &format!("{}/api/jobs", self.handler.base()),
            &body,
            &ctx.rule_name,
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl Handler for OpsDiscoveredRemedies {
    fn name(&self) -> &'static str {
        "ops.file_discovered_remedies"
    }
    async fn invoke(
        &self,
        args: &[(String, boss_dispatcher::rules::expr::Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        if !args.is_empty() {
            return Err(fault(
                "completion takes no rule-supplied authority or arguments",
            ));
        }
        self.complete(verb_files(), ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Path, Query},
        routing::get,
    };
    use std::{collections::BTreeMap, sync::Mutex};

    const STEP: &str = "8120b0f1-0000-4000-8000-000000000001";
    type Rows = Arc<Mutex<BTreeMap<String, Value>>>;

    fn instant() -> DateTime<Utc> {
        "2026-10-03T12:00:00Z".parse().unwrap()
    }
    fn context(payload: Value) -> InvocationContext {
        InvocationContext {
            event_timestamp: Some(instant()),
            rule_name: "discovery-test".into(),
            triggering_event_id: "8120b0f1-0000-4000-8000-000000000002".into(),
            triggering_topic: "jobs.estate.compared".into(),
            event_payload: payload,
        }
    }
    fn comparison() -> Value {
        json!({"scope":"instance-volumes","findings":{"disk_tight":[{
            "id":"boss/pgdata-postgres-0","namespace":"boss","claim":"pgdata-postgres-0"
        }]}})
    }
    fn declaration() -> Discovery {
        index(verb_files())
            .unwrap()
            .into_iter()
            .find(|d| d.mutation == "expand-instance-volume")
            .unwrap()
    }
    fn proposal() -> Value {
        let plan = "A fixture plan, not a live capacity claim.\n";
        json!({"verb":"plan-an-instance-volume-expansion","args":["boss","pgdata-postgres-0","30Gi"],
            "plan":plan,"plan_sha256":format!("{:x}",Sha256::digest(plan.as_bytes()))})
    }
    fn stream(bytes: &[u8]) -> Value {
        json!({"base64":STANDARD.encode(bytes),"bytes":bytes.len(),"retained_bytes":bytes.len(),
            "sha256":format!("{:x}",Sha256::digest(bytes)),"complete":true})
    }
    fn execute(value: &Value) -> Value {
        json!({"id":STEP,"spec_slug":"execute","status":"completed", "metadata":{
            "disposition":"answered","exit_code":"0","runner_host":"forge",
            "streams":{"stdout":stream(&serde_json::to_vec(value).unwrap()),"stderr":stream(b"")}
        }})
    }
    async fn port() -> (String, Rows) {
        port_with_comparison(comparison()).await
    }
    async fn port_with_comparison(latest: Value) -> (String, Rows) {
        let rows: Rows = Arc::new(Mutex::new(BTreeMap::new()));
        let reads = rows.clone();
        let writes = rows.clone();
        let objects = rows.clone();
        let notes = rows.clone();
        let app = Router::new().route("/api/jobs",get(move |Query(query):Query<BTreeMap<String,String>>| {
            let rows = reads.clone(); async move {
                let values: Vec<Value> = rows.lock().unwrap().values().filter(|row|
                    query.get("subject_id").is_none_or(|s| row["subject"]["id"] == *s)
                    && query.get("kind").is_none_or(|s| row["kind"] == *s)).cloned().collect();
                Json(json!({"total":values.len(),"data":values}))
            }
        }).post(move |Json(mut body):Json<Value>| {
            let rows = writes.clone(); async move {
                let id = body["id"].as_str().unwrap().to_string();
                body["opened_at"] = json!(instant().to_rfc3339());
                rows.lock().unwrap().entry(id.clone()).or_insert(body);
                Json(json!({"id":id}))
            }
        })).route("/api/estate/comparisons",get(move || {
            let latest=latest.clone(); async move {Json(json!({"total":1,"data":[{
                "event_id":"8120b0f1-0000-4000-8000-000000000002","kind":"jobs.estate.compared","source":"jobs",
                "timestamp":instant().to_rfc3339(),"payload":latest
            }]}))}
        })).route("/api/jobs/{id}",get(move |Path(id):Path<String>| {
            let rows = objects.clone(); async move { Json(rows.lock().unwrap().get(&id).unwrap().clone()) }
        })).route("/api/jobs/{id}/metadata",axum::routing::patch(move |Path(id):Path<String>, Json(body):Json<Value>| {
            // The jobs API's merge door: top-level keys merged into metadata.
            let rows = notes.clone(); async move {
                let mut rows = rows.lock().unwrap();
                let row = rows.get_mut(&id).unwrap();
                if row["refuses_notes"] == true {
                    return Err(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
                }
                for (key, value) in body.as_object().unwrap() {
                    row["metadata"][key] = value.clone();
                }
                Ok(Json(json!({"id":id})))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, rows)
    }
    async fn requested() -> (Arc<OpsFileRemedies>, Rows, String, InvocationContext) {
        requested_with_comparison(comparison()).await
    }
    async fn requested_with_comparison(
        latest: Value,
    ) -> (Arc<OpsFileRemedies>, Rows, String, InvocationContext) {
        let (base, rows) = port_with_comparison(latest).await;
        let handler = OpsFileRemedies::with_client(api_client(), base);
        discover_for_comparison(
            &handler,
            verb_files(),
            168,
            instant(),
            &context(comparison()),
        )
        .await
        .unwrap();
        let id = rows.lock().unwrap().keys().next().unwrap().clone();
        let ctx = context(json!({"job_id":id,"step_id":STEP}));
        (handler, rows, id, ctx)
    }
    fn answer(rows: &Rows, id: &str, step: Value) {
        let mut rows = rows.lock().unwrap();
        let request = rows.get_mut(id).unwrap();
        request["status"] = json!("closed");
        request["steps"] = json!([step]);
    }

    #[test]
    fn only_one_complete_successful_native_stdout_can_propose_explicit_signed_args() {
        let d = declaration();
        let inputs = vec!["boss".into(), "pgdata-postgres-0".into()];
        let correct = execute(&proposal());
        assert_eq!(decoded_proposal(&correct, &d, &inputs).unwrap(), proposal());
        let mut cases = Vec::new();
        for (pointer, value) in [
            ("/metadata/exit_code", json!("78")),
            ("/metadata/exit_code", json!(0)),
            ("/metadata/disposition", json!("refused")),
            ("/metadata/runner_host", json!("elsewhere")),
            ("/metadata/streams/stdout/complete", json!(false)),
            ("/metadata/streams/stdout/bytes", json!(0)),
            ("/metadata/streams/stdout/retained_bytes", json!(0)),
            ("/metadata/streams/stdout/sha256", json!("0".repeat(64))),
            ("/metadata/streams/stdout/base64", json!("not base64")),
            ("/status", json!("ready")),
        ] {
            let mut bad = correct.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            cases.push(bad);
        }
        let mut interrupted = correct.clone();
        interrupted["metadata"]["streams_unread"] = json!("capture failed");
        cases.push(interrupted);
        for bytes in [b"{}\n{}".as_slice(), b"\xff".as_slice(), b"".as_slice()] {
            let mut bad = correct.clone();
            bad["metadata"]["streams"]["stdout"] = stream(bytes);
            cases.push(bad);
        }
        for (pointer, value) in [
            ("/verb", json!("expand-instance-volume")),
            ("/args/0", json!("boss-playground")),
            ("/args/1", json!("other-claim")),
            ("/args/2", json!("30")),
            ("/plan_sha256", json!("0".repeat(64))),
        ] {
            let mut bad = proposal();
            *bad.pointer_mut(pointer).unwrap() = value;
            cases.push(execute(&bad));
        }
        let mut extra = proposal();
        extra["authority"] = json!("approved");
        cases.push(execute(&extra));
        for bad in cases {
            assert!(decoded_proposal(&bad, &d, &inputs).is_err(), "{bad}");
        }
    }

    fn duplicate_proposals() -> Vec<Vec<u8>> {
        let original = serde_json::to_string(&proposal()).unwrap();
        [
            r#""args":["foreign","claim","1Gi"]"#,
            r#""verb":"another-verb""#,
            r#""plan":"another plan""#,
            r#""plan_sha256":"another hash""#,
            r#""ar\u0067s":["foreign","claim","1Gi"]"#,
            r#""verb":"plan-an-instance-volume-expansion""#,
        ]
        .iter()
        .map(|first| format!("{{{first},{}", &original[1..]).into_bytes())
        .collect()
    }

    #[test]
    fn duplicate_native_proposal_keys_are_ambiguous_even_when_escaped_or_equal() {
        let d = declaration();
        let inputs = vec!["boss".into(), "pgdata-postgres-0".into()];
        let mut accepted = Vec::new();
        for (index, bytes) in duplicate_proposals().into_iter().enumerate() {
            let mut step = execute(&proposal());
            step["metadata"]["streams"]["stdout"] = stream(&bytes);
            if decoded_proposal(&step, &d, &inputs).is_ok() {
                accepted.push(index);
            }
        }
        assert!(
            accepted.is_empty(),
            "ambiguous raw native proposals were accepted: {accepted:?}"
        );
    }

    #[tokio::test]
    async fn duplicate_native_stdout_cannot_file_a_final_approval_request() {
        let (handler, rows, id, ctx) = requested().await;
        let bytes = duplicate_proposals().remove(0);
        let mut step = execute(&proposal());
        step["metadata"]["streams"]["stdout"] = stream(&bytes);
        let raw_receipt = step.clone();
        answer(&rows, &id, step);
        let completion = OpsDiscoveredRemedies { handler };
        let result = completion.complete(verb_files(), &ctx).await;
        assert!(
            result.is_err(),
            "ambiguous native answer cannot authorize a proposal admission: {result:?}"
        );
        let rows = rows.lock().unwrap();
        assert_eq!(rows.len(), 1, "no final approval request");
        assert_eq!(
            rows[&id]["steps"][0], raw_receipt,
            "full original native receipt conserved"
        );
    }

    #[tokio::test]
    async fn the_native_discovery_answer_files_one_approval_request_with_exact_proposal_provenance()
    {
        let (handler, rows, id, ctx) = requested().await;
        answer(&rows, &id, execute(&proposal()));
        let completion = OpsDiscoveredRemedies { handler };
        completion.complete(verb_files(), &ctx).await.unwrap();
        completion.complete(verb_files(), &ctx).await.unwrap();
        let rows = rows.lock().unwrap();
        assert_eq!(rows.len(), 2);
        let final_request = rows
            .values()
            .find(|r| r["metadata"]["verb"] == "expand-instance-volume")
            .unwrap();
        assert_eq!(final_request["metadata"]["requires_approval"], true);
        assert_eq!(final_request["metadata"]["args"], proposal()["args"]);
        assert_eq!(
            final_request["metadata"]["discovered_proposal"]["proposal"],
            proposal()
        );
        assert_eq!(
            final_request["metadata"]["discovered_proposal"]["request"],
            id
        );
        assert_eq!(
            final_request["metadata"]["discovered_proposal"]["step"],
            STEP
        );
        assert!(
            final_request.get("steps").is_none(),
            "filing must not author completed approval or execution"
        );
    }

    const VOLUME: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56";
    const FINDING: &str = "disk_tight:boss/pgdata-postgres-0";
    const ALARM: &str = "a134b7ac-0000-4000-8000-000000000001";

    /// What the native discovery answers when no growth fits and exactly
    /// one replica move is decisive (design ada8f698, option A): the
    /// existing move plan for that replica, and the claim it answers.
    fn move_proposal() -> Value {
        let plan = "plan: move-volume-replica\nA fixture plan, not a live placement claim.\n";
        json!({"verb":"plan-a-volume-replica-move",
            "args":[VOLUME, format!("{VOLUME}-r-ae04deab")],
            "target":["boss","pgdata-postgres-0"],
            "plan":plan,"plan_sha256":format!("{:x}",Sha256::digest(plan.as_bytes()))})
    }
    fn open_alarm() -> Value {
        json!({"id":ALARM,"kind":"backlog-item","status":"open",
            "metadata":{"estate_finding":FINDING}})
    }
    fn verbs_of(rows: &Rows) -> Vec<String> {
        rows.lock()
            .unwrap()
            .values()
            .filter(|r| r["kind"] == "ops-request")
            .map(|r| r["metadata"]["verb"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn a_no_growth_proposal_files_only_the_read_only_move_plan_and_says_so_on_the_alarm() {
        let (handler, rows, id, ctx) = requested().await;
        rows.lock().unwrap().insert(ALARM.into(), open_alarm());
        answer(&rows, &id, execute(&move_proposal()));
        let completion = OpsDiscoveredRemedies { handler };
        completion.complete(verb_files(), &ctx).await.unwrap();
        // A redelivery finds the plan request and files nothing more.
        completion.complete(verb_files(), &ctx).await.unwrap();
        let mut verbs = verbs_of(&rows);
        verbs.sort();
        assert_eq!(
            verbs,
            [
                "plan-a-volume-replica-move",
                "plan-the-largest-instance-volume-expansion"
            ],
            "the discovery and ONE read-only plan request: never the move, never an expansion"
        );
        let rows = rows.lock().unwrap();
        let plan = rows
            .values()
            .find(|r| r["metadata"]["verb"] == "plan-a-volume-replica-move")
            .unwrap();
        assert_eq!(plan["metadata"]["host"], "forge");
        assert_eq!(plan["metadata"]["args"], move_proposal()["args"]);
        assert!(
            plan["metadata"].get("requires_approval").is_none(),
            "a plan request asks for no approval and can run no write: {plan}"
        );
        assert!(plan.get("steps").is_none(), "filing authors no step");
        let provenance = &plan["metadata"]["discovered_proposal"];
        assert_eq!(provenance["proposal"], move_proposal());
        assert_eq!(provenance["request"], id);
        assert_eq!(provenance["step"], STEP);
        assert_eq!(
            provenance["stdout"], rows[&id]["steps"][0]["metadata"]["streams"]["stdout"],
            "the native byte receipt is copied, not retyped"
        );
        assert_eq!(provenance["proposes"], "move-volume-replica");
        // The reader of the alarm learns what was proposed and where.
        let noted = rows[ALARM]["metadata"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(key, _)| key.starts_with("remedy_proposed:"))
            .map(|(_, value)| value.clone())
            .expect("the open alarm says a move was proposed");
        assert_eq!(noted["plan_request"], plan["id"]);
        assert_eq!(noted["verb"], "move-volume-replica");
        assert_eq!(noted["args"], move_proposal()["args"]);
        assert_eq!(noted["plan_sha256"], move_proposal()["plan_sha256"]);
    }

    #[test]
    fn a_no_growth_proposal_must_conserve_the_claim_and_the_plan_verbs_declared_words() {
        let d = declaration();
        let inputs = vec!["boss".into(), "pgdata-postgres-0".into()];
        assert_eq!(
            decoded_proposal(&execute(&move_proposal()), &d, &inputs).unwrap(),
            move_proposal()
        );
        let mut cases = Vec::new();
        for (pointer, value) in [
            // The claim it answers must be the claim discovery was asked.
            ("/target", json!(["boss-playground", "pgdata-postgres-0"])),
            ("/target", json!(["boss"])),
            ("/target", json!([])),
            ("/target", json!("boss/pgdata-postgres-0")),
            // The words must be the plan verb's own: a volume, its replica.
            ("/args", json!([VOLUME])),
            ("/args", json!([VOLUME, "r-ae04deab"])),
            (
                "/args",
                json!(["pgdata-postgres-0", format!("{VOLUME}-r-ae04deab")]),
            ),
            (
                "/args",
                json!([VOLUME, format!("{VOLUME}-r-ae04deab"), "0".repeat(64)]),
            ),
            // Never the mutation, nor any other verb.
            ("/verb", json!("move-volume-replica")),
            ("/verb", json!("plan-a-volume-replica-retirement")),
            ("/plan_sha256", json!("0".repeat(64))),
            ("/plan", json!("")),
        ] {
            let mut bad = move_proposal();
            *bad.pointer_mut(pointer).unwrap() = value;
            cases.push(bad);
        }
        let mut untargeted = move_proposal();
        untargeted.as_object_mut().unwrap().remove("target");
        cases.push(untargeted);
        let mut extra = move_proposal();
        extra["authority"] = json!("approved");
        cases.push(extra);
        // An explicit-size proposal has no separate target to carry: its
        // own first words are the claim.
        let mut targeted_growth = proposal();
        targeted_growth["target"] = json!(["boss", "pgdata-postgres-0"]);
        cases.push(targeted_growth);
        for bad in cases {
            assert!(
                decoded_proposal(&execute(&bad), &d, &inputs).is_err(),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn the_final_guards_and_a_cleared_finding_stop_a_no_growth_plan_too() {
        for row in [
            incumbent("open", 200, false),
            incumbent("closed", 1, false),
            incumbent("closed", 200, true),
        ] {
            let (handler, rows, id, ctx) = requested().await;
            answer(&rows, &id, execute(&move_proposal()));
            rows.lock()
                .unwrap()
                .insert(row["id"].as_str().unwrap().into(), row);
            OpsDiscoveredRemedies { handler }
                .complete(verb_files(), &ctx)
                .await
                .unwrap();
            assert_eq!(
                rows.lock().unwrap().len(),
                2,
                "an open, recent or declined expansion request answers the finding: no plan"
            );
        }
        let latest = json!({"scope":"instance-volumes","findings":{"disk_tight":[]}});
        let (handler, rows, id, ctx) = requested_with_comparison(latest).await;
        answer(&rows, &id, execute(&move_proposal()));
        OpsDiscoveredRemedies { handler }
            .complete(verb_files(), &ctx)
            .await
            .unwrap();
        assert_eq!(
            rows.lock().unwrap().len(),
            1,
            "a cleared finding needs none"
        );
    }

    #[tokio::test]
    async fn an_alarm_note_that_cannot_be_written_does_not_unfile_the_plan() {
        // The note is observability: this alarm's merge door answers 500,
        // and the plan request still stands and the firing still succeeds.
        let (handler, rows, id, ctx) = requested().await;
        let mut alarm = open_alarm();
        alarm["refuses_notes"] = json!(true);
        rows.lock().unwrap().insert(ALARM.into(), alarm.clone());
        answer(&rows, &id, execute(&move_proposal()));
        let completion = OpsDiscoveredRemedies { handler };
        completion.complete(verb_files(), &ctx).await.unwrap();
        assert_eq!(verbs_of(&rows).len(), 2);
        assert_eq!(rows.lock().unwrap()[ALARM], alarm, "nothing was written");
    }

    #[test]
    fn the_no_growth_fallback_is_a_read_only_plan_one_passkey_verb_owns() {
        let d = declaration();
        let fallback = d.no_growth.expect("the registry declares the fallback");
        assert_eq!(fallback.plan_verb, "plan-a-volume-replica-move");
        assert_eq!(fallback.mutation, "move-volume-replica");
        for (name, pointer, value) in [
            // The mutation itself, or anything needing approval.
            (
                "expand-instance-volume",
                "/discovery_remedies/0/no_growth_plan_verb",
                json!("move-volume-replica"),
            ),
            // A verb the registry does not hold.
            (
                "expand-instance-volume",
                "/discovery_remedies/0/no_growth_plan_verb",
                json!("plan-nothing-at-all"),
            ),
            // The explicit-size plan is not its own fallback.
            (
                "expand-instance-volume",
                "/discovery_remedies/0/no_growth_plan_verb",
                json!("plan-an-instance-volume-expansion"),
            ),
            // A read-only verb no passkey-gated verb names as its plan.
            (
                "expand-instance-volume",
                "/discovery_remedies/0/no_growth_plan_verb",
                json!("pod-logs"),
            ),
            (
                "expand-instance-volume",
                "/discovery_remedies/0/no_growth_plan_verb",
                json!(["plan-a-volume-replica-move"]),
            ),
            ("plan-a-volume-replica-move", "/about", json!("MUTATING")),
            (
                "plan-a-volume-replica-move",
                "/requires_approval",
                json!(true),
            ),
            ("plan-a-volume-replica-move", "/hosts", json!(["boss-gcp"])),
            ("move-volume-replica", "/requires_approval", json!(false)),
            ("move-volume-replica", "/approvers", json!([])),
            ("move-volume-replica", "/params/2/name", json!("unsigned")),
        ] {
            let mut files: Vec<(String, String)> = verb_files()
                .iter()
                .map(|(name, text)| ((*name).into(), (*text).into()))
                .collect();
            let (_, text) = files.iter_mut().find(|(n, _)| n == name).unwrap();
            let mut spec: Value = serde_json::from_str(text).unwrap();
            if let Some(field) = spec.pointer_mut(pointer) {
                *field = value;
            } else {
                spec["requires_approval"] = value;
            }
            *text = spec.to_string();
            let borrowed: Vec<(&str, &str)> = files
                .iter()
                .map(|(n, s)| (n.as_str(), s.as_str()))
                .collect();
            assert!(index(&borrowed).is_err(), "{name}{pointer} must refuse");
        }
        // A fifth key of any other name is not a declaration this reads.
        let mut files: Vec<(String, String)> = verb_files()
            .iter()
            .map(|(name, text)| ((*name).into(), (*text).into()))
            .collect();
        let (_, text) = files
            .iter_mut()
            .find(|(n, _)| n == "expand-instance-volume")
            .unwrap();
        let mut spec: Value = serde_json::from_str(text).unwrap();
        let entry = spec["discovery_remedies"][0].as_object_mut().unwrap();
        let verb = entry.remove("no_growth_plan_verb").unwrap();
        entry.insert("also_files".into(), verb);
        *text = spec.to_string();
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(n, s)| (n.as_str(), s.as_str()))
            .collect();
        assert!(index(&borrowed).is_err());
    }

    /// DELIBERATELY CHANGED with the no-growth fallback (design ada8f698):
    /// this test pinned "no growth never files", which is no longer the
    /// whole truth — a no-growth ANSWER (exit 0, a typed move proposal)
    /// now files a read-only plan request, tested above. What it still
    /// pins, by its new name, is the other half: a native REFUSAL — every
    /// exit-78 the decisive read gives (two short disks, a destination
    /// that does not admit, an unhealthy volume, a ceiling) — a partial
    /// byte record and a wrong target file NOTHING, of any verb, and the
    /// failure stays loud as this handler's permanent error.
    #[tokio::test]
    async fn refused_no_growth_and_partial_native_reads_file_no_request_of_any_verb() {
        for mode in [
            "no-growth-refused",
            "cannot-answer",
            "partial",
            "wrong-target",
            "partial-move",
            "wrong-move-target",
        ] {
            let (handler, rows, id, ctx) = requested().await;
            let mut step = execute(&proposal());
            match mode {
                "no-growth-refused" => {
                    step["metadata"]["exit_code"] = json!("78");
                    step["metadata"]["streams"]["stdout"] = stream(b"");
                    step["metadata"]["output"] = json!(
                        "REFUSED — no replica move is proposed for boss/pgdata-postgres-0 — 2 replica disks are short"
                    );
                }
                "cannot-answer" => {
                    step["metadata"]["exit_code"] = json!("1");
                    step["metadata"]["streams"]["stdout"] = stream(b"");
                }
                "partial" => step["metadata"]["streams"]["stdout"]["complete"] = json!(false),
                "partial-move" => {
                    step = execute(&move_proposal());
                    step["metadata"]["streams"]["stdout"]["complete"] = json!(false);
                }
                "wrong-move-target" => {
                    let mut p = move_proposal();
                    p["target"][1] = json!("wrong");
                    step = execute(&p);
                }
                _ => {
                    let mut p = proposal();
                    p["args"][1] = json!("wrong");
                    step = execute(&p);
                }
            }
            answer(&rows, &id, step.clone());
            assert!(
                OpsDiscoveredRemedies { handler }
                    .complete(verb_files(), &ctx)
                    .await
                    .is_err()
            );
            let rows = rows.lock().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[&id]["steps"][0], step,
                "the failed/refused receipt remains intact"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_completion_and_comparison_keep_one_discovery_and_one_final_identity() {
        let (base, rows) = port().await;
        let handler = OpsFileRemedies::with_client(api_client(), base);
        let a = context(comparison());
        let b = context(comparison());
        let (first, second) = tokio::join!(
            discover_for_comparison(&handler, verb_files(), 168, instant(), &a),
            discover_for_comparison(&handler, verb_files(), 168, instant(), &b)
        );
        first.unwrap();
        second.unwrap();
        assert_eq!(rows.lock().unwrap().len(), 1);
        let id = rows.lock().unwrap().keys().next().unwrap().clone();
        answer(&rows, &id, execute(&proposal()));
        let ctx = context(json!({"job_id":id,"step_id":STEP}));
        let completion = OpsDiscoveredRemedies { handler };
        let (first, second) = tokio::join!(
            completion.complete(verb_files(), &ctx),
            completion.complete(verb_files(), &ctx)
        );
        first.unwrap();
        second.unwrap();
        assert_eq!(rows.lock().unwrap().len(), 2);
    }

    fn incumbent(status: &str, hours: i64, declined: bool) -> Value {
        let declaration = declaration();
        let target = target(&declaration, "disk_tight:boss/pgdata-postgres-0", vec![]);
        let at = (instant() - chrono::Duration::hours(hours)).to_rfc3339();
        json!({"id":"8120b0f1-0000-4000-8000-000000000099","kind":"ops-request","status":status,
            "subject":{"subject_kind":"custom","id":target.subject()},"opened_at":at,
            "steps":if declined {json!([{"spec_slug":"approve","status":"completed","completed_at":at,
                "metadata":{"decision":"rejected"}}])} else {json!([])}})
    }

    #[tokio::test]
    async fn final_open_recent_and_declined_requests_stop_discovery_without_bypassing_an_answer() {
        for row in [
            incumbent("open", 200, false),
            incumbent("closed", 1, false),
            incumbent("closed", 200, true),
        ] {
            let (base, rows) = port().await;
            rows.lock()
                .unwrap()
                .insert(row["id"].as_str().unwrap().into(), row);
            let handler = OpsFileRemedies::with_client(api_client(), base);
            discover_for_comparison(
                &handler,
                verb_files(),
                168,
                instant(),
                &context(comparison()),
            )
            .await
            .unwrap();
            assert_eq!(
                rows.lock().unwrap().len(),
                1,
                "discovery must not bypass a final request guard"
            );
        }
    }

    #[tokio::test]
    async fn final_guards_are_read_again_on_completion_and_old_episodes_cannot_become_new_requests()
    {
        for row in [
            incumbent("open", 200, false),
            incumbent("closed", 1, false),
            incumbent("closed", 200, true),
            incumbent("closed", 200, false),
        ] {
            let (handler, rows, id, ctx) = requested().await;
            answer(&rows, &id, execute(&proposal()));
            rows.lock()
                .unwrap()
                .insert(row["id"].as_str().unwrap().into(), row.clone());
            let result = OpsDiscoveredRemedies { handler }
                .complete(verb_files(), &ctx)
                .await;
            if row["status"] == "closed"
                && row["steps"].as_array().unwrap().is_empty()
                && row["opened_at"] != (instant() - chrono::Duration::hours(1)).to_rfc3339()
            {
                assert!(
                    result.is_err(),
                    "the previous discovery must not acquire a new successor identity"
                );
            } else {
                result.unwrap();
            }
            assert_eq!(
                rows.lock().unwrap().len(),
                2,
                "only the discovery and new incumbent may exist"
            );
        }
    }

    #[tokio::test]
    async fn a_recorded_clear_after_decline_allows_a_new_episode_and_keeps_its_provenance() {
        let (base, rows) = port().await;
        let prior = incumbent("closed", 200, true);
        let prior_id = prior["id"].as_str().unwrap().to_string();
        rows.lock().unwrap().insert(prior_id.clone(), prior);
        let alarm_id = "8120b0f1-0000-4000-8000-000000000098";
        rows.lock().unwrap().insert(alarm_id.into(),json!({"id":alarm_id,"kind":"backlog-item","status":"closed",
            "metadata":{"estate_finding":"disk_tight:boss/pgdata-postgres-0","closed_at":(instant()-chrono::Duration::hours(190)).to_rfc3339(),"outcome":"stale"}}));
        let handler = OpsFileRemedies::with_client(api_client(), base);
        discover_for_comparison(
            &handler,
            verb_files(),
            168,
            instant(),
            &context(comparison()),
        )
        .await
        .unwrap();
        let id = rows
            .lock()
            .unwrap()
            .values()
            .find(|r| r["metadata"]["verb"] == "plan-the-largest-instance-volume-expansion")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        answer(&rows, &id, execute(&proposal()));
        OpsDiscoveredRemedies { handler }
            .complete(verb_files(), &context(json!({"job_id":id,"step_id":STEP})))
            .await
            .unwrap();
        let rows = rows.lock().unwrap();
        let final_request = rows
            .values()
            .find(|r| r["metadata"]["verb"] == "expand-instance-volume")
            .unwrap();
        assert_eq!(
            final_request["id"],
            remedy_request_id(
                &target(&declaration(), "disk_tight:boss/pgdata-postgres-0", vec![]).subject(),
                Some(&prior_id)
            )
        );
        assert_eq!(
            final_request["metadata"]["decline_lifted"],
            json!({"declined_request":prior_id,"cleared_alarm":alarm_id})
        );
    }

    #[tokio::test]
    async fn discovery_is_scope_bound_and_refuses_ambiguous_or_changed_target_fields_before_storage()
     {
        for mode in ["other-scope", "wrong-id", "duplicate", "missing-field"] {
            let (base, rows) = port().await;
            let handler = OpsFileRemedies::with_client(api_client(), base);
            let mut payload = comparison();
            match mode {
                "other-scope" => payload["scope"] = json!("host"),
                "wrong-id" => payload["findings"]["disk_tight"][0]["id"] = json!("elsewhere/claim"),
                "duplicate" => {
                    let row = payload["findings"]["disk_tight"][0].clone();
                    payload["findings"]["disk_tight"]
                        .as_array_mut()
                        .unwrap()
                        .push(row);
                }
                _ => payload["findings"]["disk_tight"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("claim")
                    .map(|_| ())
                    .unwrap(),
            }
            let result =
                discover_for_comparison(&handler, verb_files(), 168, instant(), &context(payload))
                    .await;
            if mode == "other-scope" {
                result.unwrap();
            } else {
                assert!(result.is_err());
            }
            assert!(
                rows.lock().unwrap().is_empty(),
                "{mode} must not partially file a target"
            );
        }
    }

    #[tokio::test]
    async fn a_current_clear_keeps_the_native_discovery_receipt_without_filing_an_unneeded_approval()
     {
        let latest = json!({"scope":"instance-volumes","findings":{"disk_tight":[]}});
        let (handler, rows, id, ctx) = requested_with_comparison(latest).await;
        answer(&rows, &id, execute(&proposal()));
        OpsDiscoveredRemedies { handler }
            .complete(verb_files(), &ctx)
            .await
            .unwrap();
        assert_eq!(
            rows.lock().unwrap().len(),
            1,
            "a resolved finding must not acquire a new expansion approval"
        );
    }

    #[test]
    fn current_comparison_coverage_identity_and_chronology_cannot_be_replaced_by_quiet() {
        let d = declaration();
        let finding = "disk_tight:boss/pgdata-postgres-0";
        let correct = json!({"total":1,"data":[{"event_id":"8120b0f1-0000-4000-8000-000000000002",
            "kind":"jobs.estate.compared","source":"jobs","timestamp":instant().to_rfc3339(),"payload":comparison()}]});
        assert!(finding_is_current(&correct, &d, finding, instant(), instant()).unwrap());
        for (pointer, value) in [
            ("/total", json!(0)),
            ("/total", json!(2)),
            ("/total", json!("1")),
            ("/data/0/event_id", json!("not-a-native-id")),
            ("/data/0/kind", json!("jobs.estate.observed")),
            ("/data/0/source", json!("elsewhere")),
            ("/data/0/timestamp", json!("unknown")),
            ("/data/0/payload/scope", json!("host")),
            ("/data/0/payload/findings/disk_tight", json!(null)),
            (
                "/data/0/payload/findings/disk_tight/0/id",
                json!("wrong/target"),
            ),
        ] {
            let mut bad = correct.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(
                finding_is_current(&bad, &d, finding, instant(), instant()).is_err(),
                "{pointer}: {bad}"
            );
        }
        let mut tied = correct.clone();
        let mut second = tied["data"][0].clone();
        second["event_id"] = json!("8120b0f1-0000-4000-8000-000000000003");
        tied["data"].as_array_mut().unwrap().push(second);
        tied["total"] = json!(2);
        assert!(
            finding_is_current(&tied, &d, finding, instant(), instant()).is_err(),
            "an arbitrary tied newest record cannot decide"
        );
        tied["data"][1]["timestamp"] =
            json!((instant() - chrono::Duration::nanoseconds(1)).to_rfc3339());
        assert!(
            finding_is_current(&tied, &d, finding, instant(), instant()).unwrap(),
            "native fractional ordering is precise"
        );
        tied["data"].as_array_mut().unwrap().reverse();
        assert!(finding_is_current(&tied, &d, finding, instant(), instant()).is_err());
        assert!(
            finding_is_current(
                &correct,
                &d,
                finding,
                instant() + chrono::Duration::seconds(1),
                instant() + chrono::Duration::seconds(1)
            )
            .is_err()
        );
        assert!(
            finding_is_current(
                &correct,
                &d,
                finding,
                instant(),
                instant() - chrono::Duration::seconds(1)
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn a_recorded_clear_after_discovery_rejects_its_proposal_even_if_the_finding_recurs() {
        let (handler, rows, id, mut ctx) = requested().await;
        answer(&rows, &id, execute(&proposal()));
        let alarm = "8120b0f1-0000-4000-8000-000000000097";
        rows.lock().unwrap().insert(
            alarm.into(),
            json!({"id":alarm,"kind":"backlog-item","status":"closed",
            "metadata":{"estate_finding":"disk_tight:boss/pgdata-postgres-0","outcome":"stale",
                "closed_at":(instant()+chrono::Duration::seconds(1)).to_rfc3339()}}),
        );
        ctx.event_timestamp = Some(instant() + chrono::Duration::seconds(2));
        assert!(
            OpsDiscoveredRemedies { handler }
                .complete(verb_files(), &ctx)
                .await
                .is_err()
        );
        assert_eq!(
            rows.lock().unwrap().len(),
            2,
            "the old discovery plus closure record remain; no approval"
        );
    }

    #[test]
    fn discovery_registry_contract_preserves_the_owning_approval_and_read_only_boundaries() {
        for (name, pointer, value) in [
            ("expand-instance-volume", "/requires_approval", json!(false)),
            ("expand-instance-volume", "/approvers", json!([])),
            (
                "expand-instance-volume",
                "/params/3/name",
                json!("unsigned"),
            ),
            (
                "expand-instance-volume",
                "/discovery_remedies/0/finding_class",
                json!("unknown"),
            ),
            (
                "expand-instance-volume",
                "/discovery_remedies/0/arg_fields",
                json!(["namespace", "namespace"]),
            ),
            (
                "plan-the-largest-instance-volume-expansion",
                "/capture",
                json!("interleaved"),
            ),
            (
                "plan-the-largest-instance-volume-expansion",
                "/about",
                json!("MUTATING"),
            ),
            (
                "plan-the-largest-instance-volume-expansion",
                "/requires_approval",
                json!(true),
            ),
            (
                "plan-the-largest-instance-volume-expansion",
                "/hosts",
                json!(["boss-gcp"]),
            ),
            (
                "plan-an-instance-volume-expansion",
                "/params/0/name",
                json!("different"),
            ),
        ] {
            let mut files: Vec<(String, String)> = verb_files()
                .iter()
                .map(|(name, text)| ((*name).into(), (*text).into()))
                .collect();
            let (_, text) = files.iter_mut().find(|(n, _)| n == name).unwrap();
            let mut spec: Value = serde_json::from_str(text).unwrap();
            if let Some(field) = spec.pointer_mut(pointer) {
                *field = value;
            } else {
                spec["requires_approval"] = value;
            }
            *text = spec.to_string();
            let borrowed: Vec<(&str, &str)> = files
                .iter()
                .map(|(n, s)| (n.as_str(), s.as_str()))
                .collect();
            assert!(index(&borrowed).is_err(), "{name}{pointer} must refuse");
        }
    }
}
