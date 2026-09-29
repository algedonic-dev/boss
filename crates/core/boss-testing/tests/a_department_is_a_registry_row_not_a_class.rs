//! THE DEPARTMENT VOCABULARY LIVES ONCE: the departments registry
//! (backlog c87e3d6d, decided 2026-09-27 on page audit 9f7ba57d;
//! CLAUDE.md §9a — collapse it if you can).
//!
//! Until that day it lived twice. The `employee` department Classes —
//! `(employee, code)` rows with `member_attribute = department`, what an
//! employee's `department` was validated against — and the
//! `departments` table behind `GET /api/departments`, which every
//! protocol reader uses (readiness, the weekly retro, routing, the
//! chrome bar). Measured live: 9 Classes against 10 registry rows, 7
//! shared, and the founder's row naming `operations`, a department only
//! the Classes held. Backlog 7c0bebf5 recorded the same drift (at 9 vs
//! 13) and closed as a duplicate while the drift stayed live, which is
//! what a comment-level fix buys.
//!
//! So an employee's and an agent's department now validate against the
//! registry (boss-people `departments`, boss-jobs agents door), and this
//! file holds the tree to the one list:
//!
//!   * no seed declares a department Class — a tenant publishing one
//!     recreates the second list on a fresh instance;
//!   * every department an example tenant's people, agents and hires
//!     sit in is a department that example declares, or the platform's
//!     own — a code only a Class held would be refused at the door;
//!   * no web reader asks the Class drawer for department names.
//!
//! What it deliberately does NOT read: infra/postgres/retired-examples/,
//! which lists the department Classes 01-registries.sql seeds so that
//! the eviction can remove them — residue named in order to delete it,
//! not a declaration — and the migration itself, which cannot be edited
//! (migrate.sh refuses a changed checksum).

use boss_testing::repo_root;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} — the derivation cannot be read, so nothing here is a verdict",
            path.display()
        )
    })
}

/// Every tenant seeds directory the tree ships: the examples and the
/// test fixtures that copy a real tenant.
fn seed_dirs() -> Vec<PathBuf> {
    let root = repo_root();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root.join("examples"))
        .expect("examples/ is readable")
        .map(|e| e.expect("an examples entry").path().join("seeds"))
        .filter(|p| p.is_dir())
        .collect();
    let fixtures = root.join("crates/orchestrators/boss-cli/tests/fixtures");
    dirs.extend(
        std::fs::read_dir(&fixtures)
            .expect("the boss-cli fixtures are readable")
            .map(|e| e.expect("a fixtures entry").path().join("seeds"))
            .filter(|p| p.is_dir()),
    );
    assert!(
        dirs.iter().any(|d| d.ends_with("examples/brewery/seeds")),
        "the brewery's seeds were not found — this test is reading nothing: {dirs:?}"
    );
    dirs
}

/// (file, code) for every `(employee, *, department)` Class row a seed
/// directory declares, in either spelling the contract admits.
fn department_classes(dir: &Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let is_department = |kind: Option<&str>, attr: Option<&str>| {
        kind == Some("employee") && attr == Some("department")
    };
    let json = dir.join("classes.json");
    if json.is_file() {
        let rows: serde_json::Value =
            serde_json::from_str(&read(&json)).expect("classes.json parses");
        for r in rows.as_array().expect("classes.json is an array") {
            if is_department(r["subject_kind"].as_str(), r["member_attribute"].as_str()) {
                found.push((
                    json.display().to_string(),
                    r["code"].as_str().unwrap_or("?").to_string(),
                ));
            }
        }
    }
    let toml_path = dir.join("classes.toml");
    if toml_path.is_file() {
        let v: toml::Value = toml::from_str(&read(&toml_path)).expect("classes.toml parses");
        for r in v
            .get("class")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default()
        {
            if is_department(
                r.get("subject_kind").and_then(|s| s.as_str()),
                r.get("member_attribute").and_then(|s| s.as_str()),
            ) {
                found.push((
                    toml_path.display().to_string(),
                    r.get("code")
                        .and_then(|s| s.as_str())
                        .unwrap_or("?")
                        .to_string(),
                ));
            }
        }
    }
    found
}

#[test]
fn no_seed_declares_a_department_class() {
    let found: Vec<(String, String)> = seed_dirs()
        .iter()
        .flat_map(|d| department_classes(d))
        .collect();
    assert!(
        found.is_empty(),
        "a seed declares an (employee, department) Class — the second department list \
         backlog c87e3d6d collapsed onto the departments registry. Declare the department \
         in seeds/departments.toml instead: {found:?}"
    );
}

/// The departments the platform's own people sit in — the operator
/// baseline's hires — which the migration seeds on every instance and
/// no example declares.
fn platform_departments() -> BTreeSet<String> {
    let v: toml::Value = toml::from_str(&read(
        &repo_root().join("infra/operator-baseline/operator_hires.toml"),
    ))
    .expect("operator_hires.toml parses");
    let found: BTreeSet<String> = v["hire"]
        .as_array()
        .expect("the baseline hires someone")
        .iter()
        .filter_map(|h| h.get("department").and_then(|d| d.as_str()))
        .map(str::to_string)
        .collect();
    assert_eq!(
        found,
        BTreeSet::from(["it".to_string()]),
        "the platform's people sit in IT"
    );
    found
}

/// Every department code a seeds directory's people, agents and hires
/// sit in, with the file that names it.
fn departments_sat_in(dir: &Path) -> Vec<(String, String)> {
    let mut sat = Vec::new();
    let employees = dir.join("employees.json");
    if employees.is_file() {
        let rows: serde_json::Value =
            serde_json::from_str(&read(&employees)).expect("employees.json parses");
        for e in rows.as_array().expect("employees.json is an array") {
            if let Some(d) = e["department"].as_str() {
                sat.push((employees.display().to_string(), d.to_string()));
            }
        }
    }
    for (file, table) in [("agents.toml", "agent"), ("operator_hires.toml", "hire")] {
        let path = dir.join(file);
        if !path.is_file() {
            continue;
        }
        let v: toml::Value = toml::from_str(&read(&path)).expect("the seed parses");
        for row in v
            .get(table)
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default()
        {
            if let Some(d) = row.get("department").and_then(|d| d.as_str()) {
                sat.push((path.display().to_string(), d.to_string()));
            }
        }
    }
    sat
}

/// The codes a seeds directory's departments.toml declares (none when
/// it has no such file).
fn declared_departments(dir: &Path) -> BTreeSet<String> {
    let path = dir.join("departments.toml");
    if !path.is_file() {
        return BTreeSet::new();
    }
    let v: toml::Value = toml::from_str(&read(&path)).expect("departments.toml parses");
    v.get("department")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|d| d.get("code").and_then(|c| c.as_str()))
        .map(str::to_string)
        .collect()
}

/// A policy grant scoped `department:<code>` admits a caller only when
/// the caller sits in `<code>` (`Predicate::DepartmentIs`), so a code no
/// department row holds admits no one, silently: measured 2026-09-28,
/// five brewery grants (head-brewer x2, lab-tech, qa-supervisor,
/// recruiter) named `brewhouse`, `lab` and `hr`, which the collapse
/// onto the registry left undeclared (backlog f7daf124). Every example
/// with a policy seed is held to the departments it declares, or the
/// platform's.
#[test]
fn every_department_a_policy_scope_names_is_one_its_tenant_declares_or_the_platforms() {
    let platform = platform_departments();
    let root = repo_root();
    let mut judged = 0;
    for dir in seed_dirs()
        .iter()
        .filter(|d| d.starts_with(root.join("examples")))
    {
        let path = dir.join("policy_rules.toml");
        if !path.is_file() {
            continue;
        }
        let declared = declared_departments(dir);
        let v: toml::Value = toml::from_str(&read(&path)).expect("policy_rules.toml parses");
        let scopes: Vec<String> = v
            .get("grants")
            .and_then(|g| g.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|g| g.get("scope").and_then(|s| s.as_str()))
            .filter_map(|s| s.strip_prefix("department:"))
            .map(str::to_string)
            .collect();
        judged += scopes.len();
        let stray: Vec<&String> = scopes
            .iter()
            .filter(|d| !declared.contains(*d) && !platform.contains(*d))
            .collect();
        assert!(
            stray.is_empty(),
            "{}: a department-scoped grant names a code neither departments.toml nor the \
             platform ({platform:?}) declares, so it admits no one: {stray:?}",
            path.display()
        );
    }
    assert!(
        judged >= 10,
        "only {judged} department scopes read (16 in the brewery on 2026-09-28) — \
         this test is reading nothing"
    );
}

/// Only the examples: a fixture copies a real tenant file by file and
/// need not carry its departments.toml, and a real tenant's roster is
/// its own repo's to hold.
#[test]
fn every_department_an_example_sits_in_is_one_it_declares_or_the_platforms() {
    let platform = platform_departments();
    let root = repo_root();
    for dir in seed_dirs()
        .iter()
        .filter(|d| d.starts_with(root.join("examples")))
    {
        let declared = declared_departments(dir);
        let sat = departments_sat_in(dir);
        let stray: Vec<&(String, String)> = sat
            .iter()
            .filter(|(_, d)| !declared.contains(d) && !platform.contains(d))
            .collect();
        assert!(
            stray.is_empty(),
            "{}: a department an employee, agent or hire sits in is neither declared in \
             departments.toml nor the platform's ({platform:?}) — the people door would refuse \
             it since c87e3d6d: {stray:?}",
            dir.display()
        );
    }
}

/// The SPA's department names come from `GET /api/departments` (the
/// web-kit loader), never from the employee Class drawer.
#[test]
fn no_web_reader_asks_the_class_drawer_for_departments() {
    let root = repo_root();
    let mut offenders = Vec::new();
    let mut stack = vec![root.join("apps/web/src"), root.join("libs/web-kit/src")];
    let mut scanned = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("a source directory is readable") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let is_source = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e, "ts" | "svelte"));
            if !is_source {
                continue;
            }
            scanned += 1;
            let text = read(&path);
            if text.contains("classesFor('employee', 'department')")
                || text.contains("classesFor(\"employee\", \"department\")")
            {
                offenders.push(path.display().to_string());
            }
        }
    }
    assert!(
        scanned > 100,
        "only {scanned} source files — the walk is reading nothing"
    );
    assert!(
        offenders.is_empty(),
        "these read department names from the employee Class drawer, which no longer holds \
         them (c87e3d6d) — read `departments()` from \
         libs/web-kit/src/session/departments.svelte.ts: {offenders:?}"
    );
}
