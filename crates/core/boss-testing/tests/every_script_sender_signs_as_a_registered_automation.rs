//! Every `automation:<id>` a script under infra/ signs as holds a row in
//! the automations registry (`infra/platform/automations/`), so a NEW
//! sender without a row fails here, by name, before it ever writes
//! (backlog ddf0773e, design abf9eeae).
//!
//! WHY. The design's enforce arm waits on the role-report tally showing
//! zero unregistered writers for 72 hours, and an id with no row gets the
//! read-only floor once it enforces. Car 1 built the roster from one week
//! of the audit log, which is a list of who WROTE that week: measured
//! 2026-10-07 on `GET /api/jobs/actor-role-reports`, five automation ids
//! the tree spells had no row (three converge and observer reads, the
//! recorded probe's reader, the disk sweep's train read), and ten more
//! were spelled in scripts that had not run in the window (the car-4
//! precondition note on the item: a clean 72 hours misses whatever fires
//! less often than that). Nothing held the roster to the tree, so each
//! was found by reading a tally. This is that pin.
//!
//! WHAT IT READS. The sender list is DERIVED, never written down: every
//! script under infra/ (a `.sh` or `.py` name, or a `#!` line naming a
//! shell or python) and every live — non-comment — line of it.
//!
//! - A literal `automation:<slug>` must be answered by the registry's own
//!   `row_of_record`: a row of that id.
//! - A literal `automation:<slug>:…` names a FAMILY member (the ops
//!   runner's `automation:ops-runner:<host>:<run>`); the prefix must be
//!   one a row declares in `signs_for`.
//! - `automation:$VAR` is an id made at run time. The file and variable
//!   must be listed in RUN_TIME_IDS with where the value comes from, so a
//!   new one is a decision somebody made rather than a sender nobody saw.
//! - `BOSS_CONVERGE_NAME=<literal>` is the one run-time id the tree does
//!   spell (infra/estate/node-roles.sh signs its estate read as
//!   `automation:<that name>`), so each literal is held to the roster too.
//!
//! It is a NAME scan over live lines: a jq filter that compares an actor
//! to `automation:ops-runner` is held like a sender, which is right — it
//! names a writer the registry should know.
//!
//! OUT OF SCOPE, and said so rather than implied: ids signed from Rust
//! (`boss_core::roles::PROBE_READER_ACTOR`, each service's
//! `User::service`, the dispatcher's own) and from a manifest's env. The
//! Rust ids that reach the jobs API today are in the roster by
//! measurement, not by this pin.
//!
//! tree-wide pin — it scans every file under infra/, which no
//! changed-file map attributes to this crate, so every scoped gate runs
//! it whatever its scope (`tree_wide_pins` in infra/gate.sh).

use boss_jobs::agents::automations::{
    AutomationActor, load_automations_dir, platform_automations_path, row_of_record,
};
use boss_testing::repo_root;
use regex::Regex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Scripts that sign against a stack on the operator's own machine. Their
/// ids never reach the estate, so the estate's registry owes them no row.
const LOCAL_STACK: &[(&str, &str)] = &[
    (
        "infra/postgres/reset-to-baseline.sh",
        "an operator's tool against a local stack, not a caller of the estate",
    ),
    (
        "infra/postgres/validate-brewery-sim.sh",
        "an operator's tool against a local stack, not a caller of the estate",
    ),
];

/// `automation:$VAR` — an id made at run time, by file and variable.
const RUN_TIME_IDS: &[(&str, &str, &str)] = &[
    (
        "infra/estate/node-roles.sh",
        "prefix",
        "the caller's BOSS_CONVERGE_NAME; every literal value in the tree is held to the roster by this pin",
    ),
    (
        "infra/forge/cluster-node-lib.sh",
        "ME",
        "DEBT: one read-only id per ops verb that sources this lib (its estate read, signed at the audit-readonly floor an unregistered id gets anyway, design abf9eeae decision 3). No row answers a verb's name and `signs_for` cannot express the family, because the ids share no prefix; each run still shows as unregistered in the role-report tally",
    ),
    (
        "infra/ops/retire-ops-runner.sh",
        "ME",
        "DEBT: the same per-verb reader shape as cluster-node-lib.sh, for one retire verb",
    ),
];

/// Files that hand node-roles.sh their own verb name
/// (`BOSS_CONVERGE_NAME="$ME"`): the same per-verb reader debt.
const VERB_NAMED_CONVERGE_READS: &[&str] = &[
    "infra/gcp/retire-second-stack.sh",
    "infra/gcp/uninstall-not-in-role.sh",
    "infra/ops/retire-ops-runner.sh",
];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.filter_map(Result::ok) {
        let p = entry.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn is_script(path: &Path, text: &str) -> bool {
    if path.extension().is_some_and(|x| x == "sh" || x == "py") {
        return true;
    }
    path.extension().is_none()
        && text.lines().next().is_some_and(|l| {
            l.starts_with("#!")
                && (l.ends_with("sh")
                    || l.contains("bash")
                    || l.contains("/sh ")
                    || l.contains("python"))
        })
}

fn live(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter(|l| !l.trim_start().starts_with('#'))
}

/// What one live line signs as.
#[derive(Debug, Default, PartialEq, Eq)]
struct Spelled {
    /// `automation:<slug>` — the whole id.
    ids: Vec<String>,
    /// `automation:<slug>:` — a family prefix, from a longer id.
    families: Vec<String>,
    /// `automation:$VAR` — the variable.
    run_time: Vec<String>,
    /// `BOSS_CONVERGE_NAME=<literal>` values.
    converge_names: Vec<String>,
    /// `BOSS_CONVERGE_NAME=$VAR`.
    converge_from_var: bool,
}

/// Compiled once: the scan reads every live script line under infra/,
/// and three compiles a line took four minutes.
fn matchers() -> &'static (Regex, Regex, Regex) {
    static M: std::sync::OnceLock<(Regex, Regex, Regex)> = std::sync::OnceLock::new();
    M.get_or_init(|| {
        (
            Regex::new(r"automation:([a-z0-9][a-z0-9-]*)(:[A-Za-z0-9$%{<])?").unwrap(),
            Regex::new(r"automation:\$\{?([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            Regex::new(r#"BOSS_CONVERGE_NAME=["']?(\$\{?[A-Za-z_]+\}?|[a-z0-9][a-z0-9-]*)"#)
                .unwrap(),
        )
    })
}

fn spelled(line: &str) -> Spelled {
    let (id, var, converge) = matchers();
    let mut out = Spelled::default();
    if !line.contains("automation:") && !line.contains("BOSS_CONVERGE_NAME=") {
        return out;
    }
    for c in id.captures_iter(line) {
        match c.get(2) {
            Some(_) => out.families.push(format!("automation:{}:", &c[1])),
            None => out.ids.push(format!("automation:{}", &c[1])),
        }
    }
    out.run_time
        .extend(var.captures_iter(line).map(|c| c[1].to_string()));
    for c in converge.captures_iter(line) {
        if c[1].starts_with('$') {
            out.converge_from_var = true;
        } else {
            out.converge_names.push(c[1].to_string());
        }
    }
    out
}

/// (rel path, line number, what it spells) for every live script line
/// under `root`/infra that spells anything.
fn senders(root: &Path) -> Vec<(String, usize, Spelled)> {
    let mut files = Vec::new();
    walk(&root.join("infra"), &mut files);
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        if !is_script(&f, &text) {
            continue;
        }
        let Ok(rel) = f.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().into_owned();
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            let s = spelled(line);
            if s != Spelled::default() {
                out.push((rel.clone(), n + 1, s));
            }
        }
    }
    out
}

fn roster() -> Vec<AutomationActor> {
    load_automations_dir(Path::new(platform_automations_path()))
        .expect("the in-tree automations bundle loads")
}

fn local_stack(rel: &str) -> bool {
    LOCAL_STACK.iter().any(|(p, _)| *p == rel)
}

#[test]
fn every_script_sender_signs_as_a_registered_automation() {
    let rows = roster();
    let all = senders(&repo_root());
    let mut unregistered: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut seen = 0;
    for (rel, n, s) in all.iter().filter(|(rel, _, _)| !local_stack(rel)) {
        let names = s
            .ids
            .iter()
            .cloned()
            .chain(s.converge_names.iter().map(|c| format!("automation:{c}")));
        for id in names {
            seen += 1;
            if row_of_record(&rows, &id).is_none() {
                unregistered
                    .entry(id)
                    .or_default()
                    .push(format!("{rel}:{n}"));
            }
        }
        for family in &s.families {
            // A member of the family, as the registry would be asked.
            if row_of_record(&rows, &format!("{family}x")).is_none() {
                unregistered
                    .entry(format!("{family}<…>"))
                    .or_default()
                    .push(format!("{rel}:{n}"));
            }
        }
    }
    assert!(
        seen >= 40,
        "the scan found {seen} spelled ids, fewer than forty — it has stopped seeing the tree, \
         and a scanner that sees nothing passes everything"
    );
    let report: Vec<String> = unregistered
        .iter()
        .map(|(id, at)| format!("{id}  ({})", at.join(", ")))
        .collect();
    assert!(
        report.is_empty(),
        "these scripts sign as an automation id that no row in infra/platform/automations/ \
         answers for. The role resolver reports such a caller as unregistered, and under \
         `enforce` gives it the read-only floor (design abf9eeae). Add \
         infra/platform/automations/<slug>.toml — the README there shows a row — at the least \
         role its requests need (`audit-readonly` for a reader), or list the file in \
         LOCAL_STACK here if it never calls the estate:\n  {}",
        report.join("\n  ")
    );
}

#[test]
fn an_id_made_at_run_time_is_a_listed_decision() {
    let all = senders(&repo_root());
    let mut unlisted = Vec::new();
    for (rel, n, s) in all.iter().filter(|(rel, _, _)| !local_stack(rel)) {
        for var in &s.run_time {
            if !RUN_TIME_IDS.iter().any(|(p, v, _)| p == rel && v == var) {
                unlisted.push(format!("{rel}:{n} signs automation:${var}"));
            }
        }
        if s.converge_from_var && !VERB_NAMED_CONVERGE_READS.contains(&rel.as_str()) {
            unlisted.push(format!("{rel}:{n} sets BOSS_CONVERGE_NAME from a variable"));
        }
    }
    assert!(
        unlisted.is_empty(),
        "these scripts sign as an automation id made at run time, which the roster pin cannot \
         hold to a row. Spell the id as a literal, or list the file and variable in \
         RUN_TIME_IDS (or VERB_NAMED_CONVERGE_READS) here with where the value comes \
         from:\n  {}",
        unlisted.join("\n  ")
    );
}

/// A listed entry that no longer matches anything. Named so the lists can
/// be trimmed; not a failure, because two cars may fix and list the same
/// file and a pin that went red on the assembled tree for that would
/// punish the fix.
#[test]
fn stale_entries_are_named() {
    let all = senders(&repo_root());
    for (rel, why) in LOCAL_STACK {
        if !all.iter().any(|(r, _, _)| r == rel) {
            println!("stale: {rel} is listed and spells no automation id now ({why})");
        }
    }
    for (rel, var, _) in RUN_TIME_IDS {
        if !all
            .iter()
            .any(|(r, _, s)| r == rel && s.run_time.iter().any(|v| v == var))
        {
            println!("stale: {rel} is listed and no longer signs automation:${var}");
        }
    }
    for rel in VERB_NAMED_CONVERGE_READS {
        if !all.iter().any(|(r, _, s)| r == rel && s.converge_from_var) {
            println!(
                "stale: {rel} is listed and no longer sets BOSS_CONVERGE_NAME from a variable"
            );
        }
    }
}

#[test]
fn the_matchers_see_each_spelling() {
    let ids = |l: &str| spelled(l).ids;
    assert_eq!(
        ids(r#"-H 'x-boss-user: {"id":"automation:install-smoke","role":"platform-admin"}'"#),
        ["automation:install-smoke"]
    );
    assert_eq!(
        ids(r#"local actor="${BOSS_SWEEP_ACTOR:-automation:disk-floor-sweep}""#),
        ["automation:disk-floor-sweep"]
    );
    assert_eq!(
        ids("$(sor_reader_header 'automation:tag-release-reader')"),
        ["automation:tag-release-reader"]
    );
    // A longer id names its family, not a row of its own.
    let member = spelled(r#"ACTOR="automation:ops-runner:$HOST:$RUN""#);
    assert!(member.ids.is_empty(), "{member:?}");
    assert_eq!(member.families, ["automation:ops-runner:"]);
    // A run-time id names its variable and no row.
    let made = spelled(r#"$(sor_reader_header "automation:$ME")"#);
    assert!(made.ids.is_empty() && made.families.is_empty(), "{made:?}");
    assert_eq!(made.run_time, ["ME"]);
    assert_eq!(spelled("x automation:${prefix}/audit").run_time, ["prefix"]);
    // The converge name, literal and handed down.
    assert_eq!(
        spelled(r#"BOSS_CONVERGE_NAME="forge-converge" read_node_roles "$NODE_ID""#).converge_names,
        ["forge-converge"]
    );
    assert_eq!(
        spelled("        BOSS_CONVERGE_NAME=observe-units \\").converge_names,
        ["observe-units"]
    );
    assert!(spelled(r#"BOSS_CONVERGE_NAME="$ME" read_node_roles "$N""#).converge_from_var);
    // The default inside node-roles.sh reads the variable; it sets nothing.
    assert_eq!(
        spelled(r#"local prefix="${BOSS_CONVERGE_NAME:-converge}""#),
        Spelled::default()
    );
    // A comment is not a sender.
    assert_eq!(live("# automation:not-a-sender\nx\n").count(), 1);
}

#[test]
fn each_entry_is_listed_once_with_its_reason() {
    let mut seen: Vec<String> = Vec::new();
    for (rel, why) in LOCAL_STACK {
        assert!(!why.is_empty(), "{rel} is listed with no reason");
        assert!(!seen.contains(&rel.to_string()), "{rel} is listed twice");
        seen.push(rel.to_string());
    }
    let mut seen: Vec<String> = Vec::new();
    for (rel, var, why) in RUN_TIME_IDS {
        let key = format!("{rel} ${var}");
        assert!(!why.is_empty(), "{key} is listed with no reason");
        assert!(!seen.contains(&key), "{key} is listed twice");
        seen.push(key);
    }
}
