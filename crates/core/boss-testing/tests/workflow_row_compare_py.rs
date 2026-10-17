//! `infra/gcp/workflow-row-compare.py` — the comparison
//! `publish-workflow.sh` makes between the tree's row for a kind and a
//! registry row — is RUN here against rows the real seed loader
//! rendered, so what it must know about the loader is held to the loader
//! rather than to a comment.
//!
//! WHY (backlog aa28f921; review 4746c046, F1). The comparison was a
//! curated list of keys, and the keys nobody had listed were the ones
//! that decide who may write: a field's `writer`, a step's `executor`,
//! `assurance_required`, `sign_offs_required`. A live row that lost them
//! read "nothing to publish" forever. The comparator now compares every
//! key either side states, minus an EXCLUDED table, so there is no list
//! to forget — and these two tests are what keep that true:
//!
//!   1. EVERY SHIPPED KIND FILE COMPARES EQUAL TO ITSELF, rendered by the
//!      real loader. Comparing every key has one cost: a key whose serde
//!      default is not "nothing" (`filled_by = executor`, an agent block's
//!      `executor_provenance = advisory`) is spelled by the registry and
//!      not by the file. The comparator names those in its SILENT table.
//!      A NEW such key reads as drift on every row here until it is
//!      named, which is the loud direction; this test is where it is loud
//!      before the car lands instead of on boss-gcp after.
//!
//!   2. EVERY KEY A ROW CAN CARRY IS COMPARED, OR EXCLUDED BY NAME WITH A
//!      REASON. One row is built with every field of `WorkflowSpec`,
//!      `StepSpec`, `StepField` and `AgentSpec` stated — held to the
//!      structs' own source, so a field added to any of them and not
//!      stated here is named. Then each key is removed from the live
//!      side in turn: the comparator must name it, unless EXCLUDED names
//!      it, and the EXCLUDED table is held to the set below.
//!
//! The verdicts the VERB reaches over the same comparison (a lost writer
//! is refused, a key nobody listed is named) are in
//! publish_workflow_sh.rs.
//!
//! No skip: python3 with tomllib is what the comparator runs on, the gate
//! image has it, and a skipped target is invisible in a receipt (the
//! item's own `from_review_a1a87eef_20261007` note, B).

use boss_core::job::{Assurance, FilledBy, StepField};
use boss_jobs::agent_spec::{AgentSpec, Effort};
use boss_jobs::audience::Audience;
use boss_jobs::executor_attestation::ExecutorProvenanceRequirement;
use boss_jobs::registry::{
    ExecutorClass, JobTrigger, StepSpec, Terminal, WorkflowSpec, WorkflowStatus,
};
use boss_testing::{repo_root, scratch_dir, write_file};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

const COMPARE: &str = "infra/gcp/workflow-row-compare.py";
const BUNDLE: &str = "infra/platform/workflows";

/// Run the comparator: its exit code and what it printed.
fn compare(toml: &Path, live: &Path, kind: &str) -> (i32, String) {
    let out = Command::new("python3")
        .arg(repo_root().join(COMPARE))
        .arg(toml)
        .arg(live)
        .arg(kind)
        .output()
        .expect("python3 runs — the comparator needs it, and so does this test");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The comparator's own EXCLUDED table, read out of the file that
/// defines it: level -> key -> reason.
fn excluded() -> BTreeMap<String, BTreeMap<String, String>> {
    let out = Command::new("python3")
        .args([
            "-c",
            "import json, runpy, sys\nprint(json.dumps(runpy.run_path(sys.argv[1], run_name='read')['EXCLUDED']))",
        ])
        .arg(repo_root().join(COMPARE))
        .output()
        .expect("python3 runs");
    assert!(
        out.status.success(),
        "{COMPARE} could not be read for its EXCLUDED table: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("EXCLUDED is level -> key -> reason")
}

#[test]
fn every_shipped_kind_file_compares_equal_to_the_row_the_loader_renders_from_it() {
    let root = scratch_dir("workflow-row-compare-bundle");
    let specs = boss_jobs::seed_loader::load_workflows(repo_root().join(BUNDLE))
        .expect("the shipped bundle loads");
    assert!(
        specs.len() >= 20,
        "the shipped bundle rendered {} kinds — a comparison this small proves nothing",
        specs.len()
    );
    let adrift: Vec<String> = specs
        .iter()
        .filter_map(|spec| {
            let live = root.join(format!("{}.json", spec.kind));
            write_file(&live, &serde_json::to_string(spec).unwrap());
            let file = repo_root().join(BUNDLE).join(format!("{}.toml", spec.kind));
            let (rc, out) = compare(&file, &live, &spec.kind);
            (rc != 0).then(|| format!("{} (exit {rc}):\n{out}", spec.kind))
        })
        .collect();
    assert!(
        adrift.is_empty(),
        "{} of {} shipped kind files do not compare EQUAL to the row the seed loader renders \
         from them. Nothing drifted: the loader writes something the file is silent about, and \
         {COMPARE} does not know it. Name the default in its SILENT table (or the projection in \
         `project`), with where the loader does it:\n\n{}",
        adrift.len(),
        specs.len(),
        adrift.join("\n")
    );
}

/// One row with EVERY field stated and none at its default. The literals
/// stand on constructor bases where one exists, as integration fixtures
/// must (caa2acc9 D2: a new core field does not break fixtures), so the
/// compiler will not always say when a field is added and left out here
/// — `every_declared_field_is_stated` below does, by name.
fn a_row_with_every_key_stated() -> WorkflowSpec {
    let field = StepField {
        required: true,
        filled_by: FilledBy::Filer,
        item_keys: vec!["anchor".into()],
        covers: Some("questions".into()),
        binds: Some("exhibits".into()),
        item_value_max_bytes: Some(1024),
        item_one_of: vec!["html".into(), "file_ref".into()],
        writer: Some("signer".into()),
        ..StepField::new("decision", "array")
    };
    let step = StepSpec {
        title: "approve".into(),
        kind: "sign-off".into(),
        ready_when: "steps.plan.done".into(),
        terminal: Some(Terminal {
            outcome: "approved".into(),
        }),
        title_template: "Approve the plan".into(),
        sign_offs_required: vec!["platform-admin".into()],
        assurance_required: Some(Assurance::Presence),
        duration_hours: Some(3.0),
        labor_hours: Some(1.0),
        wall_clock_hours: Some(2.0),
        fields: vec![field],
        authority_role: Some("platform-admin".into()),
        claimable: Some(true),
        executor: Some("runner:ops".into()),
        executor_class: Some(ExecutorClass::Agent),
        // A station, not a role: a role would be projected onto
        // `authority_role`, and removing that key would then be undone.
        audience: Some(Audience::Station("approvals".into())),
        agent: Some(AgentSpec {
            profile: "builder".into(),
            model: "opus-5".into(),
            budget_usd: 5.0,
            effort: Effort::High,
            executor_provenance: ExecutorProvenanceRequirement::Verified,
        }),
        metadata_defaults: json!({"procedure": "Read the plan before you sign it."}),
    };
    WorkflowSpec {
        version: 3,
        status: WorkflowStatus::Active,
        description: Some("A row that states every key a row can carry.".into()),
        metadata_schema: json!({"required": ["plan"]}),
        entitlements: json!({"open": "platform-admin"}),
        metadata: json!({"surfaces": ["system-ops"]}),
        on_complete_create: vec![JobTrigger {
            kind: "learning".into(),
            subject_source: "metadata:account".into(),
            metadata_seed: json!({"from": "every-key"}),
        }],
        authoring_job_id: Some(Default::default()),
        ..WorkflowSpec::platform_seed(
            "every-key",
            "Every key",
            "platform",
            vec!["custom".into()],
            vec![step],
        )
    }
}

/// The field names a struct declares, read out of its source: every
/// `pub <name>:` line between `pub struct <name> {` and its closing
/// brace. None of the four renames a key, so a field's name is its key.
fn declared_fields(rel: &str, name: &str) -> Vec<String> {
    let src = std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("{rel} could not be read: {e}"));
    let fields: Vec<String> = src
        .lines()
        .skip_while(|l| *l != format!("pub struct {name} {{"))
        .skip(1)
        .take_while(|l| *l != "}")
        .filter_map(|l| l.strip_prefix("    pub "))
        .filter_map(|l| l.split_once(':').map(|(field, _)| field.to_string()))
        .collect();
    assert!(
        fields.len() >= 3,
        "read {} field(s) of `{name}` out of {rel} — the struct moved or this reader is broken",
        fields.len()
    );
    fields
}

/// The row above must STATE every field its four structs declare, or the
/// test below tries fewer keys than a row can carry and still passes. A
/// field added to any of them and not given a value here is named.
#[test]
fn every_declared_field_is_stated() {
    let row = serde_json::to_value(a_row_with_every_key_stated()).unwrap();
    let missing: Vec<String> = [
        ("crates/core/boss-jobs/src/registry.rs", "WorkflowSpec", ""),
        (
            "crates/core/boss-jobs/src/registry.rs",
            "StepSpec",
            "/steps/0",
        ),
        (
            "crates/core/boss-core/src/job.rs",
            "StepField",
            "/steps/0/fields/0",
        ),
        (
            "crates/core/boss-jobs/src/agent_spec.rs",
            "AgentSpec",
            "/steps/0/agent",
        ),
    ]
    .into_iter()
    .flat_map(|(rel, name, pointer)| {
        let stated = row
            .pointer(pointer)
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("the row has no object at `{pointer}`"))
            .clone();
        declared_fields(rel, name)
            .into_iter()
            .filter(move |f| stated.get(f).is_none_or(Value::is_null))
            .map(move |f| format!("{name}.{f} ({rel})"))
    })
    .collect();
    assert!(
        missing.is_empty(),
        "a_row_with_every_key_stated does not state {missing:?}. Give each a value that is not its \
         default there: every_key_a_row_can_carry_is_compared_or_excluded_by_name then demands that \
         {COMPARE} compares it or excludes it by name — a key it never tried is a key a live row \
         can lose unseen (backlog aa28f921)."
    );
}

/// The row as a `[[workflow]]` file states it: the same keys, under
/// `step`. Whatever the comparator excludes may sit in it — it is
/// excluded on both sides.
fn as_kind_file(row: &Value) -> String {
    let mut wf = row.clone();
    let steps = wf.as_object_mut().unwrap().remove("steps").unwrap();
    wf["step"] = steps;
    toml::to_string(&json!({ "workflow": [wf] })).expect("a row with every key stated has no null")
}

#[test]
fn every_key_a_row_can_carry_is_compared_or_excluded_by_name() {
    let root = scratch_dir("workflow-row-compare-every-key");
    let row = serde_json::to_value(a_row_with_every_key_stated()).unwrap();
    let file = root.join("every-key.toml");
    write_file(&file, &as_kind_file(&row));
    let live = root.join("live.json");
    write_file(&live, &row.to_string());
    let (rc, out) = compare(&file, &live, "every-key");
    assert_eq!(
        rc, 0,
        "the row equals itself before anything is removed:\n{out}"
    );

    let excluded = excluded();
    // (level, where its object sits in the row, the path the comparator
    // names it by, and the key its elements are matched on)
    let levels: [(&str, &str, &str, Option<(&str, &str)>); 4] = [
        ("workflow", "", "", None),
        (
            "step",
            "/steps/0",
            "steps[approve].",
            Some(("title", "steps")),
        ),
        (
            "field",
            "/steps/0/fields/0",
            "steps[approve].fields[decision].",
            Some(("name", "steps[approve].fields")),
        ),
        ("agent", "/steps/0/agent", "steps[approve].agent.", None),
    ];
    let mut seen = 0;
    let mut wrong: Vec<String> = Vec::new();
    for (level, pointer, prefix, matched_on) in levels {
        let keys: Vec<String> = row
            .pointer(pointer)
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("the row has no object at `{pointer}`"))
            .keys()
            .cloned()
            .collect();
        let none = BTreeMap::new();
        let left_out = excluded.get(level).unwrap_or(&none);
        for key in keys {
            seen += 1;
            let mut lost = row.clone();
            lost.pointer_mut(pointer)
                .and_then(Value::as_object_mut)
                .unwrap()
                .remove(&key);
            write_file(&live, &lost.to_string());
            let (rc, out) = compare(&file, &live, "every-key");
            if left_out.contains_key(&key) {
                if rc != 0 {
                    wrong.push(format!(
                        "{level} key `{key}` is in EXCLUDED and was compared anyway (exit {rc}): {out}"
                    ));
                }
                continue;
            }
            // Every key is named under its own path. Two are named
            // under the LIST's instead: the key a list's elements are
            // matched on (an element that lost its title cannot be
            // matched, so the list is the finding), and a list that is
            // gone whole (named once, as its membership).
            let named = match matched_on {
                Some((by, list)) if by == key => list.to_string(),
                _ => format!("{prefix}{key}"),
            };
            let said = out.lines().any(|l| {
                l.split('\t').nth(1).is_some_and(|p| {
                    p.strip_prefix(named.as_str())
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '[']))
                })
            });
            if rc != 1 || !said {
                wrong.push(format!(
                    "{level} key `{key}`: a live row that lost it must read as drift naming `{named}`, \
                     and the comparator answered exit {rc}: {out}"
                ));
            }
        }
    }
    assert!(
        seen >= 40,
        "only {seen} keys were tried — the row is not the whole row"
    );
    assert!(
        wrong.is_empty(),
        "{COMPARE} neither compares nor excludes by name:\n{}",
        wrong.join("\n")
    );
}

/// The exclusions, held to the set that was reviewed. Leaving a key out
/// of the comparison is how the blind spot this file exists for was
/// made, so adding one is an edit here too, in front of a reviewer: only
/// what NO FILE CAN SAY may be left out, nothing below the workflow row
/// is, and every entry says why.
#[test]
fn the_keys_left_out_are_the_ones_no_file_can_say_and_each_says_why() {
    let excluded = excluded();
    let keys = |level: &str| -> BTreeSet<String> {
        excluded
            .get(level)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    };
    let reviewed: BTreeSet<String> = [
        "kind",
        "version",
        "status",
        "created_at",
        "authoring_job_id",
        "owning_team",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(
        keys("workflow"),
        reviewed,
        "the workflow keys {COMPARE} leaves out of the comparison are not the reviewed set"
    );
    for level in ["step", "field", "agent"] {
        assert!(
            keys(level).is_empty(),
            "{COMPARE} leaves {level} key(s) {:?} out of the comparison. A step's executor and a \
             field's writer were left out once, and a live row that lost them read equal forever \
             (backlog aa28f921).",
            keys(level)
        );
    }
    assert_eq!(
        excluded.keys().cloned().collect::<BTreeSet<_>>(),
        ["agent", "field", "step", "workflow"]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>(),
        "EXCLUDED names a level this test does not hold"
    );
    for (level, entries) in &excluded {
        for (key, why) in entries {
            assert!(
                why.trim().len() >= 20,
                "{COMPARE} excludes {level} key `{key}` without saying why (`{why}`)"
            );
        }
    }
}
