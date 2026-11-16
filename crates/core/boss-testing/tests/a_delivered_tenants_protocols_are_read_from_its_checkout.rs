//! A DELIVERED TENANT'S PROTOCOLS ARE READ FROM ITS CHECKOUT, NOT
//! EXCUSED (backlog 6db14d0c, 2026-11-05).
//!
//! `infra/lint/the-live-protocols-are-the-authored-protocols.sh` answers
//! "is every protocol the registry admits written down somewhere". Since
//! the prod flip (2026-09-16, f4f5c387) an instance may take its tenant
//! from a repository the converge checks out (instances.toml
//! `tenant_repo`), and that tenant's rows are written down THERE, not in
//! this tree. The lint knew that much and no more: every live kind whose
//! owning team this tree authors nothing for was put on a list, echoed,
//! and removed from the failures — the tenant's repository was never
//! opened. So a kind published straight into the registry under a
//! tenant's team, with no file in the tenant's repo, passed unseen:
//! measured 2026-11-05, `publish-a-blog-post` (v2/v3, authoring_job_id
//! null) had been live two days and `git log --all -S` over the tenant's
//! repo was empty (tenant-builder run e1204903).
//!
//! What this pins, with a planted tenant checkout where the converge
//! stages one (`$TENANTS_DIR/<instance>`, infra/forge/forge-defaults.sh,
//! overridden here through `BOSS_FORGE_TENANTS_DIR`, the variable the
//! converge and retire-example-reference-rows.sh read) and the registry
//! HANDED to the lint (infra/lint/lib/sor-read.sh), so nothing is sent:
//!
//!   1. a live tenant kind the checkout's seeds/workflows.toml does not
//!      author FAILS, by name;
//!   2. a row the file does author is field-compared by the lint's own
//!      comparator, and a drift is NAMED (reported bare, a verdict under
//!      --require-live);
//!   3. an equal row is silent and counted;
//!   4. no checkout is a SKIP said in words — never an OK line;
//!   5. `--compare` reads a tenant's one-file bundle with a floor sized
//!      to a tenant, where the platform floor (20) refused a 9-kind one;
//!   6. a `[[workflow]]` line INSIDE a multiline string is prose, not a
//!      block: the file's blocks are counted off the parsed TOML, never
//!      off its text (review run 23be09ae R1).
//!
//! The tenant rows are neutral synthetic ones; no tenant's own content
//! enters this tree (fcc1d57b).

use boss_testing::{repo_root, scratch_dir, without_proxy};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

const LINT: &str = "infra/lint/the-live-protocols-are-the-authored-protocols.sh";
const BUNDLE: &str = "infra/platform/workflows";
/// An address nothing is ever sent to: the registry is handed in.
const BASE: &str = "http://fixture.invalid:9";

const TEAM: &str = "fixture-tenant";
const EQUAL_KIND: &str = "fixture-tenant-equal";
const DRIFTED_KIND: &str = "fixture-tenant-drifted";
const ABSENT_KIND: &str = "fixture-tenant-absent";

/// The tenant's own bundle: one file of `[[workflow]]` blocks, as a
/// tenant repository carries it at seeds/workflows.toml.
const TENANT_FILE: &str = r#"
[[workflow]]
kind = "fixture-tenant-equal"
label = "Equal"
category = "fixture"
owning_team = "fixture-tenant"
description = "A tenant protocol whose live row says what its file says."

[[workflow]]
kind = "fixture-tenant-drifted"
label = "Drifted"
category = "fixture"
owning_team = "fixture-tenant"
description = "The sentence the tenant's file holds."
"#;

fn tenant_row(kind: &str, label: &str, description: &str) -> Value {
    json!({
        "kind": kind, "version": 3, "status": "active", "label": label,
        "category": "fixture", "owning_team": TEAM, "description": description,
        "steps": [],
    })
}

fn equal_row() -> Value {
    tenant_row(
        EQUAL_KIND,
        "Equal",
        "A tenant protocol whose live row says what its file says.",
    )
}

fn drifted_row() -> Value {
    tenant_row(
        DRIFTED_KIND,
        "Drifted",
        "A sentence published live and never written back.",
    )
}

fn absent_row() -> Value {
    tenant_row(
        ABSENT_KIND,
        "Absent",
        "Published straight into the registry; no file anywhere.",
    )
}

/// The platform bundle as the registry hands it back after publishing
/// exactly its files — the live answer the_drift_facets_are_one_list.rs
/// proves the comparator finds nothing in — so whatever this test reads
/// is a fact about the tenant rows planted beside it.
fn platform_rows() -> Vec<Value> {
    let specs = boss_jobs::seed_loader::load_workflows(repo_root().join(BUNDLE))
        .unwrap_or_else(|e| panic!("loading {BUNDLE}: {e}"));
    assert!(specs.len() >= 20, "{BUNDLE} loaded {} kinds", specs.len());
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

/// The instances this tree declares repo-sourced, read from the file the
/// lint reads. The checkout is planted for each, so the test follows the
/// tree rather than a name typed here.
fn repo_instances() -> Vec<String> {
    let path = repo_root().join("infra/cluster/instances.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let doc: toml::Value = toml::from_str(&text).unwrap();
    let names: Vec<String> = doc
        .as_table()
        .unwrap()
        .iter()
        .filter(|(_, v)| v.get("tenant_repo").is_some())
        .map(|(k, _)| k.clone())
        .collect();
    assert!(
        !names.is_empty(),
        "{} declares no tenant_repo instance, so the lint has no tenant leg to test — \
         this test (and the leg) should go with the last one",
        path.display()
    );
    names
}

struct Run {
    rc: Option<i32>,
    out: String,
    err: String,
    report: Value,
}

/// A tenants directory: a checkout per repo-sourced instance holding
/// `file` at seeds/workflows.toml, or nothing at all when `file` is None.
fn tenants(case: &str, file: Option<&str>) -> PathBuf {
    let dir = scratch_dir(&format!("delivered-tenant-{case}")).join("tenants");
    boss_testing::create_dir(&dir);
    if let Some(body) = file {
        for name in repo_instances() {
            let seeds = dir.join(&name).join("seeds");
            boss_testing::create_dir(&seeds);
            boss_testing::write_file(&seeds.join("workflows.toml"), body);
            boss_testing::write_file(&seeds.join("tenant.toml"), "id = \"fixture-tenant\"\n");
        }
    }
    dir
}

fn run(tenants_dir: &Path, tenant_rows: &[Value], extra: &[&str]) -> Run {
    let dir = tenants_dir.parent().unwrap().to_path_buf();
    let live = dir.join("live.json");
    let mut rows = platform_rows();
    rows.extend(tenant_rows.iter().cloned());
    std::fs::write(&live, serde_json::to_vec(&rows).unwrap()).unwrap();
    let report = dir.join("report.json");
    let _ = std::fs::remove_file(&report);
    let out = without_proxy(&mut Command::new("bash"))
        .arg(repo_root().join(LINT))
        .args(extra)
        .arg("--report-json")
        .arg(&report)
        .env("BOSS_JOBS_URL", BASE)
        .env("BOSS_LINT_SOR_HANDED_URL", format!("{BASE}/api/workflows"))
        .env("BOSS_LINT_SOR_HANDED_CODE", "200")
        .env("BOSS_LINT_SOR_HANDED_BODY", &live)
        .env("BOSS_FORGE_TENANTS_DIR", tenants_dir)
        .env_remove("BOSS_ESTATE")
        .output()
        .expect("bash");
    let report = std::fs::read_to_string(&report)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    Run {
        rc: out.status.code(),
        out: String::from_utf8_lossy(&out.stdout).into_owned(),
        err: String::from_utf8_lossy(&out.stderr).into_owned(),
        report,
    }
}

impl Run {
    fn all(&self) -> String {
        format!(
            "exit {:?}\n--- stdout\n{}\n--- stderr\n{}\n--- report\n{}",
            self.rc, self.out, self.err, self.report
        )
    }
    fn last_line(&self) -> &str {
        self.out
            .lines()
            .rfind(|l| l.starts_with("the-live-"))
            .unwrap_or("")
    }
}

/// 1. THE DEFECT: a live kind owned by the delivered tenant, which the
///    tenant's own checkout does not author, fails the lint BY NAME —
///    where it used to be listed as "read from the tenant's own repo"
///    by a lint that never read it.
#[test]
fn a_live_tenant_kind_its_checkout_does_not_author_fails_by_name() {
    let dir = tenants("absent", Some(TENANT_FILE));
    let r = run(&dir, &[equal_row(), absent_row()], &[]);
    assert_eq!(
        r.rc,
        Some(1),
        "an unauthored tenant kind is a failure:\n{}",
        r.all()
    );
    assert!(
        r.err
            .contains("owned by a delivered tenant that its checkout does not author")
            && r.err.contains(ABSENT_KIND),
        "the failure does not name the kind its tenant's checkout lacks:\n{}",
        r.all()
    );
    assert!(
        r.err.contains(&dir.display().to_string()) && r.err.contains("seeds/workflows.toml"),
        "the failure does not name the checkout it read:\n{}",
        r.all()
    );
    assert_eq!(
        r.report["tenant"]["absent"],
        json!([ABSENT_KIND]),
        "the report does not carry the absence:\n{}",
        r.all()
    );
    assert_eq!(
        r.report["unauthored"],
        json!([]),
        "a tenant's kind is its checkout's to author, not this tree's:\n{}",
        r.all()
    );
}

/// 2 and 3. A row the checkout authors is compared by the lint's own
///    field comparator: the one that drifted is named by kind and field
///    with an excerpt, the one that agrees is not, both are counted —
///    reported under a bare run, a verdict (2) under --require-live.
#[test]
fn a_drifted_tenant_row_is_named_and_an_equal_one_is_not() {
    let dir = tenants("drift", Some(TENANT_FILE));
    let r = run(&dir, &[equal_row(), drifted_row()], &[]);
    assert_eq!(
        r.rc,
        Some(0),
        "a drift is reported, not failed, bare:\n{}",
        r.all()
    );
    assert!(
        r.err.contains(&format!("{DRIFTED_KIND}.description"))
            && r.err.contains("never written back"),
        "the tenant drift is not named with its live excerpt:\n{}",
        r.all()
    );
    assert!(
        !r.err.contains(&format!("{EQUAL_KIND}.")),
        "a tenant row that agrees with its file was named:\n{}",
        r.all()
    );
    let t = &r.report["tenant"];
    assert_eq!(t["state"], "compared", "{}", r.all());
    assert_eq!(
        t["compared"],
        2,
        "both owned rows are compared:\n{}",
        r.all()
    );
    let drift = t["drift"].as_array().cloned().unwrap_or_default();
    assert_eq!(drift.len(), 1, "{}", r.all());
    assert_eq!(drift[0]["kind"], DRIFTED_KIND);
    assert_eq!(drift[0]["field"], "description");
    assert_eq!(drift[0]["live_version"], 3);
    assert_eq!(
        r.report["fields"]["drifted"],
        0,
        "a tenant drift is not counted as the platform bundle's:\n{}",
        r.all()
    );

    let strict = run(&dir, &[equal_row(), drifted_row()], &["--require-live"]);
    assert_eq!(
        strict.rc,
        Some(2),
        "under --require-live a tenant drift is a verdict, as a platform one is:\n{}",
        strict.all()
    );
}

#[test]
fn an_equal_tenant_row_reads_ok_and_says_what_it_compared() {
    let dir = tenants("equal", Some(TENANT_FILE));
    let r = run(&dir, &[equal_row()], &[]);
    assert_eq!(r.rc, Some(0), "{}", r.all());
    assert!(
        r.last_line().contains("OK —") && r.last_line().contains("1 delivered-tenant row"),
        "an agreeing tenant row reads OK and is counted on the summary:\n{}",
        r.all()
    );
    assert_eq!(r.report["tenant"]["state"], "compared", "{}", r.all());
    assert_eq!(r.report["tenant"]["compared"], 1, "{}", r.all());
    assert_eq!(r.report["tenant"]["absent"], json!([]), "{}", r.all());
}

/// 4. NO CHECKOUT — the pod, boss-gcp, any host the converge does not
///    stage tenants on. The leg cannot answer, and says so: SKIPPED,
///    the directory it looked in, every kind left unchecked, and no OK
///    line, under either mode. The exit is left to the halves that DID
///    run: refusing the whole comparison over one input absent on this
///    host would throw away the platform half's true answer.
#[test]
fn no_checkout_is_a_skip_said_in_words_never_an_ok() {
    let dir = tenants("none", None);
    for mode in [&[][..], &["--require-live"][..]] {
        let r = run(&dir, &[equal_row()], mode);
        assert_eq!(r.rc, Some(0), "{mode:?}:\n{}", r.all());
        assert!(
            r.err.contains("SKIPPED the tenant comparison")
                && r.err.contains(&dir.display().to_string())
                && r.err.contains(EQUAL_KIND)
                && r.err.contains("NOTHING IS CLAIMED"),
            "{mode:?}: the skip does not say where it looked and what it left unchecked:\n{}",
            r.all()
        );
        assert!(
            !r.last_line().contains("OK —") && r.last_line().contains("SKIPPED"),
            "{mode:?}: a run whose tenant leg did not run must not read OK:\n{}",
            r.all()
        );
        assert_eq!(r.report["tenant"]["state"], "skipped", "{}", r.all());
        assert_eq!(
            r.report["tenant"]["unchecked"],
            json!([EQUAL_KIND]),
            "{}",
            r.all()
        );
    }
}

/// 5. The comparator itself, on a tenant's one-file bundle: compared
///    against a floor sized to a tenant rather than refused by the
///    platform bundle's 20.
#[test]
fn compare_reads_a_tenant_file_with_a_floor_sized_to_a_tenant() {
    let dir = scratch_dir("delivered-tenant-compare");
    let file = dir.join("workflows.toml");
    boss_testing::write_file(&file, TENANT_FILE);
    let live = dir.join("live.json");
    std::fs::write(
        &live,
        serde_json::to_vec(&json!([equal_row(), drifted_row()])).unwrap(),
    )
    .unwrap();
    let out = Command::new("bash")
        .arg(repo_root().join(LINT))
        .arg("--compare")
        .arg(&file)
        .arg(&live)
        .output()
        .expect("bash");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "a two-kind tenant file was refused:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("COUNTS\tparsed=2\tcompared=2\tdrifted=1")
            && stdout.contains(&format!("DRIFT\t{DRIFTED_KIND}\tdescription\tv3")),
        "{stdout}\n{stderr}"
    );
}

/// `--compare <file> <live>` over a planted tenant file, as test 5 runs
/// it: (exit, stdout, stderr).
fn compare(case: &str, file_body: &str, rows: &[Value]) -> (Option<i32>, String, String) {
    let dir = scratch_dir(&format!("delivered-tenant-{case}"));
    let file = dir.join("workflows.toml");
    boss_testing::write_file(&file, file_body);
    let live = dir.join("live.json");
    std::fs::write(&live, serde_json::to_vec(rows).unwrap()).unwrap();
    let out = Command::new("bash")
        .arg(repo_root().join(LINT))
        .arg("--compare")
        .arg(&file)
        .arg(&live)
        .output()
        .expect("bash");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A tenant file of ONE workflow whose description documents an example
/// containing a literal `[[workflow]]` line — legal TOML prose inside a
/// multiline string, which tomllib reads as one block.
fn one_workflow_file(description: &str) -> String {
    format!(
        "[[workflow]]\nkind = \"{EQUAL_KIND}\"\nlabel = \"Equal\"\ncategory = \"fixture\"\n\
         owning_team = \"{TEAM}\"\ndescription = \"\"\"{description}\"\"\"\n"
    )
}

/// 6. THE REVIEW'S FINDING (run 23be09ae R1, at 367b6a7f): the tenant
///    file's block count was checked against a regex over its raw LINES,
///    so a valid one-workflow file whose multiline description holds a
///    `[[workflow]]` line read as parsed 1 / compared 1 / drifted 0 and
///    was still REFUSED 7 — "the text holds 2 headers, the reader is
///    broken". A header inside a string is prose; only the parsed TOML
///    says how many blocks there are. Paired with the ordinary
///    description as its control (green before and after), and with the
///    floor's refusal, which must survive the repair.
#[test]
fn a_workflow_header_inside_a_multiline_string_is_prose_not_a_block() {
    for (case, description) in [
        ("prose-control", "A neutral sentence."),
        (
            "prose-header",
            "A documented example:\n[[workflow]]\nEnd of example.",
        ),
    ] {
        let row = tenant_row(EQUAL_KIND, "Equal", description);
        let (rc, out, err) = compare(case, &one_workflow_file(description), &[row]);
        assert_eq!(
            rc,
            Some(0),
            "{case}: a valid one-workflow tenant file equal to its live row was refused:\n{out}\n{err}"
        );
        assert!(
            out.contains("COUNTS\tparsed=1\tcompared=1\tdrifted=0") && !err.contains("REFUSED"),
            "{case}: {out}\n{err}"
        );
    }

    // The refusal the count sits beside still refuses: the same file
    // against an answer that admits none of its kinds compares nothing.
    let description = "A documented example:\n[[workflow]]\nEnd of example.";
    let (rc, out, err) = compare(
        "prose-header-floor",
        &one_workflow_file(description),
        &[absent_row()],
    );
    assert_eq!(
        rc,
        Some(7),
        "a tenant file comparing nothing must still refuse at the floor:\n{out}\n{err}"
    );
    assert!(err.contains("floor is 1"), "{out}\n{err}");
}
