//! tree-wide pin — it walks every unit file under infra/, and a unit
//! authored anywhere there is held to it.
//!
//! No systemd unit in the tree EXECUTES a path under a home directory —
//! as its command, as an argument to an interpreter, as its working
//! directory or as its environment file — none runs as an account that
//! is not root, and no root unit is handed a path under a home — except
//! the units named here, each with the reason it still does and the work
//! that removes it. One start line no unit file shows, the ops runner's,
//! is a row of its own (backlog a604a35b;
//! decided by David on design-doc c98c79aa, question root-tree, and as
//! position B on the packet, 2026-10-07).
//!
//! THE DEFECT. Eight forge units executed `/home/david/boss/infra/…`.
//! Five of them ran as root — the converge, the backup, the CI reaper
//! and the two observers — so a shell as that checkout's owner edited
//! what root runs on the host that holds the admin kubeconfig, the
//! forge's tokens and the estate machine token (review 9a1e289b). Three
//! ran as the owner himself and reached root through a passwordless
//! `sudo docker`. Nothing in the tree could tell a unit that had moved
//! from one that had not, so the next unit authored beside them would
//! have copied the path.
//!
//! WHAT THIS IS. A roster, not a prohibition with holes: every exception
//! is a row saying what the unit still needs from the account, and a row
//! that is no longer true fails as loudly as a unit that is not on it —
//! so a car that moves a unit off must delete its row, and the roster
//! can only shrink without anyone having to remember it.

use boss_testing::repo_root;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Units whose Exec lines still name a path under /home/, and why.
const EXECUTES_FROM_A_HOME: &[(&str, &str)] = &[
    (
        "infra/forge/cluster-deploy-runner.service",
        "builds the cluster image on david's ROOTLESS docker daemon, pushes it with his registry login, fetches his checkout and the tenants' with his credential helper and reads his kubeconfig: moving it is a decision about which daemon builds and who holds the push credential (handed back on backlog a604a35b), not a path swap",
    ),
    (
        "infra/forge/disk-floor-sweep.service",
        "prunes david's rootless docker daemon as its own user and verifies old tags through his registry login before removing them: it moves with the cluster converge, when that daemon does",
    ),
];

/// Start lines an INSTALLER writes, so no unit file in the tree shows
/// them: (what, the file that decides it, the line in it, why).
const GENERATED_FROM_A_HOME: &[(&str, &str, &str, &str)] = &[(
    "boss-ops-runner.service on the forge (drop-in forge.conf)",
    "infra/forge/install.sh",
    "ops_runner_repo=\"${BOSS_FORGE_REPO_DIR:-/home/david/boss}\"",
    "root's ops runner executes every verb script from the checkout, every minute: the largest road this car leaves open. It waits on stage 2 of design c98c79aa — separating where a verb's code runs from the git checkout the publish, merge and tag verbs work on (follow-up of backlog a604a35b); that car deletes this row and the INSTALL_OPS_RUNNER_REPO line",
)];

/// Units that run as an account other than root, the account, and why.
const RUNS_AS_AN_ACCOUNT: &[(&str, &str, &str)] = &[
    (
        "infra/forge/cluster-deploy-runner.service",
        "david",
        "see EXECUTES_FROM_A_HOME: bound to his rootless docker daemon and registry login",
    ),
    (
        "infra/forge/disk-floor-sweep.service",
        "david",
        "see EXECUTES_FROM_A_HOME: prunes his rootless docker daemon",
    ),
    (
        "infra/boss-codebase-metrics.service",
        "david",
        "boss-gcp daily, from /opt/boss: the boss-gcp stage of design c98c79aa, after the forge's (follow-up of backlog a604a35b)",
    ),
    (
        "infra/boss-protocol-drift.service",
        "david",
        "boss-gcp daily, from /opt/boss: the boss-gcp stage of design c98c79aa (follow-up of backlog a604a35b)",
    ),
    (
        "infra/boss-surface-usage.service",
        "david",
        "boss-gcp daily, from /opt/boss: the boss-gcp stage of design c98c79aa (follow-up of backlog a604a35b)",
    ),
    (
        "infra/boss-views-catchup.service",
        "david",
        "boss-gcp, from /opt/boss: the boss-gcp stage of design c98c79aa (follow-up of backlog a604a35b)",
    ),
    (
        "infra/boss-search-reindex.service",
        "david",
        "boss-gcp, from /opt/boss: the boss-gcp stage of design c98c79aa (follow-up of backlog a604a35b)",
    ),
    (
        "infra/boss-forge-token-audit.service",
        "david",
        "in no role of infra/estate/roles.toml and installed nowhere (measured 2026-10-07): retire it or move it with boss-gcp's",
    ),
    (
        "infra/train/boss-train.service",
        "david",
        "the host conductor unit, from before the conductor ran in the cluster: retire it or move it with boss-gcp (follow-up of backlog a604a35b)",
    ),
    (
        "infra/ml/boss-ml-inference-batch.service",
        "boss",
        "boss-gcp's ML batch, a service account and not a person; listed so the account is a decision and not an accident",
    ),
];

/// `Environment=` values under /home/ on a unit that runs as root: a
/// path root reads that another account writes. (unit, variable, why).
const ROOT_IS_HANDED_A_HOME_PATH: &[(&str, &str, &str)] = &[(
    "infra/forge/cluster-watchdog.service",
    "BOSS_FORGE_LAST_BUILT",
    "the build stamp the cluster converge writes as david; cluster-watchdog.sh takes it only as seven hex characters and never prints anything else it finds — held by effect in the_watchdog_reads_the_admin_kubeconfig_david_placed.rs, a_stamp_that_is_not_a_build_name_rolls_nothing_and_is_never_printed",
)];

fn units() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "service") {
                out.push(p);
            }
        }
    }
    let root = repo_root();
    let mut found = Vec::new();
    walk(&root.join("infra"), &mut found);
    found.sort();
    assert!(
        found.len() >= 20,
        "the walk found only {} units",
        found.len()
    );
    found
        .into_iter()
        .map(|p| {
            let rel = p.strip_prefix(&root).unwrap().display().to_string();
            (rel, std::fs::read_to_string(&p).unwrap())
        })
        .collect()
}

/// EVERY WAY A UNIT REACHES A HOME DIRECTORY TO RUN OR CONFIGURE ITSELF,
/// as `<directive>: <word>` (review 5f3736a2, F3). The first cut read
/// only the first word of an Exec line, so a unit that went through an
/// interpreter (`ExecStart=/bin/bash /home/…/x.sh`), started in a home
/// (`WorkingDirectory=`, where a relative command or a sourced file
/// resolves), or took its environment from one (`EnvironmentFile=` —
/// PATH, LD_PRELOAD, BASH_ENV) passed it. Read here: every word of every
/// Exec line, an assignment's right-hand side included, and the
/// directives that name a directory or a file systemd itself opens.
/// `Environment=` is the third test's, for root units.
fn home_reaches(unit: &str) -> Vec<String> {
    const PATH_DIRECTIVES: &[&str] = &[
        "WorkingDirectory",
        "RootDirectory",
        "RootImage",
        "EnvironmentFile",
        "ExecSearchPath",
        "BindPaths",
        "BindReadOnlyPaths",
        "LoadCredential",
    ];
    let under_home = |word: &str| -> bool {
        let w = word.trim_matches(['"', '\'']);
        let w = w.trim_start_matches(['-', '@', ':', '+', '!', '~']);
        w.starts_with("/home/")
            || w.split(['=', ':'])
                .any(|part| part.trim_matches(['"', '\'']).starts_with("/home/"))
    };
    let mut found = Vec::new();
    for line in unit.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !(key.starts_with("Exec") || PATH_DIRECTIVES.contains(&key)) {
            continue;
        }
        for word in value.split_whitespace() {
            if under_home(word) {
                found.push(format!("{key}: {word}"));
            }
        }
    }
    found
}

fn user(unit: &str) -> String {
    unit.lines()
        .filter_map(|l| l.strip_prefix("User="))
        .next_back()
        .unwrap_or("root")
        .trim()
        .to_string()
}

#[test]
fn no_unit_executes_a_path_under_a_home_but_the_roster() {
    let roster: BTreeMap<&str, &str> = EXECUTES_FROM_A_HOME.iter().copied().collect();
    let mut offenders = Vec::new();
    let mut stale: Vec<&str> = roster.keys().copied().collect();
    for (rel, text) in units() {
        let from_home = home_reaches(&text);
        if from_home.is_empty() {
            continue;
        }
        stale.retain(|r| *r != rel);
        if !roster.contains_key(rel.as_str()) {
            offenders.push(format!(
                "{rel} (User={}): {}",
                user(&text),
                from_home.join(", ")
            ));
        }
    }
    assert!(
        offenders.is_empty(),
        "these units execute a path under a home directory, which that account can rewrite — run them from root's tree (/var/lib/boss/tree/current, infra/forge/root-tree.sh):\n  {}",
        offenders.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "these roster rows are no longer true — the unit executes nothing under a home; delete the row: {stale:?}"
    );
}

#[test]
fn no_unit_runs_as_an_account_but_the_roster() {
    let roster: BTreeMap<&str, &str> = RUNS_AS_AN_ACCOUNT
        .iter()
        .map(|(u, a, _)| (*u, *a))
        .collect();
    let mut offenders = Vec::new();
    let mut stale: Vec<&str> = roster.keys().copied().collect();
    for (rel, text) in units() {
        let who = user(&text);
        if who == "root" {
            continue;
        }
        match roster.get(rel.as_str()) {
            Some(want) if *want == who => stale.retain(|r| *r != rel),
            Some(want) => offenders.push(format!("{rel}: User={who}, and the roster says {want}")),
            None => offenders.push(format!("{rel}: User={who}")),
        }
    }
    assert!(
        offenders.is_empty(),
        "these units run as an account that is not root and are not on the roster — an unattended service on a person's account is that person's every shell away from it (backlog a604a35b):\n  {}",
        offenders.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "these roster rows are no longer true — the unit runs as root; delete the row: {stale:?}"
    );
    for (unit, _, why) in RUNS_AS_AN_ACCOUNT {
        assert!(why.len() > 40, "{unit}: a roster row says why");
    }
    for (unit, why) in EXECUTES_FROM_A_HOME {
        assert!(why.len() > 40, "{unit}: a roster row says why");
    }
}

#[test]
fn a_root_unit_is_handed_no_path_under_a_home_but_the_roster() {
    let mut offenders = Vec::new();
    let mut stale: Vec<(&str, &str)> = ROOT_IS_HANDED_A_HOME_PATH
        .iter()
        .map(|(u, v, _)| (*u, *v))
        .collect();
    for (rel, text) in units() {
        if user(&text) != "root" {
            continue;
        }
        for line in text.lines().filter(|l| l.starts_with("Environment=")) {
            let Some((name, value)) = line["Environment=".len()..].split_once('=') else {
                continue;
            };
            if !value.starts_with("/home/") {
                continue;
            }
            if ROOT_IS_HANDED_A_HOME_PATH
                .iter()
                .any(|(u, v, _)| *u == rel && *v == name)
            {
                stale.retain(|(u, v)| !(*u == rel && *v == name));
            } else {
                offenders.push(format!("{rel}: {line}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these root units are handed a path under a home directory — a file another account writes and can repoint, read or replayed by root:\n  {}",
        offenders.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "these roster rows are no longer true; delete them: {stale:?}"
    );
}

/// THE PIN'S OWN CONTROLS: one fixture unit per shape, each of which
/// must be read as reaching a home — and two that must not. The four
/// middle shapes passed the first cut of this file (review 5f3736a2,
/// F3, ran).
#[test]
fn the_pin_reads_every_way_a_unit_reaches_a_home() {
    let unit = |body: &str| format!("[Service]\nType=oneshot\nUser=root\n{body}\n");
    for (shape, body) in [
        (
            "the command itself",
            "ExecStart=/home/david/boss/infra/x.sh",
        ),
        (
            "through an interpreter",
            "ExecStart=/bin/bash /home/david/boss/infra/x.sh",
        ),
        (
            "a best-effort pre-step through sh -c",
            "ExecStartPre=-/bin/sh -c /home/david/boss/infra/pre.sh",
        ),
        (
            "a post-step's argument",
            "ExecStartPost=/usr/bin/env BASH_ENV=/home/david/.bashrc /usr/bin/true",
        ),
        (
            "the working directory",
            "WorkingDirectory=/home/david/boss\nExecStart=/usr/bin/env bash infra/x.sh",
        ),
        (
            "an optional working directory",
            "WorkingDirectory=-/home/david/boss\nExecStart=/usr/bin/true",
        ),
        (
            "the environment file",
            "EnvironmentFile=-/home/david/.config/boss/unit.env\nExecStart=/usr/bin/true",
        ),
        (
            "a stop hook",
            "ExecStart=/usr/bin/true\nExecStopPost=-/home/david/boss/infra/boss-step.sh x run",
        ),
    ] {
        assert!(
            !home_reaches(&unit(body)).is_empty(),
            "a unit that reaches a home by {shape} passed the pin:\n{body}"
        );
    }
    for (shape, body) in [
        (
            "root's tree",
            "ExecStart=/var/lib/boss/tree/current/infra/x.sh\nEnvironmentFile=/etc/boss/sor.env",
        ),
        (
            "a comment naming a home",
            "# ExecStart=/home/david/boss/infra/x.sh\nExecStart=/usr/bin/true",
        ),
    ] {
        assert!(
            home_reaches(&unit(body)).is_empty(),
            "{shape} was read as reaching a home: {:?}",
            home_reaches(&unit(body))
        );
    }
}

/// THE ROAD NO UNIT FILE SHOWS (review 5f3736a2, F3). The ops runner's
/// ExecStart is not in infra/ops/boss-ops-runner.service: the installer
/// GENERATES it into a drop-in, and on the forge it names the checkout —
/// so root executes /home/david/boss every minute, with every verb
/// script, and every test above is green. It is a row here so that the
/// largest remaining road is on the roster, and the row fails as stale
/// the day the installer stops pointing there.
#[test]
fn the_generated_ops_runner_start_line_is_on_the_roster() {
    let read = |p: &str| std::fs::read_to_string(repo_root().join(p)).unwrap();
    for (what, generator, needle, why) in GENERATED_FROM_A_HOME {
        assert!(why.len() > 40, "{what}: a roster row says why");
        assert!(
            read(generator).contains(needle),
            "{what}: {generator} no longer carries `{needle}` — if the runner has left the checkout, delete this row; if the line only moved, re-point it"
        );
    }
    // And the generator really is where the start line comes from.
    assert!(
        read("infra/ops/install-ops-runner.sh").contains("ExecStart=%s/infra/ops/ops-runner.sh"),
        "install-ops-runner.sh no longer generates the runner's ExecStart; re-derive this row"
    );
    assert!(
        !GENERATED_FROM_A_HOME.is_empty(),
        "the ops runner's row is gone: say in this file where root's runner executes from now"
    );
}
