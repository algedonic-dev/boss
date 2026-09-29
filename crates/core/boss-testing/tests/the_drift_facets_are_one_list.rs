//! THE DRIFT FACETS ARE ONE LIST, HELD IN TWO LANGUAGES (backlog
//! e5dc276d; CLAUDE.md §9a).
//!
//! Two comparators answer "does this protocol's file say what its live
//! row says": the daily drift lint,
//! `infra/lint/the-live-protocols-are-the-authored-protocols.sh` (its
//! Python `fields_report`), and `boss_jobs::bootstrap::workflow_changes`,
//! which `boss tenant publish` uses to decide whether a live kind is
//! kept, superseded or already as declared. Their comments said they
//! compared "the SAME list", and nothing held them to it: the Rust copy
//! lacked the step's `agent` block from 2026-09-19, when the lint gained
//! it (backlog 1b847556), and every key the lint gained on 2026-09-28
//! (backlog 462cdfe3 — workflow `metadata`, optional fields, predicates,
//! step kinds, procedures). So a publish said "already as declared" for
//! a kind the drift report named adrift that same day.
//!
//! WHY A PIN AND NOT A COLLAPSE. The collapse would have the lint ask
//! the one Rust definition through a `boss` verb, so the shell held no
//! list. The lint is a build-free pre-flight: it runs before the gate
//! builds anything, so the only `boss` it could call is one built from
//! some OTHER tree — the comparator of the car before, judging this
//! car's bundle. That is a worse copy than the one it replaces, so the
//! two stay two and this holds them equal, both ways:
//!
//!   1. the NAMED lists — the fields compared verbatim, the per-step
//!      facets rendered by name, the keys each side leaves out, the key
//!      the publish path fills — read out of the lint's own source and
//!      compared entry by entry with the Rust constants, naming the
//!      entry one side lacks;
//!   2. the BEHAVIOUR — both comparators run over the REAL platform
//!      bundle against one live answer, first as published (both must
//!      find nothing) and then with a table of edits (both must name
//!      the same kind and facet for every one, and each edit must be
//!      named at all).
//!
//! The prose copies (docs/tenant-contract.md and `boss tenant contract`)
//! are one definition already, `boss_cli`'s `INSTANCE_IS_THE_TRUTH`,
//! held to the doc by its own test; that constant is pinned to these
//! lists beside it in `tenant.rs`.

use boss_jobs::bootstrap::{
    COMPARED_FIELDS, FILLED_WHEN_SILENT, NAMED_STEP_FACETS, ROW_ONLY, STEP_NAMED, workflow_changes,
};
use boss_jobs::registry::WorkflowSpec;
use boss_testing::{repo_root, scratch_dir};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::process::Command;

const LINT: &str = "infra/lint/the-live-protocols-are-the-authored-protocols.sh";
const BUNDLE: &str = "infra/platform/workflows";

fn lint_source() -> String {
    let path = repo_root().join(LINT);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The quoted strings on the ONE line of the lint that assigns `name`,
/// e.g. `FIELDS = ("label", "description", "category")`. A name
/// assigned twice, or not at all, is a lint this pin can no longer
/// read — refused, never read as an empty list.
fn python_list(src: &str, name: &str) -> BTreeSet<String> {
    let prefix = format!("{name} = ");
    let lines: Vec<&str> = src.lines().filter(|l| l.starts_with(&prefix)).collect();
    assert_eq!(
        lines.len(),
        1,
        "{LINT} must assign {name} exactly once at the start of a line; found {lines:?}"
    );
    let set: BTreeSet<String> = lines[0][prefix.len()..]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    assert!(
        !set.is_empty(),
        "{LINT}: {name} holds no quoted entry: {}",
        lines[0]
    );
    set
}

#[test]
fn every_list_the_lint_compares_by_is_the_rust_list() {
    let src = lint_source();
    let pairs: [(&str, &str, &[&str]); 5] = [
        ("FIELDS", "COMPARED_FIELDS", &COMPARED_FIELDS),
        ("NAMED_STEP_FACETS", "NAMED_STEP_FACETS", &NAMED_STEP_FACETS),
        ("STEP_NAMED", "STEP_NAMED", &STEP_NAMED),
        ("ROW_ONLY", "ROW_ONLY", &ROW_ONLY),
        (
            "FILLED_WHEN_SILENT",
            "FILLED_WHEN_SILENT",
            &FILLED_WHEN_SILENT,
        ),
    ];
    let mut bad = Vec::new();
    for (py, rs, rust) in pairs {
        let lint = python_list(&src, py);
        let rust: BTreeSet<String> = rust.iter().map(|s| s.to_string()).collect();
        for missing in rust.difference(&lint) {
            bad.push(format!(
                "`{missing}` is in boss_jobs::bootstrap::{rs} and not in the lint's {py}"
            ));
        }
        for missing in lint.difference(&rust) {
            bad.push(format!(
                "`{missing}` is in the lint's {py} and not in boss_jobs::bootstrap::{rs}"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "the drift lint ({LINT}) and boss tenant publish (boss_jobs::bootstrap::workflow_changes) \
         compare different facets — add the facet to BOTH, or neither:\n  {}",
        bad.join("\n  ")
    );
}

/// The real bundle, loaded the way `boss tenant publish` loads it.
fn bundle() -> Vec<WorkflowSpec> {
    let specs = boss_jobs::seed_loader::load_workflows(repo_root().join(BUNDLE))
        .unwrap_or_else(|e| panic!("loading {BUNDLE}: {e}"));
    assert!(specs.len() >= 20, "{BUNDLE} loaded {} kinds", specs.len());
    specs
}

/// A live answer: every kind as the registry would hand it back after
/// publishing exactly this spec.
fn published(specs: &[WorkflowSpec]) -> Vec<Value> {
    specs
        .iter()
        .map(|s| {
            let mut row = serde_json::to_value(s).expect("a spec serializes");
            row["version"] = json!(7);
            row["status"] = json!("active");
            row
        })
        .collect()
}

/// `(kind, facet)` for every DRIFT line the lint's comparator prints
/// over the real bundle against `live`.
fn lint_drift(live: &[Value]) -> BTreeSet<(String, String)> {
    let dir = scratch_dir("the-drift-facets-are-one-list");
    let answer = dir.join("live.json");
    std::fs::write(&answer, serde_json::to_vec(live).unwrap()).unwrap();
    let out = Command::new("bash")
        .arg(repo_root().join(LINT))
        .arg("--compare")
        .arg(repo_root().join(BUNDLE))
        .arg(&answer)
        .output()
        .expect("running the lint's comparator");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{LINT} --compare exited {:?}\nstdout:\n{stdout}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let counts: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("COUNTS\t"))
        .collect();
    assert_eq!(counts.len(), 1, "no COUNTS line: {stdout}");
    assert!(
        counts[0].contains(&format!("compared={}", live.len())),
        "the lint compared fewer kinds than the answer holds: {}",
        counts[0]
    );
    // A DRIFT line is a finding. So is an ABSENT line — the file states
    // no `description` — when the live row holds text, because a
    // supersede would publish the silence over it; the Rust side names
    // that one as a change, and an ABSENT beside an empty live field as
    // nothing (bootstrap.rs, COMPARED_FIELDS).
    let live_text = |kind: &str, field: &str| {
        live.iter()
            .find(|r| r["kind"] == kind)
            .and_then(|r| r[field].as_str())
            .is_some_and(|s| !s.is_empty())
    };
    stdout
        .lines()
        .filter_map(|l| {
            let cells: Vec<&str> = l.split('\t').collect();
            match cells[0] {
                "DRIFT" => Some((cells[1].to_string(), cells[2].to_string())),
                "ABSENT" if live_text(cells[1], cells[2]) => {
                    Some((cells[1].to_string(), cells[2].to_string()))
                }
                _ => None,
            }
        })
        .collect()
}

/// The same question asked of the Rust comparator.
fn rust_drift(specs: &[WorkflowSpec], live: &[Value]) -> BTreeSet<(String, String)> {
    specs
        .iter()
        .zip(live)
        .flat_map(|(spec, row)| {
            workflow_changes(row, spec)
                .into_iter()
                .map(|c| (spec.kind.clone(), c.field))
        })
        .collect()
}

fn assert_same(lint: &BTreeSet<(String, String)>, rust: &BTreeSet<(String, String)>) {
    let only_lint: Vec<_> = lint.difference(rust).collect();
    let only_rust: Vec<_> = rust.difference(lint).collect();
    assert!(
        only_lint.is_empty() && only_rust.is_empty(),
        "the two comparators disagree over the real bundle.\n  named by the lint only: \
         {only_lint:?}\n  named by boss_jobs::bootstrap::workflow_changes only: {only_rust:?}"
    );
}

#[test]
fn a_bundle_published_as_written_is_adrift_on_neither_side() {
    let specs = bundle();
    let live = published(&specs);
    let lint = lint_drift(&live);
    let rust = rust_drift(&specs, &live);
    assert_same(&lint, &rust);
    assert!(
        lint.is_empty(),
        "a row published from its own file drifts: {lint:?}"
    );
}

/// One edit to one live row, and the facet each comparator must name
/// for it.
type Edit = (&'static str, &'static str, fn(&mut Value));

/// The edits: one per facet the lists name, plus the silences — a
/// value the publish path fills when the file is silent, and one it
/// writes as `null` / `false` / an empty list — which must be named by
/// NEITHER side. Each lands on its own kind, so a finding is
/// attributable to its edit.
const EDITS: &[Edit] =
    &[
        ("description", "description", |r| {
            r["description"] = json!("edited live")
        }),
        ("label", "label", |r| r["label"] = json!("Edited live")),
        ("category", "category", |r| {
            r["category"] = json!("elsewhere")
        }),
        ("metadata", "metadata", |r| {
            r["metadata"] = json!({"surfaces": ["a-surface-the-file-never-named"]})
        }),
        ("subject_kinds", "subject_kinds", |r| {
            r["subject_kinds"] = json!(["a-kind-the-file-never-named"])
        }),
        ("a step the file lacks", "steps.titles", |r| {
            let mut extra = r["steps"][0].clone();
            extra["title"] = json!("only-live");
            r["steps"].as_array_mut().unwrap().push(extra);
        }),
        ("title_template", "steps.<0>.title_template", |r| {
            r["steps"][0]["title_template"] = json!("Edited live")
        }),
        ("ready_when", "steps.<0>.ready_when", |r| {
            r["steps"][0]["ready_when"] = json!("false")
        }),
        ("a required field", "steps.<0>.required", |r| {
            r["steps"][0]["fields"].as_array_mut().unwrap().push(
                json!({"name": "only_live_required", "field_type": "string", "required": true}),
            )
        }),
        ("an optional field", "steps.<0>.optional", |r| {
            r["steps"][0]["fields"].as_array_mut().unwrap().push(
                json!({"name": "only_live_optional", "field_type": "string", "required": false}),
            )
        }),
        ("agent", "steps.<0>.agent", |r| {
            r["steps"][0]["agent"] = json!({"profile": "a-profile-the-file-never-named",
            "model": "m", "budget_usd": 1.0, "effort": "low"})
        }),
        ("metadata_defaults", "steps.<0>.metadata_defaults", |r| {
            r["steps"][0]["metadata_defaults"] = json!({"procedure": "a procedure only live holds"})
        }),
        ("the kind", "steps.<0>.kind", |r| {
            r["steps"][0]["kind"] = json!("a-kind-only-live-holds")
        }),
        // THE SILENCES: named by neither side.
        ("defaults the publish path writes", "", |r| {
            r["steps"][0]["claimable"] = json!(false);
            r["steps"][0]["terminal"] = Value::Null;
            r["entitlements"] = json!({});
        }),
        ("a number spelled 2 and 2.0", "", |r| {
            // An integer the registry would hand back as a float.
            if let Some(h) = r["steps"][0]["duration_hours"].as_f64() {
                r["steps"][0]["duration_hours"] = json!(h);
            }
        }),
    ];

#[test]
fn every_edit_to_a_live_row_is_named_the_same_way_by_both() {
    let specs = bundle();
    let mut live = published(&specs);
    // Kinds the edits can land on: at least one step, and the first
    // step's authority_role not filled from an audience by the loader
    // (the file-silent case below would otherwise be a file claim on
    // the Rust side only; see FILLED_WHEN_SILENT in bootstrap.rs).
    let targets: Vec<usize> = (0..specs.len())
        .filter(|&i| {
            specs[i].steps.len() >= 2
                && specs[i].steps[0].audience.is_none()
                && specs[i].steps[0].authority_role.is_none()
        })
        .collect();
    assert!(
        targets.len() > EDITS.len(),
        "{BUNDLE} has {} kinds the edits can land on, {} edits",
        targets.len(),
        EDITS.len()
    );
    // The publish path fills authority_role when the file is silent:
    // named by neither side on the kind after the last edit.
    let silent = targets[EDITS.len()];
    live[silent]["steps"][0]["authority_role"] = json!("filled-by-the-publish-path");

    let mut want = BTreeSet::new();
    for (&i, (what, facet, edit)) in targets.iter().zip(EDITS) {
        edit(&mut live[i]);
        if !facet.is_empty() {
            let step0 = format!("steps.{}.", specs[i].steps[0].title);
            want.insert((
                what,
                specs[i].kind.clone(),
                facet.replace("steps.<0>.", &step0),
            ));
        }
    }

    let lint = lint_drift(&live);
    let rust = rust_drift(&specs, &live);
    assert_same(&lint, &rust);

    // Non-vacuity: each edit is named, on its own kind, by its facet.
    for (what, kind, facet) in &want {
        assert!(
            lint.contains(&(kind.clone(), facet.clone())),
            "the edit `{what}` on `{kind}` was not named as `{facet}`: {lint:?}"
        );
    }
    let edited: BTreeSet<&String> = want.iter().map(|(_, k, _)| k).collect();
    let stray: Vec<_> = lint.iter().filter(|(k, _)| !edited.contains(k)).collect();
    assert!(
        stray.is_empty(),
        "a kind that was not edited, or was edited only in ways the publish path fills in, \
         was named: {stray:?}"
    );
}
