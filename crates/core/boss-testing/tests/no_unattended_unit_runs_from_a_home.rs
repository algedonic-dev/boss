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
//!
//! A TRIPWIRE, NOT A BOUNDARY (delta review aa901496, N2 and N3; backlog
//! 62b09c57). This reads the TEXT of the unit files the tree carries and
//! stops the next unit being authored onto a home by habit. It is not
//! what keeps root off a home on the host, and nothing should cite it as
//! that. The review ran seven shapes past its second cut — a drop-in, a
//! continuation line, an `Environment=` whose home path is not its first
//! assignment, `~david`, a path that climbs out of root's tree, `User =
//! david` with spaces, a `.socket`'s `ExecStartPre=` — and each is a
//! fixture in `the_pin_reads_every_way_a_unit_reaches_a_home` now. What
//! it still cannot see, by construction:
//!
//!   - what systemd LOADED on the host. A unit or a drop-in written
//!     there by hand, or by an installer not rostered under
//!     GENERATED_FROM_A_HOME, is not in this tree. The read that answers
//!     for the host is `boss ops forge unit-status <unit>`
//!     (infra/forge/OPERATIONS.md, failure mode 9).
//!   - one hop of indirection: a script in root's tree that sources,
//!     execs or changes directory to a home, a symlink there that points
//!     at one, a path assembled from a variable.
//!   - a home not spelled `/home/`, `~` or `%h`.
//!
//! The boundary is the tree root fetched and verified itself
//! (infra/forge/root-tree.sh) and the account each unit runs as.

use boss_testing::repo_root;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Units whose Exec lines still name a path under /home/, and why.
/// Empty since backlog 683d7fe9's second car: the cluster converge and the
/// disk sweep, the last two, run as root from root's tree on the system
/// docker daemon, with the broker's registry push token (design c8502e17,
/// question image-build, David 2026-10-07).
const EXECUTES_FROM_A_HOME: &[(&str, &str)] = &[];

/// Start lines an INSTALLER writes, so no unit file in the tree shows
/// them: (what, the file that decides it, the line in it, why).
///
/// Empty since stage 2 of design c98c79aa (backlog a604a35b): the one
/// row was the forge's ops runner, whose drop-in named the checkout.
/// It names root's tree now — held by effect in root_tree_sh.rs,
/// `started_from_the_checkout_the_installer_brings_the_tree_and_hands_over`
/// — and the checkout reaches the verbs only as BOSS_OPS_CHECKOUT, a
/// directory their git runs in as its owner (the pin below).
const GENERATED_FROM_A_HOME: &[(&str, &str, &str, &str)] = &[];

/// Units that run as an account other than root, the account, and why.
const RUNS_AS_AN_ACCOUNT: &[(&str, &str, &str)] = &[
    // The forge's cluster-deploy-runner and disk-floor-sweep left this
    // roster with backlog 683d7fe9's second car: root, from root's tree.
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
/// Empty since backlog 683d7fe9's second car: the watchdog's one row, the
/// converge's build stamp in david's home, went with the converge to root
/// (forge-defaults.sh STAMP_FILE, under its root-only state directory).
const ROOT_IS_HANDED_A_HOME_PATH: &[(&str, &str, &str)] = &[];

/// EVERY FILE SYSTEMD READS A DIRECTIVE FROM, not only `*.service` (delta
/// review aa901496, N2). A `.socket` carries Exec lines of its own
/// (`ExecStartPre=` there runs before the service exists), and a drop-in
/// — `<unit>.d/<name>.conf` — replaces or adds any line of the unit it
/// sits beside.
fn is_unit_file(p: &Path) -> bool {
    const UNIT_KINDS: &[&str] = &["service", "socket", "timer", "path", "mount", "automount"];
    let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
    if UNIT_KINDS.contains(&ext) {
        return true;
    }
    let beside_a_unit = p
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|d| d.to_str())
        .and_then(|d| d.strip_suffix(".d"))
        .and_then(|unit| unit.rsplit_once('.'))
        .is_some_and(|(_, kind)| UNIT_KINDS.contains(&kind));
    ext == "conf" && beside_a_unit
}

/// A unit file as systemd reads it: comments dropped, and a line ending
/// in a backslash joined to the next (systemd.syntax(7)) — so a path on
/// the second line of an `ExecStart=` is read as part of it.
fn logical_lines(unit: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for raw in unit.lines() {
        let t = raw.trim();
        if t.starts_with('#') || t.starts_with(';') {
            continue;
        }
        match t.strip_suffix('\\') {
            Some(head) => {
                cur.push_str(head);
                cur.push(' ');
            }
            None => {
                cur.push_str(t);
                out.push(std::mem::take(&mut cur));
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `Key = value`, with the whitespace systemd allows around the `=`.
fn directive(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once('=')?;
    Some((key.trim(), value.trim()))
}

/// A word that names a home, wherever in the word the home is: at its
/// start, after an assignment or a colon, behind a path that climbs
/// (`/var/lib/boss/tree/../../../home/david`), as `~` or `~account`, or
/// as systemd's own `%h`.
fn names_a_home(word: &str) -> bool {
    let unquoted = |w: &str| w.trim_matches(['"', '\'']).to_string();
    let w = unquoted(word);
    let w = unquoted(w.trim_start_matches(['-', '@', ':', '+', '!']));
    w.contains("/home/")
        || w.ends_with("/home")
        || w.contains("%h")
        || w.split(['=', ':'])
            .any(|part| unquoted(part).starts_with('~'))
}

fn units() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if is_unit_file(&p) {
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
    let mut found = Vec::new();
    for line in logical_lines(unit) {
        let Some((key, value)) = directive(&line) else {
            continue;
        };
        if !(key.starts_with("Exec") || PATH_DIRECTIVES.contains(&key)) {
            continue;
        }
        for word in value.split_whitespace() {
            if names_a_home(word) {
                found.push(format!("{key}: {word}"));
            }
        }
    }
    found
}

/// The LAST `User=` wins, as in systemd; none, or an emptied one, is
/// root. Read with the whitespace systemd allows: `User = david` was
/// read as root until review aa901496 (N2).
fn user(unit: &str) -> String {
    logical_lines(unit)
        .iter()
        .filter_map(|l| directive(l))
        .filter(|(key, _)| *key == "User")
        .map(|(_, who)| who.trim_matches(['"', '\'']).to_string())
        .next_back()
        .filter(|who| !who.is_empty())
        .unwrap_or_else(|| "root".to_string())
}

/// Every `Environment=` assignment whose value names a home, as
/// `(variable, the word)` — EVERY assignment on the line, not the first
/// (review aa901496, N2: `Environment=A=/etc/x B=/home/david/y` passed).
/// A word with no `=` of its own is the tail of a quoted value and
/// belongs to the variable before it.
fn environment_homes(unit: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for line in logical_lines(unit) {
        let Some(("Environment", value)) = directive(&line) else {
            continue;
        };
        let mut name = String::from("?");
        for word in value.split_whitespace() {
            let bare = word.trim_matches(['"', '\'']);
            let val = match bare.split_once('=') {
                Some((n, v)) => {
                    name = n.to_string();
                    v
                }
                None => bare,
            };
            if names_a_home(val) {
                found.push((name.clone(), word.to_string()));
            }
        }
    }
    found
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
        for (name, word) in environment_homes(&text) {
            if ROOT_IS_HANDED_A_HOME_PATH
                .iter()
                .any(|(u, v, _)| *u == rel && *v == name)
            {
                stale.retain(|(u, v)| !(*u == rel && *v == name));
            } else {
                offenders.push(format!("{rel}: Environment={word}"));
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
        // The shapes delta review aa901496 (N2) ran past the second cut.
        (
            "a continuation line",
            "ExecStart=/usr/bin/env bash \\\n    /home/david/boss/infra/x.sh",
        ),
        (
            "a tilde account",
            "ExecStart=/bin/sh -c ~david/boss/infra/x.sh",
        ),
        (
            "the account's own home as the working directory",
            "User=david\nWorkingDirectory=~\nExecStart=/usr/bin/env bash infra/x.sh",
        ),
        (
            "systemd's home specifier",
            "ExecStart=/bin/bash %h/boss/infra/x.sh",
        ),
        (
            "a path that climbs out of root's tree",
            "ExecStart=/var/lib/boss/tree/current/../../../../../home/david/boss/infra/x.sh",
        ),
        (
            "spaces around the equals sign",
            "ExecStart = /home/david/boss/infra/x.sh",
        ),
        (
            "a socket's pre-step",
            "[Socket]\nListenStream=/run/x.sock\nExecStartPre=/home/david/boss/infra/pre.sh",
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
    // The account, with the whitespace systemd allows and the last one
    // winning; none, or an emptied one, is root.
    for (body, who) in [
        ("User=david", "david"),
        ("User = david", "david"),
        ("  User=david  ", "david"),
        ("User=david\nUser=", "root"),
        ("# User=david", "root"),
    ] {
        assert_eq!(user(&format!("[Service]\n{body}\n")), who, "{body:?}");
    }
    // What a root unit is handed: every assignment on the line.
    for (body, var) in [
        ("Environment=A=/home/david/x", "A"),
        ("Environment=A=/etc/x B=/home/david/y", "B"),
        ("Environment=\"A=/etc/x\" \"B=/home/david/y\"", "B"),
        ("Environment = A=/home/david/x", "A"),
        ("Environment=A=/etc/x \\\n    B=~david/y", "B"),
        ("Environment=PATH=/usr/bin:/home/david/bin", "PATH"),
    ] {
        let got = environment_homes(&unit(body));
        assert_eq!(
            got.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            [var],
            "{body:?} was read as {got:?}"
        );
    }
    assert!(
        environment_homes(&unit("Environment=A=/etc/boss/x B=/var/lib/boss/y")).is_empty(),
        "an environment with no home in it was read as handing one"
    );
    // Which files are read at all: a socket and a drop-in are a unit's
    // lines as much as its .service is; a stray .conf is not.
    for (path, read) in [
        ("infra/forge/x.service", true),
        ("infra/forge/x.socket", true),
        ("infra/forge/x.timer", true),
        ("infra/forge/x.service.d/override.conf", true),
        ("infra/forge/x.socket.d/10-pre.conf", true),
        ("infra/forge/nginx.d/site.conf", false),
        ("infra/forge/x.sh", false),
    ] {
        assert_eq!(is_unit_file(Path::new(path)), read, "{path}");
    }
}

/// THE ROAD NO UNIT FILE SHOWS (review 5f3736a2, F3). The ops runner's
/// ExecStart is not in infra/ops/boss-ops-runner.service: the installer
/// GENERATES it into a drop-in, and on the forge it named the checkout —
/// so root executed /home/david/boss every minute, with every verb
/// script, and every test above was green. A start line an installer
/// writes onto a home is a row of GENERATED_FROM_A_HOME, which fails as
/// stale the day its installer stops writing it.
#[test]
fn a_generated_start_line_on_a_home_is_on_the_roster() {
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
        "install-ops-runner.sh no longer generates the runner's ExecStart; re-derive this test"
    );
}

/// Forge verbs whose working tree defaults to the tree they run from on
/// purpose: (script, variable, why root's tree is the right one).
const WORKS_ON_ROOTS_TREE: &[(&str, &str, &str)] = &[
    (
        "infra/forge/retire-example-reference-rows.sh",
        "TREE",
        "reads infra/cluster/instances.toml and examples/ as FILES, with no git: root's tree is the converged main itself, which the checkout only approximates",
    ),
    (
        "infra/forge/tenant-census.sh",
        "TREE",
        "reads examples/brewery seeds and data as FILES, with no git: root's tree is the converged main itself",
    ),
];

/// WHERE A VERB'S CODE RUNS AND WHERE ITS GIT WORKS ARE TWO PLACES
/// (stage 2 of design c98c79aa, backlog a604a35b). The forge's ops runner
/// executes root's tree, an export with no `.git` and no `forgejo` remote,
/// so a verb that reads or pushes git in "the tree I am in" would refuse
/// there — or, finding that tree root's, run git as root. Every forge
/// verb whose working tree is a seam defaulting to its own directory
/// therefore takes the checkout its runner is handed, BOSS_OPS_CHECKOUT,
/// first — or is a row of WORKS_ON_ROOTS_TREE saying why its own tree is
/// right; and every one that runs git in its TREE takes the checkout.
/// Each runs that git as the checkout's owner, never as root, which its
/// own test holds. A verb authored beside them fails here, naming itself.
#[test]
fn a_forge_verb_that_works_on_git_takes_the_checkout_it_is_handed() {
    // `NAME="${SEAM:-$REPO}"` or `NAME="${SEAM:-$(cd "$HERE/../.." && pwd)}"`:
    // a whole tree, defaulting straight to the script's own.
    let own_tree_default = regex::Regex::new(
        r#"^([A-Z_]+)="\$\{[A-Z_]+:-(\$REPO|\$\(cd "\$(HERE|SELF_DIR)/\.\./\.\." && pwd\))\}"$"#,
    )
    .unwrap();
    let root = repo_root();
    let mut scripts: Vec<String> = Vec::new();
    for e in std::fs::read_dir(root.join("infra/ops/verbs")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let on_the_forge = v["hosts"]
            .as_array()
            .is_some_and(|h| h.iter().any(|x| x == "forge"));
        let script = v["argv"][0].as_str().unwrap_or("");
        if on_the_forge && script.contains('/') && !scripts.iter().any(|s| s == script) {
            scripts.push(script.to_string());
        }
    }
    scripts.sort();
    let mut works_on_git = Vec::new();
    let mut takes_the_checkout = Vec::new();
    let mut offenders = Vec::new();
    let mut stale: Vec<(&str, &str)> = WORKS_ON_ROOTS_TREE
        .iter()
        .map(|(s, v, _)| (*s, *v))
        .collect();
    for s in &scripts {
        let Ok(text) = std::fs::read_to_string(root.join(s)) else {
            continue;
        };
        let code: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with('#'))
            .collect();
        if code.iter().any(|l| l.contains("${BOSS_OPS_CHECKOUT:-")) {
            takes_the_checkout.push(s.as_str());
        }
        for l in &code {
            let Some(var) = own_tree_default.captures(l).map(|c| c[1].to_string()) else {
                continue;
            };
            if WORKS_ON_ROOTS_TREE
                .iter()
                .any(|(rs, rv, _)| rs == s && *rv == var)
            {
                stale.retain(|(rs, rv)| !(rs == s && *rv == var));
            } else {
                offenders.push(format!("{s}: {l}"));
            }
        }
        let git_in_tree = code
            .iter()
            .any(|l| l.contains("git -C '$TREE'") || l.contains("git -C \"$TREE\""));
        if git_in_tree {
            works_on_git.push(s.as_str());
            let takes_it = code
                .iter()
                .any(|l| l.starts_with("TREE=") && l.contains("${BOSS_OPS_CHECKOUT:-"));
            if !takes_it {
                offenders.push(format!(
                    "{s}: runs git in a TREE that does not take BOSS_OPS_CHECKOUT"
                ));
            }
        }
    }
    // The controls: the pin reads the verbs it was written for, and each
    // of the two shapes it judges is present in the tree it reads.
    for want in [
        "infra/forge/tag-release.sh",
        "infra/forge/sweep-archive-branches.sh",
    ] {
        assert!(
            works_on_git.contains(&want),
            "the pin did not find {want} running git in its TREE — it reads nothing: {works_on_git:?}"
        );
    }
    assert!(
        takes_the_checkout.contains(&"infra/forge/merge-tenant-main.sh"),
        "merge-tenant-main.sh derives the forge's URL from its checkout's remote and must take the handed checkout: {takes_the_checkout:?}"
    );
    assert!(
        offenders.is_empty(),
        "these forge verbs work on a tree that defaults to the one they run from — on the forge that is root's export, with no .git and no forge remote: default it to ${{BOSS_OPS_CHECKOUT:-…}} first, or add a WORKS_ON_ROOTS_TREE row saying why root's tree is right:\n  {}",
        offenders.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "these WORKS_ON_ROOTS_TREE rows are no longer true — delete them: {stale:?}"
    );
    for (s, _, why) in WORKS_ON_ROOTS_TREE {
        assert!(why.len() > 40, "{s}: a roster row says why");
    }
}
