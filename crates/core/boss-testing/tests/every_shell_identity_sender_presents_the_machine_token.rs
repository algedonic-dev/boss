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
        "infra/cluster/dev-scratch-reclaim.sh",
        "the dev pod's sidecar: the pod's token is moving behind an uncredentialed worker (design c395e62c), decided with that isolation",
    ),
    (
        "infra/forge/probe-bin/boss-sor-read",
        "the recorded probe's reader runs car-supplied shell on the forge and must not hold the estate token; its own reader door is backlog d26515c5",
    ),
    (
        "infra/forge/recovery-kit-read.sh",
        "installed root-owned outside the checkout and cannot source the reader (its own header says so)",
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
/// Three remain (measured 2026-10-08 on origin/main 07dc168f1 and on the
/// live record). Two for one reason: each is run by a unit whose
/// ExecStart is a copy installed BY HAND at /usr/local/lib/boss, outside
/// any checkout, so `../lib/secret-header.sh` is not beside it; neither
/// unit is in a role of infra/estate/roles.toml; and neither identity
/// appears once in 72 hours of `machine_gate.would_refuse` facts.
/// Stamping the tree's copy would turn this pin green over an installed
/// copy that presents nothing. The third is the one of the three that
/// IS on the live record, and it is held back by a binding, not by an
/// install: see its entry. The three other files of the roster car
/// 217c2d66 left were stamped on 2026-10-08 (backlog 44b2087e):
/// discover-admission-source.sh, the ML batch and the Python token audit.
const NOT_YET: &[(&str, &str)] = &[
    (
        "infra/codebase-observe/observe-codebase.sh",
        "its unit runs a copy installed by hand at /usr/local/lib/boss/observe-codebase.sh, with no reader lib beside it; in no role of infra/estate/roles.toml and on no host (0 would-refuse facts as automation:observe-codebase in 72 hours to 2026-10-08, 0 packets on 2026-10-07). Leaves this list when its unit's ExecStart is the checkout's own copy under a role (then it is stamped like infra/codebase-metrics.sh, one call before its one POST), or when the script and its unit are deleted",
    ),
    (
        "infra/forge/probe-admission-source-access.sh",
        "runs on the forge under the ops runner (root, the token readable) and is on the live record: 1 would-refuse fact as automation:probe-admission-source-access, GET /api/estate/nodes, 2026-10-05T18:43Z. Not stamped here because its bytes and every file it sources are a BOUND METHOD of design f927de0c: infra/forge/admission-reader-identity.py METHODS hashes this script as `host` and holds its consumed closure equal to the table (admission-identity-test.py test_method_table_equals_the_derived_consumed_closure went red the moment this script sourced the reader), so the stamp changes the identity that design's independent source review is to release. Leaves this list when a car under design f927de0c stamps it exactly as discover-admission-source.sh is stamped (one reader call before the read, the header file closed right after it) AND adds ../lib/secret-header.sh to METHODS, so the reviewed identity is the stamped one",
    ),
    (
        "infra/install-smoke/nightly.sh",
        "as observe-codebase.sh: a hand-installed copy at /usr/local/lib/boss/install-smoke-nightly.sh with no reader lib beside it, in no role and on no host (0 would-refuse facts as automation:install-smoke in 72 hours to 2026-10-08); and its two requests are sent from inside its EXIT trap, the one shape the reader's own cleanup cannot serve without secret_header_close. Leaves this list when its unit's ExecStart is the checkout's own copy under a role (then both requests are stamped and the trap calls secret_header_close after the second, as cluster-deploy-lib.sh answer_converge_requests does), or when the script and its unit are deleted",
    ),
];

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

fn is_shell(path: &Path, text: &str) -> bool {
    if path.extension().is_some_and(|x| x == "sh") {
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
        .filter(|(rel, signs, presents)| *signs && !*presents && !listed(rel))
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

/// Sends that do not go to a gated service port: another party's API, a
/// download, a registry, a local stack, or a path the gate exempts.
const NOT_THE_ESTATE: &[Site] = &[
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
    site(
        "infra/forge/ci-image-report.sh",
        "\"$($CURL -sS -m 15",
        3,
        "the forge's container registry: its ping, its token endpoint and a tag read",
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
        "infra/forge/journal-read.sh",
        "curl -s -m",
        3,
        "a host's systemd-journal-gatewayd, not a service port",
    ),
    site(
        "infra/forge/locomotive.sh",
        "-H \"$FORGE_HDR\"",
        1,
        "the forge's API, with the forge token",
    ),
    site(
        "infra/forge/move-forgejo-data.sh",
        "code=$(curl -sS --max-time",
        3,
        "the forge's own healthz and its container registry",
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
        "claude) curl -fsSL https://claude.ai/install.sh | bash",
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
        "answer=$(curl \"$@\")",
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
        "echo \"curl -fsSL https://claude.ai/install.sh",
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
fn sends(reader: &Reader, command: &str) -> usize {
    let words: Vec<&str> = command.split_whitespace().collect();
    words
        .windows(2)
        .filter(|w| {
            is_curl_word(w[0], &reader.aliases)
                && (w[1].starts_with(['-', '"', '$', '@', '\'']) || w[1].starts_with("http"))
        })
        .count()
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
fn aliases_of(text: &str) -> Vec<String> {
    live(text)
        .flat_map(|l| {
            ASSIGNMENT
                .captures_iter(before_comment(l))
                .filter(|c| {
                    let value = c[2].to_ascii_lowercase();
                    value.contains("curl") && !value.contains("$(") && !value.contains('`')
                })
                .map(|c| c[1].to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

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
    NOT_THE_ESTATE.iter().chain(NOT_A_SEND).chain(OWED)
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
                    .find(|s| s.file == rel && command.contains(s.needle))
            {
                refused.push(format!(
                    "{rel}:{n}: hands the estate token's header to a request NOT_THE_ESTATE names ({}): {shown}",
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
    ];
    for (name, body) in refused_cases {
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

/// The token never rides a request the roster says leaves the estate: the
/// header file is filled for one host, and nothing at the request checks
/// where THIS request goes.
#[test]
fn a_rostered_request_out_of_the_estate_may_not_carry_the_header() {
    let entry = &NOT_THE_ESTATE[0];
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
        &root.join("infra/forge/some-other-sender.sh"),
        &format!("#!/usr/bin/env bash\n{line}"),
    );
    let (refused, _) = unstamped_sends(&root);
    assert_eq!(
        refused.len(),
        2,
        "the same line in another file is refused: {refused:#?}"
    );
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
