//! Tenant Workflow-publish bootstrap, shared by tenant `prepare`
//! steps (the brewery's converged prepare and the used-device-shop's
//! prepare both call this) — the Workflow-registry sibling of
//! `boss_policy::bootstrap::publish_policy_rules`: one impl seeds a
//! tenant's `workflows.toml` through the public API so the offline
//! regen, the live demo, and a fresh-VM install cannot drift.
//!
//! [`publish_workflows`] opens one `workflow-design` Job per
//! Workflow in the seed file, walks it to closure, and lets the
//! `workflow-publish` dispatch path land the spec in the registry.
//!
//! Tenant kinds arrive with full provenance this way: audit_log
//! captures the meta-Job that authored each, including author /
//! approver / published-at. See
//! [`crate::registry::platform_workflows`] for the meta-kind itself.
//!
//! Idempotent: if a `workflow-design` Job has already published a
//! given target kind (the registry has an active row with an
//! `authoring_job_id`), the publish keeps it — and when the file
//! differs from the live row on a facet the drift lint compares
//! ([`workflow_changes`]), the kind is NAMED in the outcome with those
//! facets (design e187198f, 2026-09-18: the instance is the truth,
//! and a repo edit that does not land is named, never silent; until
//! then a differing kind was skipped without a word, so a repo edit to
//! `seeds/workflows.toml` was dead text on a running instance).
//! `force_republish` supersedes ONLY the kinds that differ, each
//! change named. Re-running after a partial failure resumes from
//! where it left off.
//!
//! Hard-fails on any non-2xx response. The seed regens that consume
//! this output expect every kind to actually land in the registry.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use boss_core::machine_token::BlockingClient;
use boss_core::publish::{FieldChange, KeptRow, UpdatedRow};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::registry::WorkflowSpec;

/// Open one `workflow-design` Job per Workflow in `seeds`, walk each
/// to closure, and let the `workflow-publish` dispatch path land the
/// spec in the registry.
///
/// `api_base` is the jobs-api (or gateway) base URL; `owning_team`
/// is stamped on every loaded spec (convention: the tenant id or
/// `"<tenant>-bootstrap"` — see
/// [`crate::seed_loader::load_workflows_with_owning_team`]); `dev`
/// auto-walks the sign-off step (development only);
/// `force_republish` re-publishes an already-operator-published kind
/// whose file differs (a new version supersedes); `x_boss_user` overrides the
/// default `automation:bootstrap` / `platform-admin` / `operator`
/// header when `Some`.
///
/// Idempotent + hard-fails on any non-2xx response — see the
/// module docs.
///
/// `client` is the caller's, and it is a
/// [`BlockingClient`]: it stamps the estate machine token on every
/// request from the process's watched source and follows no redirect
/// (design 6805c764 car 2, the blocking-senders slice, 2026-09-29).
/// Until then the walk built its own client and baked the token read
/// at walk start into its header map, so a walk outlasting a rotation's
/// overlap window sent a revoked value. Taking the client also lets a
/// test hand it a fixed source rather than the process's live one
/// (backlog 2ee29275, F2).
pub fn publish_workflows(
    client: &BlockingClient,
    api_base: &str,
    seeds: &Path,
    owning_team: &str,
    dev: bool,
    force_republish: bool,
    x_boss_user: Option<&str>,
) -> Result<WorkflowPublishOutcome> {
    let user_header = x_boss_user.map(|s| s.to_string()).unwrap_or_else(|| {
        json!({
            "id": "automation:bootstrap",
            "role": "platform-admin",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": "platform",
        })
        .to_string()
    });
    // WHO SIGNS. The walk's synthetic approvals are recorded against
    // the actor the walk RUNS AS — the id in the header every call
    // below already carries — never a named person. Until 2026-09-18
    // this was a literal `emp-cto` (backlog 3c23662d): a signature by
    // someone who did not sign, the forged-actor defect CLAUDE.md
    // §Doors names, on every tenant's bootstrap.
    let signer = signer_of(&user_header)?;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-boss-user",
        reqwest::header::HeaderValue::from_str(&user_header).context("x-boss-user header value")?,
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    let specs = crate::seed_loader::load_workflows_with_owning_team(seeds, owning_team)
        .with_context(|| format!("loading workflows.toml for `{owning_team}`"))?;

    info!(
        seeds = %seeds.display(),
        api_base = %api_base,
        owning_team = %owning_team,
        kind_count = specs.len(),
        dev = dev,
        "starting workflow bootstrap"
    );

    let mut out = WorkflowPublishOutcome::default();
    for spec in &specs {
        // Skip if already operator-published. The registry's
        // `created_by` discriminator is the source of truth: rows
        // landed via a Job have `created_by = "job-<uuid>"`,
        // rows that came from `platform_workflows()` carry
        // `created_by = "bootstrap"`.
        let (provenance, live) = active_kind(client, api_base, &headers, &spec.kind)?;
        let changes = match provenance {
            Provenance::OperatorPublished => {
                // The kind is live: what does the file say differently?
                // Named either way (design e187198f) — kept under the
                // default, superseded under force.
                let changes = workflow_changes(&live, spec);
                if changes.is_empty() {
                    info!(kind = %spec.kind, "already published as the file declares; skipping");
                    out.unchanged += 1;
                    continue;
                }
                if !force_republish {
                    info!(kind = %spec.kind, differs = ?changes.iter().map(|c| c.field.as_str()).collect::<Vec<_>>(), "already operator-published and the file differs; kept (the instance is the truth)");
                    out.kept.push(KeptRow {
                        id: spec.kind.clone(),
                        differs: changes.into_iter().map(|c| c.field).collect(),
                    });
                    continue;
                }
                info!(kind = %spec.kind, "already operator-published; --force-republish set, publishing new version");
                Some(changes)
            }
            Provenance::BootstrapOwned | Provenance::Missing => None,
        };
        bootstrap_kind(client, api_base, &headers, spec, dev, &signer)
            .with_context(|| format!("bootstrap of `{}`", spec.kind))?;
        match changes {
            Some(changes) => out.superseded.push(UpdatedRow {
                id: spec.kind.clone(),
                changes,
            }),
            None => out.published.push(spec.kind.clone()),
        }
    }

    info!(
        published = out.published.len(),
        superseded = out.superseded.len(),
        kept = out.kept.len(),
        unchanged = out.unchanged,
        total = specs.len(),
        owning_team = %owning_team,
        "workflow bootstrap complete"
    );
    Ok(out)
}

/// What a workflow publish did: kinds published (new, or over a
/// bootstrap-owned row), kinds a `force_republish` SUPERSEDED with a
/// new version (each differing facet named from → to), kinds kept as
/// the instance publishes them with the differing facets named, and
/// kinds already as the file declares.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkflowPublishOutcome {
    pub published: Vec<String>,
    pub superseded: Vec<UpdatedRow>,
    pub kept: Vec<KeptRow>,
    pub unchanged: usize,
}

impl WorkflowPublishOutcome {
    /// One line for a publish report.
    pub fn summary(&self) -> String {
        let mut s = format!("{} published", self.published.len());
        if !self.published.is_empty() {
            s.push_str(&format!(" ({})", self.published.join(", ")));
        }
        if !self.superseded.is_empty() {
            s.push_str(&format!(
                ", superseded {}: {}",
                self.superseded.len(),
                self.superseded
                    .iter()
                    .map(UpdatedRow::render)
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        if self.unchanged > 0 {
            s.push_str(&format!(", {} already as declared", self.unchanged));
        }
        let kept = boss_core::publish::render_kept(&self.kept, Some("workflows"));
        if !kept.is_empty() {
            s.push_str("; ");
            s.push_str(&kept);
        }
        s
    }
}

// THE FACETS A LIVE KIND AND ITS FILE ARE COMPARED ON. The daily drift
// lint, `infra/lint/the-live-protocols-are-the-authored-protocols.sh`,
// compares the same facets in Python (`fields_report`), so the publish
// line and the drift measurement name the same disagreements. Until
// 2026-09-28 this said so in a comment and nothing held it: this copy
// lacked the step's `agent` block from 2026-09-19 (backlog 1b847556)
// and every key the lint gained on 2026-09-28 (backlog 462cdfe3), so a
// publish read "already as declared" for a kind the drift report named
// adrift (backlog e5dc276d). The lists below are pinned entry by entry
// to the lint's, and the two comparators are run over the real bundle
// side by side, by `boss-testing/tests/the_drift_facets_are_one_list.rs`
// — the lint is a build-free pre-flight, so it cannot ask this
// definition through a binary built from its own tree (the pin's header
// says why that is not a collapse). The reason each entry is on each
// list is the lint's comment, which is not restated here.

/// Workflow fields compared VERBATIM as text: the lint's `FIELDS`. A
/// file silent on one is the lint's ABSENT line rather than a DRIFT,
/// and here a change only when the live row holds text the silence
/// would overwrite.
pub const COMPARED_FIELDS: [&str; 3] = ["label", "description", "category"];

/// The per-step facets rendered BY NAME for a title both sides hold —
/// `required` (sorted required field names), `optional` (sorted
/// `name:field_type` of every other field), `title_template`, `agent`
/// (the block with its numbers as floats): the lint's
/// `NAMED_STEP_FACETS`.
pub const NAMED_STEP_FACETS: [&str; 4] = ["required", "optional", "title_template", "agent"];

/// Step keys the named facets read, so they are not ALSO compared under
/// their own name: the lint's `STEP_NAMED`. Every other step key is
/// compared under its own name (`ready_when`, `kind`,
/// `metadata_defaults`, … and whatever is added next).
pub const STEP_NAMED: [&str; 4] = ["title", "title_template", "agent", "fields"];

/// Row keys never compared as a workflow field — columns a file cannot
/// state, the loader-stamped `owning_team`, and the steps (compared as
/// facets): the lint's `ROW_ONLY`. Every other workflow key is compared
/// under its own name.
pub const ROW_ONLY: [&str; 8] = [
    "kind",
    "version",
    "status",
    "created_at",
    "authoring_job_id",
    "owning_team",
    "step",
    "steps",
];

/// Step keys the publish path FILLS when a file is silent, so they are
/// compared only when the file states one: the lint's
/// `FILLED_WHEN_SILENT`. One asymmetry is the loader's, not this list's:
/// a step naming an `audience` gets `authority_role` written from it at
/// load, so here the file states one, while the lint reads the raw file
/// and does not — a live row whose `authority_role` alone moves is named
/// here and not there. Its `audience` is compared on both sides.
pub const FILLED_WHEN_SILENT: [&str; 1] = ["authority_role"];

/// One canonical string for a structural value; `None` for no claim.
/// Absent, `null`, `false`, `""` and an empty list or table are one
/// claim — the publish path writes the default the file was silent
/// about — and every number is a float (TOML `2`, registry `2.0`). The
/// lint's `canon`.
fn canon(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null | Value::Bool(false) => None,
        Value::String(s) if s.is_empty() => None,
        Value::Array(a) if a.is_empty() => None,
        Value::Object(o) if o.is_empty() => None,
        v => Some(floats(v).to_string()),
    }
}

/// `v` with every number a float, at every depth, and every table's
/// keys inserted in sorted order (so the rendering is one string
/// whichever map the build's serde_json uses).
fn floats(v: &Value) -> Value {
    match v {
        Value::Number(n) => n.as_f64().map(Value::from).unwrap_or(Value::Null),
        Value::Array(a) => Value::Array(a.iter().map(floats).collect()),
        Value::Object(o) => sorted(o.iter().map(|(k, v)| (k, floats(v)))),
        other => other.clone(),
    }
}

fn sorted<'a>(entries: impl Iterator<Item = (&'a String, Value)>) -> Value {
    let map: BTreeMap<&String, Value> = entries.collect();
    Value::Object(map.into_iter().map(|(k, v)| (k.clone(), v)).collect())
}

/// The agent block as one canonical string, or `None` for no block:
/// its TOP-LEVEL numbers as floats (`budget_usd = 5` against the
/// registry's `5.0`). The lint's `agent_facet`.
fn agent_facet(block: Option<&Value>) -> Option<String> {
    let o = block?.as_object()?;
    Some(
        sorted(o.iter().map(|(k, v)| match v {
            Value::Number(n) => (k, n.as_f64().map(Value::from).unwrap_or(Value::Null)),
            other => (k, other.clone()),
        }))
        .to_string(),
    )
}

fn text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// Every step facet one side states, and its titles in order. The
/// lint's `step_facets`; a value is `None` for "no claim".
fn step_facets(steps: Option<&Value>) -> (BTreeMap<String, Option<String>>, Vec<String>) {
    let steps: Vec<&serde_json::Map<String, Value>> = steps
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_object).collect())
        .unwrap_or_default();
    let titles: Vec<String> = steps.iter().map(|s| text(s.get("title"))).collect();
    let mut out = BTreeMap::from([
        ("steps.count".to_string(), Some(titles.len().to_string())),
        ("steps.titles".to_string(), Some(titles.join(","))),
    ]);
    for (s, t) in steps.iter().zip(&titles) {
        let fields: Vec<&serde_json::Map<String, Value>> = s
            .get("fields")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_object).collect())
            .unwrap_or_default();
        let required =
            |f: &serde_json::Map<String, Value>| f.get("required") == Some(&Value::Bool(true));
        let mut req: Vec<String> = fields
            .iter()
            .filter(|f| required(f))
            .map(|f| text(f.get("name")))
            .collect();
        req.sort();
        let mut opt: Vec<String> = fields
            .iter()
            .filter(|f| !required(f))
            .map(|f| format!("{}:{}", text(f.get("name")), text(f.get("field_type"))))
            .collect();
        opt.sort();
        out.insert(format!("steps.{t}.required"), Some(req.join(",")));
        out.insert(format!("steps.{t}.optional"), Some(opt.join(",")));
        out.insert(
            format!("steps.{t}.title_template"),
            Some(text(s.get("title_template"))),
        );
        out.insert(format!("steps.{t}.agent"), agent_facet(s.get("agent")));
        for (key, value) in s.iter().filter(|(k, _)| !STEP_NAMED.contains(&k.as_str())) {
            out.insert(format!("steps.{t}.{key}"), canon(Some(value)));
        }
    }
    (out, titles)
}

/// The step facets compared, in the lint's order: the count, the title
/// list, then per title held on BOTH sides its named facets and every
/// other key either side states. A step on one side only is one
/// finding, in `steps.titles`, never a line per facet against nothing.
fn facets_to_compare(
    tree: &BTreeMap<String, Option<String>>,
    live: &BTreeMap<String, Option<String>>,
    tree_titles: &[String],
    live_titles: &[String],
) -> Vec<String> {
    let mut out = vec!["steps.count".to_string(), "steps.titles".to_string()];
    for t in tree_titles.iter().filter(|t| live_titles.contains(t)) {
        let prefix = format!("steps.{t}.");
        out.extend(NAMED_STEP_FACETS.iter().map(|k| format!("{prefix}{k}")));
        let rest: std::collections::BTreeSet<&String> = tree
            .keys()
            .chain(live.keys())
            .filter(|k| {
                k.strip_prefix(&prefix)
                    .is_some_and(|rest| !NAMED_STEP_FACETS.contains(&rest))
            })
            .collect();
        out.extend(rest.into_iter().cloned());
    }
    out
}

/// Every facet the file's `spec` disagrees with the `live` row on,
/// from → to (a missing claim renders as `""`): the named fields, then
/// every other workflow key, then the step facets, each in the order
/// the drift lint prints them.
pub fn workflow_changes(live: &Value, spec: &WorkflowSpec) -> Vec<FieldChange> {
    let file = serde_json::to_value(spec).unwrap_or(Value::Null);
    let mut out = Vec::new();
    for f in COMPARED_FIELDS {
        // A file silent on the field (only `description` can be) is the
        // lint's ABSENT line, not a DRIFT one — but a supersede would
        // publish that silence over the live text, so a live row that
        // holds some is named here, `text → ` (the pin reads ABSENT the
        // same way).
        let (tree, have) = (
            text(file.get(f).filter(|v| !v.is_null())),
            text(live.get(f)),
        );
        if tree != have {
            out.push(FieldChange::new(f, have, tree));
        }
    }
    let keys = |v: &Value| -> Vec<String> {
        v.as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    };
    let others: std::collections::BTreeSet<String> = keys(&file)
        .into_iter()
        .chain(keys(live))
        .filter(|k| !ROW_ONLY.contains(&k.as_str()) && !COMPARED_FIELDS.contains(&k.as_str()))
        .collect();
    for k in others {
        let (tree, have) = (canon(file.get(&k)), canon(live.get(&k)));
        if tree != have {
            out.push(FieldChange::new(
                &k,
                have.unwrap_or_default(),
                tree.unwrap_or_default(),
            ));
        }
    }
    let (tree_facets, tree_titles) = step_facets(file.get("steps"));
    let (live_facets, live_titles) = step_facets(live.get("steps"));
    for facet in facets_to_compare(&tree_facets, &live_facets, &tree_titles, &live_titles) {
        let tree = tree_facets.get(&facet).cloned().flatten();
        let have = live_facets.get(&facet).cloned().flatten();
        if tree == have {
            continue;
        }
        let last = facet.rsplit('.').next().unwrap_or_default();
        if tree.is_none() && FILLED_WHEN_SILENT.contains(&last) {
            continue;
        }
        out.push(FieldChange::new(
            &facet,
            have.unwrap_or_default(),
            tree.unwrap_or_default(),
        ));
    }
    out
}

/// The actor id an `x-boss-user` header names — the walk's signer.
/// Refuses a header with no `id` rather than signing as nobody.
fn signer_of(user_header: &str) -> Result<String> {
    serde_json::from_str::<Value>(user_header)
        .ok()
        .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string))
        .filter(|id| !id.is_empty())
        .context("x-boss-user header names no actor id, so the walk has nobody to sign as")
}

fn jobs_url(api_base: &str, path: &str) -> String {
    format!("{}{}", api_base.trim_end_matches('/'), path)
}

/// Where the active row for `kind` came from. `Missing` means no
/// row exists; `BootstrapOwned` is from `platform_workflows()`;
/// `OperatorPublished` is from a real Job (or an admin PUT).
#[derive(Debug, PartialEq, Eq)]
enum Provenance {
    Missing,
    BootstrapOwned,
    OperatorPublished,
}

/// Classify an active-kind response body. The wire shape doesn't
/// expose `created_by`; the next-best signal we have without a schema
/// change is `authoring_job_id`, which is set iff the row came from
/// `publish_authored`. A `bootstrap`-owned row never has it.
fn provenance_of(body: &Value) -> Provenance {
    if body
        .get("authoring_job_id")
        .and_then(|v| v.as_str())
        .is_some()
    {
        Provenance::OperatorPublished
    } else {
        Provenance::BootstrapOwned
    }
}

/// The active row for `kind` as the registry answers it, with its
/// provenance; `Missing` rides an empty body.
fn active_kind(
    client: &BlockingClient,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
    kind: &str,
) -> Result<(Provenance, Value)> {
    let url = jobs_url(api_base, &format!("/api/workflows/{kind}"));
    let resp = client.get(&url).headers(headers.clone()).send()?;
    if resp.status() == 404 {
        return Ok((Provenance::Missing, Value::Null));
    }
    if !resp.status().is_success() {
        anyhow::bail!(
            "GET {url} → {} {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }
    let body: Value = resp.json()?;
    Ok((provenance_of(&body), body))
}

fn bootstrap_kind(
    client: &BlockingClient,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
    target: &WorkflowSpec,
    dev: bool,
    signer: &str,
) -> Result<()> {
    info!(kind = %target.kind, "opening workflow-design Job");

    // 1. POST /api/jobs to open a workflow-design Job whose
    //    Subject points at the target kind. The metadata carries
    //    a placeholder; metadata for individual steps is written
    //    through each step's merge door as the walk reaches it.
    let create_body = json!({
        "kind": "workflow-design",
        // Subject is uniformly a {subject_kind, id} pair — every
        // kind, including this meta-Job's `workflow` subject, uses
        // the same shape.
        "subject": {
            "subject_kind": "workflow",
            "id": target.kind,
        },
        "title": format!("Design `{}`", target.kind),
        "owner_id": "automation:bootstrap",
        "status": "open",
        "priority": "standard",
        // opened_on is intentionally omitted so the jobs-api stamps it
        // from the sim clock (its create_job default) — the SAME clock the
        // step-walk below closes the Job against. A hardcoded epoch date
        // diverged from the prior-day seed anchor the close lands on
        // (seed_tenant_data's configure_clock_to_epoch rebases the clock to
        // the prior day), so closed_on < opened_on whenever Workflows publish
        // after that rebase — tripping the lifecycle-ordering invariant.
        "metadata": json!({
            "target_kind": target.kind,
        }),
        "tags": [],
    });
    let create_url = jobs_url(api_base, "/api/jobs");
    let resp = client
        .post(&create_url)
        .headers(headers.clone())
        .json(&create_body)
        .send()?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "POST {create_url} → {} {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }
    let job: Value = resp.json()?;
    let job_id = job
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("POST /api/jobs returned no id"))?
        .to_string();
    info!(kind = %target.kind, %job_id, "Job opened");

    // 2. List the Job's steps. The materializer expanded the four
    //    tiers into four steps; we walk them in sort_order.
    let steps_url = jobs_url(api_base, &format!("/api/jobs/{job_id}/steps"));
    let resp = client.get(&steps_url).headers(headers.clone()).send()?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "GET {steps_url} → {} {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }
    let mut steps: Vec<Value> = resp.json()?;
    steps.sort_by_key(|s| s.get("sort_order").and_then(|v| v.as_i64()).unwrap_or(0));

    // 3. Walk each step to done.
    //
    // THE STATUS IS RE-READ, NOT TAKEN FROM THE LIST ABOVE, because
    // this protocol now FORKS. `workflow-design` gained a
    // `not-published` terminal (8686485c) which is Skipped the moment
    // `approve` completes carrying `decision = "approved"` — the
    // decision this walk itself writes. The step API refuses a write
    // to a resolved step, and `walk_step` bails on any non-success, so
    // a walk that assumed every step was completable would fail the
    // bootstrap of EVERY tenant Workflow on the first boot after that
    // version went live.
    //
    // The snapshot cannot answer this: at list time `not-published` is
    // still Pending, and it becomes Skipped several requests later.
    // Only a fresh read at the moment of the write is true.
    //
    // Written for the general case rather than for this one step: any
    // future version of this kind that branches is safe here without a
    // second visit. The two 2026-09-02 boot-bricks are the reason that
    // matters — production boot must not be where a protocol change
    // first meets this walker.
    for step in &steps {
        let step_id = step
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("step missing id"))?;
        let step_kind = step.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let current = current_step(client, api_base, headers, &job_id, step_id)?;
        let status = current
            .as_ref()
            .and_then(|s| s.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if already_resolved(status) {
            info!(
                kind = %target.kind, step_id, step_kind, status,
                "step already resolved by the fork; not walking it"
            );
            continue;
        }
        // The re-read is also the freshest view of the step's own
        // fields and metadata, which is what the completion contract is
        // validated against.
        let step = current.as_ref().unwrap_or(step);
        walk_step(
            client, api_base, headers, &job_id, step_id, step_kind, step, target, dev, signer,
        )
        .with_context(|| format!("walk_step `{step_kind}` ({step_id})"))?;
    }

    info!(kind = %target.kind, "publish complete");
    Ok(())
}

/// The completion keys a walked step is missing: every `fields[]` entry
/// the materialized step declares `required` that neither the step's
/// existing metadata nor the Workflow's defaults already carry, filled
/// with a type-appropriate value — and ONLY those, since they go to the
/// step merge door, which keeps every key it is not sent (e39a9d2a).
/// Sibling of the sim workforce's `synth_field_value` (boss-sim), kept
/// local because core cannot depend on an orchestrator; the value
/// policy is deliberately simpler — a publish walk is a dev-mode
/// artifact, not a population.
fn synthesized_completion_metadata(step: &Value) -> serde_json::Map<String, Value> {
    let existing = step
        .get("metadata")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut out = serde_json::Map::new();
    for f in step
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if !f.get("required").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let Some(name) = f.get("name").and_then(Value::as_str) else {
            continue;
        };
        if existing.contains_key(name) {
            continue;
        }
        let ftype = f.get("field_type").and_then(Value::as_str).unwrap_or("");
        let value = match ftype {
            "number" | "integer" => json!(1),
            "boolean" => json!(true),
            "array" => json!([]),
            "object" => json!({}),
            // Fixed sentinels, not wall-clock: the walker runs at seed
            // time and must stay deterministic (no-wallclock).
            "date" => json!("2026-01-01"),
            "date-time" => json!("2026-01-01T00:00:00Z"),
            "uri" => json!("https://docs.example.internal/sop"),
            s if s.contains('|') => {
                json!(s.split('|').next().unwrap_or("").trim())
            }
            _ => json!(format!(
                "{} (walked at publish)",
                name.replace(['-', '_'], " ")
            )),
        };
        out.insert(name.to_string(), value);
    }
    out
}

/// The keys the walk WRITES on a step at completion — ONE definition
/// across every arm of `walk_step`, pinned by the platform-bundle
/// closure test below. Both 2026-09-02 boot-bricks (`sign_off_context`
/// through the task arm in the morning, `decision` through the
/// sign-off arm in the evening) were the same defect reached through
/// different arms: an arm-local completion body that didn't fill the
/// step's live-required fields. A registry row must never be able to
/// brick boot through an arm the last incident didn't happen to
/// exercise.
///
/// Every arm writes every live-required field the step is missing,
/// synthesized. On top of that:
/// - `sign-off` — `signed_by`, and a truthful `decision: "approved"`
///   wherever the step declares one unauthored (the walk IS the
///   approval; `synthesized_completion_metadata` alone would record
///   the enum's first variant, "pending", on a step the walk is about
///   to approve and complete). `signed_by` is the actor the walk runs
///   as — the record says who actually did it, which the literal it
///   replaced did not.
/// - `workflow-publish` — the full WorkflowSpec the dispatch handler
///   publishes from.
///
/// It is a PATCH, not the step's metadata (backlog e39a9d2a, design
/// 93d2bddb): `walk_step` sends it to the step merge door, which keeps
/// every stored key it is not sent, so no stored key rides along. Until
/// 2026-09-28 this returned the step's whole read metadata with these
/// keys on top, for a step PUT that replaced metadata wholesale — a
/// read-merge-write that lost any key written between the read and the
/// write. `authority_role` is no longer written here either: the
/// approve step materializes it from its Workflow row, and the merge
/// door strips it from every patch (the persisted value wins), so
/// sending it would claim a write that never happens.
fn walk_completion_patch(
    step_kind: &str,
    step: &Value,
    publish_spec: Option<&Value>,
    signer: &str,
) -> serde_json::Map<String, Value> {
    let authored = step
        .get("metadata")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut out = synthesized_completion_metadata(step);
    match step_kind {
        "sign-off" => {
            let declares_decision = step
                .get("fields")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|f| f.get("name").and_then(Value::as_str) == Some("decision"));
            if declares_decision && !authored.contains_key("decision") {
                out.insert("decision".into(), json!("approved"));
            }
            out.insert("signed_by".into(), json!(signer));
        }
        "workflow-publish" => {
            if let Some(spec) = publish_spec {
                out.insert("workflow_spec".into(), spec.clone());
            }
        }
        _ => {}
    }
    out
}

/// A step the walk must not write to, because something else already
/// decided it. A fork skips the branch not taken, and the step API
/// refuses a write to a resolved step — correctly, since a completed
/// or skipped step is a fact, not a draft.
///
/// The vocabulary is the wire's, lowercase, as `StepStatus` serializes.
/// Anything else — `pending`, `ready`, `active`, or a status this build
/// does not know — is walkable: the walk's job is to drive the packet
/// forward, and refusing an unfamiliar status would strand a bootstrap
/// on a value someone added later.
fn already_resolved(status: &str) -> bool {
    matches!(status, "completed" | "skipped")
}

/// Re-read one step. `None` when it cannot be read, which the caller
/// treats as "use the snapshot" rather than as a failure — the write
/// that follows reports its own refusal loudly, so falling back here
/// cannot hide anything, while bailing would brick a bootstrap on a
/// single flaky GET.
fn current_step(
    client: &BlockingClient,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
    job_id: &str,
    step_id: &str,
) -> Result<Option<Value>> {
    let url = jobs_url(api_base, &format!("/api/jobs/{job_id}/steps"));
    let resp = match client.get(&url).headers(headers.clone()).send() {
        Ok(r) => r,
        Err(e) => {
            warn!(%url, error = %e, "re-reading steps failed; falling back to the snapshot");
            return Ok(None);
        }
    };
    if !resp.status().is_success() {
        warn!(
            %url,
            status = %resp.status(),
            "re-reading steps failed; falling back to the snapshot"
        );
        return Ok(None);
    }
    let rows: Vec<Value> = match resp.json() {
        Ok(r) => r,
        Err(e) => {
            warn!(%url, error = %e, "step list did not parse; falling back to the snapshot");
            return Ok(None);
        }
    };
    Ok(rows
        .into_iter()
        .find(|s| s.get("id").and_then(Value::as_str) == Some(step_id)))
}

#[allow(clippy::too_many_arguments)]
fn walk_step(
    client: &BlockingClient,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
    job_id: &str,
    step_id: &str,
    step_kind: &str,
    step: &Value,
    target: &WorkflowSpec,
    dev: bool,
    signer: &str,
) -> Result<()> {
    let url = jobs_url(api_base, &format!("/api/jobs/{job_id}/steps/{step_id}"));

    // EACH WRITE THROUGH ITS OWN DOOR (backlog e39a9d2a, design
    // 93d2bddb): the keys the walk writes go to the step merge door,
    // then the status flips through a PUT that carries no metadata at
    // all. The walk used to PUT the step's whole read metadata back
    // beside the status — a read-merge-write, which the step PUT's
    // wholesale replace made a lost update against any key written
    // between the read and the write, and which the decided end state
    // (the step PUT refuses ANY metadata body) refuses outright.
    //
    // One match answers both per-kind questions — which spec the patch
    // carries, and which role (if any) stamps the step — so the kind
    // is read in one place.
    let (publish_spec, stamp_role) = match step_kind {
        "sign-off" => {
            if !dev {
                anyhow::bail!(
                    "sign-off step must be approved by a real reviewer; \
                     re-run with --dev for unattended bootstrap (development only)"
                );
            }
            (None, Some("workflow-approver"))
        }
        // The terminal step. Its metadata MUST carry the full
        // WorkflowSpec so the dispatch handler in
        // boss-jobs::http::update_step can call publish_authored — it
        // reads the STORED step, so the spec landing through the merge
        // door before the status flip is what it publishes.
        "workflow-publish" => (
            Some(
                serde_json::to_value(target)
                    .context("serializing WorkflowSpec for publish step")?,
            ),
            None,
        ),
        other => {
            // `outcome` joined `task` here when the kind gained its
            // `not-published` terminal (8686485c). It is normally
            // SKIPPED and never reaches this arm at all; it is named so
            // the warning keeps meaning "a kind nobody expected here"
            // rather than firing on a step the protocol declares.
            if !matches!(other, "task" | "outcome") {
                warn!(step_kind = %other, "unrecognized step kind on workflow-design; flipping to done");
            }
            (None, None)
        }
    };

    // An empty patch is no write: the step already holds what its
    // completion needs.
    let patch = walk_completion_patch(step_kind, step, publish_spec.as_ref(), signer);
    if !patch.is_empty() {
        let md_url = format!("{url}/metadata");
        let resp = client
            .patch(&md_url)
            .headers(headers.clone())
            .json(&Value::Object(patch))
            .send()
            .with_context(|| format!("PATCH {md_url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("PATCH {md_url} returned {status}: {body}");
        }
    }

    if let Some(role) = stamp_role {
        // Sign-off contract: metadata lands first (above), the stamp
        // attests the final shape, then the status flip below completes
        // it. The order is load-bearing: a merge that moves the shape
        // voids the stamps it leaves behind (design 87329a13).
        //
        // The `workflow-design` approve step requires the
        // `workflow-approver` authority (boss-jobs registry), so the
        // stamp's `role` must equal that — the sign-off endpoint
        // rejects any role not in `sign_offs_required`. The stamp
        // goes out under the walk's own header — a `platform-admin`
        // automation identity, which holds
        // `step-signoff:workflow-approver` via the core policy
        // defaults — so seed-time provisioning never depends on the
        // tenant's approver grants having loaded first, and the
        // stamp names the actor that made it.
        let stamp_url = format!("{url}/sign-offs");
        let resp = client
            .post(&stamp_url)
            .headers(headers.clone())
            .json(&json!({ "role": role }))
            .send()
            .with_context(|| format!("POST {stamp_url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("POST {stamp_url} returned {status}: {body}");
        }
    }

    let resp = client
        .put(&url)
        .headers(headers.clone())
        .json(&json!({ "status": "completed" }))
        .send()
        .with_context(|| format!("PUT {url}"))?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "PUT {url} → {} {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }
    info!(step_kind, "step done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authoring_job_id_marks_operator_published() {
        let body = json!({ "kind": "device-intake", "authoring_job_id": "job-abc" });
        assert_eq!(provenance_of(&body), Provenance::OperatorPublished);
    }

    #[test]
    fn missing_authoring_job_id_is_bootstrap_owned() {
        let body = json!({ "kind": "device-intake" });
        assert_eq!(provenance_of(&body), Provenance::BootstrapOwned);
        let null_id = json!({ "kind": "device-intake", "authoring_job_id": null });
        assert_eq!(provenance_of(&null_id), Provenance::BootstrapOwned);
    }

    #[test]
    fn jobs_url_trims_trailing_slash() {
        assert_eq!(
            jobs_url("http://localhost:7900/", "/api/jobs"),
            "http://localhost:7900/api/jobs"
        );
    }

    /// The live-vs-file comparison names the lint's facets (design
    /// e187198f), in the lint's order: a description edit, a step added
    /// on one side (once, in `steps.count` and `steps.titles`, never
    /// per facet against nothing), a required field turned optional
    /// (both sets move), and — since 2026-09-28, when this copy caught
    /// up with the lint (backlog e5dc276d) — a predicate, which is
    /// "every other key" and was out until the lint measured it
    /// verbatim. Identical rows compare empty.
    #[test]
    fn a_live_kind_is_compared_on_the_lints_facets() {
        let file: WorkflowSpec = serde_json::from_value(json!({
            "kind": "receive-a-sponsorship", "version": 1, "status": "active",
            "label": "Receive a sponsorship", "description": "Stripe reported a payment",
            "category": "sales", "subject_kinds": ["account"],
            "steps": [
                {"title": "received", "kind": "trigger", "ready_when": "true",
                 "title_template": "Stripe reported a payment", "fields": []},
                {"title": "reconcile", "kind": "task", "ready_when": "steps.received.done",
                 "title_template": "Match the payment",
                 "fields": [{"name": "stripe_event_id", "field_type": "string", "required": true}]}
            ],
            "metadata_schema": {}, "entitlements": {}, "metadata": {},
            "on_complete_create": [], "owning_team": "acme", "authoring_job_id": null,
            "created_at": "2026-09-18T00:00:00Z"
        }))
        .unwrap();
        let mut live = serde_json::to_value(&file).unwrap();
        assert!(workflow_changes(&live, &file).is_empty());

        live["description"] = json!("Stripe reported a payment (edited in /it/registry)");
        live["steps"][1]["ready_when"] = json!("steps.received.done && true");
        live["steps"][1]["fields"][0]["required"] = json!(false);
        live["steps"].as_array_mut().unwrap().push(
            json!({"title": "sponsored", "kind": "outcome", "ready_when": "steps.reconcile.done",
                         "title_template": "Sponsorship recognized", "fields": []}),
        );
        let changes = workflow_changes(&live, &file);
        let fields: Vec<&str> = changes.iter().map(|c| c.field.as_str()).collect();
        assert_eq!(
            fields,
            [
                "description",
                "steps.count",
                "steps.titles",
                "steps.reconcile.required",
                "steps.reconcile.optional",
                "steps.reconcile.ready_when",
            ],
            "{changes:?}"
        );
        assert_eq!(changes[1].render(), "steps.count 3 → 2");
        assert_eq!(
            changes[3].render(),
            "steps.reconcile.required  → stripe_event_id"
        );
        assert_eq!(
            changes[4].render(),
            "steps.reconcile.optional stripe_event_id:string → "
        );
        assert!(
            !fields.iter().any(|f| f.contains("sponsored")),
            "a step on one side only is one finding, in steps.titles"
        );
    }

    /// WHO runs a step is a facet (backlog 1b847556 in the lint; this
    /// copy lacked it until backlog e5dc276d): a live row lacking the
    /// agent block its file declares is named by its step, and the same
    /// block agrees though the file's budget is the integer 5 and the
    /// registry's the float 5.0. What the publish path fills when the
    /// file is silent — `authority_role`, `null`, `false`, `[]`, `{}`
    /// — and a number spelled 2 and 2.0 are named by nobody, while a
    /// workflow key the file states (`metadata`) is.
    #[test]
    fn the_agent_block_and_every_other_key_are_facets_the_publish_path_fills_are_not() {
        let file: WorkflowSpec = serde_json::from_value(json!({
            "kind": "k", "version": 1, "status": "active", "label": "K", "category": "platform",
            "subject_kinds": [], "owning_team": "platform", "created_at": "2026-09-28T00:00:00Z",
            "metadata": {"surfaces": ["system-incidents"]},
            "steps": [
                {"title": "opened", "kind": "trigger", "ready_when": "true", "duration_hours": 2},
                {"title": "proven", "kind": "task", "ready_when": "steps.opened.done",
                 "agent": {"profile": "builder", "model": "opus-5[1m]", "budget_usd": 5, "effort": "high"}}
            ]
        }))
        .unwrap();
        let mut live = serde_json::to_value(&file).unwrap();
        live["steps"][1]["agent"] = Value::Null;
        live["steps"][0]["authority_role"] = json!("platform-admin");
        live["steps"][0]["claimable"] = json!(false);
        live["steps"][0]["duration_hours"] = json!(2.0);
        live["entitlements"] = json!([]);
        live["metadata_schema"] = Value::Null;
        live["metadata"] = json!({"surfaces": ["system-design"]});
        let fields: Vec<String> = workflow_changes(&live, &file)
            .into_iter()
            .map(|c| c.field)
            .collect();
        assert_eq!(fields, ["metadata", "steps.proven.agent"]);

        live["steps"][1]["agent"] = json!({"profile": "builder", "model": "opus-5[1m]", "budget_usd": 5.0, "effort": "high"});
        live["metadata"] = json!({"surfaces": ["system-incidents"]});
        assert!(workflow_changes(&live, &file).is_empty());
    }

    /// A file silent on `description` is the lint's ABSENT line: no
    /// change against a live row that is silent too (`null`, as the
    /// registry hands it back), and a named one against live text a
    /// supersede would overwrite.
    #[test]
    fn a_file_silent_on_its_description_differs_only_from_live_text() {
        let file: WorkflowSpec = serde_json::from_value(json!({
            "kind": "k", "version": 1, "status": "active", "label": "K", "category": "platform",
            "subject_kinds": [], "owning_team": "platform", "created_at": "2026-09-28T00:00:00Z",
            "steps": []
        }))
        .unwrap();
        let mut live = serde_json::to_value(&file).unwrap();
        assert_eq!(live["description"], Value::Null);
        assert!(workflow_changes(&live, &file).is_empty());
        live["description"] = json!("edited in the registry");
        let changes = workflow_changes(&live, &file);
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0].render(), "description edited in the registry → ");
    }

    #[test]
    fn a_publish_outcome_names_what_it_kept() {
        let out = WorkflowPublishOutcome {
            published: vec!["x".into()],
            superseded: vec![],
            kept: vec![KeptRow {
                id: "receive-a-sponsorship".into(),
                differs: vec!["description".into()],
            }],
            unchanged: 2,
        };
        assert_eq!(
            out.summary(),
            "1 published (x), 2 already as declared; kept: receive-a-sponsorship differs on \
             description (the instance is the truth; --take workflows overwrites)"
        );
    }
}

#[cfg(test)]
mod walker_tests {
    use super::*;
    use serde_json::json;

    /// The 2026-09-02 crash-loop shape verbatim: a live-registry task
    /// step requiring `sign_off_context` that the walker's bare
    /// completion missed. The synthesized patch must carry it — and
    /// ONLY it: the patch goes to the step merge door, which keeps the
    /// stored keys, so re-sending them would only race a concurrent
    /// writer (backlog e39a9d2a).
    #[test]
    fn the_walker_fills_a_live_required_field_and_sends_nothing_else() {
        let step = json!({
            "metadata": { "already": "here" },
            "fields": [
                {"name": "sign_off_context", "field_type": "string", "required": true},
                {"name": "optional_note", "field_type": "string", "required": false}
            ]
        });
        let md = synthesized_completion_metadata(&step);
        assert!(
            md.get("sign_off_context")
                .and_then(|v| v.as_str())
                .is_some()
        );
        assert!(
            !md.contains_key("already"),
            "a stored key the walk does not change is not re-sent"
        );
        assert!(!md.contains_key("optional_note"), "optional stays unfilled");
    }

    /// Nothing required, or everything already present -> EMPTY map,
    /// so the walk writes no metadata and only flips the status.
    #[test]
    fn a_satisfied_step_synthesizes_nothing() {
        assert!(synthesized_completion_metadata(&json!({})).is_empty());
        let satisfied = json!({
            "metadata": { "sign_off_context": "authored" },
            "fields": [{"name": "sign_off_context", "field_type": "string", "required": true}]
        });
        assert!(synthesized_completion_metadata(&satisfied).is_empty());
    }

    /// Type-appropriateness for the shapes the registry declares —
    /// enums take their first variant, numbers count, dates are fixed
    /// sentinels (the walker is seed-time and must be deterministic).
    #[test]
    fn synthesized_values_are_type_appropriate() {
        let step = json!({ "fields": [
            {"name": "verdict", "field_type": "pass|fail", "required": true},
            {"name": "count", "field_type": "number", "required": true},
            {"name": "when", "field_type": "date", "required": true}
        ]});
        let md = synthesized_completion_metadata(&step);
        assert_eq!(md.get("verdict"), Some(&json!("pass")));
        assert_eq!(md.get("count"), Some(&json!(1)));
        assert_eq!(md.get("when"), Some(&json!("2026-01-01")));
    }

    /// The signer is the actor the header names; a header naming no
    /// actor is refused rather than signed as nobody (backlog 3c23662d).
    #[test]
    fn the_walk_signs_as_the_actor_its_header_names() {
        let header = json!({"id": "automation:tenant-seed", "role": "platform-admin"}).to_string();
        assert_eq!(signer_of(&header).unwrap(), "automation:tenant-seed");
        assert!(signer_of(r#"{"role":"platform-admin"}"#).is_err());
        assert!(signer_of(r#"{"id":""}"#).is_err());
        assert!(signer_of("not json").is_err());
    }

    /// The 2026-09-02 EVENING crash-loop shape verbatim: the
    /// workflow-design `approve` step requires `decision` (cdfe2e1a)
    /// and the sign-off arm's bare completion missed it. The walk IS
    /// the approval, so the record must say `approved` — not the
    /// enum's first variant, which is `pending`.
    #[test]
    fn the_sign_off_walk_records_a_truthful_decision() {
        let step = json!({
            "metadata": { "authority_role": "workflow-approver" },
            "fields": [{
                "name": "decision",
                "field_type": "pending|approved|rejected|changes-requested",
                "required": true
            }]
        });
        let md = walk_completion_patch("sign-off", &step, None, "automation:bootstrap");
        assert_eq!(md.get("decision"), Some(&json!("approved")));
        assert!(
            !md.contains_key("authority_role"),
            "the step holds its authority from its Workflow row, and the merge door strips it"
        );
        assert_eq!(
            md.get("signed_by"),
            Some(&json!("automation:bootstrap")),
            "the walk signs as the actor it runs as, never a named person"
        );
    }

    /// An authored decision is a record already made; the walk must
    /// not overwrite it, so its patch does not name the key at all.
    #[test]
    fn an_authored_decision_survives_the_walk() {
        let step = json!({
            "metadata": { "decision": "changes-requested" },
            "fields": [{
                "name": "decision",
                "field_type": "pending|approved|rejected|changes-requested",
                "required": true
            }]
        });
        let md = walk_completion_patch("sign-off", &step, None, "automation:bootstrap");
        assert!(!md.contains_key("decision"), "{md:?}");
    }

    /// The walk must not write to a step a FORK already resolved.
    ///
    /// `workflow-design` gained a `not-published` terminal (8686485c),
    /// which is Skipped the moment `approve` completes carrying
    /// `decision = "approved"` — the decision this very walk writes. The
    /// step API refuses a write to a resolved step and `walk_step` bails
    /// on any non-success, so without this guard the first boot after
    /// that version went live would fail the bootstrap of every tenant
    /// Workflow.
    ///
    /// Only `completed` and `skipped` are resolved. An unfamiliar
    /// status is walkable on purpose: refusing one would strand a
    /// bootstrap on a value added after this build shipped.
    #[test]
    fn only_a_resolved_step_is_skipped_by_the_walk() {
        assert!(already_resolved("completed"));
        assert!(already_resolved("skipped"));
        for walkable in ["pending", "ready", "active", "", "some-future-status"] {
            assert!(
                !already_resolved(walkable),
                "`{walkable}` must still be walked"
            );
        }
    }

    /// …and the kind actually declares a step that can be skipped, so
    /// the guard above is load-bearing rather than decorative. If a
    /// later version flattens the fork this reads as a prompt to check
    /// whether the guard is still earning its place, not as a failure
    /// to route around.
    #[test]
    fn workflow_design_has_a_branch_the_walk_can_meet_skipped() {
        let specs = crate::seed_loader::load_workflows(crate::registry::platform_bundle_path())
            .expect("platform bundle loads");
        let design = specs
            .iter()
            .find(|s| s.kind == "workflow-design")
            .expect("workflow-design present in the bundle");
        let terminals: Vec<&str> = design
            .steps
            .iter()
            .filter(|s| s.terminal.is_some())
            .map(|s| s.title.as_str())
            .collect();
        assert!(
            terminals.len() >= 2,
            "a rejection needs somewhere to go: terminals are {terminals:?}"
        );
    }

    /// A sign-off with no declared fields still carries the signer,
    /// and nothing it does not write: no stored key re-sent, and no
    /// stray `decision` on a step that never declared one.
    #[test]
    fn a_fieldless_sign_off_writes_its_signer_and_adds_no_decision() {
        let step = json!({ "metadata": { "already": "here" } });
        let md = walk_completion_patch("sign-off", &step, None, "automation:bootstrap");
        assert_eq!(
            md,
            serde_json::Map::from_iter([("signed_by".to_string(), json!("automation:bootstrap"))])
        );
    }

    /// Other live-required fields on a sign-off step synthesize the
    /// same way the task arm fills them — a registry row must never
    /// be able to brick boot through EITHER arm.
    #[test]
    fn the_sign_off_walk_fills_other_required_fields() {
        let step = json!({
            "fields": [
                {"name": "decision", "field_type": "pending|approved", "required": true},
                {"name": "review_notes", "field_type": "string", "required": true}
            ]
        });
        let md = walk_completion_patch("sign-off", &step, None, "automation:bootstrap");
        assert_eq!(md.get("decision"), Some(&json!("approved")));
        assert!(md.get("review_notes").and_then(|v| v.as_str()).is_some());
    }

    /// The publish arm writes the spec WITH the required fields, not
    /// instead of them — "instead" is the same brick-boot defect the
    /// other two arms already had — and without the stored keys, which
    /// the merge door keeps.
    #[test]
    fn the_publish_walk_writes_the_spec_beside_the_required_fields() {
        let step = json!({
            "metadata": { "already": "here" },
            "fields": [{"name": "release_note", "field_type": "string", "required": true}]
        });
        let spec = json!({"kind": "x"});
        let md = walk_completion_patch(
            "workflow-publish",
            &step,
            Some(&spec),
            "automation:bootstrap",
        );
        assert_eq!(md.get("workflow_spec"), Some(&spec));
        assert!(!md.contains_key("already"));
        assert!(md.get("release_note").is_some());
    }

    /// CLOSURE over the platform bundle: every step of the one kind
    /// the walk creates (`workflow-design`) must be completable by
    /// the walk — the metadata each arm leaves (the step's defaults
    /// with the walk's patch merged over them, as the merge door
    /// stores it) must satisfy both the kind bundle's fields and the
    /// step's authored fields, exactly the union the completion
    /// handler validates.
    ///
    /// Both 2026-09-02 boot-bricks (`sign_off_context` through the
    /// task arm in the morning, `decision` through the sign-off arm
    /// in the evening) fail HERE first. Production boot must never
    /// again be the first place a bundle's required field meets the
    /// walker.
    #[test]
    fn every_workflow_design_step_is_completable_by_the_walk() {
        let specs = crate::seed_loader::load_workflows(crate::registry::platform_bundle_path())
            .expect("platform bundle loads");
        let registry = crate::step_registry::StepRegistry::v1();
        let design = specs
            .iter()
            .find(|s| s.kind == "workflow-design")
            .expect("workflow-design present in the bundle");
        let dummy_spec = json!({"kind": "closure-test-target"});
        // Both 2026-09-02 boot-bricks were ONE step's metadata. A
        // `workflow-design` that lost its steps would pass this while
        // covering none of them — measured, it does.
        boss_testing::assert_roster_floor!(
            design.steps,
            4,
            "`workflow-design`'s steps (5 on 2026-09-11)"
        );
        for step in &design.steps {
            let step_value = json!({
                "metadata": step.metadata_defaults,
                "fields": serde_json::to_value(&step.fields).unwrap(),
            });
            let mut stored = step_value["metadata"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            stored.extend(walk_completion_patch(
                &step.kind,
                &step_value,
                Some(&dummy_spec),
                "automation:bootstrap",
            ));
            let md = Value::Object(stored);
            if let Err(errors) = registry.validate_metadata(&step.kind, &md).and_then(|()| {
                crate::step_registry::StepRegistry::validate_authored_fields(&step.fields, &md)
            }) {
                panic!(
                    "workflow-design step `{}` (kind `{}`) is not completable by the walk \
                     ({errors:?}) — a boot walking an unpublished kind would 400 here and \
                     crash-loop the stack",
                    step.title, step.kind,
                );
            }
        }
    }
}
