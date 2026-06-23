//! Rule registry — D2 of dispatcher-as-event-router.
//!
//! A `Rule` is one TOML row: a name, the topic it listens for, an optional
//! `when` predicate, and an ordered list of side-effect invocations (`do`).
//! The registry holds the parsed rules and answers "which rules match
//! this incoming event."
//!
//! The matching layer is pure: given `(topic, payload)` and a helper
//! table, it returns the rules whose `on_event` topic matches AND whose
//! `when` predicate evaluates true, with each rule's `do` args
//! pre-evaluated to concrete `Value`s. No I/O, no NATS, no handler
//! dispatch — those live in the next pass.

use super::expr::{self, Expr, HelperResolver, Value};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Raw TOML shape — deserializes directly from the registry file
// ---------------------------------------------------------------------------

/// Top-level TOML document: `[[rule]] ...` blocks.
#[derive(Debug, Serialize, Deserialize)]
pub struct RawRegistry {
    #[serde(default, rename = "rule")]
    pub rules: Vec<RawRule>,
}

/// One rule as written in TOML. Strings get parsed into Expr at
/// registry-load time so authoring errors surface up front.
#[derive(Debug, Serialize, Deserialize)]
pub struct RawRule {
    pub name: String,
    pub on_event: String,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default, rename = "do")]
    pub do_steps: Vec<RawDoStep>,
    #[serde(default)]
    pub delay: Option<String>,
    #[serde(default = "default_version")]
    pub version: u32,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RawDoStep {
    pub handler: String,
    #[serde(default)]
    pub args: HashMap<String, String>,
}

/// Parse the raw TOML rule registry WITHOUT building the runtime Expr
/// trees — the read-only `/api/dispatcher/rules` surface serves the
/// rules verbatim (name, on_event, when, do/args) for the cascade
/// visualization. The runtime dispatch path uses `Registry::from_toml`,
/// which additionally compiles the predicates.
pub fn parse_raw(src: &str) -> Result<RawRegistry, RegistryError> {
    toml::from_str(src).map_err(|e| RegistryError::Toml(e.to_string()))
}

// ---------------------------------------------------------------------------
// Parsed shape — what the matcher walks at runtime
// ---------------------------------------------------------------------------

/// A rule with `when` / `args` already parsed into Expr trees. Built once at
/// registry load; runtime cost is just walking the trees.
#[derive(Debug, Clone)]
pub struct Rule {
    pub name: String,
    pub on_event: TopicPattern,
    pub when: Option<Expr>,
    pub do_steps: Vec<DoStep>,
    pub delay: Option<String>,
    pub version: u32,
}

#[derive(Debug, Clone)]
pub struct DoStep {
    pub handler: String,
    /// Args evaluate at match time. Each entry's value is an Expr tree;
    /// the matcher returns the evaluated values keyed by the same name.
    pub args: Vec<(String, Expr)>,
}

// ---------------------------------------------------------------------------
// Topic patterns
// ---------------------------------------------------------------------------

/// NATS-style hierarchical topic match. Segments separated by `.`:
/// - `*` matches exactly one segment
/// - `>` matches one or more trailing segments (only allowed at the end)
/// - bareword matches that segment literally
///
/// Example: `step.done.*` matches `step.done.procurement` but not
/// `step.done.procurement.detail`. `step.done.>` matches both.
#[derive(Debug, Clone)]
pub struct TopicPattern {
    raw: String,
    segments: Vec<TopicSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TopicSegment {
    Literal(String),
    SingleWildcard,
    TrailingWildcard,
}

impl TopicPattern {
    pub fn parse(raw: &str) -> Result<Self, RegistryError> {
        let segments: Result<Vec<TopicSegment>, RegistryError> = raw
            .split('.')
            .enumerate()
            .map(|(i, seg)| match seg {
                "*" => Ok(TopicSegment::SingleWildcard),
                ">" => Ok(TopicSegment::TrailingWildcard),
                "" => Err(RegistryError::InvalidTopic {
                    topic: raw.to_string(),
                    reason: "empty segment".into(),
                }),
                lit => {
                    // `>` is only legal as the last segment. Catch a `>` in
                    // a literal slot too — `foo.>.bar` isn't allowed.
                    if lit.contains('>') {
                        return Err(RegistryError::InvalidTopic {
                            topic: raw.to_string(),
                            reason: format!("'>' in non-final segment {i}"),
                        });
                    }
                    Ok(TopicSegment::Literal(lit.to_string()))
                }
            })
            .collect();
        let segments = segments?;
        for (i, seg) in segments.iter().enumerate() {
            if seg == &TopicSegment::TrailingWildcard && i != segments.len() - 1 {
                return Err(RegistryError::InvalidTopic {
                    topic: raw.to_string(),
                    reason: "'>' must be the final segment".into(),
                });
            }
        }
        Ok(TopicPattern {
            raw: raw.to_string(),
            segments,
        })
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// True iff `topic` matches this pattern.
    pub fn matches(&self, topic: &str) -> bool {
        let parts: Vec<&str> = topic.split('.').collect();
        let mut i = 0;
        let mut j = 0;
        while i < self.segments.len() {
            match &self.segments[i] {
                TopicSegment::TrailingWildcard => {
                    // Must have at least one remaining segment to match `>`.
                    return j < parts.len();
                }
                TopicSegment::SingleWildcard => {
                    if j >= parts.len() {
                        return false;
                    }
                    i += 1;
                    j += 1;
                }
                TopicSegment::Literal(lit) => {
                    if j >= parts.len() || parts[j] != lit {
                        return false;
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        // Pattern fully consumed — exact match only if topic is too.
        j == parts.len()
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Registry {
    rules: Vec<Rule>,
}

impl Registry {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_toml(src: &str) -> Result<Self, RegistryError> {
        let raw: RawRegistry =
            toml::from_str(src).map_err(|e| RegistryError::Toml(e.to_string()))?;
        Self::from_raw(raw)
    }

    pub fn from_raw(raw: RawRegistry) -> Result<Self, RegistryError> {
        let mut rules = Vec::with_capacity(raw.rules.len());
        for r in raw.rules {
            rules.push(Rule::from_raw(r)?);
        }
        // Per-name uniqueness — same rule shouldn't be authored twice.
        let mut seen = std::collections::HashSet::new();
        for r in &rules {
            if !seen.insert(r.name.clone()) {
                return Err(RegistryError::DuplicateName(r.name.clone()));
            }
        }
        Ok(Registry { rules })
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }
}

impl Rule {
    pub fn from_raw(r: RawRule) -> Result<Self, RegistryError> {
        let on_event = TopicPattern::parse(&r.on_event)?;
        let when = match r.when {
            Some(src) => Some(expr::parse(&src).map_err(|e| RegistryError::BadExpr {
                rule: r.name.clone(),
                location: "when".into(),
                src,
                err: e.to_string(),
            })?),
            None => None,
        };
        let mut do_steps = Vec::with_capacity(r.do_steps.len());
        for ds in r.do_steps {
            let mut parsed_args = Vec::with_capacity(ds.args.len());
            // Sort so the evaluation order is deterministic across runs
            // (HashMap iteration order isn't, and downstream assertions
            // expect stable layout).
            let mut arg_pairs: Vec<(String, String)> = ds.args.into_iter().collect();
            arg_pairs.sort_by(|a, b| a.0.cmp(&b.0));
            for (k, v) in arg_pairs {
                let e = expr::parse(&v).map_err(|e| RegistryError::BadExpr {
                    rule: r.name.clone(),
                    location: format!("do[].args.{k}"),
                    src: v.clone(),
                    err: e.to_string(),
                })?;
                parsed_args.push((k, e));
            }
            do_steps.push(DoStep {
                handler: ds.handler,
                args: parsed_args,
            });
        }
        Ok(Rule {
            name: r.name,
            on_event,
            when,
            do_steps,
            delay: r.delay,
            version: r.version,
        })
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("toml parse: {0}")]
    Toml(String),
    #[error("invalid topic {topic:?}: {reason}")]
    InvalidTopic { topic: String, reason: String },
    #[error("rule {rule:?} {location} could not parse {src:?}: {err}")]
    BadExpr {
        rule: String,
        location: String,
        src: String,
        err: String,
    },
    #[error("duplicate rule name {0:?}")]
    DuplicateName(String),
}

// ---------------------------------------------------------------------------
// Matcher
// ---------------------------------------------------------------------------

/// A rule that matched an incoming event, with its handler invocations'
/// args resolved to concrete Values. What the side-effect handler dispatch
/// layer consumes (in the next pass).
#[derive(Debug, Clone)]
pub struct MatchedRule {
    pub rule_name: String,
    pub invocations: Vec<MatchedInvocation>,
}

#[derive(Debug, Clone)]
pub struct MatchedInvocation {
    pub handler: String,
    pub args: Vec<(String, Value)>,
}

/// Walk the registry against an incoming event and return all matched
/// rules with their args evaluated.
///
/// Pure: same inputs → same outputs. Rule iteration order is registry
/// declaration order so audit_log + tests have a stable layout.
pub fn match_event(
    registry: &Registry,
    topic: &str,
    payload: &serde_json::Value,
    helpers: &dyn HelperResolver,
) -> Result<Vec<MatchedRule>, MatchError> {
    let ctx = expr::Context { payload, helpers };
    let mut out = Vec::new();
    for rule in registry.rules() {
        if !rule.on_event.matches(topic) {
            continue;
        }
        if let Some(when) = &rule.when {
            let v = expr::eval(when, &ctx).map_err(|err| MatchError::PredicateFailed {
                rule: rule.name.clone(),
                err,
            })?;
            match v.as_bool() {
                Some(true) => {}
                Some(false) => continue,
                None => {
                    return Err(MatchError::PredicateNotBool {
                        rule: rule.name.clone(),
                        kind: v.kind(),
                    });
                }
            }
        }
        let mut invocations = Vec::with_capacity(rule.do_steps.len());
        for ds in &rule.do_steps {
            let mut args = Vec::with_capacity(ds.args.len());
            for (k, expr) in &ds.args {
                let val = expr::eval(expr, &ctx).map_err(|err| MatchError::ArgFailed {
                    rule: rule.name.clone(),
                    handler: ds.handler.clone(),
                    arg: k.clone(),
                    err,
                })?;
                args.push((k.clone(), val));
            }
            invocations.push(MatchedInvocation {
                handler: ds.handler.clone(),
                args,
            });
        }
        out.push(MatchedRule {
            rule_name: rule.name.clone(),
            invocations,
        });
    }
    Ok(out)
}

#[derive(Debug, Error)]
pub enum MatchError {
    #[error("rule {rule:?} predicate evaluation failed: {err}")]
    PredicateFailed { rule: String, err: expr::EvalError },
    #[error("rule {rule:?} predicate returned non-bool {kind}")]
    PredicateNotBool { rule: String, kind: &'static str },
    #[error("rule {rule:?} handler {handler:?} arg {arg:?} failed: {err}")]
    ArgFailed {
        rule: String,
        handler: String,
        arg: String,
        err: expr::EvalError,
    },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::expr::{HelperResolver, NoHelpers, Value};
    use super::*;
    use serde_json::json;

    // ----- topic patterns -----

    #[test]
    fn topic_literal_match() {
        let p = TopicPattern::parse("step.done.procurement").unwrap();
        assert!(p.matches("step.done.procurement"));
        assert!(!p.matches("step.done.billing"));
        assert!(!p.matches("step.done.procurement.extra"));
    }

    #[test]
    fn topic_single_wildcard() {
        let p = TopicPattern::parse("step.done.*").unwrap();
        assert!(p.matches("step.done.procurement"));
        assert!(p.matches("step.done.billing"));
        assert!(!p.matches("step.done"));
        assert!(!p.matches("step.done.procurement.extra"));
    }

    #[test]
    fn topic_trailing_wildcard() {
        let p = TopicPattern::parse("step.done.>").unwrap();
        assert!(p.matches("step.done.procurement"));
        assert!(p.matches("step.done.procurement.extra"));
        assert!(!p.matches("step.done")); // > requires >=1 trailing
    }

    #[test]
    fn topic_rejects_trailing_in_middle() {
        assert!(matches!(
            TopicPattern::parse("step.>.done"),
            Err(RegistryError::InvalidTopic { .. })
        ));
    }

    #[test]
    fn topic_rejects_empty_segment() {
        assert!(matches!(
            TopicPattern::parse("step..done"),
            Err(RegistryError::InvalidTopic { .. })
        ));
    }

    // ----- registry load -----

    const RESTOCK_RULE_TOML: &str = r#"
[[rule]]
name = "spawn-restock-on-low-inventory"
on_event = "inventory.parts.consumed"
when = "on_hand <= reorder_point AND NOT open_po_exists(part_sku)"
[[rule.do]]
handler = "jobs.spawn"
args = { kind = "\"ingredient-restock\"", subject = "vendor_for(part_sku)" }
"#;

    #[test]
    fn loads_design_doc_canonical_rule() {
        let reg = Registry::from_toml(RESTOCK_RULE_TOML).unwrap();
        assert_eq!(reg.rules().len(), 1);
        let r = &reg.rules()[0];
        assert_eq!(r.name, "spawn-restock-on-low-inventory");
        assert_eq!(r.on_event.raw(), "inventory.parts.consumed");
        assert!(r.when.is_some());
        assert_eq!(r.do_steps.len(), 1);
        assert_eq!(r.do_steps[0].handler, "jobs.spawn");
        assert_eq!(r.do_steps[0].args.len(), 2);
    }

    #[test]
    fn empty_registry_is_legal() {
        let reg = Registry::from_toml("").unwrap();
        assert_eq!(reg.rules().len(), 0);
    }

    #[test]
    fn rejects_malformed_when() {
        let toml = r#"
[[rule]]
name = "bad"
on_event = "x"
when = "a AND"
"#;
        assert!(matches!(
            Registry::from_toml(toml),
            Err(RegistryError::BadExpr { location, .. }) if location == "when"
        ));
    }

    #[test]
    fn rejects_duplicate_name() {
        let toml = r#"
[[rule]]
name = "dupe"
on_event = "x"
[[rule]]
name = "dupe"
on_event = "y"
"#;
        assert!(matches!(
            Registry::from_toml(toml),
            Err(RegistryError::DuplicateName(_))
        ));
    }

    #[test]
    fn args_order_is_deterministic_across_loads() {
        // Same TOML loaded twice — the (alphabetically) sorted arg order
        // must be stable. Catches HashMap iteration drift.
        let toml = r#"
[[rule]]
name = "r"
on_event = "x"
[[rule.do]]
handler = "h"
args = { zebra = "\"z\"", alpha = "\"a\"", mango = "\"m\"" }
"#;
        let r1 = Registry::from_toml(toml).unwrap();
        let r2 = Registry::from_toml(toml).unwrap();
        let keys1: Vec<&String> = r1.rules()[0].do_steps[0]
            .args
            .iter()
            .map(|(k, _)| k)
            .collect();
        let keys2: Vec<&String> = r2.rules()[0].do_steps[0]
            .args
            .iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys1, keys2);
        assert_eq!(
            keys1,
            vec![
                &"alpha".to_string(),
                &"mango".to_string(),
                &"zebra".to_string()
            ]
        );
    }

    // ----- matcher -----

    struct MockHelpers;

    impl HelperResolver for MockHelpers {
        fn call(&self, name: &str, args: &[Value]) -> Result<Value, expr::EvalError> {
            match name {
                "open_po_exists" => match args.first() {
                    Some(Value::String(sku)) => Ok(Value::Bool(sku == "PKG-WITH-PO")),
                    _ => Err(expr::EvalError::TypeError {
                        expected: "string",
                        got: "other",
                    }),
                },
                "vendor_for" => match args.first() {
                    Some(Value::String(sku)) => Ok(Value::String(format!("vnd-for-{sku}"))),
                    _ => Err(expr::EvalError::TypeError {
                        expected: "string",
                        got: "other",
                    }),
                },
                _ => Err(expr::EvalError::UnknownHelper(name.to_string())),
            }
        }
    }

    #[test]
    fn matches_canonical_rule_when_predicate_true() {
        let reg = Registry::from_toml(RESTOCK_RULE_TOML).unwrap();
        let payload = json!({
            "part_sku": "PKG-CO2-50LB",
            "on_hand": 10,
            "reorder_point": 20,
        });
        let matched =
            match_event(&reg, "inventory.parts.consumed", &payload, &MockHelpers).unwrap();
        assert_eq!(matched.len(), 1);
        let m = &matched[0];
        assert_eq!(m.rule_name, "spawn-restock-on-low-inventory");
        assert_eq!(m.invocations.len(), 1);
        let inv = &m.invocations[0];
        assert_eq!(inv.handler, "jobs.spawn");
        // Args are sorted alphabetically: kind, subject.
        let args: Vec<(&str, &Value)> = inv.args.iter().map(|(k, v)| (k.as_str(), v)).collect();
        assert_eq!(
            args,
            vec![
                ("kind", &Value::String("ingredient-restock".into())),
                ("subject", &Value::String("vnd-for-PKG-CO2-50LB".into())),
            ]
        );
    }

    #[test]
    fn does_not_match_when_predicate_false() {
        let reg = Registry::from_toml(RESTOCK_RULE_TOML).unwrap();
        let payload = json!({
            "part_sku": "PKG-WITH-PO",
            "on_hand": 10,
            "reorder_point": 20,
        });
        let matched =
            match_event(&reg, "inventory.parts.consumed", &payload, &MockHelpers).unwrap();
        assert!(matched.is_empty());
    }

    #[test]
    fn does_not_match_different_topic() {
        let reg = Registry::from_toml(RESTOCK_RULE_TOML).unwrap();
        let payload = json!({
            "part_sku": "PKG-CO2-50LB",
            "on_hand": 10,
            "reorder_point": 20,
        });
        let matched = match_event(&reg, "step.done.procurement", &payload, &MockHelpers).unwrap();
        assert!(matched.is_empty());
    }

    #[test]
    fn matches_wildcard_topic() {
        let toml = r#"
[[rule]]
name = "log-every-step-done"
on_event = "step.done.*"
[[rule.do]]
handler = "audit.log"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let payload = json!({});
        assert_eq!(
            match_event(&reg, "step.done.procurement", &payload, &NoHelpers)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            match_event(&reg, "step.done.billing", &payload, &NoHelpers)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            match_event(&reg, "step.opened", &payload, &NoHelpers)
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn multiple_rules_match_independently() {
        let toml = r#"
[[rule]]
name = "r1"
on_event = "step.done.procurement"
[[rule.do]]
handler = "h1"
[[rule]]
name = "r2"
on_event = "step.done.>"
[[rule.do]]
handler = "h2"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let payload = json!({});
        let matched = match_event(&reg, "step.done.procurement", &payload, &NoHelpers).unwrap();
        assert_eq!(matched.len(), 2);
        assert_eq!(matched[0].rule_name, "r1");
        assert_eq!(matched[1].rule_name, "r2");
    }

    #[test]
    fn iteration_order_is_registry_declaration_order() {
        let toml = r#"
[[rule]]
name = "second"
on_event = "x"
[[rule.do]]
handler = "h"
[[rule]]
name = "first"
on_event = "x"
[[rule.do]]
handler = "h"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let matched = match_event(&reg, "x", &json!({}), &NoHelpers).unwrap();
        // Declaration order: "second" then "first".
        assert_eq!(matched[0].rule_name, "second");
        assert_eq!(matched[1].rule_name, "first");
    }

    #[test]
    fn predicate_non_bool_is_match_error() {
        let toml = r#"
[[rule]]
name = "weird"
on_event = "x"
when = "42"
[[rule.do]]
handler = "h"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        assert!(matches!(
            match_event(&reg, "x", &json!({}), &NoHelpers),
            Err(MatchError::PredicateNotBool { .. })
        ));
    }
}
