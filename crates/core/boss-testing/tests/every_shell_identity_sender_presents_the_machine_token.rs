//! Every shell script under infra/ that signs a request to the system of
//! record (`x-boss-user`) also PRESENTS the estate machine token, through
//! the one reader — or is listed here with the reason it does not
//! (design 6805c764; backlog 2710c8fc).
//!
//! WHY. Every service's machine gate records, and under `enforce` will
//! refuse, a request without the token. The Rust half has had this pin
//! since car 2 (`every_x_boss_user_sender_stamps_the_machine_token.rs`);
//! the shell half had only `every_shell_sender_reads_the_machine_token_
//! from_its_mount.rs`, which holds HOW a script takes the token and says
//! nothing about a script that takes none. Measured 2026-10-06: seven
//! send sites in six host libraries signed a request and never called
//! the reader, and they were found by reading three days of
//! `machine_gate.would_refuse` facts, not by a check.
//!
//! WHAT IT READS. The sender list is DERIVED, never written down: every
//! shell file under infra/ (a `.sh` name, or a `#!` line naming sh or
//! bash) with a live — non-comment — line naming `x-boss-user`. Such a
//! file passes when a live line calls the reader
//! (`machine_token_header <VAR> …`), or hands curl a header file its
//! caller's reader call made:
//!
//!     ${…MT_HDR:+-H "$…MT_HDR"}        (or …TOKEN_HDR…, the pod door's)
//!
//! That variable can only be filled by `machine_token_header` — the
//! mount pin refuses a header written by hand. A file that signs and
//! does neither is refused by name unless NOT_YET or BY_DESIGN lists it.
//! It is a NAME scan: a file that calls the reader and then forgets to
//! hand curl the file on ONE of its requests passes it. That was the
//! defect of backlog 44b2087e, and the second half of this file — PER
//! CALL SITE, below — judges every command that runs curl instead.
//!
//! THE TWO LISTS ARE NOT THE SAME KIND OF ENTRY. BY_DESIGN names a
//! sender that must not hold the token, or for which that is an open,
//! recorded decision. NOT_YET names a sender of the same shape as the
//! ones this car stamped, left for a later car — a debt, with the reason
//! it was not paid here. A listed file that has since been stamped is
//! not an error (two cars may stamp and list the same file; a pin that
//! went red on the assembled tree for that would punish the fix), so a
//! stale entry is reported by `stale_entries_are_named`, which prints and
//! passes.
//!
//! OUT OF SCOPE: a script that sends NO identity at all. The workflow
//! publish's live read was one (infra/gcp/publish-workflow.sh, stamped
//! in this car and pinned by name below); only the gate's own record
//! finds the next.
//!
//! tree-wide pin — it scans every file under infra/, which no
//! changed-file map attributes to this crate, so every scoped gate runs
//! it whatever its scope (`tree_wide_pins` in infra/gate.sh).

use boss_testing::repo_root;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// Senders that hold no token on purpose, or by a decision on the record.
const BY_DESIGN: &[(&str, &str)] = &[
    (
        "infra/gate-runner/run.sh",
        "in-cluster non-holder: the gate runs a car's own code, so whether it may hold the token is the decision owed before enforce (2710c8fc before_enforce)",
    ),
    (
        "infra/forge/probe-bin/boss-sor-read",
        "the recorded probe's reader runs car-supplied shell on the forge and must not hold the estate token; its own reader door is backlog d26515c5",
    ),
    (
        "infra/postgres/reset-to-baseline.sh",
        "an operator's tool against a local stack, not a caller of the estate",
    ),
    (
        "infra/postgres/validate-brewery-sim.sh",
        "an operator's tool against a local stack, not a caller of the estate",
    ),
];

/// Lints: a lint is a question about a tree and holds no token by rule
/// (infra/lint/a-lint-sources-a-header-lib-only-hermetic.sh; the runners
/// hand every lint an empty token directory). The live readers among
/// them are that lint's one exception, judged there.
const BY_DESIGN_DIRS: &[&str] = &["infra/lint/"];

/// Host senders owed a reader call: a debt, not a licence. Each entry
/// says WHY it is still owed and WHAT REMOVES IT (`Leaves this list when
/// …`, held by `each_owed_entry_says_what_removes_it`), so the list
/// cannot become a place a sender is simply left.
///
/// None remain (2026-10-10). The three other files of the roster car
/// 217c2d66 left were stamped on 2026-10-08 (backlog 44b2087e):
/// discover-admission-source.sh, the ML batch and the Python token audit.
/// Two more were listed here until 2026-10-10 —
/// infra/codebase-observe/observe-codebase.sh and
/// infra/install-smoke/nightly.sh, each run by a unit whose ExecStart
/// was a copy installed BY HAND at /usr/local/lib/boss — and were
/// deleted with their units rather than stamped (see
/// `no_unit_runs_a_hand_installed_copy`). A third was listed until
/// 2026-10-10 — infra/forge/probe-admission-source-access.sh, the one on
/// the live record (2 would-refuse facts in the 7 days to 2026-10-10, the
/// last 2026-10-05T18:43Z), held back because its bytes are a bound
/// method of design f927de0c. That design folded, and the script is
/// stamped as discover-admission-source.sh is, with
/// ../lib/secret-header.sh bound in
/// infra/forge/admission-reader-identity.py METHODS, so the reviewed
/// identity is the stamped one; both halves of this pin now judge it.
const NOT_YET: &[(&str, &str)] = &[];

/// Files this pin holds by NAME as well, because their request signs
/// nothing and the derivation above cannot see them.
const UNSIGNED_BUT_STAMPED: &[&str] = &["infra/gcp/publish-workflow.sh"];

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

/// Shell by NAME. `.bash` and `.inc` are here because a sourced fragment
/// carries no `#!` line, so under either name it was no shell file at all
/// to both halves of this pin (backlog 7369b078, F5 of review 9c484ca8;
/// none exists under infra/ on 2026-10-09 — the first one is read from
/// its first commit).
const SHELL_EXTENSIONS: &[&str] = &["sh", "bash", "inc"];

fn is_shell(path: &Path, text: &str) -> bool {
    if path
        .extension()
        .is_some_and(|x| SHELL_EXTENSIONS.iter().any(|s| x == *s))
    {
        return true;
    }
    path.extension().is_none()
        && text.lines().next().is_some_and(|l| {
            l.starts_with("#!") && (l.ends_with("sh") || l.contains("bash") || l.contains("/sh "))
        })
}

fn live(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter(|l| !l.trim_start().starts_with('#'))
}

fn signs(text: &str) -> bool {
    live(text).any(|l| l.contains("x-boss-user"))
}

/// A live line that takes the token from the one reader — a
/// `machine_token_header <VAR> …` call — or that hands curl a header file
/// a caller's reader call made: `${X:+-H "$X"}` with one variable, whose
/// name ends `MT_HDR` or `TOKEN_HDR` (infra/forge/publish-pr-state.sh is
/// handed its caller's). The four deposit and train-key scripts pass on
/// the call: they put the file into an argv array rather than the `:+`
/// form.
fn presents(text: &str) -> bool {
    let call = Regex::new(r"(^|[\s;&|({!])machine_token_header\s+[A-Za-z_]+").unwrap();
    let hand = Regex::new(r#"\$\{([A-Za-z_]*(?:MT|TOKEN)_HDR):\+-H "\$([A-Za-z_]+)"\}"#).unwrap();
    live(text).any(|l| call.is_match(l) || hand.captures_iter(l).any(|c| c[1] == c[2]))
}

/// (rel path, signs, presents) for every shell file under `root`/infra.
fn senders(root: &Path) -> Vec<(String, bool, bool)> {
    let mut files = Vec::new();
    walk(&root.join("infra"), &mut files);
    files.sort();
    files
        .into_iter()
        .filter_map(|f| {
            let text = std::fs::read_to_string(&f).ok()?;
            if !is_shell(&f, &text) {
                return None;
            }
            let rel = f.strip_prefix(root).ok()?.to_string_lossy().into_owned();
            Some((rel, signs(&text), presents(&text)))
        })
        .collect()
}

fn listed(rel: &str) -> bool {
    BY_DESIGN.iter().chain(NOT_YET).any(|(p, _)| *p == rel)
        || BY_DESIGN_DIRS.iter().any(|d| rel.starts_with(d))
}

#[test]
fn every_shell_identity_sender_presents_the_machine_token() {
    let all = senders(&repo_root());
    assert!(
        all.iter().filter(|(_, s, p)| *s && *p).count() >= 20,
        "the scan found fewer than twenty stamped senders — it has stopped seeing the tree, and a scanner that sees nothing passes everything"
    );
    let missing: Vec<&str> = all
        .iter()
        .filter(|(rel, signs, presents)| {
            *signs
                && !*presents
                && !listed(rel)
                && !TOKENLESS_PROBES.iter().any(|site| site.file == rel)
        })
        .map(|(rel, _, _)| rel.as_str())
        .collect();
    assert!(
        missing.is_empty(),
        "these scripts sign a request to the system of record (x-boss-user) and never present \
         the estate machine token, so every machine gate records them as a caller it would \
         refuse. Call the one reader before the request and hand curl its file — \
         `machine_token_header MT_HDR \"$BASE\" || MT_HDR=\"\"` then `${{MT_HDR:+-H \"$MT_HDR\"}}` \
         (infra/lib/secret-header.sh; the token is presented, never required) — or list the \
         file in BY_DESIGN / NOT_YET here with its reason:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn the_unsigned_live_read_stays_stamped() {
    let all = senders(&repo_root());
    for rel in UNSIGNED_BUT_STAMPED {
        let row = all.iter().find(|(r, _, _)| r == rel);
        assert!(
            row.is_some_and(|(_, _, presents)| *presents),
            "{rel} sends a request with no identity, which the derivation cannot see; it is held \
             by name, and it no longer hands curl the reader's header file"
        );
    }
}

/// A listed file that is gone, no longer signs, or is now stamped. Named
/// so the list can be trimmed; not a failure (see the module doc).
#[test]
fn stale_entries_are_named() {
    let all = senders(&repo_root());
    for (rel, why) in BY_DESIGN.iter().chain(NOT_YET) {
        match all.iter().find(|(r, _, _)| r == rel) {
            None => println!(
                "stale: {rel} is listed and is no longer a shell file under infra/ ({why})"
            ),
            Some((_, false, _)) => println!("stale: {rel} is listed and signs nothing now ({why})"),
            Some((_, true, true)) => println!("stale: {rel} is listed and is stamped now ({why})"),
            Some((_, true, false)) => {}
        }
    }
}

#[test]
fn the_matchers_see_a_sender_and_a_presentation() {
    assert!(signs("curl -H \"x-boss-user: $U\" \"$BASE/api/jobs\"\n"));
    assert!(!signs(
        "# curl -H 'x-boss-user: …' is what this does NOT do\n"
    ));
    assert!(presents("curl ${MT_HDR:+-H \"$MT_HDR\"} \"$u\"\n"));
    assert!(presents(
        "  ${ALERT_PEOPLE_MT_HDR:+-H \"$ALERT_PEOPLE_MT_HDR\"} \\\n"
    ));
    assert!(presents("curl ${TOKEN_HDR:+-H \"$TOKEN_HDR\"} \"$u\"\n"));
    assert!(!presents("# ${MT_HDR:+-H \"$MT_HDR\"}\n"));
    assert!(!presents("curl ${AUTH_HDR:+-H \"$AUTH_HDR\"} \"$u\"\n"));
    assert!(!presents("curl ${MT_HDR:+-H \"$OTHER\"} \"$u\"\n"));
    assert!(presents(
        "machine_token_header MT_HDR \"$BASE\" || MT_HDR=\"\"\n"
    ));
    assert!(presents(
        "    if ! machine_token_header MT_HDR \"$BASE\"; then\n"
    ));
    assert!(!presents("# machine_token_header MT_HDR \"$BASE\"\n"));
    assert!(!presents("declare -F machine_token_header >/dev/null\n"));
}

/// The list holds no sender twice and none that is also a directory rule.
#[test]
fn each_file_is_listed_once() {
    let mut seen: Vec<&str> = Vec::new();
    for (rel, why) in BY_DESIGN.iter().chain(NOT_YET) {
        assert!(!seen.contains(rel), "{rel} is listed twice");
        assert!(!why.is_empty(), "{rel} is listed with no reason");
        assert!(
            !BY_DESIGN_DIRS.iter().any(|d| rel.starts_with(d)),
            "{rel} is under a listed directory already"
        );
        seen.push(rel);
    }
}

/// A debt names its own end. An entry that only says why a sender is
/// unstamped is a place to leave it; `Leaves this list when …` is what a
/// later car reads to know the entry can go.
#[test]
fn each_owed_entry_says_what_removes_it() {
    for (rel, why) in NOT_YET {
        assert!(
            why.contains("Leaves this list when "),
            "{rel} is owed a reader call and its entry does not say what removes it: \
             write `Leaves this list when …` into its reason"
        );
    }
}

/// No unit in the tree runs a copy installed BY HAND at
/// /usr/local/lib/boss (backlog 44b2087e R1, 2026-10-10). Such a copy
/// sits outside every checkout, so `../lib/secret-header.sh` is not
/// beside it and no stamp in the tree's copy reaches what runs; no
/// converge installs it, so no role in infra/estate/roles.toml places
/// it on a host. Two such units were the last NOT_YET debts of that
/// shape — observe-codebase (scope `codebase`: 0 estate observations on
/// the record, against 10178 for `host-units`, read 2026-10-10T00:58Z)
/// and the install smoke (`docker compose` on boss-gcp, which has no
/// container runtime, per .forgejo/workflows/install-smoke.yml) — and
/// neither appeared once in 7 days of machine-gate would-refuse facts.
/// They were deleted rather than stamped. A sender that must run on a
/// host runs the checkout's copy under a role, as
/// infra/codebase-metrics.sh does.
#[test]
fn no_unit_runs_a_hand_installed_copy() {
    let mut files = Vec::new();
    walk(&repo_root().join("infra"), &mut files);
    let units: Vec<PathBuf> = files
        .into_iter()
        .filter(|p| p.extension().is_some_and(|x| x == "service"))
        .collect();
    assert!(
        units.len() >= 20,
        "found {} unit files under infra/ — the walk has stopped seeing the tree",
        units.len()
    );
    let root = repo_root();
    let offenders: Vec<String> = units
        .iter()
        .flat_map(|u| {
            let rel = u
                .strip_prefix(&root)
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or_default();
            std::fs::read_to_string(u)
                .unwrap_or_default()
                .lines()
                .enumerate()
                .filter(|(_, l)| l.starts_with("Exec") && l.contains("/usr/local/lib/boss/"))
                .map(|(i, l)| format!("{rel}:{}: {l}", i + 1))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "these units run a copy installed by hand at /usr/local/lib/boss, outside any \
         checkout: no stamp in the tree reaches it and no role places it on a host. Run the \
         checkout's own copy under a role of infra/estate/roles.toml, or delete the unit and \
         its script:\n  {}",
        offenders.join("\n  ")
    );
}

// =====================================================================
// NOT SHELL (backlog 44b2087e, 2026-10-08; df38075a F2; 7369b078 F3)
// =====================================================================
//
// THE DEFECT. Both halves of this file read SHELL: a `.sh` name or a
// `#!` line naming sh or bash. infra/maintenance/forge-token-audit.py
// signs `x-boss-user` and sends through urllib, so neither half saw it,
// two reviews named it by hand, and it sat in no list in the tree.
//
// THE RULE, a denial like the per-call-site one: every Python file under
// infra/ that can open a connection — an `import` or `from` line naming a
// network client (`urllib.request`, `http.client`, `requests`, `httpx`,
// `aiohttp`, `socket`) — takes the token from the ONE reader, or
// is named in PYTHON_NOT_THE_ESTATE with where its requests go. "From
// the one reader" means a live line that names both the library and its
// function: the file runs infra/lib/secret-header.sh as a child and
// reads the header line off the child's stdout. A Python file that
// spelled the header itself, or read the slot itself, would be a second
// copy of the host rule and the slot rules (CLAUDE.md §9a), so the
// header's NAME on a live line of one is refused as well.
//
// WHAT IT DOES NOT SEE. Whether the header the child returned is put on
// EVERY request the file sends (forge-token-audit.py sends one, and
// `forge_token_audit.rs` reads that request off a socket); a client
// reached through a module this list does not name, or imported by
// `__import__` or `importlib`; a `subprocess` run
// of curl from Python (none in the tree); and any language that is
// neither shell nor Python — under infra/ today that is nothing that
// sends (the step plugins are browser code and sign nothing).

/// Python files that open connections which are not to a gated service
/// port. (file, where its requests go).
const PYTHON_NOT_THE_ESTATE: &[(&str, &str)] = &[
    (
        "infra/forge/isolation-receiver.py",
        "a21c491c R1 bounded synthetic receiver: controls connect only to its original-request-derived isolation-fixture Service in boss-dev; fake routes and fake credentials never address an estate service, and this fixture must not read or hold the estate machine token",
    ),
    (
        "infra/agent/private-collector.py",
        "accepted001 private collection only: the image-owned contract fixes HTTPS boss-managed-cache-collection.boss.svc:7444/v1/collect, authenticated by the distinct Pod-bound boss-managed-cache-collection audience; this is not a machine-token-gated service port, and the collector must never hold the estate machine token",
    ),
    (
        "infra/dev/dev-lifecycle.py",
        "the cluster's Kubernetes API, with the dev session's own service-account credential, to retire one Job by UID",
    ),
    (
        "infra/forge/gcs-readback.py",
        "Google Cloud Storage's JSON API, to read back the offsite archive",
    ),
];

/// As an `import` or `from` line names them, both spellings of the two
/// standard-library ones.
const PYTHON_NETWORK_CLIENTS: &[&str] = &[
    "urllib.request",
    "urllib import request",
    "http.client",
    "http import client",
    "requests",
    "httpx",
    "aiohttp",
    "socket",
];

/// A word of `line`, not a piece of a longer one: `socket` and not
/// `websocket_url`, `http.client` and not `http.clients`.
fn names(line: &str, word: &str) -> bool {
    line.match_indices(word).any(|(i, _)| {
        let before = line[..i].chars().next_back();
        let after = line[i + word.len()..].chars().next();
        let part = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        !part(before) && !part(after)
    })
}

/// (refused files with the reason, roster entries that excuse nothing).
fn python_senders(root: &Path) -> (Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    walk(&root.join("infra"), &mut files);
    files.sort();
    let mut refused = Vec::new();
    let mut used = vec![false; PYTHON_NOT_THE_ESTATE.len()];
    for f in files {
        if f.extension().is_none_or(|x| x != "py") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let rel = f.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        // A `*_test.py` or a file under a `tests/` directory serves a
        // fixture (`http.server`) or drives one; it is not installed.
        if rel.ends_with("_test.py") || rel.contains("/tests/") {
            continue;
        }
        let connects = live(&text).any(|l| {
            let l = l.trim_start();
            (l.starts_with("import ") || l.starts_with("from "))
                && PYTHON_NETWORK_CLIENTS.iter().any(|c| names(l, c))
        });
        if !connects {
            continue;
        }
        if let Some(i) = PYTHON_NOT_THE_ESTATE.iter().position(|(p, _)| *p == rel) {
            used[i] = true;
            continue;
        }
        let reads = live(&text).any(|l| l.contains("secret-header.sh"))
            && live(&text).any(|l| l.contains("machine_token_header "));
        if !reads {
            refused.push(format!(
                "{rel}: opens a connection and never takes the machine token from the one reader"
            ));
        } else if live(&text).any(|l| l.contains("x-boss-machine-token")) {
            refused.push(format!(
                "{rel}: spells the machine token's header itself; the name comes back from the reader with the value"
            ));
        }
    }
    let stale = PYTHON_NOT_THE_ESTATE
        .iter()
        .zip(&used)
        .filter(|(_, u)| !**u)
        .map(|((p, why), _)| format!("{p} is listed and opens no connection now ({why})"))
        .collect();
    (refused, stale)
}

#[test]
fn every_python_sender_takes_the_machine_token_from_the_one_reader_or_is_rostered() {
    let (refused, stale) = python_senders(&repo_root());
    for s in &stale {
        println!("stale: {s}");
    }
    assert!(
        refused.is_empty(),
        "these Python files under infra/ can send a request and do not present the estate \
         machine token, so a request one sends to a service port is one every machine gate \
         records as a caller it would refuse. Take the header from the one reader, as \
         infra/maintenance/forge-token-audit.py does (`machine_token_headers`: \
         infra/lib/secret-header.sh run as a child, the header line read from its stdout — \
         presented, never required), or name the file in PYTHON_NOT_THE_ESTATE here with \
         where its requests go:\n  {}",
        refused.join("\n  ")
    );
    for (p, why) in PYTHON_NOT_THE_ESTATE {
        assert!(repo_root().join(p).is_file(), "{p} is gone — drop it");
        assert!(!why.is_empty(), "{p} is listed with no reason");
    }
}

/// The Python scan, on a planted tree that is never executed.
#[test]
fn only_the_named_synthetic_isolation_receiver_is_outside_the_estate() {
    let root = boss_testing::scratch_dir("python-synthetic-isolation-receiver");
    std::fs::create_dir_all(root.join("infra/forge")).unwrap();
    for path in [
        "infra/forge/isolation-receiver.py",
        "infra/forge/another-receiver.py",
    ] {
        boss_testing::write_file(&root.join(path), "import http.client\n");
    }
    let (refused, _) = python_senders(&root);
    assert_eq!(refused.len(), 1, "{refused:#?}");
    assert!(refused[0].starts_with("infra/forge/another-receiver.py:"));
}

/// The Python scan, on a planted tree that is never executed.
#[test]
fn an_unstamped_python_sender_is_refused() {
    let root = boss_testing::scratch_dir("python-senders");
    let dir = root.join("infra/maintenance");
    std::fs::create_dir_all(&dir).unwrap();
    let reader = "LIB = os.path.join(HERE, \"..\", \"lib\", \"secret-header.sh\")\n\
                  READER = 'machine_token_header H \"$2\" || exit 0'\n";
    let cases: &[(&str, String, bool)] = &[
        // The measured shape: signed, sent through urllib, no token.
        (
            "signed.py",
            "import urllib.request\nreq = urllib.request.Request(u, headers={\"x-boss-user\": R})\n".into(),
            true,
        ),
        // No identity at all: the name scan's blind spot.
        (
            "unsigned.py",
            "from urllib.request import urlopen\nurlopen(u)\n".into(),
            true,
        ),
        ("http-client.py", "import http.client\n".into(), true),
        ("third-party.py", "import requests\nrequests.get(u)\n".into(), true),
        // The reader named only in a comment.
        (
            "reader-in-a-comment.py",
            "import urllib.request\n# secret-header.sh machine_token_header H\n".into(),
            true,
        ),
        // Takes it from the reader AND spells the header: a second copy.
        (
            "spells-the-header.py",
            format!("import urllib.request\n{reader}h = {{\"x-boss-machine-token\": v}}\n"),
            true,
        ),
        ("stamped.py", format!("import urllib.request\n{reader}"), false),
        // Parses a URL, opens nothing.
        (
            "parses-only.py",
            "from urllib.parse import urlsplit\nimport websockets_notes\n# import socket\n\"\"\"answers requests\"\"\"\n".into(),
            false,
        ),
        ("from-import.py", "from urllib import request\n".into(), true),
        // A fixture's server is not an installed sender.
        ("door_test.py", "import http.client\n".into(), false),
    ];
    for (name, body, _) in cases {
        boss_testing::write_file(&dir.join(name), body);
    }
    let (refused, _) = python_senders(&root);
    for (name, _, want) in cases {
        let got = refused
            .iter()
            .filter(|l| l.starts_with(&format!("infra/maintenance/{name}:")))
            .count();
        assert_eq!(
            got,
            usize::from(*want),
            "{name}: refused {got} time(s): {refused:#?}"
        );
    }
}

// =====================================================================
// PER CALL SITE (backlog 44b2087e, 2026-10-07)
// =====================================================================
//
// THE DEFECT. Everything above judges a FILE: a script that signs and
// calls the reader once passes, whatever its other requests do. Measured
// live 2026-10-07 18:00Z: infra/ops/ops-runner.sh stamped its seven
// writes and not its every-minute queue read (77 tokenless GET /api/jobs
// in 44 minutes), infra/forge/read-publish-checks.sh stamped six sites
// and not its packet read, and eleven more sites in eight files had the
// same shape. Each was found in the gate's own record of callers it
// would refuse, not by a check.
//
// THE RULE, AND WHY IT IS A DENIAL RATHER THAN A MATCH. The per-file
// scan looks for the bad shape (a line naming `x-boss-user`), so a
// request that spells its identity another way is not seen at all. This
// one starts from the other end: in every shell file under infra/,
// EVERY command that runs curl — the word itself, a path to it, a
// variable or a function whose name holds `curl`, wherever on the line
// it stands and however many lines the command spans — must hand curl a
// header file that THIS file's own `machine_token_header <VAR>` call
// filled, in that same command. A send that does not is refused by file
// and line unless a roster below names it and says why. So a new request
// is refused until someone decides about it, whatever it sends and
// however its headers are spelled; nothing has to recognise it as a
// request to the system of record first.
//
// WHAT COUNTS AS THE HEADER, three spellings and no others:
//
//     ${V:+-H "$V"}      -H "$V"      "${A[@]}"
//
// where V is a variable a live `machine_token_header V …` call in the
// same file fills (or HANDED names, for a lib its caller fills), and A
// is an array a live `A+=(-H "$V")` line in the same file extends. A
// name that merely ends `MT_HDR` is not enough: the per-file scan took
// `${TOKEN_HDR:+…}` on its name, and infra/estate/install-cli-from-image.sh
// fills a `TOKEN_HDR` with a registry bearer. Text after ` #` is a
// comment and holds no header.
//
// AND THE OTHER DIRECTION: a command the roster says leaves the estate
// (NOT_THE_ESTATE) that carries the reader's header file is refused too.
// The reader checks the host where it is CALLED, so a file filled for the
// system of record and handed to GitHub's API would take the token there.
//
// WHAT IT DOES NOT SEE, said so nobody reads green as more than it is.
// A sender that is not curl (the pod door `boss-api`, the `boss` CLI and
// the probe's `boss-sor-read` each stamp, or by design do not, inside
// themselves); a curl reached through a name with no `curl` in it that
// is assigned in ANOTHER file or handed in as an argument (one assigned
// in the same file is followed: `api=…/boss-api-curl.sh; "$api" …`),
// which an author has to write on purpose; a manifest's
// embedded script (infra/lint/a-manifest-sender-presents-the-machine-token.sh
// holds those); and whether the variable was filled for the host this
// request goes to — the reader decides the host once, where it is
// called. The two senders measured live are also run against a stub,
// request by request (`ops_runner_sh.rs`, `read_publish_checks_sh.rs`).
//
// THAT LIST WAS SHORTER THAN THE TRUTH (backlog 7369b078, F5 of review
// 9c484ca8, which found six more and none of them in the tree). Closed
// on 2026-10-09, each with a planted case below: a name given a curl by
// `${N:=…}`, `N=$(command -v curl)`, `printf -v N` or `read N <<<`; a
// curl that is only a parameter's default (`"${DOOR:-curl}"`); curl as
// xargs's command, where nothing follows the word; a fragment named
// `.bash` or `.inc`; and a unit's own `Exec…=` line (the section A UNIT
// RUNS NO REQUEST OF ITS OWN). STILL UNSEEN, and written here so green
// is not read as more: a name filled by `read` from a file or a pipe, by
// `eval`, by `declare`/`local -n`, or by a loop variable (`for c in curl;
// do "$c" …`); a curl word built at run time (`"cu${x}rl"`, `$(echo
// curl)`); xargs given its command through `sh -c`; `env`, `timeout`,
// `nice` or `sudo` in front of a NAME that is one of the unseen kinds
// (in front of the word curl itself they change nothing — the word is
// still read); a shell file with no `#!` line under an extension
// SHELL_EXTENSIONS does not name, or embedded in a heredoc of a file
// that is not shell (a Dockerfile `RUN`, a Python string); and a unit
// file that is not `.service` (a `.timer` runs no command; a drop-in
// `.conf` could).

/// One roster entry: in `file`, at most `sends` unstamped sends on
/// commands whose text holds `needle`.
struct Site {
    file: &'static str,
    needle: &'static str,
    sends: usize,
    why: &'static str,
}

const fn site(file: &'static str, needle: &'static str, sends: usize, why: &'static str) -> Site {
    Site {
        file,
        needle,
        sends,
        why,
    }
}

/// A negative probe must reach the estate without its credential; otherwise
/// it measures an accepted request and cannot prove the refusal it claims.
/// One declaration feeds the file scan and the bounded per-command roster.
const TOKENLESS_PROBES: &[Site] = &[site(
    "infra/ops/machine-gate-refuses-tokenless.sh",
    "\"${BOSS_JOBS_URL%/}/api/jobs?limit=1\"",
    1,
    "2710c8fc: the enforcing car's negative probe must present no machine token and receive401; its own fixture pins the empty header, disabled curl config, valid synthetic identity and exact status",
)];

/// Sends that do not go to a gated service port: another party's API, a
/// download, a registry, a local stack, or a path the gate exempts.
const NOT_THE_ESTATE: &[Site] = &[
    site(
        "infra/agent/publish-image.sh",
        "\"$registry_url\"",
        1,
        "The shared OCI reader's registry requests: registry_url is validated before curl to /v2/ on the two published image repository authorities, without userinfo, fragment or traversal; owning actual-curl controls refuse service paths before transport. Only the anonymous registry bearer file may ride here, never the estate machine token.",
    ),
    site(
        "infra/gcp/adopt-root-tree.sh",
        "\"$BOSS_FORGE_URL/api/v1/repos/david/boss\"",
        1,
        "Forgejo repository authentication with its scoped broker token; the estate machine token must not be sent to Forgejo. Lifecycle reads in this file remain stamped.",
    ),
    site(
        "infra/gcp/root-tree-token-recv.sh",
        "\"$BOSS_FORGE_URL/api/v1/repos/david/boss\"",
        1,
        "Forgejo repository authentication with the staged read-only broker token; this receiver must not hold or send the estate machine token",
    ),
    site(
        "infra/forge/publish-github-pr.sh",
        "\"$GITHUB_API/repos/$MIRROR_SLUG/$1\"",
        1,
        "GitHub's public API, for the mirror; the estate token must never leave the estate",
    ),
    site(
        "infra/forge/publish-pr-state.sh",
        "\"$GITHUB_API/repos/$MIRROR_SLUG/",
        2,
        "GitHub's public API: the PR and its check-runs",
    ),
    site(
        "infra/forge/read-publish-checks.sh",
        "\"$GITHUB_API/repos/$MIRROR_SLUG/$1\"",
        1,
        "GitHub's public API: check-runs and annotations",
    ),
    site(
        "infra/forge/cluster-deploy-lib.sh",
        "-H \"Host: $site\" \"http://$addr/\"",
        1,
        "a tenant site asked through the cluster's ingress address, to see that it serves; not a service port",
    ),
    site(
        "infra/forge/cluster-watchdog.sh",
        "\"$JOBS_API/api/jobs/health\"",
        1,
        "the jobs health path, which every gate exempts for GET by exact path; the watchdog must owe nothing to a token",
    ),
    site(
        "infra/forge/probe-reader-deposit.sh",
        "-H \"$READER_HDR\"",
        1,
        "asks each gate about the PROBE READER's value through the value door (its one rostered caller), never the estate token",
    ),
    site(
        "infra/forge/probe-bin/boss-gateway-read",
        "\"${base}${path}\"",
        1,
        "the recorded probe's read of the gateway's public port, which carries nothing that says who asks",
    ),
    // One entry per destination, each needle the URL argument as the
    // file spells it. Until 2026-10-09 this was one entry of three sends
    // whose needle was curl and its flags (`"$($CURL -sS -m 15`), as were
    // journal-read.sh's and move-forgejo-data.sh's below (7369b078 F5);
    // `every_roster_needle_names_a_destination_or_a_credential` holds it.
    site(
        "infra/forge/ci-image-report.sh",
        "\"$REGISTRY_URL/v2/\"",
        1,
        "the forge's container registry: its ping",
    ),
    site(
        "infra/forge/ci-image-report.sh",
        "\"$turl\"",
        1,
        "the forge's container registry: the token endpoint its ping named, for an anonymous pull token",
    ),
    site(
        "infra/forge/ci-image-report.sh",
        "\"$murl\"",
        1,
        "the forge's container registry: a tag read, with the registry's own pull token",
    ),
    site(
        "infra/forge/github-act.sh",
        "-H \"@$WORK/auth\"",
        1,
        "GitHub's API, with the App installation token",
    ),
    site(
        "infra/forge/install.sh",
        "https://dl.k8s.io/release/",
        1,
        "the kubectl download",
    ),
    site(
        "infra/forge/netboot.sh",
        "-o \"$tmp\" -- \"$url\"",
        1,
        "the Talos Image Factory download of the declared netboot kernel and initramfs (backlog ee31ab73); the estate token must never leave the estate",
    ),
    site(
        "infra/forge/journal-read.sh",
        "\"${BASE}/machine\"",
        1,
        "a host's systemd-journal-gatewayd, not a service port: its /machine cutoff",
    ),
    site(
        "infra/forge/journal-read.sh",
        "\"${BASE}/entries\"",
        1,
        "a host's systemd-journal-gatewayd: the one tail entry of the freshness read",
    ),
    site(
        "infra/forge/journal-read.sh",
        "\"Range: entries=:-${COUNT}:${COUNT}\" \"$url\"",
        1,
        "a host's systemd-journal-gatewayd: the entries the caller asked for ($url is ${BASE}/entries and its filters)",
    ),
    site(
        "infra/forge/locomotive.sh",
        "-H \"$FORGE_HDR\"",
        1,
        "the forge's API, with the forge token",
    ),
    site(
        "infra/forge/move-forgejo-data.sh",
        "\"$HEALTHZ\"",
        1,
        "the forge's own healthz",
    ),
    site(
        "infra/forge/move-forgejo-data.sh",
        "\"http://$REG_HOST/v2/token?service=container_registry\"",
        1,
        "the forge's container registry: its token endpoint",
    ),
    site(
        "infra/forge/move-forgejo-data.sh",
        "\"http://$REG_HOST/v2/$REG_OWNER/$REG_NAME/manifests/$1\"",
        1,
        "the forge's container registry: one manifest",
    ),
    site(
        "infra/forge/offsite-push.sh",
        "-H \"@$AUTH\"",
        1,
        "GitHub's API, for the disaster-recovery repository",
    ),
    site(
        "infra/forge/protect-main.sh",
        "\"$CURL\" \"${args[@]}\" \"$url\"",
        1,
        "the forge's API: branch protection",
    ),
    site(
        "infra/forge/prune-registry-versions.sh",
        "-K \"$cfg\"",
        1,
        "the forge's package registry",
    ),
    site(
        "infra/forge/registry-token-deposit.sh",
        "\"$REG_URL/v2/token?service=container_registry\"",
        1,
        "the forge's container registry: the login exchange that proves the push token (its own Authorization rides in a header file; the estate token is not the registry's to see)",
    ),
    site(
        "infra/estate/install-cli-from-image.sh",
        "curl \"${args[@]}\" \"$url\"",
        1,
        "the container registry the CLI image is pulled from",
    ),
    site(
        "infra/estate/install-cluster-operator.sh",
        "https://github.com/siderolabs/talos/releases/download/",
        1,
        "the talosctl download",
    ),
    site(
        "infra/caddy/setup.sh",
        "https://dl.cloudsmith.io/public/caddy/stable/",
        2,
        "Caddy's package key and source list",
    ),
    site(
        "infra/nats/setup.sh",
        "curl -sSL \"${URL}\"",
        1,
        "the NATS release download",
    ),
    site(
        "infra/dev/dev-session.sh",
        "claude) curl -q -fsSL https://claude.ai/install.sh | bash",
        1,
        "an agent CLI's installer",
    ),
    site(
        "infra/seed-brewery-tenant.sh",
        "\"http://127.0.0.1:$port/api/$svc/health\"",
        1,
        "a local stack's health paths, exempt at every gate",
    ),
    site(
        "infra/oss-quickstart/tenant-launch.sh",
        "\"http://127.0.0.1:7950/api/dispatcher/readyz\"",
        1,
        "the quickstart container's own dispatcher readiness, an exact-path exemption; a quickstart holds no estate token",
    ),
    site(
        "infra/cluster/dev-memory-watch.sh",
        "/api/v1/namespaces/$ns/pods/$POD\"",
        1,
        "the cluster's own API, read with the pod's service-account token for its restart counts; not a service port, and the estate token must not ride to it",
    ),
];

/// Lines the scan reads as a send that run no request of their own.
const NOT_A_SEND: &[Site] = &[
    site(
        "infra/lib/curl-through-a-roll.sh",
        "curl -q \"$@\" >&\"$write_fd\"",
        1,
        "the retry wrapper: it runs its caller's argv whole, and the header is judged on the caller's line",
    ),
    site(
        "infra/boss-api-curl.sh",
        "curl_through_a_roll \"${BOSS_API_RETRY_DEADLINE:-150}\"",
        2,
        "the roll-posture door: it passes its caller's argv to the wrapper, and the header is judged on the caller's line. Counted twice because the label it hands the wrapper, `boss-api-curl`, is itself a word that names curl",
    ),
    site(
        "infra/dev/dev-session.sh",
        "echo \"curl -q -fsSL https://claude.ai/install.sh",
        1,
        "prose: the install command printed for a dry run",
    ),
    site(
        "infra/forge/landed-train-shas.lib.sh",
        "train_sha_sets \"$curl_cmd\" \"$jobs_url\"",
        1,
        "hands the curl command's NAME to train_sha_sets, whose one request is stamped where it is sent",
    ),
    site(
        "infra/forge/disk-floor-sweep.sh",
        "landed_train_shas \"$SWEEP_CURL_CMD\"",
        1,
        "hands the curl command's NAME to the landed-trains lib, which stamps its own request",
    ),
    site(
        "infra/forge/prune-registry-versions.sh",
        "train_sha_sets curl \"$JOBS_URL\"",
        1,
        "hands the curl command's NAME to the landed-trains lib, which stamps its own request",
    ),
    site(
        "infra/caddy/setup.sh",
        "echo \"Test (after cert issues): curl -v",
        1,
        "prose: a command printed for the operator",
    ),
    site(
        "infra/tla/run-tlc.sh",
        "curl -sL -o ${JAR}",
        1,
        "prose: a download command printed for the operator",
    ),
];

/// Sends to a gated port that are owed the header and are another car's
/// to give: a debt with the reason it was not paid by the per-call-site
/// car. Not a licence; a new site matches none of these.
///
/// EMPTY since 2026-10-08. Its one entry was infra/ml/run-inference-batch.sh,
/// whose seven POSTs now carry the reader's header file. What that
/// sender still owes is not a call site and so is not this roster's to
/// hold: its unit runs as `boss`, which cannot read the token directory,
/// so it presents nothing until the unit's account can (the ownership
/// question of backlog ca356c79) — the gate's own record says when.
const OWED: &[Site] = &[];

/// A variable or an argv array a lib is HANDED already filled, by a
/// reader call in the file that sources it. (file, name, where it is
/// filled). The scan reads one file at a time and cannot follow a
/// `source`, so the hand-over is written here and is the one thing in
/// this section a reader has to check by opening the caller.
const HANDED: &[(&str, &str, &str)] = &[
    (
        "infra/forge/publish-pr-state.sh",
        "MT_HDR",
        "its two callers' `machine_token_header MT_HDR` (publish-github-pr.sh, read-publish-checks.sh); the function refuses to run when the variable was never set",
    ),
    (
        "infra/forge/landed-train-shas.lib.sh",
        "mt_hdr",
        "a local copy of LTS_MT_HDR, which this lib's own reader call fills for one URL, left empty when the request goes to another",
    ),
    (
        "infra/lib/runner-delivery-ack.sh",
        "HDRS",
        "the argv array its two callers build and extend with the reader's file (runner-credential-deposit.sh `HDRS+=(-H \"$MT_HDR\")`, runner-credential-recv.sh the same)",
    ),
];

/// Joined commands: (first line number, text). A line ending in a
/// backslash continues on the next; a comment line is dropped unless it
/// continues a command.
fn commands(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut start = 0;
    for (n, line) in text.lines().enumerate() {
        if buf.is_empty() {
            start = n + 1;
            if line.trim_start().starts_with('#') {
                continue;
            }
        }
        if let Some(head) = line.strip_suffix('\\') {
            buf.push_str(head);
            buf.push(' ');
            continue;
        }
        buf.push_str(line);
        out.push((start, std::mem::take(&mut buf)));
    }
    if !buf.is_empty() {
        out.push((start, buf));
    }
    out
}

/// Is this whitespace-separated word curl, by any of its names? What is
/// left after the syntax a command word can be wrapped in — `$(`, a
/// quote, `!`, `{`, `|` — is curl itself, a path, a function or a file
/// whose name holds `curl`, or a `$VAR` / `${VAR}` / `${VAR:-…}` whose
/// name does — or whose name is one of `aliases`, the variables this
/// file assigns a curl to (`api="$here/../boss-api-curl.sh"`, `c=curl`).
fn is_curl_word(word: &str, aliases: &[String]) -> bool {
    let core = word.rsplit('(').next().unwrap_or(word);
    let core = core.trim_start_matches(['|', ';', '&', '{', '!', '`']);
    let core = core.trim_matches(['"', '\'', '`']);
    if let Some(var) = core.strip_prefix('$') {
        let var = var.strip_prefix('{').unwrap_or(var);
        let name: String = var
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let rest = &var[name.len()..];
        // `${DOOR:-curl}` / `${DOOR:=curl}`: the name says nothing and
        // the default is the curl (backlog 7369b078, F5).
        let default = rest.strip_prefix(':').unwrap_or(rest);
        if default.starts_with(['-', '=']) && default.to_ascii_lowercase().contains("curl") {
            return true;
        }
        return (name.to_ascii_lowercase().contains("curl") || aliases.contains(&name))
            && (rest.is_empty() || rest.starts_with('}') || rest.starts_with(':'));
    }
    core.to_ascii_lowercase().contains("curl")
        && core
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'))
}

/// How many times `command` runs curl with an argument: a curl word
/// followed by a word that opens like one (a flag, a quoted or expanded
/// value, a header file, a URL). Prose names curl and then says `exit`
/// or `failed`; a command hands it something.
///
/// OR IT IS xargs's COMMAND (backlog 7369b078, F5 of review 9c484ca8):
/// `… | xargs curl` hands curl its arguments on stdin, so the word can be
/// the last on the line and still send. A curl word that follows `xargs`
/// and its options is a send whatever comes after it.
fn sends(reader: &Reader, command: &str) -> usize {
    let words: Vec<&str> = command.split_whitespace().collect();
    (0..words.len())
        .filter(|&i| {
            is_curl_word(words[i], &reader.aliases)
                && (words.get(i + 1).is_some_and(|next| {
                    next.starts_with(['-', '"', '$', '@', '\'']) || next.starts_with("http")
                }) || xargs_runs(&words, i))
        })
        .count()
}

/// Is `words[i]` the command `xargs` runs? Walk back over xargs's own
/// options and their values (`-0`, `-r`, `-n 1`, `-P 4`, `-I {}`).
fn xargs_runs(words: &[&str], i: usize) -> bool {
    words[..i]
        .iter()
        .rev()
        .find(|w| {
            let bare = w.trim_matches(['"', '\'']);
            !(bare.starts_with('-') || bare == "{}" || bare.chars().all(|c| c.is_ascii_digit()))
        })
        .is_some_and(|w| {
            w.trim_start_matches(['|', ';', '&', '('])
                .ends_with("xargs")
        })
}

/// The command without a trailing comment. Cutting too early only loses
/// headers, which refuses; it never finds one.
fn before_comment(command: &str) -> &str {
    let cut = [" #", "\t#"]
        .iter()
        .filter_map(|m| command.find(m))
        .min()
        .unwrap_or(command.len());
    &command[..cut]
}

/// What one file's own lines say about where the token's header lives.
struct Reader {
    vars: Vec<String>,
    arrays: Vec<String>,
    /// Variables this file assigns a curl to, under a name that does not
    /// say so. infra/cluster/dev-scratch-reclaim.sh sends through
    /// `"$api"`, set to boss-api-curl.sh two lines above; without this a
    /// request through such a name is no curl word at all.
    aliases: Vec<String>,
}

/// `NAME=value`, value up to the first whitespace.
static ASSIGNMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[\s;&|(])([A-Za-z_][A-Za-z0-9_]*)=(\S+)").unwrap());

/// The names a file assigns a curl to: the value names curl and is not a
/// command substitution (`out=$(curl …)` holds an ANSWER, and `"$out"`
/// handed to jq is not a request).
///
/// AND THE FOUR WAYS TO NAME ONE THAT ARE NOT `NAME=value` (backlog
/// 7369b078, F5 of review 9c484ca8; none is in the tree on 2026-10-09):
/// a default assigned in the expansion (`${NAME:=curl}`), the path a
/// lookup prints (`NAME=$(command -v curl)` — a substitution, and the one
/// whose answer IS a curl), `printf -v NAME … curl`, and `read NAME <<<
/// …curl…`. Each is read off its own line; a `read` from a file or a
/// pipe names nothing this scan can know.
fn aliases_of(text: &str) -> Vec<String> {
    live(text)
        .flat_map(|l| {
            let l = before_comment(l);
            ASSIGNMENT
                .captures_iter(l)
                .filter(|c| {
                    let value = c[2].to_ascii_lowercase();
                    value.contains("curl") && !value.contains("$(") && !value.contains('`')
                })
                .map(|c| c[1].to_string())
                .chain(
                    ALIAS_BY_DEFAULT
                        .captures_iter(l)
                        .filter(|c| c[2].to_ascii_lowercase().contains("curl"))
                        .map(|c| c[1].to_string()),
                )
                .chain(
                    [&*ALIAS_BY_LOOKUP, &*ALIAS_BY_PRINTF, &*ALIAS_BY_READ]
                        .into_iter()
                        .flat_map(|re| re.captures_iter(l).map(|c| c[1].to_string())),
                )
                .collect::<Vec<_>>()
        })
        .collect()
}

/// `${NAME:=value}` / `${NAME=value}`.
static ALIAS_BY_DEFAULT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*):?=([^}]*)\}").unwrap());
/// `NAME=$(command -v …curl)`, `which` and `type -p` the same.
static ALIAS_BY_LOOKUP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:^|[\s;&|(])([A-Za-z_][A-Za-z0-9_]*)="?\$\(\s*(?:command\s+-v|which|type\s+-[pP])\s+\S*(?i:curl)"#,
    )
    .unwrap()
});
/// `printf -v NAME … curl`.
static ALIAS_BY_PRINTF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s;&|(])printf\s+-v\s+([A-Za-z_][A-Za-z0-9_]*)\s.*(?i:curl)").unwrap()
});
/// `read [-r …] NAME <<< …curl…`.
static ALIAS_BY_READ: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s;&|(])read\s+(?:-\S+\s+)*([A-Za-z_][A-Za-z0-9_]*)\s*<<<.*(?i:curl)")
        .unwrap()
});

/// A reader CALL, not a mention: at the start of the line, or behind
/// `if`, `!`, a command separator or a case arm's `)`.
static READER_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^\s*(?:if\s+)?(?:!\s+)?|[;&|{()]\s*(?:!\s+)?)machine_token_header\s+([A-Za-z_][A-Za-z0-9_]*)",
    )
    .unwrap()
});
/// `A+=(-H "$V")`.
static ARRAY_EXTEND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"([A-Za-z_][A-Za-z0-9_]*)\+=\(\s*-H\s+"\$([A-Za-z_][A-Za-z0-9_]*)"\s*\)"#).unwrap()
});
/// `-H "$V"`, with the `${G:+` guard in front of it when there is one.
static HEADER_FILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:\$\{([A-Za-z_][A-Za-z0-9_]*):\+)?-H\s+"\$([A-Za-z_][A-Za-z0-9_]*)""#).unwrap()
});
/// `"${A[@]}"`.
static ARRAY_USE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""\$\{([A-Za-z_][A-Za-z0-9_]*)\[@\]\}""#).unwrap());

fn reader_of(rel: &str, text: &str) -> Reader {
    let (call, extend) = (&*READER_CALL, &*ARRAY_EXTEND);
    let mut vars: Vec<String> = live(text)
        .flat_map(|l| {
            call.captures_iter(l)
                .map(|c| c[1].to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    vars.extend(
        HANDED
            .iter()
            .filter(|(f, _, _)| *f == rel)
            .map(|(_, v, _)| v.to_string()),
    );
    let arrays = live(text)
        .flat_map(|l| {
            extend
                .captures_iter(before_comment(l))
                .filter(|c| vars.iter().any(|v| *v == c[2]))
                .map(|c| c[1].to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    let mut arrays: Vec<String> = arrays;
    arrays.extend(
        HANDED
            .iter()
            .filter(|(f, _, _)| *f == rel)
            .map(|(_, v, _)| v.to_string()),
    );
    Reader {
        vars,
        arrays,
        aliases: aliases_of(text),
    }
}

/// How many times `command` hands curl the reader's header file.
fn stamps(reader: &Reader, command: &str) -> usize {
    let command = before_comment(command);
    let (file, array) = (&*HEADER_FILE, &*ARRAY_USE);
    file.captures_iter(command)
        .filter(|c| {
            c.get(1).is_none_or(|guard| guard.as_str() == &c[2])
                && reader.vars.iter().any(|v| *v == c[2])
        })
        .count()
        + array
            .captures_iter(command)
            .filter(|c| reader.arrays.iter().any(|a| *a == c[1]))
            .count()
}

fn roster() -> impl Iterator<Item = &'static Site> {
    NOT_THE_ESTATE
        .iter()
        .chain(TOKENLESS_PROBES)
        .chain(NOT_A_SEND)
        .chain(OWED)
}

/// (refused lines, roster entries that excuse nothing now).
fn unstamped_sends(root: &Path) -> (Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    walk(&root.join("infra"), &mut files);
    files.sort();
    let mut refused = Vec::new();
    let mut used: Vec<usize> = roster().map(|_| 0).collect();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        if !is_shell(&f, &text) {
            continue;
        }
        let rel = f.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        if listed(&rel) {
            continue;
        }
        let reader = reader_of(&rel, &text);
        for (n, command) in commands(&text) {
            let (sent, stamped) = (sends(&reader, &command), stamps(&reader, &command));
            let shown = command.split_whitespace().collect::<Vec<_>>().join(" ");
            // THE OTHER DIRECTION. The reader judges the host once, where
            // it is called; a header file it filled for the system of
            // record and handed to a request the roster says leaves the
            // estate would carry the token out with it.
            if stamped > 0
                && let Some(out) = NOT_THE_ESTATE
                    .iter()
                    .chain(TOKENLESS_PROBES)
                    .find(|s| s.file == rel && command.contains(s.needle))
            {
                refused.push(format!(
                    "{rel}:{n}: hands the estate token's header to a request the tokenless roster names ({}): {shown}",
                    out.why
                ));
                continue;
            }
            if sent <= stamped {
                continue;
            }
            match roster().position(|s| s.file == rel && command.contains(s.needle)) {
                Some(i) => {
                    used[i] += sent - stamped;
                    let entry = roster().nth(i).unwrap();
                    if used[i] > entry.sends {
                        refused.push(format!(
                            "{rel}:{n}: one more unstamped send than the roster entry {:?} excuses ({}): {shown}",
                            entry.needle, entry.sends
                        ));
                    }
                }
                None => refused.push(format!(
                    "{rel}:{n}: {sent} send(s), {stamped} carrying the reader's header: {shown}"
                )),
            }
        }
    }
    let stale = roster()
        .zip(&used)
        .filter(|(s, u)| **u < s.sends)
        .map(|(s, u)| {
            format!(
                "{}: {:?} excuses {} send(s) and {} match now ({})",
                s.file, s.needle, s.sends, u, s.why
            )
        })
        .collect();
    (refused, stale)
}

#[test]
fn every_send_from_a_shell_script_carries_the_machine_token_or_is_rostered() {
    let (refused, stale) = unstamped_sends(&repo_root());
    for s in &stale {
        // Printed, not failed: two cars may stamp and roster the same
        // site, and a pin that went red on the assembled tree for that
        // would punish the fix (the module doc's rule for NOT_YET).
        println!("stale: {s}");
    }
    assert!(
        refused.is_empty(),
        "these commands run curl without the estate machine token's header file, so a request \
         they send to a service port is one every machine gate records as a caller it would \
         refuse — the shape that left the ops runner's every-minute queue read unstamped beside \
         seven stamped writes (backlog 44b2087e). Hand curl the file this script's own reader \
         call filled, in the same command, reads as well as writes: \
         `${{MT_HDR:+-H \"$MT_HDR\"}}` after `machine_token_header MT_HDR \"$BASE\"` \
         (infra/lib/secret-header.sh; presented, never required). A request that must not carry \
         it — another party's API, a download, an exempt health path — is named in \
         NOT_THE_ESTATE in this file with its reason:\n  {}",
        refused.join("\n  ")
    );
}

#[test]
fn the_per_site_scan_sees_the_tree() {
    let root = repo_root();
    let mut files = Vec::new();
    walk(&root.join("infra"), &mut files);
    let mut stamped = 0;
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        if !is_shell(&f, &text) {
            continue;
        }
        let rel = f
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let reader = reader_of(&rel, &text);
        stamped += commands(&text)
            .iter()
            .filter(|(_, c)| sends(&reader, c) > 0 && stamps(&reader, c) >= sends(&reader, c))
            .count();
    }
    assert!(
        stamped >= 50,
        "the per-site scan found {stamped} stamped sends, fewer than fifty — it has stopped \
         seeing the tree, and a scanner that sees nothing passes everything"
    );
}

/// Every roster entry names a file that exists and gives a reason, and
/// no two entries of one file can claim the same command.
#[test]
fn the_site_rosters_are_well_formed() {
    let root = repo_root();
    let all: Vec<&Site> = roster().collect();
    for (i, s) in all.iter().enumerate() {
        assert!(root.join(s.file).is_file(), "{} is gone — drop it", s.file);
        assert!(
            !s.why.is_empty() && !s.needle.is_empty() && s.sends > 0,
            "{}",
            s.file
        );
        assert!(
            !listed(s.file),
            "{} is excused whole by BY_DESIGN / NOT_YET; a site entry for it is never read",
            s.file
        );
        for t in &all[i + 1..] {
            assert!(
                s.file != t.file || !(s.needle.contains(t.needle) || t.needle.contains(s.needle)),
                "{}: {:?} and {:?} overlap, so which one a command spends is an accident of order",
                s.file,
                s.needle,
                t.needle
            );
        }
    }
    for (f, v, why) in HANDED {
        assert!(root.join(f).is_file(), "{f} is gone — drop it");
        assert!(!why.is_empty() && !v.is_empty());
    }
}

/// The scan, on the shapes that have hidden a send. A planted tree,
/// never executed. Each file is one case; the names say which.
#[test]
fn an_unstamped_send_is_refused_however_it_is_spelled() {
    let root = boss_testing::scratch_dir("per-site-spellings");
    let dir = root.join("infra/forge");
    std::fs::create_dir_all(&dir).unwrap();
    let head = "#!/usr/bin/env bash\nmachine_token_header MT_HDR \"$BASE\" || MT_HDR=\"\"\n";
    let refused_cases: &[(&str, &str)] = &[
        // The measured shape: a stamped write and an unstamped read.
        (
            "read-beside-write.sh",
            "curl -fsS -X PUT ${MT_HDR:+-H \"$MT_HDR\"} \"$BASE/api/jobs/x\"\n\
             if ! jobs=$(curl -fsS -H \"x-boss-user: $U\" \\\n    \"$BASE/api/jobs?kind=x\" 2>&1); then exit 1; fi\n",
        ),
        // No identity on the line at all: the per-file scan's blind spot.
        (
            "unsigned.sh",
            "body=\"$(\"${CURL}\" -sSf -X POST \"${BASE}/api/x\")\"\n",
        ),
        // The identity in a variable, the header's name never written.
        (
            "identity-in-a-variable.sh",
            "curl -fsS \"${hdrs[@]}\" \"$BASE/api/x\"\n",
        ),
        // The door by variable, with an environment prefix.
        (
            "door-by-variable.sh",
            "code=\"$(WAIT=5 \"$API_CURL\" -sS -H \"x-boss-user: $U\" \"$BASE/api/x\")\"\n",
        ),
        // The header named only in a comment on the command's own line.
        (
            "header-in-a-comment.sh",
            "curl -fsS \"$BASE/api/x\" # ${MT_HDR:+-H \"$MT_HDR\"}\n",
        ),
        // Guarded by another variable: sent only when THAT one is set.
        (
            "guarded-by-another.sh",
            "curl -fsS ${OTHER:+-H \"$MT_HDR\"} \"$BASE/api/x\"\n",
        ),
        // A header file the reader never filled.
        (
            "another-header.sh",
            "curl -fsS ${RC_HDR:+-H \"$RC_HDR\"} \"$BASE/api/x\"\n",
        ),
        // Two sends in one command, one header.
        (
            "two-sends-one-header.sh",
            "curl -s ${MT_HDR:+-H \"$MT_HDR\"} \"$BASE/a\" || curl -s \"$BASE/b\"\n",
        ),
        // After exec, by path, and through a function named for curl.
        ("exec-path.sh", "exec /usr/bin/curl -fsS \"$BASE/api/x\"\n"),
        (
            "function.sh",
            "out=$(api_curl -s -X POST \"$BASE/api/x\")\n",
        ),
        (
            "default-expansion.sh",
            "\"${CURL:-curl}\" -fsS \"$BASE/api/x\"\n",
        ),
        // Through a name that does not say curl, assigned in this file:
        // the door dev-scratch-reclaim.sh uses, and the plain alias.
        (
            "door-under-another-name.sh",
            "api=\"$here/../boss-api-curl.sh\"\n[ -x \"$api\" ] || api=boss-api-curl.sh\n\
             \"$api\" -fsS -H \"x-boss-user: $U\" \"$BASE/api/x\"\n",
        ),
        ("alias.sh", "c=curl\n$c -fsS \"$BASE/api/x\"\n"),
        // The default is the curl, and the name says nothing.
        (
            "default-under-another-name.sh",
            "\"${DOOR:-curl}\" -fsS \"$BASE/api/x\"\n",
        ),
        // The four ways to give a curl another name that are not
        // `NAME=value` (backlog 7369b078, F5 of review 9c484ca8).
        (
            "alias-by-default.sh",
            ": \"${api:=curl}\"\n\"$api\" -fsS \"$BASE/api/x\"\n",
        ),
        (
            "alias-by-command-v.sh",
            "api=\"$(command -v curl)\"\n\"$api\" -fsS \"$BASE/api/x\"\n",
        ),
        (
            "alias-by-printf-v.sh",
            "printf -v api '%s' curl\n\"$api\" -fsS \"$BASE/api/x\"\n",
        ),
        (
            "alias-by-read.sh",
            "read -r api <<< \"$here/boss-api-curl.sh\"\n\"$api\" -fsS \"$BASE/api/x\"\n",
        ),
        // curl as xargs's command: its arguments arrive on stdin, so
        // nothing follows the word on the line.
        (
            "xargs-at-line-end.sh",
            "printf '%s\\n' \"$BASE/api/x\" | xargs curl\n",
        ),
        (
            "xargs-with-flags.sh",
            "printf '%s\\n' \"$BASE/api/x\" | xargs -r -n 1 curl -fsS\n",
        ),
        // Shell under a name that is not `.sh` and has no `#!` line: the
        // head here is a comment, as a sourced fragment's is.
        ("fragment.bash", "curl -fsS \"$BASE/api/x\"\n"),
        ("fragment.inc", "curl -fsS \"$BASE/api/x\"\n"),
    ];
    for (name, body) in refused_cases {
        // A fragment has no `#!` line: its extension alone makes it shell.
        let head = if name.ends_with(".sh") {
            head
        } else {
            "# sourced\n# by its caller\n"
        };
        boss_testing::write_file(&dir.join(name), &format!("{head}{body}"));
    }
    // A file whose header variable only LOOKS like the token's: no reader
    // call fills it (the registry bearer in install-cli-from-image.sh).
    boss_testing::write_file(
        &dir.join("named-like-it.sh"),
        "#!/bin/sh\nsecret_header TOKEN_HDR \"Authorization: Bearer $T\"\n\
         # machine_token_header TOKEN_HDR is what this does NOT call\n\
         echo \"made no machine_token_header TOKEN_HDR call\"\n\
         curl -fsS ${TOKEN_HDR:+-H \"$TOKEN_HDR\"} \"$BASE/api/x\"\n",
    );
    let passing: &[(&str, &str)] = &[
        (
            "stamped.sh",
            "curl -fsS ${MT_HDR:+-H \"$MT_HDR\"} \"$BASE/api/x\"\n",
        ),
        (
            "stamped-across-lines.sh",
            "if ! out=$(\"$API_CURL\" -fsS -H \"x-boss-user: $U\" \\\n    ${MT_HDR:+-H \"$MT_HDR\"} \\\n    \"$BASE/api/x\"); then exit 1; fi\n",
        ),
        (
            "stamped-unconditionally.sh",
            "[ -z \"$MT_HDR\" ] || curl -sS -H \"$MT_HDR\" \"$BASE/api/x\"\n",
        ),
        (
            "stamped-by-array.sh",
            "HDRS=(-H \"x-boss-user: $U\")\n[ -z \"$MT_HDR\" ] || HDRS+=(-H \"$MT_HDR\")\n\
             curl -sS \"${HDRS[@]}\" \"$BASE/api/x\"\n",
        ),
        (
            "an-answer-is-not-a-door.sh",
            "out=$(curl -fsS ${MT_HDR:+-H \"$MT_HDR\"} \"$BASE/api/x\")\n\
             jq -n --arg body \"$out\" '{body: $body}'\necho \"$out\" \"$BASE\"\n",
        ),
        (
            "prose.sh",
            "for tool in jq curl date; do :; done\ncurl_err=\"$work/curl.err\"\n\
             echo \"the read failed (curl exit $rc), and curl's message is above\"\n\
             API_CURL=\"$HERE/boss-api-curl.sh\"\n[ -x \"$API_CURL\" ] || API_CURL=boss-api-curl.sh\n",
        ),
    ];
    for (name, body) in passing {
        boss_testing::write_file(&dir.join(name), &format!("{head}{body}"));
    }
    let (refused, _) = unstamped_sends(&root);
    let named = |name: &str| {
        refused
            .iter()
            .filter(|l| l.contains(&format!("/{name}:")))
            .count()
    };
    for (name, _) in refused_cases {
        assert_eq!(named(name), 1, "{name} is one unstamped send: {refused:#?}");
    }
    assert_eq!(named("named-like-it.sh"), 1, "{refused:#?}");
    for (name, _) in passing {
        assert_eq!(
            named(name),
            0,
            "{name} sends nothing unstamped: {refused:#?}"
        );
    }
    assert!(
        refused
            .iter()
            .any(|l| l.starts_with("infra/forge/read-beside-write.sh:4:")),
        "the refusal names the line the command starts on: {refused:#?}"
    );
}

/// A request the roster requires to be tokenless stays tokenless: an
/// external request must not leak it and a negative probe must not carry it.
#[test]
fn a_rostered_tokenless_request_may_not_carry_the_header() {
    for entry in [&NOT_THE_ESTATE[0], &TOKENLESS_PROBES[0]] {
        let root = boss_testing::scratch_dir("per-site-token-leaves");
        std::fs::create_dir_all(root.join(entry.file).parent().unwrap()).unwrap();
        let head = "#!/usr/bin/env bash\nmachine_token_header MT_HDR \"$BASE\" || MT_HDR=\"\"\n";
        boss_testing::write_file(
            &root.join(entry.file),
            &format!(
                "{head}curl -fsS ${{MT_HDR:+-H \"$MT_HDR\"}} \\\n    {} > \"$2\"\n",
                entry.needle
            ),
        );
        let (refused, _) = unstamped_sends(&root);
        assert_eq!(refused.len(), 1, "{refused:#?}");
        assert!(
            refused[0].contains("hands the estate token's header")
                && refused[0].starts_with(&format!("{}:3:", entry.file)),
            "{refused:#?}"
        );
        // The same request without the header is the rostered one, and passes.
        boss_testing::write_file(
            &root.join(entry.file),
            &format!("{head}curl -fsS {} > \"$2\"\n", entry.needle),
        );
        assert!(unstamped_sends(&root).0.is_empty());
    }
}

/// A roster entry excuses the sends it counts and not one more, and only
/// in its own file.
#[test]
fn a_roster_entry_excuses_its_count_and_no_more() {
    let entry = &NOT_THE_ESTATE[0];
    let root = boss_testing::scratch_dir("per-site-roster-count");
    let line = format!("curl -fsS {} > \"$2\"\n", entry.needle);
    std::fs::create_dir_all(root.join(entry.file).parent().unwrap()).unwrap();
    boss_testing::write_file(
        &root.join(entry.file),
        &format!("#!/usr/bin/env bash\n{line}"),
    );
    let (refused, stale) = unstamped_sends(&root);
    assert!(refused.is_empty(), "{refused:#?}");
    assert!(
        !stale.iter().any(|s| s.contains(entry.needle)),
        "an entry spent in full is not stale: {stale:#?}"
    );
    boss_testing::write_file(
        &root.join(entry.file),
        &format!("#!/usr/bin/env bash\n{}", line.repeat(entry.sends + 1)),
    );
    let (refused, _) = unstamped_sends(&root);
    assert_eq!(
        refused.len(),
        1,
        "the send past the count is refused: {refused:#?}"
    );
    boss_testing::write_file(
        &root.join(std::path::Path::new(entry.file).with_file_name("some-other-sender.sh")),
        &format!("#!/usr/bin/env bash\n{line}"),
    );
    let (refused, _) = unstamped_sends(&root);
    assert_eq!(
        refused.len(),
        2,
        "the same line in another file is refused: {refused:#?}"
    );
}

/// A roster needle says WHERE the request goes, or which credential it
/// carries — never only curl and its flags (backlog 7369b078, F5 of
/// review 9c484ca8). Three did: `curl -s -m` (journal-read.sh),
/// `code=$(curl -sS --max-time` (move-forgejo-data.sh) and
/// `"$($CURL -sS -m 15` (ci-image-report.sh), each excusing three sends.
/// Such an entry excuses whatever request is next written with the same
/// flags in that file, up to its count, wherever it goes: after one of
/// the three was deleted, a new `curl -s -m 5 "$BASE/api/jobs"` would
/// have been excused as "a host's systemd-journal-gatewayd".
fn needle_names_more_than_curl_and_its_flags(needle: &str) -> bool {
    needle.split_whitespace().any(|w| {
        let bare = w.trim_matches(['"', '\'']);
        !is_curl_word(w, &[])
            && !bare.starts_with('-')
            && !bare.is_empty()
            && !bare.chars().all(|c| c.is_ascii_digit())
    })
}

#[test]
fn every_roster_needle_names_a_destination_or_a_credential() {
    assert!(!needle_names_more_than_curl_and_its_flags("curl -s -m"));
    assert!(!needle_names_more_than_curl_and_its_flags(
        "code=$(curl -sS --max-time"
    ));
    assert!(!needle_names_more_than_curl_and_its_flags(
        "\"$($CURL -sS -m 15"
    ));
    assert!(needle_names_more_than_curl_and_its_flags(
        "\"${BASE}/machine\""
    ));
    assert!(needle_names_more_than_curl_and_its_flags(
        "-H \"$FORGE_HDR\""
    ));
    for s in roster() {
        assert!(
            needle_names_more_than_curl_and_its_flags(s.needle),
            "{}: the needle {:?} is curl and its flags and nothing else, so it excuses the next \
             request written with those flags, wherever that one goes. Name the destination \
             (the URL argument as the file spells it) or the credential header it carries",
            s.file,
            s.needle
        );
    }
}

// =====================================================================
// A UNIT RUNS NO REQUEST OF ITS OWN (backlog 7369b078, F5 of review
// 9c484ca8)
// =====================================================================
//
// Everything above reads shell FILES. A systemd unit's `ExecStart=` (or
// any `Exec…=` line) is a command line too, and one that ran curl — bare,
// or inside `/bin/sh -c '…'` — would send a request no scan here opens:
// the unit is not shell by name or by `#!`. None does today (measured
// 2026-10-09 on origin/main cb0790715: 11 units under infra/ name curl,
// every one in a comment). So this is a plain denial with no roster: a
// unit runs a script, and the script is judged where it stands. A request
// that truly belongs in a unit line gets a roster here, written by the
// car that needs it, with the reason.

/// (refused `file:line: text`, unit files with at least one Exec line).
fn unit_sends(root: &Path) -> (Vec<String>, usize) {
    let mut files = Vec::new();
    walk(&root.join("infra"), &mut files);
    files.sort();
    let none = reader_of("", "");
    let mut refused = Vec::new();
    let mut units = 0;
    for f in files {
        if f.extension().is_none_or(|x| x != "service") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let rel = f.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        let mut has_exec = false;
        for (n, command) in commands(&text) {
            let line = command.trim_start();
            if line.starts_with(';') || !line.starts_with("Exec") {
                continue;
            }
            let Some((_, run)) = line.split_once('=') else {
                continue;
            };
            has_exec = true;
            // systemd's own prefixes on the executable: `-`, `@`, `+`,
            // `!`, `!!`, `:`.
            let run = run
                .trim_start()
                .trim_start_matches(['-', '@', '+', '!', ':']);
            if sends(&none, run) > 0 {
                refused.push(format!("{rel}:{n}: {}", line.trim_end()));
            }
        }
        units += usize::from(has_exec);
    }
    (refused, units)
}

#[test]
fn no_unit_runs_curl_in_its_own_exec_line() {
    let (refused, units) = unit_sends(&repo_root());
    assert!(
        units >= 10,
        "the unit scan found {units} unit files with an Exec line, fewer than ten — it has \
         stopped seeing the tree, and a scanner that sees nothing passes everything"
    );
    assert!(
        refused.is_empty(),
        "these unit lines run curl themselves, so the request is sent by no shell file and the \
         per-call-site rule in this file never reads it — it would reach a service port without \
         the estate machine token, or carry a secret in a command line. Move the request into a \
         script under infra/ that calls `machine_token_header` and hands curl its file, and have \
         the unit run the script:\n  {}",
        refused.join("\n  ")
    );
}

#[test]
fn a_unit_that_runs_curl_is_refused() {
    let root = boss_testing::scratch_dir("unit-sends");
    let dir = root.join("infra/forge");
    std::fs::create_dir_all(&dir).unwrap();
    let cases: &[(&str, &str, usize)] = &[
        (
            "bare.service",
            "[Service]\nExecStart=/usr/bin/curl -fsS http://127.0.0.1:7900/api/jobs\n",
            1,
        ),
        (
            "prefixed.service",
            "[Service]\nExecStartPre=-curl -fsS \"http://sor/api/x\"\n",
            1,
        ),
        (
            "in-a-shell.service",
            "[Service]\nExecStart=/bin/sh -c 'curl -fsS -X POST \"$BOSS_JOBS_URL/api/jobs\"'\n",
            1,
        ),
        (
            "continued.service",
            "[Service]\nExecStopPost=/bin/sh -c 'test -n \"$X\" && \\\n    curl -fsS \"$U\"'\n",
            1,
        ),
        (
            "runs-a-script.service",
            "[Service]\n# curl -fsS http://x is what this does NOT run\n; ExecStart=/usr/bin/curl -fsS http://x\n\
             Environment=BOSS_API_CURL=/usr/local/lib/boss/boss-api-curl.sh\n\
             ExecStart=/usr/local/lib/boss/ops-runner.sh --once\n",
            0,
        ),
    ];
    for (name, body, _) in cases {
        boss_testing::write_file(&dir.join(name), body);
    }
    // Not a unit: the scan reads `.service` only.
    boss_testing::write_file(
        &dir.join("notes.txt"),
        "ExecStart=/usr/bin/curl -fsS http://x\n",
    );
    let (refused, units) = unit_sends(&root);
    assert_eq!(units, cases.len());
    for (name, _, want) in cases {
        let got = refused
            .iter()
            .filter(|l| l.starts_with(&format!("infra/forge/{name}:")))
            .count();
        assert_eq!(got, *want, "{name}: refused {got} time(s): {refused:#?}");
    }
    assert_eq!(refused.len(), 4, "{refused:#?}");
}

#[test]
fn the_per_site_matchers_read_a_command_not_a_line() {
    assert!(is_curl_word("curl", &[]));
    assert!(is_curl_word("jobs=$(curl", &[]));
    assert!(is_curl_word("reply=\"$(\"$curl_cmd\"", &[]));
    assert!(is_curl_word("\"$API_CURL\"", &[]));
    assert!(is_curl_word("\"${CURL}\"", &[]));
    assert!(is_curl_word("$CURL", &[]));
    assert!(is_curl_word("/usr/bin/curl", &[]));
    assert!(is_curl_word("boss-api-curl.sh", &[]));
    assert!(is_curl_word("curl_through_a_roll", &[]));
    assert!(is_curl_word("|curl", &[]));
    assert!(!is_curl_word("curl's", &[]));
    assert!(!is_curl_word("curl,", &[]));
    assert!(!is_curl_word("\"$TMP/curl.err\"", &[]));
    assert!(!is_curl_word("API_CURL=\"$HERE/boss-api-curl.sh\"", &[]));
    assert!(!is_curl_word("$BASE", &[]));
    assert!(is_curl_word("\"${DOOR:-curl}\"", &[]));
    assert!(is_curl_word("\"${DOOR:=/usr/bin/curl}\"", &[]));
    assert!(!is_curl_word("\"${DOOR:-wget}\"", &[]));
    let none = reader_of("x.sh", "");
    assert_eq!(sends(&none, "echo \"failed (curl exit $rc)\""), 0);
    assert_eq!(sends(&none, "command -v curl >/dev/null"), 0);
    assert_eq!(sends(&none, "curl \"$url\" -fsS"), 1);
    assert_eq!(
        sends(&none, "curl http://127.0.0.1:7900/api/jobs/health"),
        1
    );
    assert_eq!(sends(&none, "\"$api\" -fsS \"$u\""), 0);
    let door = reader_of("x.sh", "    api=\"$here/../boss-api-curl.sh\"\n");
    assert_eq!(door.aliases, vec!["api".to_string()]);
    assert_eq!(sends(&door, "\"$api\" -fsS \"$u\""), 1);
    assert!(aliases_of("out=$(curl -s \"$u\")\nlocal x=\"$(\"$CURL\" -s)\"\n").is_empty());
    // Not `NAME=value`, and a name all the same (7369b078 F5).
    assert_eq!(aliases_of(": \"${api:=curl}\"\n"), vec!["api"]);
    assert_eq!(
        aliases_of("echo \"${door=/x/boss-api-curl.sh}\"\n"),
        vec!["door"]
    );
    assert_eq!(
        aliases_of("  api=$(command -v curl) || exit 1\n"),
        vec!["api"]
    );
    assert_eq!(aliases_of("api=\"$(which curl)\"\n"), vec!["api"]);
    assert_eq!(
        aliases_of("printf -v api '%s' \"$dir/curl\"\n"),
        vec!["api"]
    );
    assert_eq!(aliases_of("IFS= read -r api <<< curl\n"), vec!["api"]);
    assert!(aliases_of("# : \"${api:=curl}\"\n").is_empty());
    assert!(aliases_of("x=1 # ${api:=curl}\n").is_empty());
    assert!(aliases_of("printf -v msg '%s' \"$rc\"\nread -r line <<< \"$out\"\n").is_empty());
    assert!(aliases_of("command -v curl >/dev/null || exit 1\n").is_empty());
    // xargs hands curl its arguments on stdin.
    assert_eq!(sends(&none, "echo \"$u\" | xargs curl"), 1);
    assert_eq!(sends(&none, "echo \"$u\" | xargs -0 -n 1 -I {} curl"), 1);
    assert_eq!(sends(&none, "echo \"$u\" | xargs -r curl -fsS"), 1);
    assert_eq!(sends(&none, "find . -print0 | xargs -0 grep -l curl"), 0);
    assert_eq!(sends(&none, "for tool in jq xargs curl; do :; done"), 0);
    // Shell by extension, with no `#!` line.
    assert!(is_shell(Path::new("infra/lib/x.bash"), "f() { :; }\n"));
    assert!(is_shell(Path::new("infra/lib/x.inc"), "f() { :; }\n"));
    assert!(!is_shell(Path::new("infra/lib/x.txt"), "f() { :; }\n"));
    let cmds = commands("a \\\n  b\n# c\nd # e\n");
    assert_eq!(
        cmds,
        vec![(1, "a    b".to_string()), (4, "d # e".to_string())]
    );
    assert_eq!(
        before_comment("curl x # ${MT_HDR:+-H \"$MT_HDR\"}"),
        "curl x"
    );
}

// =====================================================================
// A LINT THAT READS THE RECORD, RUN BY A SCRIPT THAT HOLDS THE TOKEN
// (backlog 44b2087e, 2026-10-09)
// =====================================================================
//
// THE DEFECT. Everything above judges the commands a file runs itself,
// and excuses infra/lint/ whole, because a lint holds no token by rule
// (BY_DESIGN_DIRS). So a script that stamps every request it sends and
// then RUNS a lint that reads the record sends that lint's read with no
// token, and neither half of this pin sees it: the script's own curls
// are stamped and the lint's curl is in an excused file. Measured
// 2026-10-09 10:04Z on the machine-gate window (24 h): 24 would-refuse
// facts each for `GET /api/workflows` (jobs) and `GET /api/dispatcher/
// rules` (dispatcher), signed `automation:gate-runner` from the forge's
// address, one pair per converge, at the second the converge's last
// phase ran (09:19:38Z) — infra/forge/registry-drift.sh, which stamps
// its own reads and writes, running the two live-registry lints. The
// daily infra/protocol-drift.sh runs one of them the same way.
//
// THE RULE. The lints that send a request of their own are DERIVED: each
// shell file under infra/lint/ whose live text sources `lib/sor-read.sh`
// (the one helper a lint reads the record through, held by
// a_lint_read_of_the_record_is_signed.rs). In every shell file outside
// infra/lint/ that calls the reader (`machine_token_header <VAR>`), every
// command that RUNS one of them — `bash <it>`, `sh <it>`, or <it> as the
// command word, named literally or through a variable this file assigns
// its path — must hand it the read in that same command:
// `BOSS_LINT_SOR_HANDED_URL=…` (with `_CODE` and `_BODY`), which
// lint_sor_read answers from without a request, and which the script
// reads itself, stamped. A file that holds no token sends the same
// request either way and is not judged here.
//
// WHAT IT DOES NOT SEE. A lint run through a loop over the directory
// (infra/gate.sh's roster, which holds no token), through a name built at
// run time, or from Rust (boss-cli hands the edit-level lint its answer
// as BOSS_EDIT_LEVEL_ANSWER, backlog 934ccad1); and a stamping file that
// exports the handed variables once and runs the lint later — written
// per command on purpose, so the hand-over is read where the lint runs.

/// The helper a lint reads the record through.
const LINT_READ_LIB: &str = "lib/sor-read.sh";
/// The hand-over lint_sor_read answers from without a request.
const HANDED_READ: &str = "BOSS_LINT_SOR_HANDED_URL=";

/// The basenames of the lints that send a request of their own.
fn record_reading_lints(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    walk(&root.join("infra/lint"), &mut files);
    files.sort();
    files
        .into_iter()
        .filter(|f| f.extension().is_some_and(|x| x == "sh"))
        .filter(|f| {
            std::fs::read_to_string(f).is_ok_and(|t| live(&t).any(|l| l.contains(LINT_READ_LIB)))
        })
        .filter_map(|f| Some(f.file_name()?.to_string_lossy().into_owned()))
        .collect()
}

static LINT_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:^|[\s;(])([A-Za-z_][A-Za-z0-9_]*)=("[^"]*"|'[^']*'|[^\s;]+)"#).unwrap()
});

/// Variables this file assigns a path naming one of `lints`.
fn lint_vars(text: &str, lints: &[String]) -> Vec<String> {
    let mut out: Vec<String> = live(text)
        .flat_map(|l| LINT_ASSIGNMENT.captures_iter(l))
        .filter(|c| lints.iter().any(|b| c[2].contains(b.as_str())))
        .map(|c| c[1].to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Does this word name one of `lints`, literally or through one of `vars`?
fn names_a_lint(word: &str, lints: &[String], vars: &[String]) -> bool {
    lints.iter().any(|b| word.contains(b.as_str()))
        || vars.iter().any(|v| {
            [format!("${v}"), format!("${{{v}}}"), format!("${{{v}:")]
                .iter()
                .any(|r| {
                    word.match_indices(r.as_str()).any(|(i, m)| {
                        r.ends_with(['}', ':'])
                            || !word[i + m.len()..]
                                .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                    })
                })
        })
}

/// A word as the command it may start: what follows its last `$(`, with
/// the quote, `(` or `!` in front of it dropped. `out=$(bash` is `bash`;
/// `"$LINT"` is `$LINT"`, which still names its variable.
fn command_word(word: &str) -> &str {
    word.rsplit("$(")
        .next()
        .unwrap_or(word)
        .trim_start_matches(['(', '"', '\'', '!'])
}

/// Does this command RUN one of `lints`: as the argument of `bash` or
/// `sh` (after their flags), as the command word after any `NAME=value`
/// prefixes, or as the command word of a `$(…)`?
fn runs_a_lint(command: &str, lints: &[String], vars: &[String]) -> bool {
    let raw: Vec<&str> = before_comment(command).split_whitespace().collect();
    let first = raw.iter().position(|w| !ASSIGNMENT_WORD.is_match(w));
    raw.iter().enumerate().any(|(i, w)| {
        let word = command_word(w);
        let is_command_word = Some(i) == first || w.contains("$(");
        (is_command_word && names_a_lint(word, lints, vars))
            || (matches!(word, "bash" | "sh")
                && raw[i + 1..]
                    .iter()
                    .find(|a| !a.starts_with('-'))
                    .is_some_and(|a| names_a_lint(a, lints, vars)))
    })
}

static ASSIGNMENT_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*=").unwrap());

static READER_CALL_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|[\s;&|({!])machine_token_header\s+[A-Za-z_]+").unwrap());

/// (refused `file:line: command`, runs seen) for every stamping shell file
/// outside infra/lint/ under `root`.
fn unhanded_lint_runs(root: &Path) -> (Vec<String>, usize) {
    let lints = record_reading_lints(root);
    let mut files = Vec::new();
    walk(&root.join("infra"), &mut files);
    files.sort();
    let mut refused = Vec::new();
    let mut runs = 0;
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let rel = f.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        if !is_shell(&f, &text)
            || rel.starts_with("infra/lint/")
            || !live(&text).any(|l| READER_CALL_LINE.is_match(l))
        {
            continue;
        }
        let vars = lint_vars(&text, &lints);
        for (n, command) in commands(&text) {
            if !runs_a_lint(&command, &lints, &vars) {
                continue;
            }
            runs += 1;
            if !before_comment(&command).contains(HANDED_READ) {
                let shown = command.split_whitespace().collect::<Vec<_>>().join(" ");
                refused.push(format!("{rel}:{n}: {shown}"));
            }
        }
    }
    (refused, runs)
}

#[test]
fn a_stamping_script_hands_the_lints_it_runs_their_read() {
    let root = repo_root();
    assert!(
        record_reading_lints(&root).len() >= 2,
        "fewer than two lints under infra/lint/ source {LINT_READ_LIB} — the derivation has \
         stopped seeing the tree: {:?}",
        record_reading_lints(&root)
    );
    let (refused, runs) = unhanded_lint_runs(&root);
    assert!(
        refused.is_empty(),
        "these commands, in a script that holds the estate machine token, run a lint that reads \
         the record itself — and a lint holds no token by rule, so that read reaches a service \
         port with none, a caller every machine gate records as one it would refuse (the \
         converge's registry-drift phase sent one pair per converge, backlog 44b2087e). Read it \
         in this script, stamped, and hand it in on the same command: \
         `BOSS_LINT_SOR_HANDED_URL=<the URL the lint asks> BOSS_LINT_SOR_HANDED_CODE=<code> \
         BOSS_LINT_SOR_HANDED_BODY=<file> bash <lint>` (infra/lint/lib/sor-read.sh):\n  {}",
        refused.join("\n  ")
    );
    assert!(
        runs >= 3,
        "the scan found {runs} runs of a record-reading lint from a stamping script, fewer than \
         the three it was written against (infra/forge/registry-drift.sh twice, \
         infra/protocol-drift.sh once) — it has stopped seeing them"
    );
}

/// The scan, on a planted tree. Never executed.
#[test]
fn an_unhanded_lint_run_is_refused_however_it_is_spelled() {
    let root = boss_testing::scratch_dir("unhanded-lint-runs");
    let lint = root.join("infra/lint");
    let forge = root.join("infra/forge");
    std::fs::create_dir_all(&lint).unwrap();
    std::fs::create_dir_all(&forge).unwrap();
    // Shell by their `.sh` name; read as text, never run.
    for (name, text) in [
        ("the-live-x.sh", ". infra/lint/lib/sor-read.sh || exit 3\n"),
        (
            "hermetic.sh",
            "# reads lib/sor-read.sh? no: this is a comment\nexit 0\n",
        ),
    ] {
        std::fs::write(lint.join(name), text).unwrap();
    }
    let stamps = "#!/usr/bin/env bash\nmachine_token_header MT_HDR \"$BASE\" || MT_HDR=\"\"\n";
    let cases: &[(&str, &str, &str, usize)] = &[
        (
            "by-variable.sh",
            stamps,
            "LINT=\"infra/lint/the-live-x.sh\"\nbash \"$LINT\" --require-live > \"$t/out\" 2>&1\n",
            1,
        ),
        ("by-path.sh", stamps, "bash infra/lint/the-live-x.sh\n", 1),
        (
            "in-a-substitution.sh",
            stamps,
            "PLINT=infra/lint/the-live-x.sh\nout=$(bash \"$REPO/$PLINT\" 2>&1) || rc=$?\n",
            1,
        ),
        (
            "env-prefix-without-the-read.sh",
            stamps,
            "BOSS_JOBS_URL=\"$B\" bash \\\n    \"$REPO/infra/lint/the-live-x.sh\" --require-live\n",
            1,
        ),
        (
            "as-the-command-word.sh",
            stamps,
            "L=\"$REPO/infra/lint/the-live-x.sh\"\n\"$L\" --require-live\n",
            1,
        ),
        (
            "handed.sh",
            stamps,
            "LINT=infra/lint/the-live-x.sh\nBOSS_LINT_SOR_HANDED_URL=\"$B/api/x\" BOSS_LINT_SOR_HANDED_CODE=\"$c\" \\\n    BOSS_LINT_SOR_HANDED_BODY=\"$t/x.json\" bash \"$LINT\" --require-live\n",
            0,
        ),
        (
            "handed-only-in-a-comment.sh",
            stamps,
            "bash infra/lint/the-live-x.sh # BOSS_LINT_SOR_HANDED_URL=\"$u\"\n",
            1,
        ),
        (
            "a-test-of-the-file-is-no-run.sh",
            stamps,
            "LINT=infra/lint/the-live-x.sh\n[ -f \"$LINT\" ] || exit 3\necho \"$(basename \"$LINT\")\"\n",
            0,
        ),
        (
            "a-longer-name-is-another-variable.sh",
            stamps,
            "LINT=infra/lint/the-live-x.sh\nbash \"$LINT_OTHER\"\n",
            0,
        ),
        (
            "a-hermetic-lint.sh",
            stamps,
            "bash infra/lint/hermetic.sh\n",
            0,
        ),
        (
            "holds-no-token.sh",
            "#!/usr/bin/env bash\n",
            "bash infra/lint/the-live-x.sh\n",
            0,
        ),
    ];
    for (name, head, body, _) in cases {
        std::fs::write(forge.join(name), format!("{head}{body}")).unwrap();
    }
    let (refused, _) = unhanded_lint_runs(&root);
    for (name, _, _, want) in cases {
        let got = refused
            .iter()
            .filter(|l| l.starts_with(&format!("infra/forge/{name}:")))
            .count();
        assert_eq!(got, *want, "{name}: refused {got} time(s): {refused:#?}");
    }
    assert!(
        refused
            .iter()
            .any(|l| l.starts_with("infra/forge/by-path.sh:3:")),
        "the refusal names the line the run is on: {refused:#?}"
    );
}
