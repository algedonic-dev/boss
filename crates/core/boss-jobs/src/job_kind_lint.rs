//! Static validation for `JobKindSpec` rows — the **viability lint**.
//!
//! A JobKind is a program written in the StepType alphabet; its step
//! DAG is implicit in the steps' `ready_when` predicates. The lint
//! proves the program is well-formed before it can run:
//!
//! - **Phase 0 — metadata shapes.** Every value in a step's
//!   `metadata_defaults` matches its StepType field's declared type
//!   (catches a not-in-enum value like `channel = "in-person"` at
//!   author time instead of mid-run).
//! - **Phase 1 — structural.** At least one trigger (`ready_when =
//!   "true"`), at least one terminal, every `steps.<slug>` reference
//!   resolves, the dependency graph is acyclic, and every leaf is a
//!   declared terminal.
//! - **Phase 2 — reachability.** Every step is reachable forward from
//!   some trigger and backward from some terminal — no dead code.
//! - **Phase 3 — fork coverage.** Where a step is a fork point (≥2
//!   successors discriminate on its outcome), every value of the
//!   discriminating enum is handled by some successor, or a wildcard
//!   fallback covers the open-ended case.
//!
//! Runs at author time (`/system/job-kinds`), publish time (the
//! `job-kind-publish` dispatch path), and boot time (boss-jobs-api
//! refuses to start against a broken registry).

use crate::registry::{JobKindSpec, StepSpec, predicate_step_refs};
use crate::step_registry::StepRegistry;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// One viability failure. `step` is the offending step slug (empty
/// for whole-JobKind structural failures).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobKindLintError {
    pub job_kind: String,
    pub step: String,
    pub reason: String,
}

impl std::fmt::Display for JobKindLintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.step.is_empty() {
            write!(f, "[{}] {}", self.job_kind, self.reason)
        } else {
            write!(
                f,
                "[{}] step `{}`: {}",
                self.job_kind, self.step, self.reason
            )
        }
    }
}

/// Validate a single JobKindSpec. Returns every violation found; an
/// empty Vec means the JobKind is viable.
pub fn validate_job_kind(spec: &JobKindSpec, registry: &StepRegistry) -> Vec<JobKindLintError> {
    let mut errs = Vec::new();
    // Phase 0 — metadata default value shapes.
    for step in &spec.steps {
        check_metadata_defaults_values(spec, step, registry, &mut errs);
    }
    // Phases 1–3 — viability of the predicate graph.
    check_viability(spec, registry, &mut errs);
    errs
}

/// Validate every JobKindSpec in a list. One call, every error
/// reported — used by the tenant seed bundles (`load_job_kinds_*`
/// returns a Vec).
pub fn validate_all(specs: &[JobKindSpec], registry: &StepRegistry) -> Vec<JobKindLintError> {
    let mut errs = Vec::new();
    for spec in specs {
        errs.extend(validate_job_kind(spec, registry));
    }
    errs
}

fn err(spec: &JobKindSpec, step: &str, reason: impl Into<String>) -> JobKindLintError {
    JobKindLintError {
        job_kind: spec.kind.clone(),
        step: step.to_string(),
        reason: reason.into(),
    }
}

// ---------------------------------------------------------------------------
// Phases 1–3 — viability
// ---------------------------------------------------------------------------

fn check_viability(spec: &JobKindSpec, registry: &StepRegistry, errs: &mut Vec<JobKindLintError>) {
    let slugs: HashSet<&str> = spec.steps.iter().map(|s| s.title.as_str()).collect();

    // Duplicate slugs make every `steps.<slug>` reference ambiguous.
    let mut seen = HashSet::new();
    for s in &spec.steps {
        if !seen.insert(s.title.as_str()) {
            errs.push(err(
                spec,
                &s.title,
                "duplicate step title (slugs must be unique)",
            ));
        }
    }

    // ----- Phase 1: structural invariants -----
    let triggers: Vec<&StepSpec> = spec
        .steps
        .iter()
        .filter(|s| s.ready_when.trim() == "true")
        .collect();
    if triggers.is_empty() {
        errs.push(err(
            spec,
            "",
            "no trigger step (none with ready_when = \"true\")",
        ));
    }
    let terminals: Vec<&StepSpec> = spec.steps.iter().filter(|s| s.terminal.is_some()).collect();
    if terminals.is_empty() {
        errs.push(err(spec, "", "no terminal step (none carries an outcome)"));
    }

    // Predicate refs must resolve, and must parse.
    for s in &spec.steps {
        if s.ready_when.trim().is_empty() {
            errs.push(err(spec, &s.title, "empty ready_when predicate"));
            continue;
        }
        if boss_expr::parse(&s.ready_when).is_err() {
            errs.push(err(
                spec,
                &s.title,
                format!("ready_when does not parse: `{}`", s.ready_when),
            ));
            continue;
        }
        for r in predicate_step_refs(&s.ready_when) {
            if !slugs.contains(r.as_str()) {
                errs.push(err(
                    spec,
                    &s.title,
                    format!("ready_when references unknown step `{r}`"),
                ));
            }
        }
    }

    // Dependency edges: A → B iff B.ready_when references A.
    let deps: HashMap<String, Vec<String>> = spec
        .steps
        .iter()
        .map(|s| (s.title.clone(), predicate_step_refs(&s.ready_when)))
        .collect();

    if let Some(cycle) = find_cycle(&spec.steps, &deps) {
        errs.push(err(
            spec,
            "",
            format!("ready_when predicates form a cycle: {}", cycle.join(" → ")),
        ));
        return; // reachability + fork coverage assume acyclic.
    }

    // Every leaf (referenced by no successor) must be a declared
    // terminal — otherwise it dead-ends the Job.
    let referenced: HashSet<&str> = deps.values().flatten().map(|s| s.as_str()).collect();
    for s in &spec.steps {
        let is_leaf = !referenced.contains(s.title.as_str());
        if is_leaf && s.terminal.is_none() {
            errs.push(err(
                spec,
                &s.title,
                "leaf step is not a terminal (nothing depends on it and it carries no outcome)",
            ));
        }
    }

    // ----- Phase 2: reachability -----
    let forward = reachable_forward(&spec.steps, &deps, &triggers);
    let backward = reachable_backward(&spec.steps, &deps, &terminals);
    for s in &spec.steps {
        if !forward.contains(s.title.as_str()) {
            errs.push(err(spec, &s.title, "unreachable from any trigger"));
        } else if !backward.contains(s.title.as_str()) {
            errs.push(err(spec, &s.title, "cannot reach any terminal"));
        }
    }

    // ----- Phase 3: fork coverage -----
    check_fork_coverage(spec, registry, &deps, errs);
}

/// DFS cycle detection over the predicate dependency graph. Returns
/// the offending chain (slugs) if a cycle exists.
fn find_cycle(steps: &[StepSpec], deps: &HashMap<String, Vec<String>>) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    fn dfs(
        node: &str,
        deps: &HashMap<String, Vec<String>>,
        state: &mut HashMap<String, Mark>,
        stack: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        state.insert(node.to_string(), Mark::Visiting);
        stack.push(node.to_string());
        for dep in deps.get(node).into_iter().flatten() {
            // Edge node depends-on dep; walk toward predecessors to
            // surface a readable cycle chain.
            match state.get(dep.as_str()).copied() {
                Some(Mark::Visiting) => {
                    let mut chain = stack.clone();
                    chain.push(dep.clone());
                    return Some(chain);
                }
                Some(Mark::Done) => {}
                None => {
                    if let Some(c) = dfs(dep, deps, state, stack) {
                        return Some(c);
                    }
                }
            }
        }
        stack.pop();
        state.insert(node.to_string(), Mark::Done);
        None
    }

    let mut state: HashMap<String, Mark> = HashMap::new();
    let mut stack: Vec<String> = Vec::new();
    for s in steps {
        if !state.contains_key(s.title.as_str()) {
            if let Some(c) = dfs(&s.title, deps, &mut state, &mut stack) {
                return Some(c);
            }
            stack.clear();
        }
    }
    None
}

/// BFS forward from the triggers: a step B is reachable once any step
/// it depends on is reachable (or it is itself a trigger).
fn reachable_forward(
    steps: &[StepSpec],
    deps: &HashMap<String, Vec<String>>,
    triggers: &[&StepSpec],
) -> HashSet<String> {
    let mut reached: HashSet<String> = triggers.iter().map(|t| t.title.clone()).collect();
    let mut changed = true;
    while changed {
        changed = false;
        for s in steps {
            if reached.contains(s.title.as_str()) {
                continue;
            }
            let any_dep_reached = deps
                .get(s.title.as_str())
                .into_iter()
                .flatten()
                .any(|d| reached.contains(d.as_str()));
            if any_dep_reached {
                reached.insert(s.title.clone());
                changed = true;
            }
        }
    }
    reached
}

/// BFS backward from the terminals: a step A can reach a terminal if
/// it is one, or if some step that depends on A can.
fn reachable_backward(
    steps: &[StepSpec],
    deps: &HashMap<String, Vec<String>>,
    terminals: &[&StepSpec],
) -> HashSet<String> {
    let mut reached: HashSet<String> = terminals.iter().map(|t| t.title.clone()).collect();
    let mut changed = true;
    while changed {
        changed = false;
        for s in steps {
            for dep in deps.get(s.title.as_str()).into_iter().flatten() {
                // s depends on dep, so dep can reach whatever s can.
                if reached.contains(s.title.as_str()) && !reached.contains(dep.as_str()) {
                    reached.insert(dep.clone());
                    changed = true;
                }
            }
        }
    }
    reached
}

/// Phase 3: for every fork point (a step whose outcome ≥2 successors
/// discriminate on), prove the successors cover every value the
/// discriminating enum can take, or that a wildcard fallback handles
/// the open-ended case.
fn check_fork_coverage(
    spec: &JobKindSpec,
    registry: &StepRegistry,
    deps: &HashMap<String, Vec<String>>,
    errs: &mut Vec<JobKindLintError>,
) {
    for fork in &spec.steps {
        let successors: Vec<&StepSpec> = spec
            .steps
            .iter()
            .filter(|s| {
                deps.get(s.title.as_str())
                    .is_some_and(|d| d.contains(&fork.title))
            })
            .collect();
        if successors.len() < 2 {
            continue; // not a fork.
        }
        // Which of fork's metadata fields do successors branch on?
        let fields = discriminator_fields(&fork.title, &successors);
        // A fallback successor is one satisfied by `fork` completing
        // with NO particular metadata (references fork.done only).
        let has_fallback = successors.iter().any(|s| {
            eval_pred(&s.ready_when, &synth_fork_payload(spec, &fork.title, None)) == Some(true)
        });

        for field in &fields {
            match enum_domain(registry, &fork.kind, field) {
                Some(values) => {
                    for v in &values {
                        let payload = synth_fork_payload(
                            spec,
                            &fork.title,
                            Some((field, Value::String(v.clone()))),
                        );
                        let covered = successors
                            .iter()
                            .any(|s| eval_pred(&s.ready_when, &payload) == Some(true));
                        if !covered {
                            errs.push(err(
                                spec,
                                &fork.title,
                                format!(
                                    "fork outcome {field} = \"{v}\" is handled by no successor (orphan outcome)"
                                ),
                            ));
                        }
                    }
                }
                None => {
                    // Free-text / open-ended discriminator: D9 requires
                    // an explicit wildcard fallback.
                    if !has_fallback {
                        errs.push(err(
                            spec,
                            &fork.title,
                            format!(
                                "fork over free-text `{field}` needs a fallback successor (ready_when = \"steps.{}.done\")",
                                fork.title
                            ),
                        ));
                    }
                }
            }
        }
    }
}

/// The metadata fields of `fork` that the successors' predicates read
/// (`steps.<fork>.metadata.<field>`).
fn discriminator_fields(fork: &str, successors: &[&StepSpec]) -> Vec<String> {
    let mut fields = Vec::new();
    for s in successors {
        let Ok(expr) = boss_expr::parse(&s.ready_when) else {
            continue;
        };
        for path in boss_expr::references(&expr) {
            if path.len() == 4 && path[0] == "steps" && path[1] == fork && path[2] == "metadata" {
                let f = path[3].clone();
                if !fields.contains(&f) {
                    fields.push(f);
                }
            }
        }
    }
    fields
}

/// The declared enum values of `field` on StepType `kind`, if the
/// field_type is pipe-shaped (`a|b|c`). `None` for free-text fields.
fn enum_domain(registry: &StepRegistry, kind: &str, field: &str) -> Option<Vec<String>> {
    let st = registry.get(kind)?;
    let f = st.fields.iter().find(|f| f.name == field)?;
    if f.field_type.contains('|') {
        Some(f.field_type.split('|').map(|s| s.to_string()).collect())
    } else {
        None
    }
}

/// Build a synthetic predicate payload where `fork` has completed
/// (optionally with one metadata field set), every other step is
/// not-done. Used to probe successor coverage at lint time.
fn synth_fork_payload(spec: &JobKindSpec, fork: &str, field: Option<(&str, Value)>) -> Value {
    let mut steps = serde_json::Map::new();
    for s in &spec.steps {
        let done = s.title == fork;
        let metadata = match (done, &field) {
            (true, Some((f, v))) => serde_json::json!({ *f: v.clone() }),
            _ => serde_json::json!({}),
        };
        steps.insert(
            s.title.clone(),
            serde_json::json!({ "done": done, "metadata": metadata }),
        );
    }
    serde_json::json!({
        "subject": {},
        "job": { "metadata": {} },
        "steps": Value::Object(steps),
    })
}

fn eval_pred(src: &str, payload: &Value) -> Option<bool> {
    let expr = boss_expr::parse(src).ok()?;
    let ctx = boss_expr::Context {
        payload,
        helpers: &boss_expr::NoHelpers,
    };
    boss_expr::eval(&expr, &ctx).ok()?.as_bool()
}

// ---------------------------------------------------------------------------
// Phase 0 — metadata default value shapes (carried over from v1)
// ---------------------------------------------------------------------------

/// Every value present in a step's `metadata_defaults` matches the
/// StepType field's declared `field_type`.
///
/// Catches a bad enum literal — e.g. `channel = "in-person"` when the
/// field's enum is `email|phone|meeting|demo|other` — at
/// JobKind-load time, so it fails fast instead of surfacing at
/// step-completion time mid-run.
///
/// Permissive about placeholders: empty strings on date / date-time /
/// uri fields are accepted (seeds leave these blank for
/// completion-time fill). Templated values like `"{subject.id}"` pass
/// through. Unknown field names are ignored. Skipped for unknown step
/// kinds (a separate error class).
fn check_metadata_defaults_values(
    spec: &JobKindSpec,
    step: &StepSpec,
    registry: &StepRegistry,
    errs: &mut Vec<JobKindLintError>,
) {
    let Some(step_type) = registry.get(&step.kind) else {
        return;
    };
    let Some(metadata) = step.metadata_defaults.as_object() else {
        return;
    };

    for field in &step_type.fields {
        let Some(value) = metadata.get(field.name) else {
            continue;
        };
        if is_placeholder_default(field.field_type, value) {
            continue;
        }
        if let Some(reason) = check_field_value(field.field_type, value, field.name) {
            errs.push(JobKindLintError {
                job_kind: spec.kind.clone(),
                step: step.title.clone(),
                reason,
            });
        }
    }

    // Inline authoring: step-authored fields get the same
    // defaults-shape check as the kind bundle's fields.
    for field in &step.fields {
        let Some(value) = metadata.get(&field.name) else {
            continue;
        };
        if is_placeholder_default(&field.field_type, value) {
            continue;
        }
        if let Some(reason) = check_field_value(&field.field_type, value, &field.name) {
            errs.push(JobKindLintError {
                job_kind: spec.kind.clone(),
                step: step.title.clone(),
                reason,
            });
        }
    }
}

/// True when the value is an obvious placeholder rather than a real
/// default (e.g. `""` for a date field a downstream step populates at
/// completion time).
fn is_placeholder_default(field_type: &str, value: &Value) -> bool {
    matches!(
        (field_type, value),
        ("date" | "date-time" | "uri", Value::String(s)) if s.is_empty()
    )
}

/// Per-field type check shaped for the lint surface. Template tokens
/// (`{subject.id}`, `{day_minus_13}`) stand in for materialize-time
/// values — accept them; the runtime type-checks the expanded value.
fn check_field_value(field_type: &str, value: &Value, field_name: &str) -> Option<String> {
    if let Value::String(s) = value
        && is_template_token(s)
    {
        return None;
    }
    let ok = match field_type {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "date" => value.as_str().is_some_and(|s| s.len() == 10),
        "date-time" => value.as_str().is_some_and(|s| s.len() >= 19),
        "uri" => value.is_string(),
        s if s.contains('|') => {
            let allowed: Vec<&str> = s.split('|').collect();
            value.as_str().is_some_and(|v| allowed.contains(&v))
        }
        _ => true,
    };
    if ok {
        return None;
    }
    Some(format!(
        "metadata_defaults field `{}` value {} does not match declared type `{}`",
        field_name,
        truncate_value(value),
        field_type,
    ))
}

/// True iff the value is a single template token like `{subject.id}`
/// or `{day_minus_13}` — expanded at materialize-time, so the
/// seed-time lint can't type-check it.
fn is_template_token(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 3 || !s.starts_with('{') || !s.ends_with('}') {
        return false;
    }
    let inner = &s[1..s.len() - 1];
    !inner.contains(['{', '}', ' ', '\t', '\n'])
}

/// Render a JSON value short enough to fit in an error message.
fn truncate_value(value: &Value) -> String {
    let s = value.to_string();
    if s.len() > 80 {
        format!("{}…", &s[..77])
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{StepSpec, Terminal};
    use serde_json::json;

    /// Minimal viable two-step JobKind: a trigger that flows into a
    /// terminal. Passes all phases.
    fn viable_spec(kind: &str) -> JobKindSpec {
        JobKindSpec::platform_seed(
            kind,
            kind,
            "test",
            vec!["asset".into()],
            vec![
                StepSpec {
                    title: "start".into(),
                    kind: "trigger".into(),
                    ready_when: "true".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "finish".into(),
                    kind: "task".into(),
                    ready_when: "steps.start.done".into(),
                    terminal: Some(Terminal {
                        outcome: "done".into(),
                    }),
                    ..Default::default()
                },
            ],
        )
    }

    #[test]
    fn minimal_viable_jobkind_passes() {
        let reg = StepRegistry::v1();
        assert!(validate_job_kind(&viable_spec("ok"), &reg).is_empty());
    }

    #[test]
    fn missing_trigger_fails() {
        let reg = StepRegistry::v1();
        let mut spec = viable_spec("no-trigger");
        spec.steps[0].ready_when = "steps.finish.done".into(); // no "true"
        let errs = validate_job_kind(&spec, &reg);
        assert!(errs.iter().any(|e| e.reason.contains("no trigger")));
    }

    #[test]
    fn missing_terminal_fails() {
        let reg = StepRegistry::v1();
        let mut spec = viable_spec("no-terminal");
        spec.steps[1].terminal = None;
        let errs = validate_job_kind(&spec, &reg);
        assert!(errs.iter().any(|e| e.reason.contains("no terminal")));
    }

    #[test]
    fn dangling_reference_fails() {
        let reg = StepRegistry::v1();
        let mut spec = viable_spec("dangling");
        spec.steps[1].ready_when = "steps.ghost.done".into();
        let errs = validate_job_kind(&spec, &reg);
        assert!(
            errs.iter()
                .any(|e| e.reason.contains("unknown step `ghost`"))
        );
    }

    #[test]
    fn cycle_fails() {
        let reg = StepRegistry::v1();
        let mut spec = viable_spec("cyclic");
        // start depends on finish, finish depends on start.
        spec.steps[0].ready_when = "steps.finish.done".into();
        spec.steps[1].ready_when = "steps.start.done".into();
        let errs = validate_job_kind(&spec, &reg);
        assert!(errs.iter().any(|e| e.reason.contains("cycle")));
    }

    #[test]
    fn unreachable_step_fails() {
        let reg = StepRegistry::v1();
        let mut spec = viable_spec("unreachable");
        // An island step depending on nothing reachable and reaching
        // no terminal.
        spec.steps.push(StepSpec {
            title: "island".into(),
            kind: "task".into(),
            ready_when: "steps.island.done".into(), // self-ref → its own cycle guard
            ..Default::default()
        });
        let errs = validate_job_kind(&spec, &reg);
        assert!(!errs.is_empty());
    }

    #[test]
    fn fork_orphan_outcome_is_caught() {
        // approval.decision is an enum approved|rejected|changes-requested.
        // Cover only "approved" → the other two are orphan outcomes.
        let reg = StepRegistry::v1();
        let spec = JobKindSpec::platform_seed(
            "fork",
            "fork",
            "test",
            vec!["asset".into()],
            vec![
                StepSpec {
                    title: "decide".into(),
                    kind: "sign-off".into(),
                    ready_when: "true".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "ship".into(),
                    kind: "task".into(),
                    ready_when: "steps.decide.metadata.decision = \"approved\"".into(),
                    terminal: Some(Terminal {
                        outcome: "shipped".into(),
                    }),
                    ..Default::default()
                },
                StepSpec {
                    title: "scrap".into(),
                    kind: "task".into(),
                    ready_when: "steps.decide.metadata.decision = \"rejected\"".into(),
                    terminal: Some(Terminal {
                        outcome: "scrapped".into(),
                    }),
                    ..Default::default()
                },
            ],
        );
        let errs = validate_job_kind(&spec, &reg);
        assert!(
            errs.iter()
                .any(|e| e.reason.contains("orphan outcome")
                    && e.reason.contains("changes-requested")),
            "expected changes-requested orphan, got: {errs:?}"
        );
    }

    #[test]
    fn outreach_channel_outside_enum_fails_at_load_time() {
        let reg = StepRegistry::v1();
        let mut spec = viable_spec("regression-21");
        spec.steps[1].kind = "outreach".into();
        spec.steps[1].metadata_defaults =
            json!({ "channel": "in-person", "recipient_id": "{subject.id}" });
        let errs = validate_job_kind(&spec, &reg);
        assert!(
            errs.iter()
                .any(|e| e.reason.contains("channel") && e.reason.contains("in-person")),
            "expected channel enum violation, got: {errs:?}"
        );
    }

    #[test]
    fn templated_subject_id_passes_string_field() {
        let reg = StepRegistry::v1();
        let mut spec = viable_spec("campaign");
        spec.steps[1].kind = "outreach".into();
        spec.steps[1].metadata_defaults =
            json!({ "channel": "email", "recipient_id": "{subject.id}" });
        // No Phase-0 violation (other phases still pass for this shape).
        assert!(
            !validate_job_kind(&spec, &reg)
                .iter()
                .any(|e| e.reason.contains("metadata_defaults")),
            "{{subject.id}} is a valid string template"
        );
    }
}
